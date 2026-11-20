"""Music Assistant's Sendspin server library, aiosendspin, against a pinnad on this computer
(ADR 0027).

pinnad runs from target/release with the Sendspin switch on, its files in a temporary
directory and every port its own, so it runs beside a development pinnad. Its speaker is the
PipeWire backend with a fake `pw-record` (silence) and `pw-play` (what it plays, and when each
piece arrived) first on PATH, so a test reads what the satellite would have sounded. The test
is the server: it dials pinnad as Music Assistant does once it finds the record, approves its
unpaired access, and streams through a group as Music Assistant's provider does.

Run: just sendspin-conformance (needs uv; the first run fetches aiosendspin from PyPI).
"""

import asyncio
import contextlib
import importlib.metadata
import json
import os
import socket
import subprocess
import sys
import time
import urllib.error
import urllib.request
from collections.abc import Callable
from dataclasses import dataclass
from pathlib import Path

import numpy as np
import pytest
from aiosendspin.models.types import AudioCodec, ConnectionReason
from aiosendspin.noise import Identity
from aiosendspin.noise.trust_store import InMemoryServerPairingStore
from aiosendspin.server import AudioFormat, SendspinClient, SendspinServer
from aiosendspin.server.connection import SendspinConnection
from aiosendspin.server.push_stream import PushStream, StreamStoppedError

from audio import BYTES_PER_FRAME, RATE, monotonic_us

ROOT = Path(__file__).resolve().parents[2]
TARGET = Path(os.environ.get("CARGO_TARGET_DIR", ROOT / "target"))
PINNAD = Path(os.environ.get("PINNAD_BIN", TARGET / "release" / "pinnad"))

NAME = "Conformance"
SERVER_NAME = "Conformance server"
# pinnad writes each 20 ms chunk to the player this long before it sounds
# (`SpeakerConfig::lead`): on PipeWire, a frame sounds when it was written plus this.
SPEAKER_LEAD_US = 100_000
# Music Assistant fills a group's buffer this far ahead (`_PRODUCER_BUFFER_LIMIT_US`), so
# the player's buffer_capacity is what holds the server back.
PRODUCER_AHEAD_US = 30_000_000
# The slices of PCM the producer commits.
SLICE = RATE // 50
PCM_S16 = AudioFormat(sample_rate=RATE, bit_depth=16, channels=2)

PW_PLAY = """#!{python}
# pinnad's speaker on the PipeWire backend: what it plays goes to $PINNA_SPEAKER, and when
# each piece arrived to $PINNA_SPEAKER.times: monotonic us, offset, length.
import os, sys, time
path = os.environ["PINNA_SPEAKER"]
fd = os.open(path, os.O_WRONLY | os.O_APPEND | os.O_CREAT, 0o644)
times = open(path + ".times", "a")
while data := sys.stdin.buffer.read1(1 << 16):
    t = time.clock_gettime_ns(time.CLOCK_MONOTONIC) // 1000
    offset = os.lseek(fd, 0, os.SEEK_END)
    os.write(fd, data)
    times.write(f"{{t}} {{offset}} {{len(data)}}\\n")
    times.flush()
"""

PW_RECORD = """#!{python}
# pinnad's microphone on the PipeWire backend: silence, 16 kHz mono s16, in real time.
import sys, time
out = sys.stdout.buffer
due = time.monotonic()
while True:
    out.write(bytes(640))
    out.flush()
    due += 0.02
    time.sleep(max(0.0, due - time.monotonic()))
"""


def pytest_report_header(config):
    return f"aiosendspin {importlib.metadata.version('aiosendspin')}"


def free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def sendspin(status: dict) -> dict:
    return status.get("sendspin", {})


def media(status: dict) -> dict:
    return status.get("media", {})


