"""Conformance of pinna-esphome against the real aioesphomeapi client (plaintext and Noise).

Run: just esphome-conformance
The demo example is started on a free port (every test using the `demo` fixture runs once
plaintext and once with a Noise key); its stdout JSON lines are the oracle for what
the device side received from the client.
"""

import asyncio
import base64
import importlib.metadata
import json
import os
import time
from pathlib import Path

import pytest
import pytest_asyncio
from aioesphomeapi import APIClient
from aioesphomeapi.core import (
    InvalidEncryptionKeyAPIError,
    RequiresEncryptionAPIError,
)
from aioesphomeapi.model import (
    ButtonInfo,
    EntityCategory,
    Event,
    EventInfo,
    LightInfo,
    LightState,
    MediaPlayerCommand,
    MediaPlayerEntityFeature as MF,
    MediaPlayerFormatPurpose,
    MediaPlayerInfo,
    MediaPlayerState,
    NumberInfo,
    NumberMode,
    NumberState,
    SelectInfo,
    SelectState,
    SwitchInfo,
    SwitchState,
    TextSensorInfo,
    TextSensorState,
    UpdateCommand,
    UpdateInfo,
    UpdateState,
    VoiceAssistantEventType as VE,
    VoiceAssistantTimerEventType as VT,
)

assert importlib.metadata.version("aioesphomeapi") == "46.2.0", "pinned client required"

ROOT = Path(__file__).resolve().parents[4]
TARGET = Path(os.environ.get("CARGO_TARGET_DIR", ROOT / "target"))
DEMO = Path(os.environ.get("PINNA_DEMO_BIN", TARGET / "debug" / "examples" / "demo"))

KEY = base64.b64encode(bytes(range(32))).decode()
WRONG_KEY = base64.b64encode(bytes(range(1, 33))).decode()

LIGHT_KEY = 3
MUTE_KEY = 2


class Demo:
    def __init__(self, proc, psk):
        self.proc = proc
        self.psk = psk
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

    def client(self, client_info, psk="default", **kw):
        """Client for this demo; `psk` defaults to the key the demo runs with."""
        psk = self.psk if psk == "default" else psk
        return APIClient("127.0.0.1", self.port, None, client_info=client_info, noise_psk=psk, **kw)

    def cmd(self, name):
        return lambda line: line.get("command") == name

    def send(self, text):
        self.proc.stdin.write(text.encode() + b"\n")

    async def stop(self):
        if self.proc.returncode is None:
            self.proc.kill()
        await self.proc.wait()
        self._reader.cancel()


async def start_demo(psk):
    assert DEMO.exists(), f"{DEMO} missing: build with cargo build -p pinna-esphome --example demo"
    proc = await asyncio.create_subprocess_exec(
        str(DEMO),
        "0",
        *(["--key", psk] if psk else []),
        stdin=asyncio.subprocess.PIPE,
        stdout=asyncio.subprocess.PIPE,
    )
    d = Demo(proc, psk)
    ready = await d.wait(lambda l: l.get("ready"), "ready")
    d.port = ready["port"]
    return d


@pytest_asyncio.fixture(params=[None, KEY], ids=["plaintext", "noise"])
async def demo(request):
    d = await start_demo(request.param)
    yield d
    await d.stop()


@pytest_asyncio.fixture
async def noise_demo():
    d = await start_demo(KEY)
    yield d
    await d.stop()


@pytest_asyncio.fixture
async def client(demo):
    c = demo.client("conformance")
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
    assert info.webserver_port == 80
    assert info.voice_assistant_feature_flags == 61


@pytest.mark.asyncio
async def test_entities(client):
    entities, services = await client.list_entities_services()
    assert not services
    by_key = {e.key: e for e in entities}
    assert LIGHT_KEY in by_key and MUTE_KEY in by_key
    # 1 light, 9 switches, 4 numbers, 1 button, 8 events, 1 text sensor,
    # 1 media player, 2 selects and 1 update.
    assert len(by_key) == len(entities) == 1 + 9 + 4 + 1 + 8 + 1 + 1 + 2 + 1
    light, sw = by_key[LIGHT_KEY], by_key[MUTE_KEY]
    assert isinstance(light, LightInfo) and light.name == "LED ring"
    assert light.object_id == "led_ring"
    assert list(light.effects) == ["Solid", "Pulse", "Flash"]
    assert 35 in [int(m) for m in light.supported_color_modes]
    assert isinstance(sw, SwitchInfo) and sw.name == "Microphone mute"


