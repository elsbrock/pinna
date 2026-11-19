"""End-to-end voice flow against a real pinnad process.

pinnad captures from a WAV fixture that contains the wake word ("Alexa", twice),
runs the real openWakeWord detector, and talks the ESPHome API to this test,
which plays Home Assistant's part via aioesphomeapi:

  wake word -> VoiceAssistantRequest(start, wake word phrase)
  -> port 0 -> microphone audio over the API -> STT_VAD_END
  -> TTS_END(url) -> RUN_END -> device back to idle -> second wake word.

A streamed reply is served by a local HTTP server that the test releases
piece by piece; the speaker plays to nothing in real time.

Run: just e2e   (needs `just models`)
"""

import array
import asyncio
import base64
import functools
import http.server
import json
import math
import os
import queue
import socket
import struct
import subprocess
import tempfile
import threading
import urllib.error
import urllib.request
import wave
from pathlib import Path

import pytest
from aioesphomeapi import (
    APIClient,
    MediaPlayerState,
    VoiceAssistantEventType,
    VoiceAssistantTimerEventType,
)
from aioesphomeapi.model import Event

ROOT = Path(__file__).resolve().parents[2]
FIXTURE = ROOT / "crates/pinna-wakeword/tests/fixtures/alexa-twice.wav"
MODELS = ROOT / "blobs/models/openwakeword-0.5.1"
PINNAD = Path(os.environ.get("PINNAD_BIN", ROOT / "target/release/pinnad"))


def free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


@pytest.fixture
def pinnad():
    yield from start_pinnad()


def start_pinnad(
    settings: str | None = None, extra: str = "", fixture: Path = FIXTURE, before_start=None
):
    """Run pinnad on `fixture`; `settings` seeds its settings.toml, `extra`
    adds to its pinnad.toml, and `before_start` gets the settings file's path
    to put more files beside it."""
    if not MODELS.exists():
        pytest.skip("wake word models missing (just models)")
    port = free_port()
    with tempfile.TemporaryDirectory() as d:
        settings_file = Path(d) / "settings.toml"
        if settings is not None:
            settings_file.write_text(settings)
        if before_start is not None:
            before_start(settings_file)
        cfg = Path(d) / "pinnad.toml"
        cfg.write_text(
            f"""
[device]
name = "pinna-e2e"
friendly_name = "E2E"
[audio]
backend = "wav:{fixture}"
[wake]
models = "{MODELS}"
[esphome]
port = {port}
mdns = false
[leds]
backend = "none"
{extra}"""
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
                str(settings_file),
            ],
            stdout=logf,
            stderr=subprocess.STDOUT,
            env={**{k: v for k, v in os.environ.items() if k != "JOURNAL_STREAM"}, "RUST_LOG": os.environ.get("RUST_LOG", "info")},
        )
        try:
            yield port, proc, settings_file
        finally:
            proc.terminate()
            proc.wait(timeout=10)
            logf.seek(0)
            print("---- pinnad log ----\n" + logf.read())


