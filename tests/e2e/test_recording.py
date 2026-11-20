"""End-to-end recording across a reply that the wake word interrupts (ADR 0030).

pinnad runs with a dashboard password (seeded in secrets.toml, with an
encryption key so the ESPHome API stays open), the web listener and the wake
word fixture ("Alexa", twice). The test plays the owner: it signs in, asks for
a recording confirmation, presses Action (a line on pinnad's stdin) and reads
`RecordAudio` over Connect's streaming JSON. Meanwhile it plays Home Assistant,
whose requests run as usual: the first gets a long reply, which the second
"Alexa" interrupts.

  login -> StartConfirmation(RECORDING) -> Action -> grant -> RecordAudio
  -> the second wake word interrupts the reply during the recording
  -> StopRecording -> `ended` (STOPPED)

Run: just e2e   (needs `just models`)
"""

import asyncio
import base64
import hashlib
import http.client
import json
import os
import struct
import subprocess
import tempfile
import threading
import time
from pathlib import Path

import pytest
from aioesphomeapi import APIClient, VoiceAssistantEventType
from aioesphomeapi.model import Event

from test_voice_flow import FIXTURE, MODELS, PINNAD, GatedReply, free_port

PASSWORD = "correct horse"
KEY = base64.b64encode(bytes(range(32))).decode()
DEVICE = "/pinna.v1.DeviceService/"


def seed_password(path: Path):
    """The dashboard table of secrets.toml, as pinnad writes it."""
    salt = bytes(range(16))
    iterations = 1000
    digest = hashlib.pbkdf2_hmac("sha256", PASSWORD.encode(), salt, iterations)
    secrets = path.parent / "secrets.toml"
    secrets.write_text(
        "[dashboard]\n"
        f'hash = "{base64.b64encode(digest).decode()}"\n'
        f'salt = "{base64.b64encode(salt).decode()}"\n'
        f"iterations = {iterations}\n"
        f'session_key = "{base64.b64encode(bytes(range(32, 64))).decode()}"\n'
    )
    secrets.chmod(0o600)


@pytest.fixture
def pinnad_with_password():
    if not MODELS.exists():
        pytest.skip("wake word models missing (just models)")
    esphome, web = free_port(), free_port()
    with tempfile.TemporaryDirectory() as d:
        settings = Path(d) / "settings.toml"
        seed_password(settings)
        cfg = Path(d) / "pinnad.toml"
        cfg.write_text(
            f"""
[device]
name = "pinna-e2e"
friendly_name = "E2E"
[audio]
backend = "wav:{FIXTURE}"
[wake]
models = "{MODELS}"
[esphome]
port = {esphome}
mdns = false
encryption_key = "{KEY}"
[web]
listen = "127.0.0.1:{web}"
root = "{d}"
[leds]
backend = "none"
"""
        )
        logf = open(Path(d) / "pinnad.log", "w+")
        proc = subprocess.Popen(
            [
                str(PINNAD),
                "--config",
                str(cfg),
                "--local-config",
                str(Path(d) / "local.toml"),
                "--settings",
                str(settings),
                "--stdin-buttons",
            ],
            stdin=subprocess.PIPE,
            stdout=logf,
            stderr=subprocess.STDOUT,
            env={
                **{k: v for k, v in os.environ.items() if k != "JOURNAL_STREAM"},
                "RUST_LOG": os.environ.get("RUST_LOG", "info"),
            },
        )
        try:
            yield esphome, web, proc
        finally:
            proc.terminate()
            proc.wait(timeout=10)
            logf.seek(0)
            print("---- pinnad log ----\n" + logf.read())


def unary(web: int, method: str, body: dict, cookie: str | None = None) -> dict:
    conn = http.client.HTTPConnection("127.0.0.1", web, timeout=20)
    headers = {"Content-Type": "application/json", "Connect-Protocol-Version": "1"}
    if cookie:
        headers["Cookie"] = cookie
    conn.request("POST", DEVICE + method, json.dumps(body), headers)
    res = conn.getresponse()
    data = json.loads(res.read() or b"{}")
    assert res.status == 200, (method, res.status, data)
    return data