async def infos(client, cls):
    entities, _ = await client.list_entities_services()
    return {e.name: e for e in entities if isinstance(e, cls)}


@pytest.mark.asyncio
async def test_entity_keys_are_unique_and_stable(client, demo):
    entities, _ = await client.list_entities_services()
    keys = [e.key for e in entities]
    assert len(set(keys)) == len(keys) and 0 not in keys
    again, _ = await client.list_entities_services()
    assert [e.key for e in again] == keys


@pytest.mark.asyncio
async def test_numbers(client, demo):
    nums = await infos(client, NumberInfo)
    expected = {
        "Volume": ("volume", "%", "mdi:volume-high"),
        "Reply volume": ("reply_volume", "%", "mdi:account-voice"),
        "Wake word sensitivity": ("wake_word_sensitivity", "", "mdi:tune-variant"),
        "LED brightness": ("led_brightness", "%", "mdi:brightness-6"),
    }
    assert set(nums) == set(expected)
    for name, (oid, unit, icon) in expected.items():
        n = nums[name]
        assert (n.object_id, n.unit_of_measurement, n.icon) == (oid, unit, icon)
        assert (n.min_value, n.max_value, n.step) == (0.0, 100.0, 1.0)
        assert n.entity_category == EntityCategory.CONFIG
        assert n.mode == NumberMode.SLIDER

    states = {}
    client.subscribe_states(lambda s: states.__setitem__(s.key, s))
    vol = nums["Volume"].key
    await eventually(lambda: vol in states, "initial number state")
    assert isinstance(states[vol], NumberState) and states[vol].missing_state is True
    client.number_command(vol, 42.0)
    cmd = await demo.wait(demo.cmd("number"), "number command")
    assert cmd["object_id"] == "volume" and cmd["value"] == 42.0
    await eventually(lambda: states[vol].missing_state is False, "echoed number state")
    assert states[vol].state == 42.0

    late = demo.client("late")
    await late.connect(login=True)
    seen = {}
    late.subscribe_states(lambda s: seen.__setitem__(s.key, s))
    await eventually(lambda: vol in seen, "number state for late client")
    assert seen[vol].state == 42.0 and not seen[vol].missing_state
    await late.disconnect()


@pytest.mark.asyncio
async def test_button_event_and_text_sensor(client, demo):
    buttons = await infos(client, ButtonInfo)
    assert set(buttons) == {"Cancel request"}
    b = buttons["Cancel request"]
    assert (b.object_id, b.icon) == ("cancel_request", "mdi:close-circle-outline")
    events = await infos(client, EventInfo)
    texts = await infos(client, TextSensorInfo)
    t = texts["Last wake word"]
    assert (set(texts), t.object_id, t.icon) == ({"Last wake word"}, "last_wake_word", "mdi:account-voice")

    states = []
    client.subscribe_states(states.append)
    await eventually(lambda: any(isinstance(s, TextSensorState) for s in states), "initial text state")
    initial = [s for s in states if isinstance(s, TextSensorState)][0]
    assert initial.key == t.key and initial.missing_state is True

    client.button_command(b.key)
    assert (await demo.wait(demo.cmd("button"), "button command"))["object_id"] == "cancel_request"
    action = events["Action button"]
    await eventually(
        lambda: any(isinstance(s, Event) and s.key == action.key and s.event_type == "press" for s in states),
        "action press event",
    )
    await eventually(
        lambda: any(isinstance(s, TextSensorState) and s.state == "Alexa" and not s.missing_state for s in states),
        "last wake word",
    )


