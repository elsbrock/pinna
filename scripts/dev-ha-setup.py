"""Set up the local Home Assistant for the virtual Echo Dot (just dev-ha).

Idempotent: onboarding once (a local account, its password and token kept in
build/dev-ha/credentials.json), then Wyoming Whisper and Piper, the virtual
Dot (ESPHome at 127.0.0.1), and an Assist pipeline that uses them as the
preferred one. Steps that are done already are skipped.

  python dev-ha-setup.py WORK_DIR ESPHOME_PORT
"""

import asyncio
import json
import secrets
import socket
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path

import websockets

HA = "http://127.0.0.1:8123"
CLIENT_ID = f"{HA}/"
WORK = Path(sys.argv[1])
ESPHOME_PORT = int(sys.argv[2])
CREDENTIALS = WORK / "credentials.json"
WHISPER, PIPER = 10300, 10200
PIPELINE = "Pinna (dev)"


def say(msg: str):
    print(f"     {msg}", flush=True)


def request(method: str, path: str, body=None, token: str | None = None, form: bool = False):
    headers = {}
    data = None
    if body is not None:
        if form:
            data = urllib.parse.urlencode(body).encode()
            headers["Content-Type"] = "application/x-www-form-urlencoded"
        else:
            data = json.dumps(body).encode()
            headers["Content-Type"] = "application/json"
    if token:
        headers["Authorization"] = f"Bearer {token}"
    req = urllib.request.Request(HA + path, data=data, headers=headers, method=method)
    with urllib.request.urlopen(req, timeout=30) as r:
        text = r.read()
        return json.loads(text) if text else None


def port_open(port: int) -> bool:
    try:
        with socket.create_connection(("127.0.0.1", port), timeout=1):
            return True
    except OSError:
        return False


def wait_for(what: str, ok, timeout: float):
    end = time.monotonic() + timeout
    while not ok():
        if time.monotonic() > end:
            raise SystemExit(f"timed out waiting for {what}")
        time.sleep(2)


def onboarding_done() -> set[str]:
    try:
        return {s["step"] for s in request("GET", "/api/onboarding") if s["done"]}
    except urllib.error.HTTPError as e:
        if e.code == 404:  # onboarding finished long ago
            return {"user", "core_config", "analytics", "integration"}
        raise


def token() -> str:
    """An access token: onboard the owner the first time, refresh after."""
    done = onboarding_done()
    if "user" not in done:
        creds = {"username": "pinna", "password": secrets.token_urlsafe(18)}
        code = request(
            "POST",
            "/api/onboarding/users",
            {"client_id": CLIENT_ID, "name": "Pinna", "language": "en", **creds},
        )["auth_code"]
        tokens = request(
            "POST",
            "/auth/token",
            {"grant_type": "authorization_code", "code": code, "client_id": CLIENT_ID},
            form=True,
        )
        creds["refresh_token"] = tokens["refresh_token"]
        CREDENTIALS.write_text(json.dumps(creds, indent=2) + "\n")
        CREDENTIALS.chmod(0o600)
        say(f"owner account created (user pinna, password in {CREDENTIALS})")
        access = tokens["access_token"]
    else:
        if not CREDENTIALS.exists():
            raise SystemExit(f"Home Assistant is onboarded but {CREDENTIALS} is missing: FRESH=1 just dev-ha")
        creds = json.loads(CREDENTIALS.read_text())
        access = request(
            "POST",
            "/auth/token",
            {"grant_type": "refresh_token", "refresh_token": creds["refresh_token"], "client_id": CLIENT_ID},
            form=True,
        )["access_token"]
    for step, body in [
        ("core_config", {}),
        ("analytics", {}),
        ("integration", {"client_id": CLIENT_ID, "redirect_uri": f"{HA}/?auth_callback=1"}),
    ]:
        if step not in done:
            request("POST", f"/api/onboarding/{step}", body, access)
    return access


def add(access: str, handler: str, data: dict, label: str):
    flow = request(
        "POST", "/api/config/config_entries/flow", {"handler": handler, "show_advanced_options": False}, access
    )
    if flow.get("type") == "abort":
        say(f"{label}: {flow.get('reason')}")
        return
    result = request("POST", f"/api/config/config_entries/flow/{flow['flow_id']}", data, access)
    kind = result.get("type")
    if kind == "create_entry":
        say(f"{label}: added")
    elif kind == "abort":
        say(f"{label}: {result.get('reason')}")
    else:
        # A step this script does not know (for example an encryption key): leave it to the UI.
        request("DELETE", f"/api/config/config_entries/flow/{flow['flow_id']}", None, access)
        raise SystemExit(f"{label}: Home Assistant asks for {result.get('step_id')!r}; add it in the UI")


