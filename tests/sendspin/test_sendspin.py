"""pinnad's Sendspin player (ADR 0027) against aiosendspin, the server library Music Assistant
runs. conftest.py says how pinnad runs and how the test plays the server; audio.py how what
the speaker played is compared with what was sent."""

import asyncio
import functools
import http.server
import io
import socket
import threading
import wave

import aiohttp
import numpy as np
import pytest
from aiosendspin.models.types import (
    AudioCodec,
    GoodbyeReason,
    MediaCommand,
    PlaybackStateType,
    PlayerCommand,
)
from aiosendspin.server.roles.controller import (
    ControllerPauseEvent,
    ControllerPlayEvent,
    ControllerStopEvent,
)
from aiosendspin.server.roles.metadata import Metadata

from audio import (
    MASTER_GAIN,
    RATE,
    Slew,
    assert_played_in_time,
    make_signal,
    monotonic_us,
    sendspin_gain,
    server_to_monotonic_us,
    tone,
    until_played,
)
from conftest import NAME, SERVER_NAME, media, prefer, sendspin


async def test_handshake_and_first_state(pinnad, server):
    status = await pinnad.status()
    assert sendspin(status)["name"] == NAME, status
    record = pinnad.record.read_text()
    assert f"\nPort={pinnad.port}\n" in record, record
    assert f"TxtText=path=/sendspin name={NAME}\n" in record, record

    client = await server.connect(pinnad.url)
    info = client.info
    assert info.name == NAME
    assert info.supported_roles == ["player@v1", "controller@v1", "metadata@v1"]
    assert info.device_info.manufacturer == "Pinna"
    support = info.player_support
    formats = [(f.codec, f.sample_rate, f.channels, f.bit_depth) for f in support.supported_formats]
    assert formats == [
        (AudioCodec.FLAC, 48_000, 2, 16),
        (AudioCodec.PCM, 48_000, 2, 16),
        (AudioCodec.FLAC, 44_100, 2, 16),
        (AudioCodec.PCM, 44_100, 2, 16),
    ]
    assert support.buffer_capacity == 1 << 20
    assert info.supported_pair_methods.pairing_psk is not None
    assert info.unpaired_access.enabled

    # The player's state waits for a burst of time messages that measures the server's clock.
    first = next(
        i for i, m in enumerate(server.received) if m.type == "client/state" and m.payload.player
    )
    times = sum(1 for m in server.received[:first] if m.type == "client/time")
    assert times >= 8, [m.type for m in server.received[: first + 1]]
    state = server.received[first].payload
    player = state.player
    assert state.available, state
    assert (player.format.codec, player.format.sample_rate) == (AudioCodec.FLAC, 48_000), player
    assert player.required_lead_time_ms == 300, player
    assert player.supported_commands == [PlayerCommand.VOLUME, PlayerCommand.MUTE], player
    assert (player.volume, player.muted) == (100, False), player

    status = await pinnad.until_sendspin("CONNECTED")
    assert sendspin(status)["server"] == SERVER_NAME, status
    assert not sendspin(status).get("exactTiming"), "PipeWire has no hardware pointer to time by"


async def test_the_clock_is_measured_in_bursts(pinnad, server):
    """At least eight time messages in a burst, and bursts at most 10 s apart: the
    specification's rule. pinnad starts one every 9 s."""
    await server.connect(pinnad.url)
    await asyncio.sleep(21)
    times = [m.at_us for m in server.of_type("client/time")]
    bursts = [[times[0]]]
    for previous, t in zip(times, times[1:]):
        if t - previous > 1_000_000:
            bursts.append([])
        bursts[-1].append(t)
    assert len(bursts) >= 3, [len(b) for b in bursts]
    assert all(len(b) >= 8 for b in bursts), [len(b) for b in bursts]
    apart = [(b[0] - a[0]) / 1e6 for a, b in zip(bursts, bursts[1:])]
    assert max(apart) <= 10, f"bursts {apart} s apart"


async def test_a_new_name_reaches_the_server(pinnad, server):
    """A rename: the record first, then a goodbye that has the server connect again and read
    the new name."""
    client = await server.connect(pinnad.url)
    record_at_goodbye = []
    server.watchers.append(
        lambda m: m.type == "client/goodbye" and record_at_goodbye.append(pinnad.record.read_text())
    )
    await pinnad.call("UpdateSettings", {"friendlyName": "Kitchen"})
    await server.until(lambda: record_at_goodbye, "client/goodbye")
    assert [g.payload.reason for g in server.of_type("client/goodbye")] == [GoodbyeReason.RESTART]
    assert "name=Kitchen\n" in record_at_goodbye[0], record_at_goodbye[0]
    await server.until(
        lambda: client.is_connected and client.info.name == "Kitchen", "the new name", 15
    )
    status = await pinnad.until_sendspin("CONNECTED")
    assert sendspin(status)["name"] == "Kitchen", sendspin(status)


