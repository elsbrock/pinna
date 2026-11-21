"""ESPHome API checks against a real device, plus an acoustic test.

direct (pinnad drives the microphone directly): the device plays a clip that
says "Alexa, what time is it" through its own speaker and must detect the wake
word on its own microphone.

aec (the device's PipeWire microphone graph with echo cancellation; run it in
a quiet room — sound the device did not play cannot be cancelled): the device
plays ~12 s of music followed by the same clip, while the beamformer output
(dot_beam), the echo-cancelled stream (dot_mic_aec) and the wake word's stream
(dot_wake, ADR 0033) are recorded on the device. The music must reach the
beamformer (speaker -> microphones -> beamformer), the echo canceller must
remove at least MIN_REDUCTION_DB of it in the speech band, dot_wake must be at
least MIN_WAKE_REDUCTION_DB below the beamformer, and the device must NOT wake
on its own voice.

Any Assist run that starts is cancelled right away.
"""

import argparse
import asyncio
import io
import math
import pathlib
import shlex
import subprocess
import sys
import wave

from aioesphomeapi import (
    APIClient,
    ButtonInfo,
    EventInfo,
    LightInfo,
    MediaPlayerCommand,
    MediaPlayerInfo,
    NumberInfo,
    SwitchInfo,
    TextSensorInfo,
)

ROOT = pathlib.Path(__file__).resolve().parents[2]
FIXTURES = ROOT / "crates/pinna-wakeword/tests/fixtures"
CLIP = FIXTURES / "alexa-what-time.wav"
MUSIC = [FIXTURES / "music-clean.wav", FIXTURES / "music-bedonly.wav"]
REMOTE_DIR = "/var/lib/pinna/test"
MIN_REDUCTION_DB = 20.0
# dot_wake has no suppressor, only a linear canceller (ADR 0033): offline it
# removed 14-20 dB of a reply on an Echo Dot (2nd gen); without it, none.
MIN_WAKE_REDUCTION_DB = 10.0
MIN_MUSIC_OVER_QUIET_DB = 15.0

EXPECTED = {
    LightInfo: {"LED ring"},
    SwitchInfo: {
        "Microphone mute",
        "Wake word",
        "Listening direction",
        "AirPlay",
        "Bluetooth audio",
        "Bluetooth pairing",
        "Sendspin",
    },
    NumberInfo: {"Volume", "Reply volume", "Wake word sensitivity", "LED brightness"},
    ButtonInfo: {"Cancel request"},
    EventInfo: {
        "Volume up button",
        "Volume down button",
        "Microphone mute button",
        "Action button",
        "Voice assistant",
        "Playback",
        "Timer",
    },
    TextSensorInfo: {"Last wake word"},
    MediaPlayerInfo: {"Media player"},
}


def fail(msg: str) -> None:
    print(f"     {msg}", flush=True)
    sys.exit(1)


def say(msg: str) -> None:
    print(f"     {msg}", flush=True)


def read_wav(data: bytes) -> tuple[list[int], int]:
    w = wave.open(io.BytesIO(data))
    raw = w.readframes(w.getnframes())
    n = len(raw) // 2
    samples = list(memoryview(raw).cast("h"))[:n]
    return samples, w.getframerate()


def aec_test_wav() -> bytes:
    """~12 s of music, then the wake word clip, then 2 s of music (16 kHz mono)."""
    frames = b""
    params = None
    music = b""
    for p in MUSIC:
        w = wave.open(str(p))
        params = params or w.getparams()
        music += w.readframes(w.getnframes())
    while len(frames) < 12 * 16000 * 2:
        frames += music
    frames = frames[: 12 * 16000 * 2]
    c = wave.open(str(CLIP))
    frames += c.readframes(c.getnframes()) + music[: 2 * 16000 * 2]
    out = io.BytesIO()
    w = wave.open(out, "wb")
    w.setparams(params)
    w.writeframes(frames)
    w.close()
    return out.getvalue()