@pytest.mark.asyncio
async def test_event_entities_and_unknown_type_dropped(client, demo):
    events = await infos(client, EventInfo)
    button_types = ["press", "long_press", "double_press"]
    expected = {
        "Volume up button": ("volume_up", "button", button_types),
        "Volume down button": ("volume_down", "button", button_types),
        "Microphone mute button": ("microphone_mute", "button", button_types),
        "Action button": ("action", "button", button_types),
        "Voice assistant": (
            "voice_assistant",
            "",
            ["wake_word", "listening", "thinking", "replying", "finished", "stopped"],
        ),
        "Playback": ("playback", "", ["started", "stopped"]),
        "Timer": ("timer", "", ["started", "updated", "cancelled", "finished"]),
        "Notification": ("notification", "", ["cleared"]),
    }
    assert set(events) == set(expected)
    for name, (oid, cls, types) in expected.items():
        e = events[name]
        assert (e.object_id, e.device_class, list(e.event_types)) == (oid, cls, types)

    got = []
    client.subscribe_states(got.append)
    # Events aren't replayed: send them once the subscription is live, which
    # the initial states show.
    await eventually(lambda: any(not isinstance(s, Event) for s in got), "initial states")
    demo.send("event timer bogus")
    demo.send("event timer started")
    await eventually(lambda: any(isinstance(s, Event) for s in got), "timer event")
    assert [(s.key, s.event_type) for s in got if isinstance(s, Event)] == [
        (events["Timer"].key, "started")
    ]


@pytest.mark.asyncio
async def test_text_sensor_update(client, demo):
    t = (await infos(client, TextSensorInfo))["Last wake word"]
    got = []
    client.subscribe_states(got.append)
    demo.send("text last_wake_word Okay Nabu")
    await eventually(
        lambda: any(isinstance(s, TextSensorState) and s.state == "Okay Nabu" for s in got), "text state"
    )
    assert all(s.key == t.key for s in got if isinstance(s, TextSensorState))


@pytest.mark.asyncio
async def test_media_player_info(client):
    players = await infos(client, MediaPlayerInfo)
    assert set(players) == {"Media player"}
    mp = players["Media player"]
    assert mp.object_id == "media_player" and mp.supports_pause is True
    want = (
        MF.PAUSE | MF.VOLUME_SET | MF.VOLUME_MUTE | MF.PLAY_MEDIA | MF.STOP | MF.PLAY | MF.MEDIA_ANNOUNCE
    )
    assert mp.feature_flags == int(want)
    fmts = [(f.format, f.sample_rate, f.num_channels, f.purpose, f.sample_bytes) for f in mp.supported_formats]
    D, A = MediaPlayerFormatPurpose.DEFAULT, MediaPlayerFormatPurpose.ANNOUNCEMENT
    assert fmts == [("mp3", 48000, 2, D, 0), ("mp3", 22050, 1, A, 0), ("wav", 22050, 1, A, 2)]


@pytest.mark.asyncio
async def test_media_player_commands(client, demo):
    mp = (await infos(client, MediaPlayerInfo))["Media player"]
    states = {}
    client.subscribe_states(lambda s: states.__setitem__(s.key, s))
    await eventually(lambda: mp.key in states, "initial media player state")
    assert states[mp.key].state == MediaPlayerState.IDLE

    n = 0

    def media_cmds():
        return [x for x in demo.lines if x.get("command") == "media_player"]

    async def command(want, state=None, **kw):
        nonlocal n
        client.media_player_command(mp.key, **kw)
        n += 1
        await demo.wait(lambda l: len(media_cmds()) >= n, "media player command")
        last = media_cmds()[n - 1]
        assert all(last[k] == v for k, v in want.items()), last
        if state is not None:
            await eventually(lambda: states[mp.key].state == state, f"state {state}")

    await command(
        {"media_url": "http://x/a.mp3", "announcement": False, "media_command": None},
        MediaPlayerState.PLAYING,
        media_url="http://x/a.mp3",
        announcement=False,
    )
    await command(
        {"media_url": "http://x/b.wav", "announcement": True},
        MediaPlayerState.ANNOUNCING,
        media_url="http://x/b.wav",
        announcement=True,
    )
    await command({"volume": 0.25, "media_url": None}, volume=0.25)
    await eventually(lambda: states[mp.key].volume == 0.25, "volume state")
    await command({"media_command": 3}, command=MediaPlayerCommand.MUTE)
    await eventually(lambda: states[mp.key].muted is True, "muted state")
    await command({"media_command": 4}, command=MediaPlayerCommand.UNMUTE)
    await eventually(lambda: states[mp.key].muted is False, "unmuted state")
    await command({"media_command": 1}, MediaPlayerState.PAUSED, command=MediaPlayerCommand.PAUSE)
    await command({"media_command": 0}, MediaPlayerState.PLAYING, command=MediaPlayerCommand.PLAY)
    await command({"media_command": 2}, MediaPlayerState.IDLE, command=MediaPlayerCommand.STOP)