async def run_flow(port: int):
    client = APIClient("127.0.0.1", port, None)
    for _ in range(50):
        try:
            await client.connect(login=True)
            break
        except Exception:
            await asyncio.sleep(0.1)
    info = await client.device_info()
    assert info.voice_assistant_feature_flags == 61

    starts: list[tuple[str, str]] = []
    audio_bytes = 0
    stops = 0
    got_audio = asyncio.Event()
    second_start = asyncio.Event()

    async def handle_start(conversation_id, flags, audio_settings, wake_word_phrase):
        starts.append((conversation_id, wake_word_phrase))
        if len(starts) == 2:
            second_start.set()
        return 0  # API audio

    async def handle_stop(abort):
        nonlocal stops
        stops += 1

    async def handle_audio(data: bytes, data2: bytes = b""):
        nonlocal audio_bytes
        audio_bytes += len(data)
        if audio_bytes >= 640 * 25:  # 0.5 s of audio
            got_audio.set()

    unsub = client.subscribe_voice_assistant(
        handle_start=handle_start, handle_stop=handle_stop, handle_audio=handle_audio
    )

    # First wake word in the fixture is at ~1.6 s (the fixture is paced in real time).
    await asyncio.wait_for(got_audio.wait(), timeout=15)
    assert starts and starts[0][1] == "Alexa", starts

    E = VoiceAssistantEventType
    client.send_voice_assistant_event(E.VOICE_ASSISTANT_RUN_START, {})
    client.send_voice_assistant_event(E.VOICE_ASSISTANT_STT_VAD_END, {})
    client.send_voice_assistant_event(E.VOICE_ASSISTANT_INTENT_END, {"conversation_id": "c1"})
    # The reply URL is unreachable: playback fails fast and the run must still end cleanly.
    client.send_voice_assistant_event(E.VOICE_ASSISTANT_TTS_END, {"url": "http://127.0.0.1:9/none.wav"})
    client.send_voice_assistant_event(E.VOICE_ASSISTANT_RUN_END, {})

    # Second wake word at ~8.7 s: only detected if the device returned to idle.
    await asyncio.wait_for(second_start.wait(), timeout=15)
    assert starts[1][1] == "Alexa"
    assert starts[0][0] != starts[1][0], "new run must get a new conversation id"

    unsub()
    await client.disconnect()
    return starts, audio_bytes


def test_wake_word_drives_assist_run(pinnad):
    port, proc, _ = pinnad
    starts, audio = asyncio.run(run_flow(port))
    assert proc.poll() is None, "pinnad exited"
    assert len(starts) == 2
    assert audio >= 640 * 25


def attenuate(src: Path, dst: Path, db: float):
    with wave.open(str(src)) as w:
        params = w.getparams()
        samples = array.array("h", w.readframes(w.getnframes()))
    g = 10 ** (db / 20)
    quiet = array.array("h", (round(s * g) for s in samples))
    with wave.open(str(dst), "wb") as w:
        w.setparams(params)
        w.writeframes(quiet.tobytes())


@pytest.fixture
def quiet_pinnad(tmp_path):
    """The fixture 48 dB quieter, like a talker across a room: at a fixed gain
    it stays under the wake word detector's absolute VAD floor (about
    -58.5 dBFS) and never wakes it. The automatic gain (ADR 0004) lifts it."""
    quiet = tmp_path / "alexa-twice-48db.wav"
    attenuate(FIXTURE, quiet, -48.0)
    yield from start_pinnad(fixture=quiet)


def test_quiet_wake_word_drives_assist_run(quiet_pinnad):
    port, proc, _ = quiet_pinnad
    starts, _ = asyncio.run(run_flow(port))
    assert proc.poll() is None, "pinnad exited"
    assert [w for _, w in starts] == ["Alexa", "Alexa"]


class GatedReply:
    """Serves one reply WAV (22.05 kHz mono, no length) in parts the test
    releases; the body ends when the test closes it (HTTP/1.0)."""

    RATE = 22050

    def __init__(self):
        self.parts: queue.Queue[bytes | None] = queue.Queue()
        self.requests = 0
        self.open = threading.Event()
        gate = self

        class Handler(http.server.BaseHTTPRequestHandler):
            def do_GET(self):
                gate.requests += 1
                self.send_response(200)
                self.send_header("Content-Type", "audio/wav")
                self.end_headers()
                self.wfile.write(wav_header(GatedReply.RATE))
                self.wfile.flush()
                gate.open.set()
                while (part := gate.parts.get()) is not None:
                    self.wfile.write(part)
                    self.wfile.flush()
                gate.open.clear()

            def log_message(self, *args):
                pass

        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.url = f"http://127.0.0.1:{self.server.server_address[1]}/reply.wav"
        threading.Thread(target=self.server.serve_forever, daemon=True).start()

    def release(self, seconds: float):
        n = int(self.RATE * seconds)
        tone = (int(8000 * math.sin(2 * math.pi * 440 * i / self.RATE)) for i in range(n))
        self.parts.put(struct.pack(f"<{n}h", *tone))

    def close(self):
        self.parts.put(None)

    def shutdown(self):
        self.close()
        self.server.shutdown()


