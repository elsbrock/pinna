"""End-to-end standalone request against a real pinnad process (ADR 0017).

pinnad starts in standalone mode with its three services on a fake
OpenAI-compatible server that this test runs. The microphone is a WAV made
from the wake word fixture's first "Alexa" and a tone standing in for the
request, so the energy VAD hears speech and its end. The request then runs:

  wake word -> end of speech -> POST /audio/transcriptions
  -> POST /chat/completions (the brain calls set_timer)
  -> POST /chat/completions (the brain answers)
  -> POST /audio/speech -> the reply plays -> back to idle.

Home Assistant's client sees no voice assistant features.

Run: just e2e   (needs `just models`)
"""

import array
import asyncio
import http.client
import http.server
import io
import json
import math
import struct
import threading
import time
import urllib.error
import wave
from pathlib import Path

from aioesphomeapi import APIClient

from test_voice_flow import FIXTURE, free_port, rpc, start_pinnad

KEY = "sk-e2e-0123456789abcdef"
TRANSCRIPT = "set a tea timer for one minute"
ANSWER = "Your tea timer runs for one minute."


def request_wav(dst: Path, lead: float = 0.0):
    """`lead` seconds of silence, the fixture's first wake word, then a 1.5 s
    tone and silence."""
    with wave.open(str(FIXTURE)) as w:
        params = w.getparams()
        rate = w.getframerate()
        head = w.readframes(int(3.2 * rate))
    tone = array.array(
        "h", (int(9000 * math.sin(2 * math.pi * 300 * i / rate)) for i in range(int(1.5 * rate)))
    )
    silence = bytes(2 * int(0.3 * rate))
    with wave.open(str(dst), "wb") as w:
        w.setparams(params)
        w.writeframes(bytes(2 * int(lead * rate)) + head + silence + tone.tobytes() + bytes(2 * 4 * rate))


