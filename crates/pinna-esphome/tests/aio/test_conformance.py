"""Conformance of pinna-esphome against the real aioesphomeapi client (plaintext).

Run: just esphome-conformance
The demo example is started on a free port; its stdout JSON lines are the oracle for what
the device side received from the client.
"""

import asyncio
import importlib.metadata
import json
import os
import time
from pathlib import Path

import pytest
import pytest_asyncio
from aioesphomeapi import APIClient
from aioesphomeapi.model import (
    LightInfo,
    LightState,
    SwitchInfo,
    SwitchState,
    VoiceAssistantEventType as VE,
    VoiceAssistantTimerEventType as VT,
)

assert importlib.metadata.version("aioesphomeapi") == "46.2.0", "pinned client required"

ROOT = Path(__file__).resolve().parents[4]
TARGET = Path(os.environ.get("CARGO_TARGET_DIR", ROOT / "target"))
DEMO = Path(os.environ.get("PINNA_DEMO_BIN", TARGET / "debug" / "examples" / "demo"))

LIGHT_KEY = 3
MUTE_KEY = 2


class Demo:
    def __init__(self, proc):
        self.proc = proc
        self.lines = []
        self.port = None
        self._reader = asyncio.create_task(self._read())

    async def _read(self):
        while line := await self.proc.stdout.readline():
            try:
                self.lines.append(json.loads(line))
            except json.JSONDecodeError:
                pass

    async def wait(self, pred, what, timeout=5.0):
        end = time.monotonic() + timeout
        while time.monotonic() < end:
            for line in self.lines:
                if pred(line):
                    return line
            await asyncio.sleep(0.01)
        raise AssertionError(f"demo never printed {what}; got {self.lines}")

    def cmd(self, name):
        return lambda line: line.get("command") == name

    def send(self, text):
        self.proc.stdin.write(text.encode() + b"\n")

    async def stop(self):
        if self.proc.returncode is None:
            self.proc.kill()
        await self.proc.wait()
        self._reader.cancel()


@pytest_asyncio.fixture
async def demo():
    assert DEMO.exists(), f"{DEMO} missing: build with cargo build -p pinna-esphome --example demo"
    proc = await asyncio.create_subprocess_exec(
        str(DEMO),
        "0",
        stdin=asyncio.subprocess.PIPE,
        stdout=asyncio.subprocess.PIPE,
    )
    d = Demo(proc)
    ready = await d.wait(lambda l: l.get("ready"), "ready")
    d.port = ready["port"]
    yield d
    await d.stop()


@pytest_asyncio.fixture
async def client(demo):
    c = APIClient("127.0.0.1", demo.port, None, client_info="conformance")
    await c.connect(login=True)
    yield c
    await c.disconnect()


async def eventually(pred, what, timeout=5.0):
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        if pred():
            return
        await asyncio.sleep(0.01)
    raise AssertionError(f"timed out waiting for {what}")


@pytest.mark.asyncio
async def test_hello_and_device_info(client, demo):
    assert (client.api_version.major, client.api_version.minor) == (1, 14)
    await demo.wait(demo.cmd("client_connected"), "client_connected")
    info = await client.device_info()
    assert info.name == "pinna-demo"
    assert info.friendly_name == "Pinna Demo"
    assert info.mac_address == "02:00:00:00:00:42"
    assert info.esphome_version == "2026.9.0"
    assert info.manufacturer == "Pinna"
    assert info.project_name == "pinna.satellite"
    assert info.suggested_area == "Lab"
    assert info.voice_assistant_feature_flags == 61


@pytest.mark.asyncio
async def test_entities(client):
    entities, services = await client.list_entities_services()
    assert not services
    by_key = {e.key: e for e in entities}
    assert set(by_key) == {LIGHT_KEY, MUTE_KEY}
    light, sw = by_key[LIGHT_KEY], by_key[MUTE_KEY]
    assert isinstance(light, LightInfo) and light.name == "LED ring"
    assert light.object_id == "led_ring"
    assert list(light.effects) == ["Spin", "Pulse"]
    assert 35 in [int(m) for m in light.supported_color_modes]
    assert isinstance(sw, SwitchInfo) and sw.name == "Microphone mute"