def wav_header(rate: int) -> bytes:
    """16-bit mono WAV header whose sizes say "unknown" (a stream)."""
    return (
        b"RIFF" + struct.pack("<I", 0xFFFFFFFF) + b"WAVE"
        + b"fmt " + struct.pack("<IHHIIHH", 16, 1, 1, rate, rate * 2, 2, 16)
        + b"data" + struct.pack("<I", 0xFFFFFFFF)
    )


async def streamed_reply_flow(port: int):
    client = APIClient("127.0.0.1", port, None)
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
    replying = asyncio.Event()
    finished = asyncio.Event()

    def on_state(state):
        if isinstance(state, Event) and state.key == voice_key and state.event_type:
            events.append(state.event_type)
            {"replying": replying, "finished": finished}.get(state.event_type, asyncio.Event()).set()

    client.subscribe_states(on_state)

    starts: list[str] = []
    got_audio = asyncio.Event()
    second_start = asyncio.Event()
    announced: list[bool] = []
    audio_bytes = 0

    async def handle_start(conversation_id, flags, audio_settings, wake_word_phrase):
        starts.append(wake_word_phrase)
        if len(starts) == 2:
            second_start.set()
        return 0

    async def handle_stop(abort):
        pass

    async def handle_audio(data: bytes, data2: bytes = b""):
        nonlocal audio_bytes
        audio_bytes += len(data)
        if audio_bytes >= 640 * 25:
            got_audio.set()

    async def handle_announcement_finished(msg):
        announced.append(msg.success)

    unsub = client.subscribe_voice_assistant(
        handle_start=handle_start,
        handle_stop=handle_stop,
        handle_audio=handle_audio,
        handle_announcement_finished=handle_announcement_finished,
    )
    reply = GatedReply()
    try:
        await asyncio.wait_for(got_audio.wait(), timeout=15)

        # Home Assistant streams the reply: the URL comes early, and the whole
        # run (TTS_END, RUN_END) is over before any audio exists.
        E = VoiceAssistantEventType
        client.send_voice_assistant_event(E.VOICE_ASSISTANT_RUN_START, {"url": reply.url})
        client.send_voice_assistant_event(E.VOICE_ASSISTANT_STT_VAD_END, {})
        client.send_voice_assistant_event(E.VOICE_ASSISTANT_INTENT_PROGRESS, {"tts_start_streaming": "1"})
        client.send_voice_assistant_event(E.VOICE_ASSISTANT_INTENT_END, {"conversation_id": "c1"})
        client.send_voice_assistant_event(E.VOICE_ASSISTANT_TTS_END, {"url": reply.url})
        client.send_voice_assistant_event(E.VOICE_ASSISTANT_RUN_END, {})

        await asyncio.sleep(1)
        assert reply.open.is_set(), "the reply was not requested"
        assert "replying" not in events and "finished" not in events, events

        # The first half second sounds while the body is still open.
        reply.release(0.5)
        await asyncio.wait_for(replying.wait(), timeout=3)
        assert reply.open.is_set(), "replying only after the download ended"
        assert not finished.is_set()

        reply.release(0.5)
        reply.close()
        await asyncio.wait_for(finished.wait(), timeout=5)
        assert announced == [True], announced
        assert reply.requests == 1

        # Back to idle: the fixture's second "Alexa" (~8.7 s) wakes it again.
        await asyncio.wait_for(second_start.wait(), timeout=15)
    finally:
        reply.shutdown()
        unsub()
        await client.disconnect()
    return events