@pytest.mark.asyncio
async def test_states_light_and_switch(client, demo):
    states = {}
    client.subscribe_states(lambda s: states.__setitem__(s.key, s))
    await eventually(lambda: LIGHT_KEY in states and MUTE_KEY in states, "initial states")
    assert isinstance(states[LIGHT_KEY], LightState) and states[LIGHT_KEY].state is False
    assert isinstance(states[MUTE_KEY], SwitchState) and states[MUTE_KEY].state is False

    client.light_command(LIGHT_KEY, state=True, brightness=0.5, rgb=(1.0, 0.0, 0.5), effect="Solid")
    cmd = await demo.wait(demo.cmd("light"), "light command")
    assert cmd["on"] is True and cmd["brightness"] == 0.5
    assert cmd["rgb"] == [1.0, 0.0, 0.5] and cmd["effect"] == "Solid"
    await eventually(lambda: states[LIGHT_KEY].state is True, "echoed light state")
    s = states[LIGHT_KEY]
    assert (s.brightness, s.red, s.green, s.blue, s.effect) == (0.5, 1.0, 0.0, 0.5, "Solid")

    client.switch_command(MUTE_KEY, True)
    assert (await demo.wait(demo.cmd("mute"), "mute command"))["state"] is True
    await eventually(lambda: states[MUTE_KEY].state is True, "echoed switch state")


@pytest.mark.asyncio
async def test_wake_word_switch(client, demo):
    switches = await infos(client, SwitchInfo)
    ww = switches["Wake word"]
    assert ww.object_id == "wake_word" and ww.key != MUTE_KEY
    assert int(ww.entity_category) == 1  # config
    states = {}
    client.subscribe_states(lambda s: states.__setitem__(s.key, s))
    await eventually(lambda: ww.key in states, "initial wake word switch state")
    client.switch_command(ww.key, True)
    cmd = await demo.wait(demo.cmd("switch"), "switch command")
    assert cmd["object_id"] == "wake_word" and cmd["state"] is True
    await eventually(lambda: states[ww.key].state is True, "echoed wake word switch")


@pytest.mark.asyncio
async def test_notification_switch(client, demo):
    switches = await infos(client, SwitchInfo)
    n = switches["Notification"]
    assert n.object_id == "notification" and n.icon == "mdi:bell-ring"
    assert int(n.entity_category) == 0  # a control, not config
    assert n.key not in (MUTE_KEY, switches["Wake word"].key)
    states = {}
    client.subscribe_states(lambda s: states.__setitem__(s.key, s))
    await eventually(lambda: n.key in states, "initial notification switch state")
    client.switch_command(n.key, True)
    cmd = await demo.wait(demo.cmd("switch"), "switch command")
    assert cmd["object_id"] == "notification" and cmd["state"] is True
    await eventually(lambda: states[n.key].state is True, "echoed notification switch")