class Biquad:
    """RBJ biquad (high-pass or low-pass)."""

    def __init__(self, kind: str, fs: float, f0: float, q: float = 0.7071):
        w0 = 2 * math.pi * f0 / fs
        alpha = math.sin(w0) / (2 * q)
        cos = math.cos(w0)
        if kind == "hp":
            b = [(1 + cos) / 2, -(1 + cos), (1 + cos) / 2]
        else:
            b = [(1 - cos) / 2, 1 - cos, (1 - cos) / 2]
        a0 = 1 + alpha
        self.b = [x / a0 for x in b]
        self.a = [-2 * cos / a0, (1 - alpha) / a0]
        self.z = [0.0, 0.0, 0.0, 0.0]

    def run(self, xs):
        b0, b1, b2 = self.b
        a1, a2 = self.a
        x1, x2, y1, y2 = self.z
        out = []
        for x in xs:
            y = b0 * x + b1 * x1 + b2 * x2 - a1 * y1 - a2 * y2
            x2, x1, y2, y1 = x1, x, y1, y
            out.append(y)
        self.z = [x1, x2, y1, y2]
        return out


def band_db(samples: list[int], rate: int, t0: float, t1: float) -> float:
    """Speech-band (300-3400 Hz) level of a segment, dBFS."""
    x = Biquad("lp", rate, 3400).run(Biquad("hp", rate, 300).run(samples))
    seg = x[int(t0 * rate) : int(t1 * rate)]
    if not seg:
        return -200.0
    p = sum(v * v for v in seg) / len(seg)
    return 10 * math.log10(p / 32768**2 + 1e-20)