def test_streamed_reply_speaks_before_it_is_complete(pinnad):
    port, proc, _ = pinnad
    events = asyncio.run(streamed_reply_flow(port))
    assert proc.poll() is None, "pinnad exited"
    assert events.index("thinking") < events.index("replying") < events.index("finished"), events


async def interrupted_reply_flow(port: int):
    client = APIClient("127.0.0.1", port, None)
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
    replying = asyncio.Event()

    def on_state(state):
        if isinstance(state, Event) and state.key == voice_key and state.event_type:
            events.append(state.event_type)
            if state.event_type == "replying":
                replying.set()

    client.subscribe_states(on_state)

    starts: list[str] = []
    stops = 0
    got_audio = asyncio.Event()
    second_start = asyncio.Event()
    announced: list[bool] = []
    audio_bytes = 0

    async def handle_start(conversation_id, flags, audio_settings, wake_word_phrase):
        starts.append(wake_word_phrase)
        if len(starts) == 2:
            second_start.set()
        return 0

    async def handle_stop(abort):
        nonlocal stops
        stops += 1

    async def handle_audio(data: bytes, data2: bytes = b""):
        nonlocal audio_bytes
        audio_bytes += len(data)
        if audio_bytes >= 640 * 25:
            got_audio.set()

    async def handle_announcement_finished(msg):
        announced.append(msg.success)

    unsub = client.subscribe_voice_assistant(
        handle_start=handle_start,
        handle_stop=handle_stop,
        handle_audio=handle_audio,
        handle_announcement_finished=handle_announcement_finished,
    )
    reply = GatedReply()
    try:
        await asyncio.wait_for(got_audio.wait(), timeout=15)

        E = VoiceAssistantEventType
        client.send_voice_assistant_event(E.VOICE_ASSISTANT_RUN_START, {"url": reply.url})
        client.send_voice_assistant_event(E.VOICE_ASSISTANT_STT_VAD_END, {})
        client.send_voice_assistant_event(E.VOICE_ASSISTANT_INTENT_PROGRESS, {"tts_start_streaming": "1"})
        client.send_voice_assistant_event(E.VOICE_ASSISTANT_INTENT_END, {"conversation_id": "c1"})
        client.send_voice_assistant_event(E.VOICE_ASSISTANT_TTS_END, {"url": reply.url})
        client.send_voice_assistant_event(E.VOICE_ASSISTANT_RUN_END, {})

        # A long reply, still sounding when the fixture's second "Alexa" (~8.7 s) comes.
        await asyncio.sleep(1)
        assert reply.open.is_set(), "the reply was not requested"
        reply.release(10)
        await asyncio.wait_for(replying.wait(), timeout=5)
        stops_before = stops

        # The wake word cuts the reply short and starts a new request.
        await asyncio.wait_for(second_start.wait(), timeout=15)
        assert starts == ["Alexa", "Alexa"], starts
        # Home Assistant hears the reply ended, and that the run stopped.
        assert announced == [True], announced
        assert stops > stops_before
        assert reply.requests == 1
    finally:
        reply.shutdown()
        unsub()
        await client.disconnect()
    return events


def test_wake_word_interrupts_a_reply(pinnad):
    port, proc, _ = pinnad
    events = asyncio.run(interrupted_reply_flow(port))
    assert proc.poll() is None, "pinnad exited"
    assert "finished" not in events, events
    assert events.index("replying") < events.index("stopped") < len(events) - 1, events
    assert events[events.index("stopped") + 1] == "wake_word", events