@pytest.mark.asyncio
async def test_states_light_and_switch(client, demo):
    states = {}
    client.subscribe_states(lambda s: states.__setitem__(s.key, s))
    await eventually(lambda: LIGHT_KEY in states and MUTE_KEY in states, "initial states")
    assert isinstance(states[LIGHT_KEY], LightState) and states[LIGHT_KEY].state is False
    assert isinstance(states[MUTE_KEY], SwitchState) and states[MUTE_KEY].state is False

    client.light_command(LIGHT_KEY, state=True, brightness=0.5, rgb=(1.0, 0.0, 0.5), effect="Spin")
    cmd = await demo.wait(demo.cmd("light"), "light command")
    assert cmd["on"] is True and cmd["brightness"] == 0.5
    assert cmd["rgb"] == [1.0, 0.0, 0.5] and cmd["effect"] == "Spin"
    await eventually(lambda: states[LIGHT_KEY].state is True, "echoed light state")
    s = states[LIGHT_KEY]
    assert (s.brightness, s.red, s.green, s.blue, s.effect) == (0.5, 1.0, 0.0, 0.5, "Spin")

    client.switch_command(MUTE_KEY, True)
    assert (await demo.wait(demo.cmd("mute"), "mute command"))["state"] is True
    await eventually(lambda: states[MUTE_KEY].state is True, "echoed switch state")


@pytest.mark.asyncio
async def test_late_subscriber_gets_current_state(demo):
    a = APIClient("127.0.0.1", demo.port, None, client_info="a")
    await a.connect(login=True)
    a.switch_command(MUTE_KEY, True)
    await demo.wait(demo.cmd("mute"), "mute command")
    b = APIClient("127.0.0.1", demo.port, None, client_info="b")
    await b.connect(login=True)
    seen = {}
    b.subscribe_states(lambda s: seen.__setitem__(s.key, s))
    await eventually(lambda: MUTE_KEY in seen, "state for late client")
    assert seen[MUTE_KEY].state is True
    await a.disconnect()
    await b.disconnect()


@pytest.mark.asyncio
async def test_third_client_is_rejected(demo):
    a = APIClient("127.0.0.1", demo.port, None, client_info="a")
    b = APIClient("127.0.0.1", demo.port, None, client_info="b")
    await a.connect(login=True)
    await b.connect(login=True)
    c = APIClient("127.0.0.1", demo.port, None, client_info="c")
    with pytest.raises(Exception):
        await c.connect(login=True)
    await a.disconnect()
    await b.disconnect()


