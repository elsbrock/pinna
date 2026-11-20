"""The test signals, the clocks, and how the suite compares what pinnad's speaker played with
what the server sent."""

import asyncio
import time
from dataclasses import dataclass

import numpy as np

# What pinnad writes to the speaker, and what the test streams: 48 kHz stereo s16.
RATE = 48_000
BYTES_PER_FRAME = 4
TONE_HZ = 997.0
# The satellite's volume the test starts pinnad at, 100: the top of its curve, -12 dB
# (`pinna_audio::volume_gain`). The stream's own volume applies on top.
MASTER_GAIN = 10 ** (-12 / 20)


def monotonic_us() -> int:
    """CLOCK_MONOTONIC, the clock pinnad and the fake speaker stamp with."""
    return time.clock_gettime_ns(time.CLOCK_MONOTONIC) // 1000


def raw_monotonic_us() -> int:
    """CLOCK_MONOTONIC_RAW, the clock aiosendspin's server timestamps audio with on Linux."""
    return time.clock_gettime_ns(time.CLOCK_MONOTONIC_RAW) // 1000


def server_to_monotonic_us(server_us: int) -> int:
    """A server timestamp on pinnad's clock. The two clocks differ by an offset that NTP's
    slewing moves by at most 500 ppm, a few us over a test."""
    return server_us + monotonic_us() - raw_monotonic_us()


class Slew:
    """How much faster pinnad's clock runs than the server's from now on. pinnad follows the
    server's clock, which NTP doesn't slew; the speaker runs on one it does. Over a test the
    difference is a few frames at most, but it is a real one."""

    def __init__(self):
        self.mono, self.raw = monotonic_us(), raw_monotonic_us()

    def __call__(self) -> float:
        return (monotonic_us() - self.mono) / (raw_monotonic_us() - self.raw) - 1


def sendspin_gain(volume: int) -> float:
    """The specification's curve for the stream's own volume."""
    return (volume / 100) ** 1.5


def stereo(left: np.ndarray, right: np.ndarray) -> np.ndarray:
    return np.round(np.stack([left, right], axis=1) * 32767).astype(np.int16)


def make_signal(span: float = 2.0, amplitude: float = 0.5) -> np.ndarray:
    """1 s of a 997 Hz tone, then a chirp from 200 Hz to 2 kHz over `span` s, which has one
    alignment only. The right channel is the left at half the level, so a swap or a mix
    shows."""
    t = np.arange(RATE) / RATE
    tone_part = np.sin(2 * np.pi * TONE_HZ * t)
    t = np.arange(int(RATE * span)) / RATE
    chirp = np.sin(2 * np.pi * (200 * t + (2000 - 200) * t**2 / (2 * span)))
    left = amplitude * np.concatenate([tone_part, chirp])
    return stereo(left, left / 2)


def tone(seconds: float, amplitude: float = 0.5, hz: float = TONE_HZ) -> np.ndarray:
    t = np.arange(int(RATE * seconds)) / RATE
    left = amplitude * np.sin(2 * np.pi * hz * t)
    return stereo(left, left)


def correlate(a: np.ndarray, b: np.ndarray) -> np.ndarray:
    """Cross-correlation of `a` with `b` at every lag where `b` fits in `a`."""
    n = len(a) + len(b)
    size = 1 << (n - 1).bit_length()
    full = np.fft.irfft(np.fft.rfft(a, size) * np.conj(np.fft.rfft(b, size)), size)
    return full[: len(a) - len(b) + 1]


def locate(played: np.ndarray, reference: np.ndarray, part: slice) -> int:
    """The frame of `played` at which `reference` starts, found by the part that has a single
    alignment."""
    return int(np.argmax(correlate(played, reference[part]))) - part.start


@dataclass
class Window:
    """100 ms of the reference against what played."""

    frame: int
    """Where it starts in the reference."""
    lag: int
    """How many frames later than expected it played, best within the search."""
    rho: float
    """The correlation there, interpolated between frames, since the player resamples and
    can play a fraction of a frame early or late: 1 is the same waveform."""
    level_db: float
    """Its level against the reference at the expected gain."""