async def interrupted_run_flow(port: int, during: str):
    """The wake word stops a run that Home Assistant hasn't ended. Home
    Assistant ends a run it cancels with RUN_END, which comes after the new
    run started: the new run must go on."""
    client = APIClient("127.0.0.1", port, None)
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
    changed = asyncio.Event()

    def on_state(state):
        if isinstance(state, Event) and state.key == voice_key and state.event_type:
            events.append(state.event_type)
            changed.set()

    async def until(done, timeout: float):
        async def wait():
            while not done():
                changed.clear()
                await changed.wait()

        await asyncio.wait_for(wait(), timeout)

    client.subscribe_states(on_state)

    starts: list[str] = []
    stops = 0
    got_audio = asyncio.Event()
    second_start = asyncio.Event()
    announced: list[bool] = []
    audio_bytes = 0

    async def handle_start(conversation_id, flags, audio_settings, wake_word_phrase):
        starts.append(wake_word_phrase)
        if len(starts) == 2:
            second_start.set()
        return 0

    async def handle_stop(abort):
        nonlocal stops
        stops += 1

    async def handle_audio(data: bytes, data2: bytes = b""):
        nonlocal audio_bytes
        audio_bytes += len(data)
        if audio_bytes >= 640 * 25:
            got_audio.set()

    async def handle_announcement_finished(msg):
        announced.append(msg.success)

    unsub = client.subscribe_voice_assistant(
        handle_start=handle_start,
        handle_stop=handle_stop,
        handle_audio=handle_audio,
        handle_announcement_finished=handle_announcement_finished,
    )
    reply = GatedReply()
    E = VoiceAssistantEventType
    try:
        await asyncio.wait_for(got_audio.wait(), timeout=15)
        client.send_voice_assistant_event(E.VOICE_ASSISTANT_RUN_START, {"url": reply.url})
        client.send_voice_assistant_event(E.VOICE_ASSISTANT_STT_VAD_END, {})
        if during == "speaking":
            # A streamed reply: the run ends only once the reply is written.
            client.send_voice_assistant_event(
                E.VOICE_ASSISTANT_INTENT_PROGRESS, {"tts_start_streaming": "1"}
            )
            await asyncio.sleep(1)
            assert reply.open.is_set(), "the reply was not requested"
            reply.release(10)
            await until(lambda: "replying" in events, 5)
        stops_before = stops

        # The fixture's second "Alexa" (~8.7 s) stops the run and starts another.
        await asyncio.wait_for(second_start.wait(), timeout=15)
        assert starts == ["Alexa", "Alexa"], starts
        assert stops > stops_before
        await until(lambda: events.count("listening") == 2, 3)

        # Home Assistant ends the run it cancelled, then the new run goes on.
        client.send_voice_assistant_event(E.VOICE_ASSISTANT_RUN_END, {})
        client.send_voice_assistant_event(E.VOICE_ASSISTANT_RUN_START, {})
        client.send_voice_assistant_event(E.VOICE_ASSISTANT_STT_VAD_END, {})
        client.send_voice_assistant_event(E.VOICE_ASSISTANT_RUN_END, {})
        await asyncio.sleep(1)
    finally:
        reply.shutdown()
        unsub()
        await client.disconnect()
    return events, announced


@pytest.mark.parametrize("during", ["speaking", "thinking"])
def test_late_end_of_a_stopped_run_leaves_the_next_one(pinnad, during):
    port, proc, _ = pinnad
    events, announced = asyncio.run(interrupted_run_flow(port, during))
    assert proc.poll() is None, "pinnad exited"
    assert events[events.index("stopped"):] == [
        "stopped", "wake_word", "listening", "thinking", "finished"
    ], events
    # A reply that sounds is reported finished; Home Assistant waits for it.
    assert announced == ([True] if during == "speaking" else []), announced


ALL_WORDS ={"alexa_v0.1", "hey_jarvis_v0.1", "hey_mycroft_v0.1", "hey_rhasspy_v0.1", "okay_nabu"}


@pytest.fixture
def pinnad_jarvis():
    """pinnad that was last told to listen for Hey Jarvis only."""
    yield from start_pinnad('wake_words = ["hey_jarvis_v0.1"]\n')