class Pinnad:
    """One pinnad process and its files."""

    def __init__(self, directory: Path):
        self.dir = directory
        self.api = free_port()
        self.port = free_port()
        self.esphome = free_port()
        self.url = f"ws://127.0.0.1:{self.port}/sendspin"
        self.record = directory / "dnssd" / "pinna-sendspin.dnssd"
        self.speaker = directory / "speaker.raw"
        self.log_file = directory / "pinnad.log"
        self.proc: subprocess.Popen | None = None

    def start(self):
        if not PINNAD.exists():
            pytest.fail(f"{PINNAD} not found: cargo build --release -p pinnad")
        bin_dir = self.dir / "bin"
        bin_dir.mkdir()
        for name, text in (("pw-play", PW_PLAY), ("pw-record", PW_RECORD)):
            path = bin_dir / name
            path.write_text(text.format(python=sys.executable))
            path.chmod(0o755)
        # The satellite's volume at the top, so the stream's own volume alone sets the level.
        (self.dir / "settings.toml").write_text("volume = 100\nsendspin = true\n")
        (self.dir / "pinnad.toml").write_text(
            f"""
[device]
name = "pinna-sendspin"
friendly_name = "{NAME}"
[audio]
backend = "pipewire"
[wake]
enabled = false
[esphome]
port = {self.esphome}
mdns = false
key_file = "{self.dir / "esphome-key"}"
[leds]
backend = "none"
[api]
listen = "127.0.0.1:{self.api}"
[sendspin]
port = {self.port}
record = "{self.record}"
"""
        )
        env = {k: v for k, v in os.environ.items() if k != "JOURNAL_STREAM"}
        env["PATH"] = f"{bin_dir}:{env.get('PATH', '')}"
        env["PINNA_SPEAKER"] = str(self.speaker)
        env["RUST_LOG"] = os.environ.get("RUST_LOG", "info")
        self.log = open(self.log_file, "w+")
        self.proc = subprocess.Popen(
            [
                str(PINNAD),
                "--config",
                str(self.dir / "pinnad.toml"),
                "--local-config",
                str(self.dir / "local.toml"),
                "--settings",
                str(self.dir / "settings.toml"),
            ],
            stdout=self.log,
            stderr=subprocess.STDOUT,
            env=env,
        )

    def stop(self):
        if self.proc is not None and self.proc.poll() is None:
            self.proc.terminate()
            try:
                self.proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                self.proc.kill()
                self.proc.wait()

    def log_text(self) -> str:
        return self.log_file.read_text(errors="replace")

    def rpc(self, method: str, body: dict) -> dict:
        """One DeviceService call in the Connect protocol's JSON, as the dashboard makes it."""
        req = urllib.request.Request(
            f"http://127.0.0.1:{self.api}/pinna.v1.DeviceService/{method}",
            data=json.dumps(body).encode(),
            headers={"Content-Type": "application/json"},
        )
        with urllib.request.urlopen(req, timeout=5) as resp:
            return json.load(resp)

    async def call(self, method: str, body: dict | None = None) -> dict:
        return await asyncio.to_thread(self.rpc, method, body or {})

    async def status(self) -> dict:
        return (await self.call("GetStatus"))["status"]

    async def until(self, pred, what: str, timeout: float = 10.0) -> dict:
        """The first status for which `pred` holds."""
        deadline = time.monotonic() + timeout
        status = None
        while time.monotonic() < deadline:
            assert self.proc.poll() is None, "pinnad exited"
            try:
                status = await self.status()
            except (urllib.error.URLError, ConnectionError):
                status = None
            if status is not None and pred(status):
                return status
            await asyncio.sleep(0.05)
        raise AssertionError(f"{what}: {json.dumps(status, indent=1) if status else status}")

    async def until_sendspin(self, state: str, timeout: float = 10.0) -> dict:
        return await self.until(
            lambda s: sendspin(s).get("state") == f"SENDSPIN_STATE_{state}",
            f"Sendspin {state}",
            timeout,
        )

    async def until_playing(self, server: str = SERVER_NAME, timeout: float = 10.0) -> dict:
        """Until `server`'s stream sounds. The player says Playing from the stream's start; the
        media player only once its first audio is due and takes the lane (ADR 0027 §13), and
        only then do pause and play go to the group."""
        return await self.until(
            lambda s: (
                sendspin(s).get("state") == "SENDSPIN_STATE_PLAYING"
                and sendspin(s).get("server") == server
                and media(s).get("source") == "sendspin"
                and media(s).get("state") == "MEDIA_STATE_PLAYING"
            ),
            f"playing from {server}",
            timeout,
        )

    def played(self, channel: int = 0) -> np.ndarray:
        """Everything the speaker played on `channel` (0 left, 1 right), as floats."""
        return self.played_since(0, channel)

    def mark(self) -> int:
        """Where the speaker file ends now, on a frame."""
        size = self.speaker.stat().st_size if self.speaker.exists() else 0
        return size - size % BYTES_PER_FRAME

    def played_since(self, mark: int, channel: int = 0) -> np.ndarray:
        """What the speaker played on `channel` after `mark`, as floats."""
        data = self.speaker.read_bytes()[mark:]
        data = data[: len(data) - len(data) % BYTES_PER_FRAME]
        return np.frombuffer(data, dtype="<i2")[channel::2] / 32768.0

    def writes(self) -> np.ndarray:
        """When each piece reached the speaker: monotonic us, byte offset, length."""
        lines = (self.dir / "speaker.raw.times").read_text().splitlines()
        return np.array([line.split() for line in lines], dtype=np.int64).reshape(-1, 3)

    def heard_us(self, frame: int) -> int:
        """When pinnad means `frame` of the speaker file to sound, on the monotonic clock.

        pinnad writes a chunk as its first frame comes within SPEAKER_LEAD_US of sounding, and
        later frames follow at 48 kHz. A piece that reached the fake speaker late only makes the
        estimate later, so the earliest estimate from the pieces of the 2 s before wins.
        """
        writes = self.writes()
        offset = frame * BYTES_PER_FRAME
        per_second = RATE * BYTES_PER_FRAME
        before = writes[(writes[:, 1] <= offset) & (writes[:, 1] > offset - 2 * per_second)]
        assert len(before), f"frame {frame} never reached the speaker"
        estimates = (
            before[:, 0] + SPEAKER_LEAD_US + (offset - before[:, 1]) * 1_000_000 // per_second
        )
        return int(estimates.min())