@pytest.mark.asyncio
async def test_update_channel_select(client, demo):
    sel = (await infos(client, SelectInfo))["Updates"]
    assert sel.object_id == "update_channel" and sel.icon == "mdi:update"
    assert list(sel.options) == ["stable", "beta", "off"]
    assert int(sel.entity_category) == 1  # config
    states = {}
    client.subscribe_states(lambda s: states.__setitem__(s.key, s))
    await eventually(lambda: sel.key in states, "initial select state")
    assert isinstance(states[sel.key], SelectState)
    assert states[sel.key].state == "stable" and not states[sel.key].missing_state
    client.select_command(sel.key, "beta")
    cmd = await demo.wait(demo.cmd("select"), "select command")
    assert cmd["object_id"] == "update_channel" and cmd["state"] == "beta"
    await eventually(lambda: states[sel.key].state == "beta", "echoed select state")


@pytest.mark.asyncio
async def test_assistant_select(client, demo):
    sel = (await infos(client, SelectInfo))["Assistant"]
    assert sel.object_id == "assistant" and sel.icon == "mdi:account-voice"
    assert list(sel.options) == ["Home Assistant", "Standalone"]
    assert int(sel.entity_category) == 1  # config
    states = {}
    client.subscribe_states(lambda s: states.__setitem__(s.key, s))
    await eventually(lambda: sel.key in states, "initial select state")
    assert states[sel.key].state == "Home Assistant" and not states[sel.key].missing_state
    client.select_command(sel.key, "Standalone")
    cmd = await demo.wait(demo.cmd("select"), "select command")
    assert cmd["object_id"] == "assistant" and cmd["state"] == "Standalone"


@pytest.mark.asyncio
async def test_firmware_update(client, demo):
    fw = (await infos(client, UpdateInfo))["Firmware"]
    assert fw.object_id == "firmware" and fw.device_class == "firmware"
    assert int(fw.entity_category) == 1  # config
    states = {}
    client.subscribe_states(lambda s: states.__setitem__(s.key, s))
    await eventually(lambda: fw.key in states, "initial update state")
    assert isinstance(states[fw.key], UpdateState) and states[fw.key].missing_state

    client.update_command(fw.key, UpdateCommand.CHECK)
    cmd = await demo.wait(demo.cmd("update"), "check command")
    assert cmd["object_id"] == "firmware" and cmd["action"] == "check"
    await eventually(lambda: not states[fw.key].missing_state, "checked update state")
    s = states[fw.key]
    assert (s.current_version, s.latest_version, s.title) == ("0.1.0", "0.2.0", "Pinna 0.2.0")
    assert (s.release_summary, s.release_url) == ("Summary.", "https://example.org/notes")
    assert not s.in_progress and not s.has_progress

    client.update_command(fw.key, UpdateCommand.INSTALL)
    await demo.wait(
        lambda line: demo.cmd("update")(line) and line["action"] == "install", "install command"
    )
    await eventually(lambda: states[fw.key].in_progress, "install in progress")
    assert states[fw.key].has_progress and states[fw.key].progress == 50.0


@pytest.mark.asyncio
async def test_late_subscriber_gets_current_state(demo):
    a = demo.client("a")
    await a.connect(login=True)
    a.switch_command(MUTE_KEY, True)
    await demo.wait(demo.cmd("mute"), "mute command")
    b = demo.client("b")
    await b.connect(login=True)
    seen = {}
    b.subscribe_states(lambda s: seen.__setitem__(s.key, s))
    await eventually(lambda: MUTE_KEY in seen, "state for late client")
    assert seen[MUTE_KEY].state is True
    await a.disconnect()
    await b.disconnect()


@pytest.mark.asyncio
async def test_third_client_is_rejected(demo):
    a = demo.client("a")
    b = demo.client("b")
    await a.connect(login=True)
    await b.connect(login=True)
    c = demo.client("c")
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
    c = demo.client("t", provide_time=True)
    await c.connect(login=True)
    demo.send("time")
    t = await demo.wait(demo.cmd("time"), "time response")
    assert abs(t["epoch_seconds"] - time.time()) < 10
    await c.disconnect()
    await demo.wait(demo.cmd("client_disconnected"), "client_disconnected")