def windows(played, reference, start, gain, window=RATE // 10, skip=RATE // 20, search=20):
    """Every 100 ms after the fade-in. The search stays under half a period of the highest
    frequency, 2 kHz, so that it can't land on the next one."""
    out = []
    for a in range(skip, len(reference) - window, window):
        r = reference[a : a + window] * gain
        lo = start + a - search
        segment = played[lo : lo + window + 2 * search]
        best = int(np.clip(np.argmax(correlate(segment, r)), 1, 2 * search - 1))

        def rho_at(k):
            p = segment[k : k + window]
            return float(np.dot(p, r) / np.sqrt(np.dot(p, p) * np.dot(r, r)))

        y0, y1, y2 = rho_at(best - 1), rho_at(best), rho_at(best + 1)
        curve = y0 - 2 * y1 + y2
        rho = y1 - (y0 - y2) ** 2 / (8 * curve) if curve < 0 else y1
        p = segment[best : best + window]
        level_db = 20 * np.log10(np.sqrt(np.mean(p**2)) / np.sqrt(np.mean(r**2)))
        out.append(Window(a, best - search, rho, float(level_db)))
    return out


def peak_hz(x: np.ndarray) -> float:
    """The strongest frequency, interpolated between bins."""
    spectrum = np.abs(np.fft.rfft(x * np.hanning(len(x))))
    k = int(np.argmax(spectrum[1:-1])) + 1
    a, b, c = np.log(spectrum[k - 1 : k + 2])
    return (k + 0.5 * (a - c) / (a - 2 * b + c)) * RATE / len(x)


async def until_played(signal: np.ndarray, start_us: int):
    """Until the speaker has had all of `signal`, which starts at server time `start_us`."""
    end_us = server_to_monotonic_us(start_us) + len(signal) * 1_000_000 // RATE
    await asyncio.sleep(max(0, end_us - monotonic_us()) / 1e6 + 0.5)


def assert_played_in_time(pinnad, signal, start_us: int, slew: float, label: str) -> int:
    """`signal` played from server time `start_us` on: its first sample when its timestamp
    says, the same waveform at the level the volumes set, and on the server's timeline to the
    end. Returns the frame of the speaker file it starts at."""
    played = pinnad.played()
    reference = signal[:, 0].astype(np.float64) / 32768
    at = locate(played, reference, slice(RATE, len(reference)))

    # As far as the fake speaker's times can show: they are late by however long it took to
    # read the pipe.
    error_ms = (pinnad.heard_us(at) - server_to_monotonic_us(start_us)) / 1000
    # The player resamples, so the waveform matches closely, not bit for bit.
    measured = windows(played, reference, at, sendspin_gain(100) * MASTER_GAIN)
    off = [w.lag - round(slew * (w.frame - len(reference) / 2)) for w in measured]
    worst = min(measured, key=lambda w: w.rho)
    levels = [w.level_db for w in measured]
    print(
        f"{label}: first sample {error_ms:+.2f} ms from its timestamp; "
        f"{min(off)}..{max(off)} frames off the timeline (clock slew {slew * 1e6:+.1f} ppm); "
        f"correlation >= {worst.rho:.5f}; level {min(levels):+.2f}..{max(levels):+.2f} dB"
    )
    assert abs(error_ms) < 5, f"the signal started {error_ms:+.2f} ms from its timestamp"
    assert worst.rho > 0.999, worst
    assert max(abs(o) for o in off) <= 12, f"left the timeline by more than 0.25 ms: {off}"
    assert all(abs(db) < 0.5 for db in levels), measured
    tone_part = slice(at + RATE // 10, at + RATE)
    hz = peak_hz(played[tone_part])
    assert abs(hz - TONE_HZ) < 1, hz
    ratio = np.std(pinnad.played(channel=1)[tone_part]) / np.std(played[tone_part])
    assert abs(ratio - 0.5) < 0.02, f"the right channel is {ratio:.3f} of the left"
    return at
