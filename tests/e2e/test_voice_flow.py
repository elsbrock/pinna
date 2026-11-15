"""End-to-end voice flow against a real pinnad process.

pinnad captures from a WAV fixture that contains the wake word ("Alexa", twice),
runs the real openWakeWord detector, and talks the ESPHome API to this test,
which plays Home Assistant's part via aioesphomeapi:

  wake word -> VoiceAssistantRequest(start, wake word phrase)
  -> port 0 -> microphone audio over the API -> STT_VAD_END
  -> TTS_END(url) -> RUN_END -> device back to idle -> second wake word.

Run: just e2e   (needs `just models`)
"""

import asyncio
import os
import socket
import subprocess
import tempfile
from pathlib import Path

import pytest
from aioesphomeapi import APIClient, VoiceAssistantEventType

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
    if not MODELS.exists():
        pytest.skip("wake word models missing (just models)")
    port = free_port()
    with tempfile.TemporaryDirectory() as d:
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
            [str(PINNAD), "--config", str(cfg)],
            stdout=logf,
            stderr=subprocess.STDOUT,
            env={**{k: v for k, v in os.environ.items() if k != "JOURNAL_STREAM"}, "RUST_LOG": os.environ.get("RUST_LOG", "info")},
        )
        try:
            yield port, proc
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
    # No speaker in this test: playback fails fast and the run must still end cleanly.
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
    port, proc = pinnad
    starts, audio = asyncio.run(run_flow(port))
    assert proc.poll() is None, "pinnad exited"
    assert len(starts) == 2
    assert audio >= 640 * 25