async def choose_words(port: int, settings_file: Path):
    client = APIClient("127.0.0.1", port, None)
    for _ in range(50):
        try:
            await client.connect(login=True)
            break
        except Exception:
            await asyncio.sleep(0.1)
    await client.device_info()

    starts: list[str] = []
    started = asyncio.Event()

    async def handle_start(conversation_id, flags, audio_settings, wake_word_phrase):
        starts.append(wake_word_phrase)
        started.set()
        return 0

    async def handle_stop(abort):
        pass

    async def handle_audio(data: bytes, data2: bytes = b""):
        pass

    unsub = client.subscribe_voice_assistant(
        handle_start=handle_start, handle_stop=handle_stop, handle_audio=handle_audio
    )

    # The choice made before the restart is what Home Assistant reads.
    config = await client.get_voice_assistant_configuration(5)
    offered = {w.id for w in config.available_wake_words}
    assert offered == ALL_WORDS, offered
    assert list(config.active_wake_words) == ["hey_jarvis_v0.1"]
    assert config.max_active_wake_words == 2

    # The fixture's first "Alexa" (~1.6 s) must not wake Hey Jarvis.
    await asyncio.sleep(5)
    assert starts == [], starts

    # Home Assistant adds Alexa: the second "Alexa" (~8.7 s) wakes it, by name.
    await client.set_voice_assistant_configuration(["hey_jarvis_v0.1", "alexa_v0.1"])
    config = await client.get_voice_assistant_configuration(5)
    assert list(config.active_wake_words) == ["hey_jarvis_v0.1", "alexa_v0.1"]
    await asyncio.wait_for(started.wait(), timeout=10)
    assert starts == ["Alexa"], starts
    assert 'wake_words = ["hey_jarvis_v0.1", "alexa_v0.1"]' in settings_file.read_text()

    unsub()
    await client.disconnect()


def test_home_assistant_chooses_wake_words(pinnad_jarvis):
    port, proc, settings_file = pinnad_jarvis
    asyncio.run(choose_words(port, settings_file))
    assert proc.poll() is None, "pinnad exited"


async def alarm_flow(port: int):
    client = APIClient("127.0.0.1", port, None)
    for _ in range(50):
        try:
            await client.connect(login=True)
            break
        except Exception:
            await asyncio.sleep(0.1)
    await client.device_info()

    starts: list[str] = []
    started = asyncio.Event()

    async def handle_start(conversation_id, flags, audio_settings, wake_word_phrase):
        starts.append(wake_word_phrase)
        started.set()
        return 0

    async def handle_stop(abort):
        pass

    async def handle_audio(data: bytes, data2: bytes = b""):
        pass

    unsub = client.subscribe_voice_assistant(
        handle_start=handle_start, handle_stop=handle_stop, handle_audio=handle_audio
    )

    # A timer ends before the fixture's first "Alexa" (~1.6 s), which stops
    # the alarm and starts no request.
    client.send_voice_assistant_timer_event(
        VoiceAssistantTimerEventType.VOICE_ASSISTANT_TIMER_FINISHED, "t1", "tea", 60, 0, False
    )
    await asyncio.sleep(5)
    assert starts == [], starts

    # The alarm is over: the second "Alexa" (~8.7 s) starts a request.
    await asyncio.wait_for(started.wait(), timeout=10)
    assert starts == ["Alexa"], starts

    unsub()
    await client.disconnect()


def test_wake_word_stops_a_ringing_alarm(pinnad):
    port, proc, settings_file = pinnad
    asyncio.run(alarm_flow(port))
    assert proc.poll() is None, "pinnad exited"
    log = (settings_file.parent / "pinnad.log").read_text()
    assert "timer alarm dismissed by wake word" in log, "the alarm stopped some other way"


@pytest.fixture
def pinnad_with_api():
    """pinnad with its control API (the dashboard's) on a free port."""
    api = free_port()
    for port, proc, settings_file in start_pinnad(extra=f'[api]\nlisten = "127.0.0.1:{api}"\n'):
        yield port, api, proc