async def speech_and_pipeline(access: str):
    """Whisper and Piper unless Home Assistant has their entities already (a
    second Wyoming entry for the same server is not refused), then the pipeline."""
    async with websockets.connect(HA.replace("http", "ws") + "/api/websocket") as ws:
        await ws.recv()
        await ws.send(json.dumps({"type": "auth", "access_token": access}))
        assert json.loads(await ws.recv())["type"] == "auth_ok"
        n = 0

        async def call(msg: dict):
            nonlocal n
            n += 1
            await ws.send(json.dumps({"id": n, **msg}))
            while True:
                r = json.loads(await ws.recv())
                if r.get("id") == n:
                    if not r.get("success"):
                        raise SystemExit(f"{msg['type']}: {r.get('error')}")
                    return r.get("result")

        async def wyoming() -> dict[str, str]:
            found: dict[str, str] = {}
            for e in await call({"type": "config/entity_registry/list"}):
                kind = e["entity_id"].split(".")[0]
                if e["platform"] == "wyoming" and kind in ("stt", "tts"):
                    found.setdefault(kind, e["entity_id"])
            return found

        have = await wyoming()
        if "stt" in have:
            say(f"Whisper (speech to text): already there ({have['stt']})")
        else:
            add(access, "wyoming", {"host": "127.0.0.1", "port": WHISPER}, "Whisper (speech to text)")
        if "tts" in have:
            say(f"Piper (text to speech): already there ({have['tts']})")
        else:
            add(access, "wyoming", {"host": "127.0.0.1", "port": PIPER}, "Piper (text to speech)")
        engines: dict[str, str] = {}
        for _ in range(30):
            engines = await wyoming()
            if len(engines) == 2:
                break
            await asyncio.sleep(2)
        else:
            raise SystemExit(f"no Wyoming speech-to-text and text-to-speech entities ({engines})")
        spec = {
            "name": PIPELINE,
            "language": "en",
            "conversation_engine": "conversation.home_assistant",
            "conversation_language": "en",
            "stt_engine": engines["stt"],
            "stt_language": "en",
            "tts_engine": engines["tts"],
            "tts_language": "en_US",
            "tts_voice": "en_US-lessac-medium",
            "wake_word_entity": None,
            "wake_word_id": None,
        }
        listed = await call({"type": "assist_pipeline/pipeline/list"})
        mine = next((p for p in listed["pipelines"] if p["name"] == PIPELINE), None)
        if mine:
            await call({"type": "assist_pipeline/pipeline/update", "pipeline_id": mine["id"], **spec})
            pid = mine["id"]
        else:
            pid = (await call({"type": "assist_pipeline/pipeline/create", **spec}))["id"]
        await call({"type": "assist_pipeline/pipeline/set_preferred", "pipeline_id": pid})
        say(f"Assist pipeline '{PIPELINE}' ({engines['stt']} -> conversation -> {engines['tts']}) is preferred")


def main():
    say("waiting for Home Assistant")
    wait_for("Home Assistant on :8123", lambda: port_open(8123), 300)
    wait_for("Home Assistant's API", lambda: _answers(), 300)
    access = token()
    say("waiting for Whisper and Piper (the first start downloads their models)")
    wait_for("Whisper on :10300", lambda: port_open(WHISPER), 900)
    wait_for("Piper on :10200", lambda: port_open(PIPER), 900)
    asyncio.run(speech_and_pipeline(access))
    if port_open(ESPHOME_PORT):
        add(access, "esphome", {"host": "127.0.0.1", "port": ESPHOME_PORT}, "virtual Echo Dot (ESPHome)")
    else:
        say(f"the virtual Echo Dot is not running (nothing on 127.0.0.1:{ESPHOME_PORT}): start just dev-vm, then run just dev-ha again")


def _answers() -> bool:
    try:
        onboarding_done()
        return True
    except (urllib.error.URLError, ConnectionError, TimeoutError):
        return False


main()