def reply_wav() -> bytes:
    """Half a second of quiet audio as the spoken reply."""
    out = io.BytesIO()
    with wave.open(out, "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(16000)
        w.writeframes(bytes(2 * 8000))
    return out.getvalue()


class Provider(http.server.ThreadingHTTPServer):
    """A fake OpenAI-compatible service that records what it was sent."""

    def __init__(self):
        super().__init__(("127.0.0.1", 0), ProviderHandler)
        self.requests: list[dict] = []
        self.chats = 0
        self.lock = threading.Lock()


class ProviderHandler(http.server.BaseHTTPRequestHandler):
    server: Provider

    def log_message(self, *args):
        pass

    def record(self, body: bytes):
        with self.server.lock:
            self.server.requests.append(
                {
                    "method": self.command,
                    "path": self.path,
                    "auth": self.headers.get("Authorization"),
                    "type": self.headers.get("Content-Type", ""),
                    "body": body,
                }
            )

    def reply(self, status: int, body: bytes, content_type: str):
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        self.record(b"")
        models = {"data": [{"id": "whisper-1"}, {"id": "gpt-test"}]}
        self.reply(200, json.dumps(models).encode(), "application/json")

    def do_POST(self):
        body = self.rfile.read(int(self.headers.get("Content-Length", "0")))
        self.record(body)
        if self.path == "/v1/audio/transcriptions":
            self.reply(200, json.dumps({"text": TRANSCRIPT}).encode(), "application/json")
        elif self.path == "/v1/chat/completions":
            with self.server.lock:
                self.server.chats += 1
                first = self.server.chats == 1
            if first:
                call = {
                    "index": 0,
                    "id": "call_1",
                    "type": "function",
                    "function": {"name": "set_timer", "arguments": '{"seconds": 60, "name": "tea"}'},
                }
                chunks = [{"choices": [{"delta": {"tool_calls": [call]}}]}]
            else:
                chunks = [{"choices": [{"delta": {"content": ANSWER}}]}]
            chunks.append({"choices": [], "usage": {"prompt_tokens": 120, "completion_tokens": 9}})
            sse = "".join(f"data: {json.dumps(c)}\n\n" for c in chunks) + "data: [DONE]\n\n"
            self.reply(200, sse.encode(), "text/event-stream")
        elif self.path == "/v1/audio/speech":
            self.reply(200, reply_wav(), "audio/wav")
        else:
            self.reply(404, b"", "text/plain")


def seed(settings_dir: Path, base_url: str):
    """Standalone mode with all three services checked on `base_url`."""
    (settings_dir / "standalone.toml").write_text(
        f"""follow_ups = false

[[connections]]
id = "custom-1"
preset = "custom"
base_url = "{base_url}"
revision = 1

[stt]
connection = "custom-1"
model = "whisper-1"
checked = 1

[chat]
connection = "custom-1"
model = "gpt-test"
checked = 1

[tts]
connection = "custom-1"
model = "nova"
checked = 1
"""
    )
    secrets = settings_dir / "secrets.toml"
    secrets.write_text(f'[connections.custom-1]\nurl = "{base_url}"\nkey = "{KEY}"\n')
    secrets.chmod(0o600)


async def no_voice_for_home_assistant(port: int):
    client = APIClient("127.0.0.1", port, None)
    for _ in range(50):
        try:
            await client.connect(login=True)
            break
        except Exception:
            await asyncio.sleep(0.1)
    info = await client.device_info()
    await client.disconnect()
    return info.voice_assistant_feature_flags


def wait_for(what: str, ok, timeout: float):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = ok()
        if value:
            return value
        time.sleep(0.1)
    raise AssertionError(f"timed out waiting for {what}")


def test_a_spoken_request_runs_on_the_owners_services(tmp_path):
    provider = Provider()
    threading.Thread(target=provider.serve_forever, daemon=True).start()
    base_url = f"http://127.0.0.1:{provider.server_port}/v1"
    mic = tmp_path / "request.wav"
    request_wav(mic)
    api = free_port()
    runs = start_pinnad(
        settings='mode = "standalone"\n',
        extra=f'[api]\nlisten = "127.0.0.1:{api}"\n',
        fixture=mic,
        before_start=lambda settings_file: seed(settings_file.parent, base_url),
    )
    try:
        port, proc, settings_file = next(runs)
        assert asyncio.run(no_voice_for_home_assistant(port)) == 0

        def paths():
            with provider.lock:
                return [r["path"] for r in provider.requests if r["method"] == "POST"]

        wait_for("the reply", lambda: "/v1/audio/speech" in paths(), 25)
        status = wait_for(
            "idle",
            lambda: (s := rpc(api, "GetStatus", {})["status"]).get("phase") == "PHASE_IDLE" and s,
            10,
        )
        assert proc.poll() is None, "pinnad exited"

        assert paths() == [
            "/v1/audio/transcriptions",
            "/v1/chat/completions",
            "/v1/chat/completions",
            "/v1/audio/speech",
        ]
        with provider.lock:
            sent = list(provider.requests)
        assert all(r["auth"] == f"Bearer {KEY}" for r in sent), [r["auth"] for r in sent]

        upload = next(r for r in sent if r["path"] == "/v1/audio/transcriptions")
        assert upload["type"].startswith("multipart/form-data"), upload["type"]
        assert b"RIFF" in upload["body"] and b"whisper-1" in upload["body"]

        chats = [json.loads(r["body"]) for r in sent if r["path"] == "/v1/chat/completions"]
        assert chats[0]["model"] == "gpt-test" and chats[0]["stream"] is True
        assert chats[0]["messages"][-1] == {"role": "user", "content": TRANSCRIPT}
        assert any(t["function"]["name"] == "set_timer" for t in chats[0]["tools"])
        tool_result = chats[1]["messages"][-1]
        assert tool_result["role"] == "tool" and tool_result["tool_call_id"] == "call_1"

        speech = json.loads(next(r for r in sent if r["path"] == "/v1/audio/speech")["body"])
        assert speech["input"] == ANSWER and speech["voice"] == "nova"

        # The brain's tool call started a device timer.
        timers = status.get("timers", [])
        assert [t.get("name") for t in timers] == ["tea"], timers
        assert not status.get("assistant", {}).get("lastError"), status.get("assistant")

        # Neither the key nor what was said reaches the log at info level.
        log = (settings_file.parent / "pinnad.log").read_text()
        assert KEY not in log
        assert TRANSCRIPT not in log and ANSWER not in log
    finally:
        provider.shutdown()
        runs.close()


class Timeline:
    """`WatchTimeline` on the satellite's API: every message it sends, as the
    dashboard reads it."""

    def __init__(self, api: int):
        self.items: list[dict] = []
        self.lock = threading.Lock()
        self.conn = http.client.HTTPConnection("127.0.0.1", api, timeout=30)
        payload = b"{}"
        self.conn.request(
            "POST",
            "/pinna.v1.DeviceService/WatchTimeline",
            b"\0" + struct.pack(">I", len(payload)) + payload,
            {"Content-Type": "application/connect+json", "Connect-Protocol-Version": "1"},
        )
        self.res = self.conn.getresponse()
        assert self.res.status == 200, self.res.status
        threading.Thread(target=self.read, daemon=True).start()

    def read(self):
        try:
            while True:
                head = self.res.read(5)
                if len(head) < 5:
                    return
                flags, size = head[0], struct.unpack(">I", head[1:])[0]
                payload = json.loads(self.res.read(size) or b"{}")
                if flags & 2:
                    return
                with self.lock:
                    self.items.append(payload)
        except (OSError, http.client.HTTPException):
            return

    def close(self):
        self.conn.close()

    def snapshot(self) -> list[dict]:
        with self.lock:
            return list(self.items)

    def events(self) -> list[dict]:
        return [e for i in self.snapshot() for e in i.get("batch", {}).get("events", [])]

    def requests(self) -> dict[str, dict]:
        """The last version of each request, by id."""
        out = {}
        for i in self.snapshot():
            for r in i.get("batch", {}).get("requests", []):
                out[r["id"]] = r
        return out


def start_debugging(api: int):
    # The API is still starting on the first calls.
    for _ in range(100):
        try:
            return rpc(api, "StartDebugging", {"minutes": 15})
        except (urllib.error.URLError, ConnectionError):
            time.sleep(0.1)
    raise AssertionError("StartDebugging never answered")


def test_a_request_with_debugging_on_leaves_its_timeline(tmp_path):
    provider = Provider()
    threading.Thread(target=provider.serve_forever, daemon=True).start()
    base_url = f"http://127.0.0.1:{provider.server_port}/v1"
    mic = tmp_path / "request.wav"
    # Silence first, so debugging is on before the wake word is heard.
    request_wav(mic, lead=6.0)
    api = free_port()
    runs = start_pinnad(
        settings='mode = "standalone"\n',
        extra=f'[api]\nlisten = "127.0.0.1:{api}"\n',
        fixture=mic,
        before_start=lambda settings_file: seed(settings_file.parent, base_url),
    )
    timeline = None
    try:
        next(runs)
        start_debugging(api)
        timeline = Timeline(api)
        state = wait_for("the state", lambda: (timeline.snapshot() or [None])[0], 5)
        assert state["state"].get("on") is True, state

        def phases():
            return [e["detail"] for e in timeline.events() if e["kind"] == "TIMELINE_EVENT_KIND_PHASE"]

        wait_for("the request to end", lambda: phases()[-1:] == ["idle"] and len(phases()) >= 4, 40)
        requests = wait_for(
            "the requests",
            lambda: (r := timeline.requests())
            and sum(q.get("outcome") == "TIMELINE_OUTCOME_DONE" for q in r.values()) >= 4
            and r,
            10,
        )
        events = timeline.events()

        def of(kind: str) -> list[dict]:
            return [e for e in events if e["kind"] == f"TIMELINE_EVENT_KIND_{kind}"]

        assert any("alexa" in e["detail"].lower() for e in of("WAKE")), of("WAKE")
        assert phases() == ["listening", "thinking", "speaking", "idle"], phases()
        assert of("END_OF_SPEECH"), events
        stages = {e["detail"]: e for e in of("STAGE")}
        for name in ["speech-to-text", "model", "text-to-speech", "tool set_timer"]:
            assert name in stages, (name, list(stages))
        for stage in stages.values():
            assert int(stage["endUnixMs"]) >= int(stage["unixMs"]) > 0, stage

        ours = sorted(
            (r for r in requests.values() if "/v1/" in r["url"]), key=lambda r: int(r["startUnixMs"])
        )
        assert [(r["method"], r["url"].split("/v1")[1]) for r in ours] == [
            ("POST", "/audio/transcriptions"),
            ("POST", "/chat/completions"),
            ("POST", "/chat/completions"),
            ("POST", "/audio/speech"),
        ], ours
        for r in ours:
            assert r["outcome"] == "TIMELINE_OUTCOME_DONE" and r["status"] == 200, r
            assert int(r["firstBodyUnixMs"]) > 0 and int(r["endUnixMs"]) > 0, r

        # What was said reaches no field of the stream.
        everything = json.dumps(timeline.snapshot())
        assert TRANSCRIPT not in everything and ANSWER not in everything
        assert KEY not in everything
        assert all(not r.get("bodyOut") and not r.get("bodyIn") for r in requests.values())

        # Stopping clears it: the next follower sees it off and empty.
        rpc(api, "StopDebugging", {})
        timeline.close()
        timeline = Timeline(api)
        first = wait_for("the state", lambda: (timeline.snapshot() or [None])[0], 5)
        assert not first["state"].get("on"), first
        time.sleep(1)
        assert len(timeline.snapshot()) == 1, timeline.snapshot()
    finally:
        if timeline:
            timeline.close()
        provider.shutdown()
        runs.close()