def rpc(api: int, method: str, body: dict) -> dict:
    """One DeviceService call in the Connect protocol's JSON, as the dashboard makes it."""
    req = urllib.request.Request(
        f"http://127.0.0.1:{api}/pinna.v1.DeviceService/{method}",
        data=json.dumps(body).encode(),
        headers={"Content-Type": "application/json"},
    )
    with urllib.request.urlopen(req, timeout=5) as resp:
        return json.load(resp)


async def dashboard_flow(port: int, api: int, media_url: str):
    client = APIClient("127.0.0.1", port, None)
    for _ in range(50):
        try:
            await client.connect(login=True)
            break
        except Exception:
            await asyncio.sleep(0.1)
    entities, _ = await client.list_entities_services()
    object_ids = {e.key: e.object_id for e in entities}
    states: dict = {}
    client.subscribe_states(lambda s: states.__setitem__(object_ids.get(s.key), s))

    async def until(object_id: str, ok):
        for _ in range(100):
            s = states.get(object_id)
            if s is not None and ok(s):
                return s
            await asyncio.sleep(0.05)
        raise AssertionError(f"{object_id}: {states.get(object_id)}")

    async def call(method: str, body: dict) -> dict:
        return await asyncio.to_thread(rpc, api, method, body)

    # Replies follow the volume (50) until something sets their own.
    await until("reply_volume", lambda s: s.state == 50)
    await until("listening_direction", lambda s: s.state)
    await until("led_ring", lambda s: not s.state)

    await call("SetRingLight", {"on": True, "rgb": 0x00FF00, "brightness": 40, "effect": "Pulse"})
    light = await until("led_ring", lambda s: s.state)
    assert (round(light.red, 2), round(light.green, 2), round(light.blue, 2)) == (0, 1, 0), light
    assert abs(light.brightness - 0.4) < 0.01 and light.effect == "Pulse", light

    await call("UpdateSettings", {"listeningDirection": False, "replyVolume": 20})
    await until("listening_direction", lambda s: not s.state)
    await until("reply_volume", lambda s: s.state == 20)
    # Home Assistant's number can't go back to following the volume; the dashboard can.
    await call("UpdateSettings", {"replyFollowsVolume": True})
    await until("reply_volume", lambda s: s.state == 50)

    await call("PlayMedia", {"url": media_url})
    await until("media_player", lambda s: s.state == MediaPlayerState.PLAYING)
    await call("ControlMedia", {"command": "MEDIA_COMMAND_PAUSE"})
    await until("media_player", lambda s: s.state == MediaPlayerState.PAUSED)
    # No stream plays: the media player's volume is the volume.
    status = (await call("ControlMedia", {"volume": 30}))["status"]
    await until("media_player", lambda s: abs(s.volume - 0.3) < 0.01)
    assert status["volume"] == 30 and status["media"]["volume"] == 30, status
    status = (await call("ControlMedia", {"command": "MEDIA_COMMAND_STOP"}))["status"]
    assert status["media"]["available"], status["media"]
    await until("media_player", lambda s: s.state == MediaPlayerState.IDLE)

    await client.disconnect()


def test_dashboard_changes_reach_home_assistant(pinnad_with_api):
    port, api, proc = pinnad_with_api
    handler = functools.partial(http.server.SimpleHTTPRequestHandler, directory=str(FIXTURE.parent))
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        url = f"http://127.0.0.1:{server.server_port}/{FIXTURE.name}"
        asyncio.run(dashboard_flow(port, api, url))
    finally:
        server.shutdown()
    assert proc.poll() is None, "pinnad exited"


def rpc_refused(api: int, method: str, body: dict) -> tuple[int, dict]:
    """A DeviceService call that the satellite refuses: the HTTP status and the error."""
    try:
        rpc(api, method, body)
    except urllib.error.HTTPError as e:
        return e.code, json.load(e)
    raise AssertionError(f"{method} was not refused")


