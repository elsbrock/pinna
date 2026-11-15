"""The virtual Echo Dot (ADR 0014) in the x86_64 VM, from the host: the
dashboard's /virtual-device socket plays the browser, aioesphomeapi plays
Home Assistant.

  - the microphone button flips the privacy latch (Status.hardware_muted);
  - the Action button starts a run, and the reply Home Assistant points at
    (a tone served from the host) comes back as speaker audio on the socket;
  - the wake word fixture streamed as microphone audio starts a run with the
    wake word phrase, and the microphone reaches Home Assistant.

  python virtual_dot.py API_PORT WEB_PORT FIXTURE_WAV [--no-wake-word]
"""

import asyncio
import io
import json
import math
import struct
import sys
import threading
import urllib.request
import wave
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

import websockets
from aioesphomeapi import APIClient, VoiceAssistantEventType as E

API_PORT, WEB_PORT, FIXTURE = int(sys.argv[1]), int(sys.argv[2]), sys.argv[3]
WAKE_WORD = "--no-wake-word" not in sys.argv
# QEMU's user network reaches the host here.
HOST_FROM_VM = "10.0.2.2"


def tone_wav() -> bytes:
    buf = io.BytesIO()
    with wave.open(buf, "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(16000)
        w.writeframes(b"".join(struct.pack("<h", int(12000 * math.sin(2 * math.pi * 440 * i / 16000))) for i in range(16000)))
    return buf.getvalue()


def serve_tone() -> int:
    body = tone_wav()

    class Handler(BaseHTTPRequestHandler):
        def do_GET(self):
            self.send_response(200)
            self.send_header("Content-Type", "audio/wav")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def log_message(self, *args):
            pass

    server = ThreadingHTTPServer(("0.0.0.0", 0), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    return server.server_address[1]


def call(method: str, body: dict) -> dict:
    req = urllib.request.Request(
        f"http://127.0.0.1:{WEB_PORT}/pinna.v1.DeviceService/{method}",
        data=json.dumps(body).encode(),
        headers={"Content-Type": "application/json"},
    )
    with urllib.request.urlopen(req, timeout=5) as r:
        return json.load(r)


def status() -> dict:
    return call("GetStatus", {})["status"]


def ok(what: str):
    print(f"\033[32mok\033[0m   {what}", flush=True)


async def until(pred, timeout: float, what: str):
    loop = asyncio.get_running_loop()
    end = loop.time() + timeout
    while not pred():
        if loop.time() > end:
            raise AssertionError(f"timed out: {what}")
        await asyncio.sleep(0.1)


async def main():
    tone_port = serve_tone()
    ha = APIClient("127.0.0.1", API_PORT, None)
    await ha.connect(login=True)
    starts: list[str] = []
    api_audio = 0

    async def handle_start(conversation_id, flags, audio_settings, wake_word_phrase):
        starts.append(wake_word_phrase or "")
        return 0  # microphone audio over the API

    async def handle_stop(abort):
        pass

    async def handle_audio(data: bytes, data2: bytes = b""):
        nonlocal api_audio
        api_audio += len(data)

    unsub = ha.subscribe_voice_assistant(handle_start=handle_start, handle_stop=handle_stop, handle_audio=handle_audio)

    def end_run(tts_url: str | None = None):
        ha.send_voice_assistant_event(E.VOICE_ASSISTANT_RUN_START, {})
        ha.send_voice_assistant_event(E.VOICE_ASSISTANT_STT_VAD_END, {})
        ha.send_voice_assistant_event(E.VOICE_ASSISTANT_INTENT_END, {"conversation_id": "vm"})
        if tts_url:
            ha.send_voice_assistant_event(E.VOICE_ASSISTANT_TTS_END, {"url": tts_url})
        ha.send_voice_assistant_event(E.VOICE_ASSISTANT_RUN_END, {})

    async with websockets.connect(f"ws://127.0.0.1:{WEB_PORT}/virtual-device", max_size=None) as ws:
        speaker_peak = 0
        speaker_bytes = 0

        async def listen():
            nonlocal speaker_peak, speaker_bytes
            async for msg in ws:
                if isinstance(msg, bytes):
                    speaker_bytes += len(msg)
                    samples = struct.unpack(f"<{len(msg) // 2}h", msg[: len(msg) // 2 * 2])
                    speaker_peak = max(speaker_peak, max((abs(s) for s in samples), default=0))

        listener = asyncio.create_task(listen())

        async def press(button: str):
            await ws.send(json.dumps({"button": button, "pressed": True}))
            await asyncio.sleep(0.15)
            await ws.send(json.dumps({"button": button, "pressed": False}))

        await until(lambda: speaker_bytes > 0, 10, "speaker audio on the socket")
        ok("speaker monitor streams to the socket")

        assert status().get("virtualDevice") is True, "Status.virtual_device"
        assert not status().get("hardwareMuted")
        await press("mute")
        await until(lambda: status().get("hardwareMuted") is True, 5, "microphone button latches")
        await press("mute")
        await until(lambda: not status().get("hardwareMuted"), 5, "microphone button unlatches")
        ok("microphone button flips the privacy latch")

        await press("action")
        await until(lambda: len(starts) == 1, 10, "Action starts a run")
        assert starts[0] == "", starts
        ok("Action button starts a run")
        # Full volume, so the reply is unmistakably louder than the null sink's
        # digital silence (at 50 % it is about -32 dB).
        volume = status().get("volume", 0)
        call("SetVolume", {"percent": 100})
        speaker_peak = 0
        try:
            end_run(f"http://{HOST_FROM_VM}:{tone_port}/reply.wav")
            await until(lambda: speaker_peak > 2000, 15, f"the reply plays on the socket (peak {speaker_peak})")
        finally:
            call("SetVolume", {"percent": volume})
        ok(f"the reply comes back as speaker audio (peak {speaker_peak})")
        await until(lambda: status().get("phase") in ("PHASE_IDLE", None), 15, "back to idle")

        if WAKE_WORD:
            with wave.open(FIXTURE) as w:
                assert (w.getframerate(), w.getnchannels(), w.getsampwidth()) == (16000, 1, 2)
                pcm = w.readframes(w.getnframes())
            api_audio = 0
            for i in range(0, len(pcm), 640):
                await ws.send(pcm[i : i + 640])
                await asyncio.sleep(0.02)
                if len(starts) == 2 and api_audio >= 640 * 25:
                    break
            await until(lambda: len(starts) == 2, 5, "the wake word starts a run")
            assert starts[1] == "Alexa", starts
            await until(lambda: api_audio >= 640 * 25, 5, "microphone audio reaches Home Assistant")
            end_run()
            ok("the wake word from the socket starts a run, and the microphone reaches Home Assistant")

        listener.cancel()
    unsub()
    await ha.disconnect()


asyncio.run(main())