@pytest.fixture
async def pinnad(tmp_path):
    """pinnad, listening: its record is out, so Music Assistant would find it now."""
    p = Pinnad(tmp_path)
    p.start()
    try:
        await p.until_sendspin("READY")
        yield p
    finally:
        p.stop()
        print("---- pinnad log ----\n" + p.log_text())
    assert p.proc.returncode in (0, -15), f"pinnad exited with {p.proc.returncode}"
    assert "panicked" not in p.log_text(), "pinnad panicked"


@dataclass
class Received:
    """A message pinnad sent, as the server parsed it."""

    at_us: int
    type: str
    payload: object


class Servers:
    """Listens in on every aiosendspin server of a test: what pinnad sends each, and every
    deviation from the specification the library notes. Music Assistant tolerates them, the
    suite doesn't. It hooks the library's message dispatch, which the versions the recipe
    pins share."""

    def __init__(self, monkeypatch):
        self.all: list[Server] = []
        self.deviations: list[str] = []
        handle = SendspinConnection._handle_message

        async def recording(conn, message, timestamp_us):
            received = Received(monotonic_us(), message.type, message.payload)
            for server in self.all:
                if conn._server is server.api:
                    server.received.append(received)
                    for watch in server.watchers:
                        watch(received)
            return await handle(conn, message, timestamp_us)

        monkeypatch.setattr(SendspinConnection, "_handle_message", recording)
        for cls, name in (
            (SendspinClient, "flag_noncompliance"),
            (SendspinConnection, "_flag_noncompliance"),
        ):
            flag = getattr(cls, name)

            def noting(obj, reason, _flag=flag):
                self.deviations.append(reason)
                return _flag(obj, reason)

            monkeypatch.setattr(cls, name, noting)

    def new(self, name: str = SERVER_NAME) -> "Server":
        server = Server(name)
        self.all.append(server)
        return server


