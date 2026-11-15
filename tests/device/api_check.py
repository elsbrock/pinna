"""ESPHome API checks against a real device, plus an acoustic loopback.

The loopback stages a clip that says "Alexa, what time is it" on the device,
plays it through the device's own speaker via the media player (file:// URL)
and expects the device's wake word detector to fire on its own microphone. The
Assist run that follows is cancelled right away.
"""

import argparse
import asyncio
import pathlib
import shlex
import subprocess
import sys

from aioesphomeapi import (
    APIClient,
    ButtonInfo,
    EventInfo,
    LightInfo,
    MediaPlayerInfo,
    NumberInfo,
    SwitchInfo,
    TextSensorInfo,
)

ROOT = pathlib.Path(__file__).resolve().parents[2]
CLIP = ROOT / "crates/pinna-wakeword/tests/fixtures/alexa-what-time.wav"

EXPECTED = {
    LightInfo: {"LED ring"},
    SwitchInfo: {"Microphone mute", "Wake word"},
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


async def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--host", required=True)
    ap.add_argument("--key-file")
    ap.add_argument("--loopback", help="path on the device to stage the clip at")
    ap.add_argument("--stage", help="ssh command prefix used to copy the clip")
    ap.add_argument("--timeout", type=float, default=20.0)
    args = ap.parse_args()

    key = pathlib.Path(args.key_file).read_text().strip() if args.key_file else None
    client = APIClient(args.host, 6053, None, noise_psk=key, client_info="pinna-device-test")
    try:
        await asyncio.wait_for(client.connect(login=True), 15)
    except Exception as e:  # noqa: BLE001 - report any connection failure
        fail(f"cannot connect to {args.host}:6053: {e}")
    info = await client.device_info()
    print(f"     device {info.name} ({info.friendly_name}) mac={info.mac_address} "
          f"voice_flags={info.voice_assistant_feature_flags}", flush=True)

    entities, _ = await client.list_entities_services()
    by_name = {}
    for e in entities:
        by_name.setdefault(type(e), set()).add(e.name)
    for cls, names in EXPECTED.items():
        missing = names - by_name.get(cls, set())
        if missing:
            fail(f"missing {cls.__name__}: {sorted(missing)}")

    if not args.loopback:
        await client.disconnect()
        return

    if args.stage:
        cmd = shlex.split(args.stage) + [
            f"mkdir -p {shlex.quote(str(pathlib.Path(args.loopback).parent))} && "
            f"cat > {shlex.quote(args.loopback)}"
        ]
        with CLIP.open("rb") as f:
            subprocess.run(cmd, stdin=f, check=True)

    keys = {(type(e), e.name): e.key for e in entities}
    voice_key = keys[(EventInfo, "Voice assistant")]
    mute_key = keys[(SwitchInfo, "Microphone mute")]
    states: dict[int, object] = {}
    woke = asyncio.Event()

    def on_state(s) -> None:
        states[s.key] = s
        if s.key == voice_key and getattr(s, "event_type", None) == "wake_word":
            woke.set()

    client.subscribe_states(on_state)
    await asyncio.sleep(1.0)
    if getattr(states.get(mute_key), "state", False):
        fail("microphone is muted; cannot run the acoustic loopback")

    client.media_player_command(keys[(MediaPlayerInfo, "Media player")], media_url=f"file://{args.loopback}")
    try:
        await asyncio.wait_for(woke.wait(), args.timeout)
    except asyncio.TimeoutError:
        fail(f"no wake word within {args.timeout:.0f} s of playing the clip")
    finally:
        client.button_command(keys[(ButtonInfo, "Cancel request")])
        await asyncio.sleep(0.5)
        await client.disconnect()
    print("     wake word detected from the device's own speaker", flush=True)


asyncio.run(main())