async def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--host", required=True)
    ap.add_argument("--key-file")
    ap.add_argument("--mode", choices=["direct", "aec", "none"], default="direct")
    ap.add_argument("--stage", help="ssh command prefix for the device (staging, recording)")
    ap.add_argument("--timeout", type=float, default=20.0)
    args = ap.parse_args()
    ssh = shlex.split(args.stage) if args.stage else None

    def remote(cmd: str, stdin: bytes | None = None, check: bool = True) -> bytes:
        if ssh is None:
            fail("--stage is required for the acoustic test")
        r = subprocess.run(ssh + [cmd], input=stdin, capture_output=True, check=False)
        if check and r.returncode != 0:
            fail(f"remote command failed: {cmd}: {r.stderr.decode(errors='replace')[:200]}")
        return r.stdout

    key = pathlib.Path(args.key_file).read_text().strip() if args.key_file else None
    client = APIClient(args.host, 6053, None, noise_psk=key, client_info="pinna-device-test")
    try:
        await asyncio.wait_for(client.connect(login=True), 15)
    except Exception as e:  # noqa: BLE001 - report any connection failure
        fail(f"cannot connect to {args.host}:6053: {e}")
    info = await client.device_info()
    say(f"device {info.name} ({info.friendly_name}) mac={info.mac_address} "
        f"voice_flags={info.voice_assistant_feature_flags}")

    entities, _ = await client.list_entities_services()
    by_name: dict[type, set[str]] = {}
    for e in entities:
        by_name.setdefault(type(e), set()).add(e.name)
    for cls, names in EXPECTED.items():
        missing = names - by_name.get(cls, set())
        if missing:
            fail(f"missing {cls.__name__}: {sorted(missing)}")
    if args.mode == "none":
        await client.disconnect()
        return

    keys = {(type(e), e.name): e.key for e in entities}
    voice_key = keys[(EventInfo, "Voice assistant")]
    mute_key = keys[(SwitchInfo, "Microphone mute")]
    player = keys[(MediaPlayerInfo, "Media player")]
    cancel = keys[(ButtonInfo, "Cancel request")]
    states: dict[int, object] = {}
    woke = asyncio.Event()

    def on_state(s) -> None:
        states[s.key] = s
        if s.key == voice_key and getattr(s, "event_type", None) == "wake_word":
            woke.set()

    client.subscribe_states(on_state)
    await asyncio.sleep(1.0)
    if getattr(states.get(mute_key), "state", False):
        fail("microphone is muted; cannot run the acoustic test")

    try:
        if args.mode == "direct":
            path = f"{REMOTE_DIR}/alexa-what-time.wav"
            remote(f"mkdir -p {REMOTE_DIR} && cat > {path}", CLIP.read_bytes())
            client.media_player_command(player, media_url=f"file://{path}")
            try:
                await asyncio.wait_for(woke.wait(), args.timeout)
            except asyncio.TimeoutError:
                fail(f"no wake word within {args.timeout:.0f} s of playing the clip")
            say("wake word detected from the device's own speaker")
            return

        path = f"{REMOTE_DIR}/aec-test.wav"
        remote(f"mkdir -p {REMOTE_DIR} && cat > {path}", aec_test_wav())
        rec = (
            f"cd {REMOTE_DIR} && rm -f beam.wav aec.wav wake.wav && export PIPEWIRE_RUNTIME_DIR=/run/pipewire && "
            "{ timeout 22 pw-record --target dot_beam --rate 16000 --channels 1 --format s16 beam.wav & "
            "timeout 22 pw-record --target dot_mic_aec --rate 16000 --channels 1 --format s16 aec.wav & "
            "timeout 22 pw-record --target dot_wake --rate 16000 --channels 1 --format s16 wake.wav & wait; }"
        )
        recorder = subprocess.Popen(ssh + [rec], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        # Start playback only once the recording runs, and note where in it.
        started = 0.0
        for _ in range(50):
            await asyncio.sleep(0.2)
            size = remote(f"stat -c %s {REMOTE_DIR}/beam.wav 2>/dev/null || echo 0", check=False)
            started = max(0, int(size.strip() or 0) - 44) / (16000 * 2)
            if started >= 1.0:
                break
        else:
            fail("the recording on the device did not start")
        client.media_player_command(player, media_url=f"file://{path}")
        await asyncio.get_running_loop().run_in_executor(None, recorder.wait)
        client.media_player_command(player, command=MediaPlayerCommand.STOP)
        beam, rate = read_wav(remote(f"cat {REMOTE_DIR}/beam.wav"))
        aec, _ = read_wav(remote(f"cat {REMOTE_DIR}/aec.wav"))
        wake, _ = read_wav(remote(f"cat {REMOTE_DIR}/wake.wav"))
        # Quiet: before playback. Music: 8-13 s into it (the canceller has converged).
        quiet = band_db(beam, rate, 0.1, max(0.5, started - 0.1))
        music_beam = band_db(beam, rate, started + 8.0, started + 13.0)
        music_aec = band_db(aec, rate, started + 8.0, started + 13.0)
        music_wake = band_db(wake, rate, started + 8.0, started + 13.0)
        say(f"beamformer: quiet {quiet:.1f} dB, music {music_beam:.1f} dB; "
            f"after echo cancellation {music_aec:.1f} dB ({music_beam - music_aec:.1f} dB removed)")
        if music_beam - quiet < MIN_MUSIC_OVER_QUIET_DB:
            fail("the music does not reach the beamformer (speaker or microphones not working)")
        if music_beam - music_aec < MIN_REDUCTION_DB:
            fail(f"echo cancellation removed less than {MIN_REDUCTION_DB:.0f} dB")
        say(f"wake word stream: music {music_wake:.1f} dB ({music_beam - music_wake:.1f} dB under the beamformer)")
        if music_beam - music_wake < MIN_WAKE_REDUCTION_DB:
            fail(f"the wake word stream is less than {MIN_WAKE_REDUCTION_DB:.0f} dB under the beamformer")
        if woke.is_set():
            fail("the device woke on its own voice (echo cancellation not effective)")
        say("no wake word on the device's own voice")
    finally:
        client.button_command(cancel)
        await asyncio.sleep(0.5)
        await client.disconnect()


asyncio.run(main())