class Server:
    """An aiosendspin server that dials pinnad as Music Assistant does."""

    def __init__(self, name: str):
        self.received: list[Received] = []
        self.watchers: list[Callable[[Received], None]] = []
        self.group_events: list[object] = []
        # No listener and no mDNS: the test dials pinnad itself, and nothing on the network
        # finds the server.
        self.api = SendspinServer(
            asyncio.get_running_loop(),
            Identity.generate(),
            name,
            pairing_store=InMemoryServerPairingStore(),
        )
        self.producer: asyncio.Task | None = None

    async def close(self):
        await self.stop_producer()
        await self.api.close()

    def of_type(self, kind: str) -> list[Received]:
        return [m for m in self.received if m.type == kind]

    async def until(self, pred, what: str, timeout: float = 10.0):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            value = pred()
            if value:
                return value
            await asyncio.sleep(0.02)
        raise AssertionError(f"{what}; pinnad sent {[m.type for m in self.received]}")

    async def connect(self, url: str, approved: str | None = None) -> SendspinClient:
        """Dials pinnad and approves its unpaired access once its hello is in, as Music
        Assistant's provider does, then waits for the player to report its state. A server
        that has `approved` the player already dials it to play, as Music Assistant does to
        start a group, and asks for playback in its first activation."""
        if approved is None:
            self.api.connect_to_client(url)
        else:
            await self.api.trust_unpaired(approved)
            self.api.connect_to_client(url, connection_reason=ConnectionReason.PLAYBACK)
        client = await self.until(
            lambda: next((c for c in self.api.clients if c.info_or_none is not None), None),
            "pinnad's client/hello",
        )
        assert client.info.unpaired_access.enabled, client.info
        if approved is None:
            await self.api.trust_unpaired(client.client_id)
        await self.until(
            lambda: (
                client.is_connected
                and client.role("player@v1") in client.active_roles
                and not client.awaits_role_state("player")
            ),
            "the player role, activated and reported",
        )
        client.group.add_event_listener(lambda _group, event: self.group_events.append(event))
        return client

    def player(self, client: SendspinClient):
        return client.role("player@v1")

    def start_producer(
        self, client: SendspinClient, signal: np.ndarray, ahead_us: int = PRODUCER_AHEAD_US
    ) -> asyncio.Future:
        """Streams `signal` (stereo s16 at 48 kHz), then silence until stopped, through the
        client's group, up to `ahead_us` ahead. A stream that already plays is replaced, as on
        a track change. The future resolves to the server time of the signal's first sample."""
        started: asyncio.Future = asyncio.get_running_loop().create_future()
        stream = client.group.start_stream()
        self.producer = asyncio.create_task(self._produce(stream, signal, started, ahead_us))
        return started

    async def _produce(self, stream: PushStream, signal, started, ahead_us):
        silence = np.zeros((SLICE, 2), dtype=np.int16)
        at = 0
        try:
            while True:
                piece = signal[at : at + SLICE] if at < len(signal) else silence
                at += SLICE
                stream.prepare_audio(np.ascontiguousarray(piece).tobytes(), PCM_S16)
                start = await stream.commit_audio()
                if not started.done():
                    started.set_result(start)
                await stream.sleep_to_limit_buffer(ahead_us if at < len(signal) else 2_000_000)
        except (StreamStoppedError, asyncio.CancelledError):
            pass

    async def stop_producer(self):
        if self.producer is not None:
            self.producer.cancel()
            with contextlib.suppress(asyncio.CancelledError):
                await self.producer
            self.producer = None


@pytest.fixture
async def servers(monkeypatch):
    s = Servers(monkeypatch)
    try:
        yield s
    finally:
        for server in s.all:
            await server.close()
    assert not s.deviations, f"aiosendspin found deviations from the specification: {s.deviations}"


@pytest.fixture
async def server(servers):
    """Music Assistant."""
    return servers.new()


def prefer(client: SendspinClient, codec: AudioCodec, rate: int = RATE):
    """The server's own choice of format, as Music Assistant's per-player setting, or its
    following the source's sample rate, makes it."""
    player = client.role("player@v1")
    wanted = AudioFormat(sample_rate=rate, bit_depth=16, channels=2)
    assert player.set_preferred_format(wanted, codec), f"{codec.value} at {rate} Hz was not offered"