def envelopes(res: http.client.HTTPResponse):
    """The JSON messages of a Connect stream, until its end frame."""
    while True:
        head = res.read(5)
        if len(head) < 5:
            return
        flags, size = head[0], struct.unpack(">I", head[1:])[0]
        payload = json.loads(res.read(size) or b"{}")
        if flags & 2:
            assert "error" not in payload, payload
            return
        yield payload


def stream(web: int, method: str, body: dict, cookie: str):
    conn = http.client.HTTPConnection("127.0.0.1", web, timeout=30)
    payload = json.dumps(body).encode()
    conn.request(
        "POST",
        DEVICE + method,
        b"\0" + struct.pack(">I", len(payload)) + payload,
        {
            "Content-Type": "application/connect+json",
            "Connect-Protocol-Version": "1",
            "Cookie": cookie,
        },
    )
    res = conn.getresponse()
    assert res.status == 200, (method, res.status, res.read())
    return conn, envelopes(res)


def login(web: int) -> str:
    # pinnad is still starting on the first call.
    for _ in range(200):
        conn = http.client.HTTPConnection("127.0.0.1", web, timeout=20)
        try:
            conn.request(
                "POST",
                "/auth/login",
                json.dumps({"password": PASSWORD}),
                {"Content-Type": "application/json"},
            )
            break
        except ConnectionRefusedError:
            time.sleep(0.1)
    res = conn.getresponse()
    res.read()
    assert res.status == 204, res.status
    return res.getheader("Set-Cookie").split(";")[0]


def record(web: int, proc: subprocess.Popen, cookie: str, out: dict, ready: threading.Event):
    """The owner's side: confirm at the device, record until told to stop."""
    asked = unary(web, "StartConfirmation", {"purpose": "CONFIRMATION_PURPOSE_RECORDING"}, cookie)
    # A signed-in dashboard sees what waits, and what for.
    security = unary(web, "GetSecurity", {}, cookie)["security"]
    assert security["confirmationPurpose"] == "CONFIRMATION_PURPOSE_RECORDING", security
    proc.stdin.write(b"action\n")
    proc.stdin.flush()
    _, events = stream(web, "WatchConfirmation", {"id": asked["id"]}, cookie)
    grant = None
    for event in events:
        if event.get("state") == "CONFIRMATION_STATE_CONFIRMED":
            grant = event["grant"]
    assert grant, "Action was not pressed"

    conn, events = stream(web, "RecordAudio", {"grant": grant, "seconds": 30}, cookie)
    first = next(events)
    assert first["started"]["sampleRate"] == 16000, first
    assert not first["started"].get("debug"), "the wav backend has no lanes"
    out["began"] = time.monotonic()
    ready.set()
    heard = bytearray()
    for event in events:
        if "chunk" in event:
            assert event["chunk"]["source"] == "AUDIO_SOURCE_HEARD", event
            heard += base64.b64decode(event["chunk"]["data"])
        else:
            out["ended"] = event["ended"]
            break
    out["heard"] = bytes(heard)
    conn.close()