async def custom_word_flow(port: int, api: int):
    async def call(method: str, body: dict) -> dict:
        return await asyncio.to_thread(rpc, api, method, body)

    async def refused(method: str, body: dict) -> tuple[int, dict]:
        return await asyncio.to_thread(rpc_refused, api, method, body)

    async def connect(on_stop) -> APIClient:
        client = APIClient("127.0.0.1", port, None)
        for _ in range(50):
            try:
                await client.connect(on_stop=on_stop, login=True)
                return client
            except Exception:
                await asyncio.sleep(0.1)
        raise AssertionError("no connection")

    async def offered() -> list[dict]:
        return (await call("GetStatus", {}))["status"]["wakeWord"]["available"]

    dropped = asyncio.Event()

    async def on_stop(expected: bool):
        dropped.set()

    # The control API starts a moment after the ESPHome one.
    for _ in range(50):
        try:
            await offered()
            break
        except urllib.error.URLError:
            await asyncio.sleep(0.1)

    client = await connect(on_stop)
    config = await client.get_voice_assistant_configuration(5)
    assert not any(w.id.startswith("custom_") for w in config.available_wake_words)

    # Bytes that are not a model are refused with a reason, by the test's own process.
    garbage = base64.b64encode(b"this is not a wake word model").decode()
    code, error = await refused("AddWakeWord", {"phrase": "Hey Garbage", "model": garbage})
    assert code == 400 and error["code"] == "invalid_argument", (code, error)
    assert "Not an ONNX or TFLite model" in error["message"], error
    assert not any(w.get("custom") for w in await offered())

    # A model that passes is added; Home Assistant is asked to reconnect once
    # the satellite is quiet, and reads the new word then.
    model = base64.b64encode((MODELS / "alexa_v0.1.onnx").read_bytes()).decode()
    added = await call("AddWakeWord", {"phrase": "Hey Custom", "model": model})
    assert added["id"] == "custom_hey_custom", added
    word = next(w for w in await offered() if w["id"] == "custom_hey_custom")
    assert word["phrase"] == "Hey Custom" and word["custom"], word
    await asyncio.wait_for(dropped.wait(), timeout=10)
    await client.disconnect()

    starts: list[str] = []
    started = asyncio.Event()

    async def handle_start(conversation_id, flags, audio_settings, wake_word_phrase):
        starts.append(wake_word_phrase)
        started.set()
        return 0

    async def handle_stop(abort):
        pass

    async def handle_audio(data: bytes, data2: bytes = b""):
        pass

    dropped.clear()
    client = await connect(on_stop)
    config = await client.get_voice_assistant_configuration(5)
    assert "custom_hey_custom" in {w.id for w in config.available_wake_words}
    unsub = client.subscribe_voice_assistant(
        handle_start=handle_start, handle_stop=handle_stop, handle_audio=handle_audio
    )
    await client.set_voice_assistant_configuration(["custom_hey_custom"])
    config = await client.get_voice_assistant_configuration(5)
    assert list(config.active_wake_words) == ["custom_hey_custom"]

    # The model is Alexa's, so the fixture's "Alexa" wakes the custom word, by its phrase.
    await asyncio.wait_for(started.wait(), timeout=15)
    assert starts == ["Hey Custom"], starts
    unsub()
    await client.disconnect()

    await call("RemoveWakeWord", {"id": "custom_hey_custom"})
    assert not any(w.get("custom") for w in await offered())
    code, error = await refused("RemoveWakeWord", {"id": "custom_hey_custom"})
    assert code == 400 and error["code"] == "failed_precondition" and "no custom wake word" in error["message"], (code, error)


def test_a_custom_wake_word_is_tested_added_used_and_removed(pinnad_with_api):
    port, api, proc = pinnad_with_api
    asyncio.run(custom_word_flow(port, api))
    assert proc.poll() is None, "pinnad exited"
