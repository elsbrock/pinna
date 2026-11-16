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

import asyncio
import http.server
import math
import os
import queue
import socket
import struct
import subprocess
import tempfile
import threading
from pathlib import Path

import pytest
from aioesphomeapi import APIClient, VoiceAssistantEventType, VoiceAssistantTimerEventType
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


def start_pinnad(settings: str | None = None):
    """Run pinnad on the fixture; `settings` seeds its settings.toml."""
    if not MODELS.exists():
        pytest.skip("wake word models missing (just models)")
    port = free_port()
    with tempfile.TemporaryDirectory() as d:
        settings_file = Path(d) / "settings.toml"
        if settings is not None:
            settings_file.write_text(settings)
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
port = {port}
mdns = false
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


ALL_WORDS = {"alexa_v0.1", "hey_jarvis_v0.1", "hey_mycroft_v0.1", "hey_rhasspy_v0.1", "okay_nabu"}


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