async def play_home_assistant(esphome: int, out: dict):
    """Home Assistant's side: answer the first request with a long reply,
    which the fixture's second "Alexa" interrupts, as in
    test_wake_word_interrupts_a_reply."""
    client = APIClient("127.0.0.1", esphome, None, noise_psk=KEY)
    for _ in range(50):
        try:
            await client.connect(login=True)
            break
        except Exception:
            await asyncio.sleep(0.1)
    await client.device_info()
    entities, _ = await client.list_entities_services()
    voice_key = next(e.key for e in entities if e.object_id == "voice_assistant")
    events: list[str] = []

    def on_state(state):
        if isinstance(state, Event) and state.key == voice_key and state.event_type:
            events.append(state.event_type)

    client.subscribe_states(on_state)
    starts: list[float] = []
    first = asyncio.Event()
    second = asyncio.Event()

    async def handle_start(conversation_id, flags, audio_settings, wake_word_phrase):
        starts.append(time.monotonic())
        (first if len(starts) == 1 else second).set()
        return 0

    async def handle_stop(abort):
        pass

    async def handle_audio(data: bytes, data2: bytes = b""):
        pass

    unsub = client.subscribe_voice_assistant(
        handle_start=handle_start, handle_stop=handle_stop, handle_audio=handle_audio
    )
    E = VoiceAssistantEventType
    reply = GatedReply()
    try:
        await asyncio.wait_for(first.wait(), timeout=15)
        client.send_voice_assistant_event(E.VOICE_ASSISTANT_RUN_START, {"url": reply.url})
        client.send_voice_assistant_event(E.VOICE_ASSISTANT_STT_VAD_END, {})
        client.send_voice_assistant_event(E.VOICE_ASSISTANT_INTENT_PROGRESS, {"tts_start_streaming": "1"})
        client.send_voice_assistant_event(E.VOICE_ASSISTANT_INTENT_END, {"conversation_id": "c"})
        client.send_voice_assistant_event(E.VOICE_ASSISTANT_TTS_END, {"url": reply.url})
        client.send_voice_assistant_event(E.VOICE_ASSISTANT_RUN_END, {})
        # A long reply, still sounding when the second "Alexa" (~8.7 s) comes.
        await asyncio.sleep(1)
        assert reply.open.is_set(), "the reply was not requested"
        reply.release(10)
        await asyncio.wait_for(second.wait(), timeout=15)
    finally:
        reply.shutdown()
        unsub()
        await client.disconnect()
    out["events"] = events
    return starts


def test_a_recording_runs_across_an_interrupted_reply(pinnad_with_password):
    esphome, web, proc = pinnad_with_password
    out: dict = {}
    ready = threading.Event()
    # One sign-in: pinnad spaces password attempts out.
    cookie = login(web)
    owner = threading.Thread(target=lambda: record(web, proc, cookie, out, ready), daemon=True)
    owner.start()

    async def run():
        ha = asyncio.create_task(play_home_assistant(esphome, out))
        await asyncio.wait_for(asyncio.to_thread(ready.wait, 30), 35)
        assert ready.is_set(), "the recording did not start"
        # The Overview says who records.
        status = unary(web, "GetStatus", {}, cookie)["status"]
        assert status["recording"]["from"] == "127.0.0.1", status
        starts = await asyncio.wait_for(ha, timeout=40)
        return starts

    starts = asyncio.run(run())
    assert proc.poll() is None, "pinnad exited"
    # The wake word still works during a recording: it interrupted the reply
    # and started a new request while the recording ran.
    assert len(starts) == 2 and starts[1] > out["began"], (starts, out["began"])
    events = out["events"]
    assert "replying" in events and events.index("replying") < events.index("stopped"), events

    unary(web, "StopRecording", {}, cookie)
    owner.join(timeout=15)
    assert not owner.is_alive(), "the recording did not end"
    assert out["ended"]["reason"] == "RECORDING_END_STOPPED", out["ended"]
    heard = out["heard"]
    # At least the 8 seconds up to the second wake word, in 16-bit mono.
    assert len(heard) >= 16000 * 2 * 6, len(heard)
    peak = max(abs(s) for s in struct.unpack(f"<{len(heard) // 2}h", heard))
    assert peak > 1000, f"the recording holds no speech (peak {peak})"
    # The grant was used once: the recording ended, a new one is a new press.
    status = unary(web, "GetStatus", {}, cookie)["status"]
    assert "recording" not in status, status