@pytest.mark.asyncio
async def test_voice_run_audio_events_timers(client, demo):
    starts, stops, audio, finished = [], [], [], []

    async def start(conversation_id, flags, settings, wake_word_phrase):
        starts.append((conversation_id, flags, wake_word_phrase))
        return 0

    async def stop(abort):
        stops.append(abort)

    async def capture(data, data2=None):
        audio.append(bytes(data))

    async def announced(result):
        finished.append(result)

    unsub = client.subscribe_voice_assistant(
        handle_start=start,
        handle_stop=stop,
        handle_audio=capture,
        handle_announcement_finished=announced,
    )
    await demo.wait(lambda l: l.get("command") == "voice_subscribed" and l["subscribed"], "subscription")

    demo.send("voice hey pinna")
    await eventually(lambda: starts, "handle_start")
    assert starts[0][0] == "conv-1"
    assert starts[0][1] & 1, "USE_VAD flag"
    assert starts[0][2] == "hey pinna"
    started = await demo.wait(demo.cmd("voice_started"), "voice_started")
    assert started["port"] == 0 and started["error"] is False

    await demo.wait(lambda l: l.get("sent") == "voice_audio_end", "end of audio")
    await eventually(lambda: len([a for a in audio if len(a) == 640]) >= 25, "25 audio frames")
    assert all(len(a) == 640 for a in audio if a), "frames are 20 ms of 16 kHz s16le"

    client.send_voice_assistant_event(VE.VOICE_ASSISTANT_RUN_START, None)
    client.send_voice_assistant_event(VE.VOICE_ASSISTANT_STT_VAD_END, None)
    client.send_voice_assistant_event(VE.VOICE_ASSISTANT_TTS_END, {"url": "http://ha.invalid/tts.mp3"})
    client.send_voice_assistant_event(VE.VOICE_ASSISTANT_RUN_END, None)
    await eventually(
        lambda: len([l for l in demo.lines if l.get("command") == "voice_event"]) == 4, "voice events"
    )
    evs = [l for l in demo.lines if l.get("command") == "voice_event"]
    assert [e["event"] for e in evs] == [1, 12, 8, 2]
    assert evs[2]["data"] == [["url", "http://ha.invalid/tts.mp3"]]

    for kind in VT:
        client.send_voice_assistant_timer_event(kind, "timer-1", "tea", 60, 45, True)
    await eventually(lambda: len([l for l in demo.lines if l.get("command") == "timer"]) == 4, "timers")
    timers = [l for l in demo.lines if l.get("command") == "timer"]
    assert [t["event"] for t in timers] == [0, 1, 2, 3]
    assert timers[0] == {
        "command": "timer", "event": 0, "id": "timer-1", "name": "tea",
        "total_seconds": 60, "seconds_left": 45, "is_active": True,
    }

    res = await client.send_voice_assistant_announcement_await_response(
        "http://ha.invalid/a.mp3", 4, "hello", "http://ha.invalid/pre.mp3", True
    )
    assert res.success
    ann = await demo.wait(demo.cmd("announce"), "announce")
    assert (ann["media_id"], ann["text"], ann["start_conversation"]) == (
        "http://ha.invalid/a.mp3", "hello", True,
    )

    unsub()
    await demo.wait(lambda l: l.get("command") == "voice_subscribed" and not l["subscribed"], "unsubscribe")


@pytest.mark.asyncio
async def test_voice_stop_reaches_client(client, demo):
    stops = []

    async def start(*a):
        return 0

    async def stop(abort):
        stops.append(abort)

    async def capture(*a):
        pass

    client.subscribe_voice_assistant(handle_start=start, handle_stop=stop, handle_audio=capture)
    await demo.wait(lambda l: l.get("command") == "voice_subscribed", "subscription")
    demo.send("voice")
    await demo.wait(demo.cmd("voice_started"), "voice_started")
    demo.send("stop")
    await eventually(lambda: stops, "handle_stop")


@pytest.mark.asyncio
async def test_wake_word_configuration(client, demo):
    cfg = await client.get_voice_assistant_configuration(3)
    assert [w.id for w in cfg.available_wake_words] == ["okay_nabu", "alexa"]
    assert list(cfg.active_wake_words) == ["okay_nabu"]
    assert cfg.max_active_wake_words == 1
    await client.set_voice_assistant_configuration(["alexa", "bogus"])
    cfg = await client.get_voice_assistant_configuration(3)
    assert list(cfg.active_wake_words) == ["alexa"]
    got = await demo.wait(demo.cmd("set_active_wake_words"), "set_active_wake_words")
    assert got["ids"] == ["alexa"]


@pytest.mark.asyncio
async def test_time_request_and_disconnect(demo):
    c = APIClient("127.0.0.1", demo.port, None, client_info="t", provide_time=True)
    await c.connect(login=True)
    demo.send("time")
    t = await demo.wait(demo.cmd("time"), "time response")
    assert abs(t["epoch_seconds"] - time.time()) < 10
    await c.disconnect()
    await demo.wait(demo.cmd("client_disconnected"), "client_disconnected")