@pytest.mark.parametrize(
    ("codec", "rate"),
    [(AudioCodec.PCM, 48_000), (AudioCodec.FLAC, 48_000), (AudioCodec.FLAC, 44_100)],
    ids=["pcm", "flac", "flac-44.1"],
)
async def test_plays_the_signal_in_time(pinnad, server, codec, rate):
    client = await server.connect(pinnad.url)
    prefer(client, codec, rate)
    signal = make_signal()
    slew = Slew()
    start_us = await asyncio.wait_for(server.start_producer(client, signal), 10)
    assert server.player(client).preferred_codec == codec
    await pinnad.until_playing()
    await until_played(signal, start_us)
    assert_played_in_time(pinnad, signal, start_us, slew(), f"{codec.value} {rate}")


async def test_a_new_track_starts_in_time(pinnad, server):
    """A track change: the server starts a new stream over the one that plays, which ends
    nothing but clears what the player holds of the old one (stream/clear)."""
    client = await server.connect(pinnad.url)
    server.start_producer(client, tone(30, hz=440))
    await pinnad.until_playing()
    await asyncio.sleep(1)
    await server.stop_producer()
    signal = make_signal()
    slew = Slew()
    start_us = await asyncio.wait_for(server.start_producer(client, signal), 10)
    await until_played(signal, start_us)
    assert sendspin(await pinnad.status())["state"] == "SENDSPIN_STATE_PLAYING"
    at = assert_played_in_time(pinnad, signal, start_us, slew(), "a new track")
    # The old track ends within the speaker's lead of the clear, well before the new one.
    before = pinnad.played()[at - RATE // 10 : at]
    assert np.max(np.abs(before)) < 1e-3, "the old track sounded on"


async def test_a_format_change_in_place_keeps_the_timeline(pinnad, server):
    """The server switches the stream's format between two chunks (an in-place stream/start),
    as Music Assistant does to follow a new track's sample rate."""
    client = await server.connect(pinnad.url)
    prefer(client, AudioCodec.FLAC, 48_000)
    signal = make_signal(span=5.0)
    slew = Slew()
    # A second ahead, so that most of the signal is sent after the switch.
    start_us = await asyncio.wait_for(server.start_producer(client, signal, ahead_us=1_000_000), 10)
    await pinnad.until_playing()
    await asyncio.sleep(max(0, server_to_monotonic_us(start_us) - monotonic_us()) / 1e6 + 2)
    prefer(client, AudioCodec.PCM, 44_100)
    await until_played(signal, start_us)
    assert server.player(client).preferred_codec == AudioCodec.PCM
    assert_played_in_time(pinnad, signal, start_us, slew(), "flac 48000, then pcm 44100")


async def test_volume_and_mute_both_ways(pinnad, server):
    client = await server.connect(pinnad.url)
    server.start_producer(client, tone(30))
    await pinnad.until_playing()
    player = server.player(client)

    async def level_db() -> float:
        """The tone's level, against full volume, over 300 ms after the ramp."""
        await asyncio.sleep(0.3)
        mark = pinnad.mark()
        await asyncio.sleep(0.3)
        rms = np.sqrt(np.mean(pinnad.played_since(mark) ** 2))
        return 20 * np.log10(max(rms, 1e-9) / (0.5 / np.sqrt(2)) / MASTER_GAIN)

    # The server sets the stream's level: pinnad applies the specification's curve and shows
    # the level as the media player's.
    assert abs(await level_db()) < 0.5
    player.set_volume(30)
    await pinnad.until(lambda s: media(s).get("volume") == 30, "media volume 30")
    assert abs(await level_db() - 20 * np.log10(sendspin_gain(30))) < 0.5
    player.set_mute(True)
    await pinnad.until(lambda s: media(s).get("muted"), "media muted")
    assert await level_db() < -60
    player.set_mute(False)
    await pinnad.until(lambda s: not media(s).get("muted"), "media unmuted")

    # The dashboard's and Home Assistant's media volume set the same level, and the server
    # hears it in client/state.
    await pinnad.call("ControlMedia", {"volume": 60})
    await server.until(lambda: player.volume == 60, "volume 60 in client/state")
    assert abs(await level_db() - 20 * np.log10(sendspin_gain(60))) < 0.5
    await pinnad.call("ControlMedia", {"command": "MEDIA_COMMAND_MUTE"})
    await server.until(lambda: player.muted, "muted in client/state")
    await pinnad.call("ControlMedia", {"command": "MEDIA_COMMAND_UNMUTE"})
    await server.until(lambda: not player.muted, "unmuted in client/state")
    assert player.volume == 60


async def test_metadata_reaches_the_media_state(pinnad, server):
    client = await server.connect(pinnad.url)
    server.start_producer(client, tone(30))
    await pinnad.until_playing()
    metadata = client.group.group_role("metadata")
    metadata.set_metadata(
        Metadata(title="Clair de lune", artist="Debussy", album="Suite bergamasque")
    )
    status = await pinnad.until(lambda s: media(s).get("title") == "Clair de lune", "the title")
    assert (media(status).get("artist"), media(status).get("album")) == (
        "Debussy",
        "Suite bergamasque",
    )
    # Music Assistant passes a title on at any length: cut to 255 bytes, and the server stays.
    metadata.set_metadata(Metadata(title="Ä" * 150, artist="Debussy"))
    status = await pinnad.until(
        lambda s: media(s).get("title", "").startswith("Ä"), "the long title"
    )
    assert media(status)["title"] == "Ä" * 127, len(media(status)["title"].encode())
    assert sendspin(status)["state"] == "SENDSPIN_STATE_PLAYING"


async def test_pause_and_play_act_on_the_group(pinnad, server):
    client = await server.connect(pinnad.url)
    client.group.group_role("controller").set_supported_commands(
        [MediaCommand.PLAY, MediaCommand.PAUSE, MediaCommand.STOP]
    )
    server.start_producer(client, tone(30))
    await pinnad.until_playing()

    # Pause on the satellite pauses the group: the server hears a controller command and, as
    # Music Assistant does, stops the group's stream.
    await pinnad.call("ControlMedia", {"command": "MEDIA_COMMAND_PAUSE"})
    await server.until(
        lambda: any(isinstance(e, ControllerPauseEvent) for e in server.group_events), "pause"
    )
    await server.stop_producer()
    await client.group.stop(keep_progress=True)
    status = await pinnad.until(lambda s: media(s).get("state") == "MEDIA_STATE_PAUSED", "paused")
    assert media(status).get("source") == "sendspin", media(status)
    assert sendspin(status)["state"] == "SENDSPIN_STATE_CONNECTED", sendspin(status)

    # Play resumes the group, here and everywhere else.
    await pinnad.call("ControlMedia", {"command": "MEDIA_COMMAND_PLAY"})
    await server.until(
        lambda: any(isinstance(e, ControllerPlayEvent) for e in server.group_events), "play"
    )
    server.start_producer(client, tone(30))
    await pinnad.until_playing()

    # Stop goes to the group too. The group then reports `stopped` as a paused one does, yet
    # the satellite shows Idle: only its own pause leaves the group paused (ADR 0027 §14).
    await pinnad.call("ControlMedia", {"command": "MEDIA_COMMAND_STOP"})
    await server.until(
        lambda: any(isinstance(e, ControllerStopEvent) for e in server.group_events), "stop"
    )
    await server.stop_producer()
    await client.group.stop(keep_progress=True)
    await until_idle(pinnad, "idle after stop")

    # A queue that runs out: Music Assistant ends the stream and stops the group.
    server.start_producer(client, tone(30))
    await pinnad.until_playing()
    await server.stop_producer()
    await client.group.stop()
    await until_idle(pinnad, "idle at the end of the queue")


async def until_idle(pinnad, what: str):
    """Until the media player is Idle, and still is a moment later: the lane goes idle once
    the stream fades out, and a group shown paused would show Paused again."""
    await pinnad.until(lambda s: media(s).get("state") == "MEDIA_STATE_IDLE", what)
    await asyncio.sleep(0.5)
    status = await pinnad.status()
    assert media(status).get("state") == "MEDIA_STATE_IDLE", media(status)
    assert media(status).get("source", "") != "sendspin", media(status)
    assert sendspin(status)["state"] == "SENDSPIN_STATE_CONNECTED", sendspin(status)


@pytest.fixture
def wav_url(tmp_path):
    """3 s of tone as a 48 kHz WAV, on a local HTTP server."""
    body = io.BytesIO()
    with wave.open(body, "wb") as w:
        w.setnchannels(2)
        w.setsampwidth(2)
        w.setframerate(RATE)
        w.writeframes(tone(3, 0.25).tobytes())
    (tmp_path / "www").mkdir()
    (tmp_path / "www" / "tone.wav").write_bytes(body.getvalue())
    handler = functools.partial(
        http.server.SimpleHTTPRequestHandler, directory=str(tmp_path / "www")
    )
    httpd = http.server.ThreadingHTTPServer(("127.0.0.1", 0), handler)
    threading.Thread(target=httpd.serve_forever, daemon=True).start()
    try:
        yield f"http://127.0.0.1:{httpd.server_port}/tone.wav"
    finally:
        httpd.shutdown()


async def test_another_source_leaves_the_group(pinnad, server, wav_url):
    client = await server.connect(pinnad.url)
    server.start_producer(client, tone(30))
    await pinnad.until_playing()
    group = client.group

    # The satellite leaves the group rather than pausing it. The group has no other member
    # here, so the server stops it; with others, they would play on.
    await pinnad.call("PlayMedia", {"url": wav_url})
    await server.until(lambda: server.of_type("client/leave"), "client/leave")
    await server.until(lambda: group.state == PlaybackStateType.STOPPED, "the group stopped")
    status = await pinnad.until(lambda s: media(s).get("url") == wav_url, "the URL playing")
    assert media(status).get("source", "") != "sendspin", media(status)
    assert sendspin(status)["state"] == "SENDSPIN_STATE_CONNECTED", sendspin(status)


async def test_reconnects_after_the_server_drops_it(pinnad, server):
    client = await server.connect(pinnad.url)
    server.start_producer(client, tone(60))
    await pinnad.until_playing()

    # The connection drops under the server, without a close on either side, as when the
    # network goes; the server dials again a second later.
    client.connection.websocket_connection.get_extra_info("socket").shutdown(socket.SHUT_RDWR)
    await pinnad.until(
        lambda s: sendspin(s)["state"] != "SENDSPIN_STATE_PLAYING", "the stream ended"
    )
    await server.until(lambda: client.is_connected, "reconnected", timeout=15)
    await pinnad.until_playing(timeout=15)
    # The group's stream goes on: the tone sounds again.
    mark = pinnad.mark()
    await asyncio.sleep(1.0)
    assert np.max(np.abs(pinnad.played_since(mark))) > 0.05, "silent after reconnecting"


async def test_a_second_server_that_plays_takes_over(pinnad, servers):
    """Two servers, as Music Assistant and a second one: playback against playback, the
    newcomer wins and the first is told so (ADR 0027 §3)."""
    first = servers.new("First server")
    client = await first.connect(pinnad.url)
    first.start_producer(client, tone(30))
    await pinnad.until_playing("First server")
    second = servers.new("Second server")
    other = await second.connect(pinnad.url, approved=client.client_id)
    await first.until(lambda: first.of_type("client/goodbye"), "client/goodbye to the first")
    assert [g.payload.reason for g in first.of_type("client/goodbye")] == [
        GoodbyeReason.ANOTHER_SERVER
    ]
    second.start_producer(other, tone(30, hz=440))
    await pinnad.until_playing("Second server")
    assert not second.of_type("client/goodbye")


async def test_a_second_server_that_does_not_play_is_turned_away(pinnad, servers):
    """Nothing against playback: the newcomer is turned away, and the music plays on."""
    first = servers.new("First server")
    client = await first.connect(pinnad.url)
    first.start_producer(client, tone(30))
    await pinnad.until_playing("First server")
    second = servers.new("Second server")
    second.api.connect_to_client(pinnad.url)
    await second.until(lambda: second.of_type("client/goodbye"), "client/goodbye to the second")
    assert [g.payload.reason for g in second.of_type("client/goodbye")] == [
        GoodbyeReason.CONCURRENT_ATTEMPT
    ]
    status = await pinnad.status()
    assert (sendspin(status)["state"], sendspin(status)["server"]) == (
        "SENDSPIN_STATE_PLAYING",
        "First server",
    )
    assert client.is_connected and not first.of_type("client/goodbye")


async def test_a_browser_is_refused(pinnad):
    async with aiohttp.ClientSession() as session:
        with pytest.raises(aiohttp.WSServerHandshakeError) as refused:
            await session.ws_connect(pinnad.url, headers={"Origin": "http://example.com"})
        assert refused.value.status == 403


async def test_switching_off_says_goodbye(pinnad, server):
    client = await server.connect(pinnad.url)
    record_at_goodbye = []
    server.watchers.append(
        lambda m: m.type == "client/goodbye" and record_at_goodbye.append(pinnad.record.exists())
    )
    await pinnad.call("UpdateSettings", {"sendspin": False})
    await server.until(lambda: record_at_goodbye, "client/goodbye")
    assert record_at_goodbye == [False], "the record outlived the connection"
    assert [g.payload.reason for g in server.of_type("client/goodbye")] == [GoodbyeReason.SHUTDOWN]
    await pinnad.until_sendspin("OFF")
    # A shutdown is not a restart: the server does not dial again, and nothing listens.
    await asyncio.sleep(2)
    assert not client.is_connected, "the server dialled again"
    async with aiohttp.ClientSession() as session:
        with pytest.raises(aiohttp.ClientConnectorError):
            await session.ws_connect(pinnad.url)