@pytest.mark.asyncio
async def test_rename_disconnects_and_reports_the_new_name(demo):
    stopped = asyncio.Event()
    expected = []

    async def on_stop(expected_disconnect):
        expected.append(expected_disconnect)
        stopped.set()

    c = demo.client("rename")
    await c.connect(on_stop=on_stop, login=True)
    await demo.wait(demo.cmd("client_connected"), "client_connected")
    demo.send("rename Küche")
    await asyncio.wait_for(stopped.wait(), 5)
    assert expected == [True], "the device asked the client to disconnect"
    again = demo.client("rename-again")
    await again.connect(login=True)
    info = await again.device_info()
    assert info.friendly_name == "Küche"
    assert info.name == "pinna-demo"
    await again.disconnect()


@pytest.mark.asyncio
async def test_noise_device_info_advertises_encryption(noise_demo):
    c = noise_demo.client("info")
    await c.connect(login=True)
    info = await c.device_info()
    assert info.api_encryption_supported
    assert info.name == "pinna-demo"
    await c.disconnect()


@pytest.mark.asyncio
async def test_noise_wrong_key_is_rejected(noise_demo):
    c = noise_demo.client("bad", psk=WRONG_KEY)
    with pytest.raises(InvalidEncryptionKeyAPIError):
        await c.connect(login=True)


@pytest.mark.asyncio
async def test_noise_server_refuses_plaintext_client(noise_demo):
    c = noise_demo.client("plain", psk=None)
    with pytest.raises(RequiresEncryptionAPIError):
        await c.connect(login=True)


@pytest.mark.asyncio
async def test_noise_set_key_rotates_key(noise_demo):
    c = noise_demo.client("rot")
    await c.connect(login=True)
    new = bytes(range(100, 132))
    # Base64 text, as Home Assistant sends it.
    assert await c.noise_encryption_set_key(base64.b64encode(new))
    got = await noise_demo.wait(noise_demo.cmd("encryption_key_set"), "encryption_key_set")
    assert bytes(got["key"]) == new
    await c.disconnect()

    with pytest.raises(InvalidEncryptionKeyAPIError):
        await noise_demo.client("old").connect(login=True)
    c = noise_demo.client("new", psk=base64.b64encode(new).decode())
    await c.connect(login=True)
    assert (await c.device_info()).name == "pinna-demo"
    # A bad key length is refused and changes nothing.
    assert not await c.noise_encryption_set_key(b"short")
    await c.disconnect()


@pytest.mark.asyncio
async def test_home_assistant_provisions_the_first_key_over_the_zero_key(request):
    from aioesphomeapi.core import ZERO_NOISE_PSK
    d = await start_demo(None)
    try:
        main = d.client("main")
        stopped = asyncio.Event()

        async def on_stop(expected):
            stopped.set()

        await main.connect(on_stop=on_stop, login=True)
        assert (await main.device_info()).api_encryption_provisionable
        side = d.client("provision", psk=ZERO_NOISE_PSK)
        await side.connect(login=True)
        # The all-zero key is no key.
        assert not await side.noise_encryption_set_key(base64.b64encode(bytes(32)))
        new = bytes(range(1, 33))
        assert await side.noise_encryption_set_key(base64.b64encode(new))
        got = await d.wait(d.cmd("encryption_key_set"), "encryption_key_set")
        assert bytes(got["key"]) == new
        # The plaintext connection ends; only the key works from now on.
        await asyncio.wait_for(stopped.wait(), 5)
        with pytest.raises(RequiresEncryptionAPIError):
            await d.client("pt", psk=None).connect(login=True)
        with pytest.raises(InvalidEncryptionKeyAPIError):
            await d.client("zero", psk=ZERO_NOISE_PSK).connect(login=True)
        c = d.client("keyed", psk=base64.b64encode(new).decode())
        await c.connect(login=True)
        assert not (await c.device_info()).api_encryption_provisionable
        await c.disconnect()
    finally:
        await d.stop()


@pytest.mark.asyncio
async def test_older_home_assistant_provisions_over_plaintext(request):
    d = await start_demo(None)
    try:
        c = d.client("pt")
        await c.connect(login=True)
        assert await c.noise_encryption_set_key(base64.b64encode(bytes(range(2, 34))))
        await d.wait(d.cmd("encryption_key_set"), "encryption_key_set")
    finally:
        await d.stop()
