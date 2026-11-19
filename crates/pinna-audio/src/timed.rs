//! A timed source for the media lane: decoded audio that sounds at the time
//! a server's clock sets for it (ADR 0027 §10–§13).
//!
//! pinnad pushes chunks stamped in server time into a [`TimedSink`] and
//! gives the lane a [`ServerClock`], which maps local monotonic time to
//! server time. For each chunk the speaker mixes, it tells the lane when
//! the chunk's first frame will be heard and at what frame rate the
//! speaker runs. The source reads its audio from the server time that
//! must be heard then, through a resampler whose speed follows both
//! clocks, and corrects what error remains:
//!
//! | Event | The source |
//! | --- | --- |
//! | its first audio is due | replaces what the lane plays (ADR 0011 §6): starts at the right sample (skips audio or waits in silence) and fades in over [`FADE`] |
//! | the clocks drift | followed by the resampler: input rate × server rate / speaker rate, plus a correction proportional to the error beyond a 0.1 ms dead band, within ±[`SPEED_MAX`] |
//! | an error 10 s of that can't bring within 1 ms | fades out, then starts again at the right sample (a jump) |
//! | audio whose end is already past | dropped |
//! | the queue runs dry, or the server's timeline has a gap | the audio before fades out; a jump when audio comes again |
//! | `clear(None)` | fades out and drops everything queued; a jump when audio comes again |
//! | `clear(Some(t))` | drops queued audio from `t` on; the rest plays without a break |
//! | `end()`, or the sink dropped | fades out, the queue is emptied, the state says `Ended` |
//! | another source, a stop or a pause through the lane, a speaker failure | ends like a live stream, and the state says why |
//!
//! 44.1 kHz audio goes through the same resampler into 48 kHz. Audio that
//! ends more than [`AHEAD_MAX`] ahead is refused, which bounds the memory a
//! server can make the source hold.

use crate::decode::PullResampler;
use crate::mix::{FADE, Ramp, frames_of};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use tokio::sync::watch;

/// Maps local time to a server's time. pinnad implements it from the
/// Sendspin time filter; the lane knows nothing of the protocol.
pub trait ServerClock: Send + Sync {
    /// The server time, in µs, that corresponds to local monotonic time
    /// `local`; `None` while it is not known.
    fn server_us(&self, local: Instant) -> Option<i64>;
    /// Server µs per local µs (1.0001: the server's clock runs 100 ppm fast).
    fn rate(&self) -> f64;
}

/// Why a timed source ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EndReason {
    /// [`TimedSink::end`], or the sink was dropped.
    Ended,
    /// Another source took the lane (a URL, a stream or another timed source).
    Replaced,
    /// Stopped through the lane ([`MediaHandle::stop`](crate::MediaHandle::stop)).
    Stopped,
    /// Paused through the lane: a timed source can't hold its timeline back.
    Paused,
    /// The speaker failed or went away.
    SpeakerFailed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimedState {
    /// Nothing sounding yet: no audio due, or waiting for audio after a
    /// clear or after the queue ran dry.
    Waiting,
    Playing,
    Ended(EndReason),
}

/// How well a timed source keeps time (ADR 0027 §11: metrics).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TimedStats {
    /// The latest error, in µs, while it plays: positive when audio sounds
    /// later than the clock says.
    pub sync_error_us: Option<f64>,
    /// 10 s or more since the last start, jump, clear, underrun or error
    /// beyond 1 ms (the specification's steady state).
    pub steady: bool,
    /// Starts at the right sample: the first one, and every one after a
    /// clear, an underrun or a large error.
    pub jumps: u64,
    /// Chunks dropped because their audio was already past.
    pub late_drops: u64,
    /// The time a frame is heard comes from the speaker's hardware pointer.
    pub exact_timing: bool,
}

/// Why [`TimedSink::push`] refused audio.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Refused {
    #[error("the timed source has ended")]
    Ended,
    #[error("{0} Hz audio; a timed source takes 44100 or 48000 Hz")]
    Rate(u32),
    #[error("audio is not whole stereo frames")]
    Torn,
    #[error("audio is due more than 30 s ahead")]
    TooFarAhead,
}

/// The input rates a timed source takes.
const RATES: [u32; 2] = [44_100, 48_000];
/// Audio due further ahead than this is refused.
pub const AHEAD_MAX: Duration = Duration::from_secs(30);
/// Errors within this are left alone.
const DEAD_BAND_US: f64 = 100.0;
/// Speed change per µs of error beyond the dead band: an error of 1 ms is
/// worked off in about a second.
const CORRECTION_PER_US: f64 = 1e-6;
/// The correction never changes the speed by more than this. The
/// specification allows ±0.5 % over any 150 ms; the margin covers the
/// rates the feed-forward ratio is built from.
pub const SPEED_MAX: f64 = 0.0045;
/// How far the feed-forward ratio may miss the speaker's rate: about
/// +320 ppm on the Echo Dot (ADR 0004) until the rate is measured. The
/// correction makes up for it, and the error it leaves slows the
/// correction down.
const RATE_MISS: f64 = 320e-6;
/// Audio already handed to the speaker when the error changes, which no
/// correction reaches: its lead, the player's buffer and decoding (ADR 0027
/// §11: 300 ms until measured).
const HANDED_ON_S: f64 = 0.3;
/// Steady state: this long after the last disturbance.
const STEADY_AFTER: Duration = Duration::from_secs(10);

/// The largest error that the correction brings within 1 ms by the time
/// steady state begins; a larger one is jumped. The correction is
/// proportional beyond the dead band, so it stays at the cap only until the
/// error is `SPEED_MAX / CORRECTION_PER_US` past the dead band (4.6 ms), and
/// from there it decays with a time constant of 1 s, towards the error a
/// missed rate leaves. With the constants above: 36.9 ms.
fn jump_us() -> f64 {
    let cap_us = SPEED_MAX / CORRECTION_PER_US;
    let left_us = RATE_MISS / CORRECTION_PER_US;
    let tau_s = 1.0 / (CORRECTION_PER_US * 1e6);
    let decay_s = tau_s * ((cap_us - left_us) / (1_000.0 - DEAD_BAND_US - left_us)).ln();
    let at_cap_s = STEADY_AFTER.as_secs_f64() - HANDED_ON_S - decay_s;
    DEAD_BAND_US + cap_us + (SPEED_MAX - RATE_MISS) * 1e6 * at_cap_s
}

/// Where the lane is about to mix: when the chunk's first frame will be
/// heard, the speaker's frame rate, and whether that time comes from the
/// hardware pointer.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Timing {
    pub first: Instant,
    pub rate: f64,
    pub exact: bool,
}

impl Timing {
    /// When frame `n` of the chunk is heard.
    pub fn at(&self, n: usize) -> Instant {
        self.first + Duration::from_secs_f64(n as f64 / self.rate)
    }
}

struct Chunk {
    /// Server time of the first frame, in µs.
    ts_us: f64,
    rate: u32,
    /// Interleaved stereo.
    pcm: Vec<i16>,
    /// The [`Buf::epoch`] it was pushed in.
    epoch: u64,
}

impl Chunk {
    fn frames(&self) -> usize {
        self.pcm.len() / 2
    }

    fn us_per_frame(&self) -> f64 {
        1e6 / f64::from(self.rate)
    }

    fn end_us(&self) -> f64 {
        self.ts_us + self.frames() as f64 * self.us_per_frame()
    }
}

struct Buf {
    chunks: VecDeque<Chunk>,
    /// Frames of the front chunk already read.
    front_off: usize,
    /// Server time just past the last audio queued, while the timeline
    /// runs on: a chunk that overlaps it is trimmed.
    tail_us: Option<f64>,
    /// Bumped by a full clear: older chunks only feed the fade-out.
    epoch: u64,
    /// Server time heard at the latest chunk the speaker mixed.
    heard_us: Option<f64>,
    ended: bool,
    gain: f32,
    late_drops: u64,
}

impl Buf {
    /// Server time of the next frame to be read.
    fn read_us(&self) -> Option<f64> {
        let c = self.chunks.front()?;
        Some(c.ts_us + self.front_off as f64 * c.us_per_frame())
    }
}

/// What the sink and the lane share.
pub(crate) struct TimedQueue {
    clock: Arc<dyn ServerClock>,
    buf: Mutex<Buf>,
    state: watch::Sender<TimedState>,
    stats: Mutex<TimedStats>,
    /// Tests turn the correction off to show that they need it.
    #[cfg(test)]
    pub(crate) correct: std::sync::atomic::AtomicBool,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl TimedQueue {
    pub(crate) fn new(clock: Arc<dyn ServerClock>) -> Arc<TimedQueue> {
        Arc::new(TimedQueue {
            clock,
            buf: Mutex::new(Buf {
                chunks: VecDeque::new(),
                front_off: 0,
                tail_us: None,
                epoch: 0,
                heard_us: None,
                ended: false,
                gain: 1.0,
                late_drops: 0,
            }),
            state: watch::channel(TimedState::Waiting).0,
            stats: Mutex::new(TimedStats::default()),
            #[cfg(test)]
            correct: std::sync::atomic::AtomicBool::new(true),
        })
    }

    /// End with `why`, unless it already ended; nothing more is taken.
    pub(crate) fn finish(&self, why: EndReason) {
        lock(&self.buf).ended = true;
        self.state.send_if_modified(|s| {
            let open = !matches!(s, TimedState::Ended(_));
            if open {
                *s = TimedState::Ended(why);
            }
            open
        });
    }

    /// The sink ended it.
    pub(crate) fn ended(&self) -> bool {
        lock(&self.buf).ended
    }

    /// Whether its first audio is due by `until` (or already past).
    pub(crate) fn due(&self, until: Instant) -> bool {
        let mut b = lock(&self.buf);
        let Some(until) = self.clock.server_us(until).map(|s| s as f64) else {
            return false;
        };
        b.heard_us = Some(until);
        b.chunks
            .iter()
            .find(|c| c.epoch == b.epoch)
            .is_some_and(|c| c.ts_us <= until)
    }

    fn push(&self, ts_us: i64, rate: u32, mut pcm: Vec<i16>) -> Result<(), Refused> {
        if !RATES.contains(&rate) {
            return Err(Refused::Rate(rate));
        }
        if !pcm.len().is_multiple_of(2) {
            return Err(Refused::Torn);
        }
        let mut b = lock(&self.buf);
        if b.ended {
            return Err(Refused::Ended);
        }
        if pcm.is_empty() {
            return Ok(());
        }
        let mut ts = ts_us as f64;
        // Due time is measured from what the speaker mixes; before it mixes
        // any, from the start of what is queued.
        let now = b
            .heard_us
            .or_else(|| b.chunks.front().map(|c| c.ts_us))
            .unwrap_or(ts);
        let upf = 1e6 / f64::from(rate);
        // Its end, not its start: one long chunk would hold as much.
        if ts + (pcm.len() / 2) as f64 * upf > now + AHEAD_MAX.as_secs_f64() * 1e6 {
            return Err(Refused::TooFarAhead);
        }
        if let Some(tail) = b.tail_us
            && ts < tail - upf / 2.0
        {
            // Audio already queued or played covers its start (after a clear
            // from a time already handed on): only the rest is new.
            let covered = ((tail - ts) / upf).round() as usize;
            if covered >= pcm.len() / 2 {
                b.late_drops += 1;
                return Ok(());
            }
            pcm.drain(..2 * covered);
            ts += covered as f64 * upf;
        }
        let epoch = b.epoch;
        let chunk = Chunk {
            ts_us: ts,
            rate,
            pcm,
            epoch,
        };
        b.tail_us = Some(chunk.end_us());
        b.chunks.push_back(chunk);
        Ok(())
    }

    fn clear(&self, from_us: Option<i64>) {
        let mut b = lock(&self.buf);
        let Some(from) = from_us.map(|f| f as f64) else {
            // Everything goes; what sounds keeps enough to fade out (one
            // chunk of mixing and the fade), the rest is let go now.
            b.epoch += 1;
            b.tail_us = None;
            let playing = *self.state.borrow() == TimedState::Playing;
            let keep_us = if playing {
                2.0 * FADE.as_secs_f64() * 1e6
            } else {
                0.0
            };
            let start = b.read_us().unwrap_or(0.0);
            let mut keep = 0;
            while keep < b.chunks.len() && b.chunks[keep].ts_us < start + keep_us {
                keep += 1;
            }
            b.chunks.truncate(keep);
            if keep == 0 {
                b.front_off = 0;
            }
            return;
        };
        let epoch = b.epoch;
        // What was read can't be recalled: later chunks start after it.
        let read = b.read_us().or(b.tail_us);
        loop {
            let (n, front_off) = (b.chunks.len(), b.front_off);
            let Some(c) = b.chunks.back_mut() else { break };
            if c.epoch != epoch || c.end_us() <= from {
                break;
            }
            // Keep what comes before `from`, and what was read of the front chunk.
            let mut keep = ((from - c.ts_us) / c.us_per_frame()).ceil().max(0.0) as usize;
            if n == 1 {
                keep = keep.max(front_off);
            }
            c.pcm.truncate(2 * keep.min(c.frames()));
            if !c.pcm.is_empty() {
                break;
            }
            b.chunks.pop_back();
            if b.chunks.is_empty() {
                b.front_off = 0;
            }
        }
        let floor = read.map_or(from, |r| r.max(from));
        b.tail_us = b.tail_us.map(|t| t.min(floor));
    }
}

/// pinnad's end of a timed source (see the [module](self)). Dropping it
/// ends the source.
pub struct TimedSink {
    q: Arc<TimedQueue>,
    state: watch::Receiver<TimedState>,
}

impl TimedSink {
    pub(crate) fn new(q: Arc<TimedQueue>) -> TimedSink {
        let state = q.state.subscribe();
        TimedSink { q, state }
    }

    /// Queue `pcm`, interleaved stereo at `rate` (44100 or 48000 Hz), whose
    /// first frame is to be heard at server time `server_ts_us`. A chunk
    /// that starts inside audio already queued or played keeps only its
    /// new part; one whose whole audio is already covered is counted as
    /// late and dropped.
    pub fn push(&self, server_ts_us: i64, rate: u32, pcm: Vec<i16>) -> Result<(), Refused> {
        self.q.push(server_ts_us, rate, pcm)
    }

    /// `None`: drop everything queued and fade out what sounds (a
    /// disturbance: the next audio starts with a jump). `Some(t)`: drop
    /// only queued audio from server time `t` on; the rest plays on.
    pub fn clear(&self, from_us: Option<i64>) {
        self.q.clear(from_us);
    }

    /// Fade out and end: the queue is emptied, nothing more is taken.
    pub fn end(&self) {
        lock(&self.q.buf).ended = true;
    }

    /// The source's own level (linear, 0 to 1), ramped like the lane's
    /// other gains.
    pub fn set_gain(&self, gain: f32) {
        if gain.is_finite() {
            lock(&self.q.buf).gain = gain.clamp(0.0, 1.0);
        }
    }

    pub fn state(&self) -> watch::Receiver<TimedState> {
        self.state.clone()
    }

    pub fn stats(&self) -> TimedStats {
        *lock(&self.q.stats)
    }
}

impl Drop for TimedSink {
    fn drop(&mut self) {
        self.end();
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Mode {
    /// No position: the next audio starts with a jump.
    Sync,
    Play,
    /// Fading out, then [`Mode::Sync`]: for a jump; after a full clear
    /// (`flush`: what it dropped is let go); or before the audio runs out or
    /// breaks (`skip_to`: the little left before that server time, too short
    /// to start again on, is let go).
    FadeOut {
        flush: bool,
        skip_to: Option<f64>,
    },
}

/// The lane's end of a timed source: renders the audio for each chunk the
/// speaker mixes.
pub(crate) struct TimedPlayer {
    q: Arc<TimedQueue>,
    rs: PullResampler,
    /// Rate of the input the resampler holds.
    rate: u32,
    /// Server time of the resampler's first input frame, in µs.
    buf0_us: f64,
    epoch: u64,
    mode: Mode,
    gain: Ramp,
    fade: Ramp,
    /// Silent output frames before a start.
    wait: usize,
    /// Input frames per output frame.
    step: f64,
    /// When the last disturbance was heard.
    disturbed: Option<Instant>,
    err_us: Option<f64>,
    jumps: u64,
}

impl TimedPlayer {
    pub(crate) fn new(q: Arc<TimedQueue>) -> TimedPlayer {
        TimedPlayer {
            q,
            rs: PullResampler::new(),
            rate: 48_000,
            buf0_us: 0.0,
            epoch: 0,
            mode: Mode::Sync,
            gain: Ramp::new(1.0, FADE),
            fade: Ramp::new(0.0, FADE),
            wait: 0,
            step: 1.0,
            disturbed: None,
            err_us: None,
            jumps: 0,
        }
    }

    pub(crate) fn queue(&self) -> &Arc<TimedQueue> {
        &self.q
    }

    /// Whether audio sounds (or is fading out).
    pub(crate) fn sounding(&self) -> bool {
        self.mode != Mode::Sync
    }

    /// Server time of the next output frame, in µs.
    fn pos_us(&self) -> f64 {
        self.buf0_us + self.rs.pos() * 1e6 / f64::from(self.rate)
    }

    /// Append `n` frames of interleaved stereo to `out`: the frames heard
    /// from `timing` on, or (without timing: a source fading out after it
    /// was retired) the audio that follows at the last speed.
    pub(crate) fn render(&mut self, timing: Option<Timing>, n: usize, out: &mut Vec<i16>) {
        let q = self.q.clone();
        let mut b = lock(&q.buf);
        self.gain.set(b.gain);
        if b.epoch != self.epoch {
            if self.mode == Mode::Play {
                self.fade_out(true);
            } else if self.mode == Mode::Sync {
                self.flush(&mut b);
            }
        }
        let clock = timing.and_then(|t| Some((t, q.clock.server_us(t.first)?, q.clock.rate())));
        if let Some((_, s0, _)) = clock {
            b.heard_us = Some(s0 as f64);
        }
        if let Some((t, s0, rho)) = clock
            && self.mode == Mode::Play
        {
            self.steer(t, s0 as f64, rho);
        }
        let mut fade_at = None;
        if self.mode == Mode::Play {
            fade_at = self.running_dry(&b, n);
        }
        if self.mode == Mode::Sync
            && let Some((t, _, _)) = clock
        {
            self.start(&mut b, &t, 0, n);
        }
        for j in 0..n {
            if self.wait > 0 {
                self.wait -= 1;
                self.gain.tick();
                out.extend_from_slice(&[0, 0]);
                continue;
            }
            if self.mode == Mode::Sync {
                self.gain.tick();
                out.extend_from_slice(&[0, 0]);
                continue;
            }
            if let Some((at, len, end)) = fade_at
                && at == j
            {
                self.fade = Ramp::new(self.fade.value(), frames_dur(len));
                self.fade.set(0.0);
                self.mode = Mode::FadeOut {
                    flush: false,
                    skip_to: Some(end),
                };
            }
            while self.rs.wants() > 0 {
                let Some(f) = self.read(&mut b) else { break };
                self.rs.push(f);
            }
            if self.rs.wants() > 0 {
                // Ran dry: whatever comes next starts with a jump.
                self.mode = Mode::Sync;
                self.rs.clear();
                self.disturbed = clock.map(|(t, _, _)| t.at(j));
                out.extend_from_slice(&[0, 0]);
                continue;
            }
            let [l, r] = self.rs.frame();
            let g = self.gain.tick() * self.fade.tick();
            out.push((l * g).round().clamp(-32768.0, 32767.0) as i16);
            out.push((r * g).round().clamp(-32768.0, 32767.0) as i16);
            let gone = self.rs.advance(self.step);
            self.buf0_us += gone as f64 * 1e6 / f64::from(self.rate);
            if let Mode::FadeOut { flush, skip_to } = self.mode
                && self.fade.value() <= 0.0
            {
                self.mode = Mode::Sync;
                self.rs.clear();
                if flush {
                    self.flush(&mut b);
                }
                if let Some(end) = skip_to {
                    self.skip(&mut b, end);
                }
                if let Some((t, _, _)) = clock {
                    self.start(&mut b, &t, j + 1, n);
                }
            }
        }
        let late = b.late_drops;
        drop(b);
        if let Some((t, _, _)) = clock {
            self.publish(t, late);
        }
    }

    /// Fade out from where the fade is now, over [`FADE`] from full level.
    fn fade_out(&mut self, flush: bool) {
        self.fade = Ramp::new(self.fade.value(), FADE);
        self.fade.set(0.0);
        self.mode = Mode::FadeOut {
            flush,
            skip_to: None,
        };
    }

    /// Let go of the unread audio before server time `end`.
    fn skip(&mut self, b: &mut Buf, end: f64) {
        while let Some((at, upf, left)) =
            b.chunks.front().filter(|c| c.epoch == self.epoch).map(|c| {
                let upf = c.us_per_frame();
                (
                    c.ts_us + b.front_off as f64 * upf,
                    upf,
                    c.frames() - b.front_off,
                )
            })
        {
            if at >= end - upf / 2.0 {
                break;
            }
            let skip = ((end - at) / upf).round() as usize;
            if skip < left {
                b.front_off += skip;
                break;
            }
            b.chunks.pop_front();
            b.front_off = 0;
        }
    }

    /// Correct the speed from the error at the chunk's first frame, or
    /// fade out for a jump.
    fn steer(&mut self, t: Timing, s0: f64, rho: f64) {
        let err = s0 - self.pos_us();
        self.err_us = Some(err);
        if err.abs() > 1_000.0 {
            self.disturbed = Some(t.first);
        }
        if err.abs() > jump_us() {
            self.fade_out(false);
            return;
        }
        let beyond = err - err.clamp(-DEAD_BAND_US, DEAD_BAND_US);
        let c = (beyond * CORRECTION_PER_US).clamp(-SPEED_MAX, SPEED_MAX);
        #[cfg(test)]
        let c = if self.q.correct.load(std::sync::atomic::Ordering::Relaxed) {
            c
        } else {
            0.0
        };
        self.step = f64::from(self.rate) * rho / t.rate * (1.0 + c);
    }

    /// If the audio queued runs out (or breaks) within this chunk and a
    /// fade: where to start fading out (frame in the chunk), over how many
    /// frames, and the server time where the audio ends.
    fn running_dry(&self, b: &Buf, n: usize) -> Option<(usize, usize, f64)> {
        let fade = frames_of(FADE) as usize;
        let need = (n + fade) as f64 * self.step + PullResampler::HALF as f64 + 1.0;
        let mut have = self.rs.len() as f64 - self.rs.pos();
        let mut off = b.front_off;
        // Where the audio the resampler holds ends.
        let mut end = self.buf0_us + self.rs.len() as f64 * 1e6 / f64::from(self.rate);
        for c in b.chunks.iter().take_while(|c| c.epoch == self.epoch) {
            if have >= need || (off == 0 && gap(end, c)) {
                break;
            }
            have += c.frames().saturating_sub(off) as f64;
            off = 0;
            end = c.end_us();
        }
        if have >= need {
            return None;
        }
        let out = ((have - PullResampler::HALF as f64 - 1.0) / self.step).max(0.0) as usize;
        Some((out.saturating_sub(fade), out.min(fade).max(1), end))
    }

    /// Start at the right sample for frame `j` of the chunk: skip what is
    /// already past, or wait in silence for what is still ahead (within
    /// this chunk). False if nothing is due in it.
    fn start(&mut self, b: &mut Buf, t: &Timing, j: usize, n: usize) -> bool {
        self.flush(b);
        while b.chunks.front().is_some_and(|c| b.front_off >= c.frames()) {
            b.chunks.pop_front();
            b.front_off = 0;
        }
        let (Some(s_j), rho) = (self.q.clock.server_us(t.at(j)), self.q.clock.rate()) else {
            return false;
        };
        let s_j = s_j as f64;
        while b.chunks.front().is_some_and(|c| c.end_us() <= s_j) {
            b.chunks.pop_front();
            b.front_off = 0;
            b.late_drops += 1;
        }
        let Some((ts, rate, upf)) = b
            .chunks
            .front()
            .map(|c| (c.ts_us, c.rate, c.us_per_frame()))
        else {
            return false;
        };
        let first = ts + b.front_off as f64 * upf;
        let per_out = rho * 1e6 / t.rate;
        let (wait, at_us) = if first >= s_j {
            let w = ((first - s_j) / per_out).ceil() as usize;
            if j + w >= n {
                return false;
            }
            (w, s_j + w as f64 * per_out)
        } else {
            // Past audio within this chunk: skip to the frame due now.
            b.front_off += ((s_j - first) / upf).floor() as usize;
            (0, s_j)
        };
        let next_us = ts + b.front_off as f64 * upf;
        self.rate = rate;
        self.rs.restart((at_us - next_us) / upf);
        self.buf0_us = next_us - PullResampler::HALF as f64 * upf;
        self.wait = wait;
        self.step = f64::from(rate) * rho / t.rate;
        self.fade = Ramp::new(0.0, FADE);
        self.fade.set(1.0);
        self.mode = Mode::Play;
        self.jumps += 1;
        self.disturbed = Some(t.at(j + wait));
        true
    }

    /// Let go of what a full clear dropped.
    fn flush(&mut self, b: &mut Buf) {
        if b.epoch == self.epoch {
            return;
        }
        while b.chunks.front().is_some_and(|c| c.epoch != b.epoch) {
            b.chunks.pop_front();
            b.front_off = 0;
        }
        self.epoch = b.epoch;
    }

    /// The next input frame for the resampler, or `None` when the queue
    /// (of this epoch) has run out or breaks.
    fn read(&mut self, b: &mut Buf) -> Option<[f32; 2]> {
        loop {
            let c = b.chunks.front()?;
            if c.epoch != self.epoch {
                return None;
            }
            if b.front_off < c.frames() {
                let i = 2 * b.front_off;
                if b.front_off == 0 {
                    // Each chunk sets its own time: rounding in the server's
                    // timestamps never adds up.
                    self.rate = c.rate;
                    self.buf0_us = c.ts_us - self.rs.len() as f64 * c.us_per_frame();
                }
                b.front_off += 1;
                return Some([f32::from(c.pcm[i]), f32::from(c.pcm[i + 1])]);
            }
            let end = c.end_us();
            b.chunks.pop_front();
            b.front_off = 0;
            // A gap in the server's timeline is a break: the audio before it
            // fades out (`running_dry`), the next starts at its own time.
            if b.chunks.front().is_some_and(|n| gap(end, n)) {
                return None;
            }
        }
    }

    fn publish(&mut self, t: Timing, late_drops: u64) {
        let playing = self.mode != Mode::Sync;
        let steady = playing
            && self
                .disturbed
                .is_some_and(|d| t.first.saturating_duration_since(d) >= STEADY_AFTER);
        *lock(&self.q.stats) = TimedStats {
            sync_error_us: if self.mode == Mode::Play {
                self.err_us
            } else {
                None
            },
            steady,
            jumps: self.jumps,
            late_drops,
            exact_timing: t.exact,
        };
        let now = if playing {
            TimedState::Playing
        } else {
            TimedState::Waiting
        };
        self.q.state.send_if_modified(|s| {
            let changed = *s != now && !matches!(s, TimedState::Ended(_));
            if changed {
                *s = now;
            }
            changed
        });
    }
}

impl Drop for TimedPlayer {
    fn drop(&mut self) {
        // Without a reason given, the speaker let it go.
        self.q.finish(EndReason::SpeakerFailed);
        let mut b = lock(&self.q.buf);
        b.chunks.clear();
        b.front_off = 0;
    }
}

/// Whether chunk `c` starts later than `end`, where the audio before it ends:
/// by more than half a frame, more than the server's rounding.
fn gap(end: f64, c: &Chunk) -> bool {
    c.ts_us - end > c.us_per_frame() / 2.0
}

fn frames_dur(frames: usize) -> Duration {
    Duration::from_secs_f64(frames as f64 / f64::from(crate::decode::OUT_RATE))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::{Decoder, Item, MediaLane, MediaPhase, MediaState, STREAM_URL};
    use crate::track::Track;
    use std::f64::consts::TAU;
    use std::sync::atomic::Ordering;
    use std::sync::mpsc::sync_channel;

    /// Frames the speaker mixes at a time (20 ms at 48 kHz).
    const CHUNK: usize = 960;
    /// The test signal: a 5 Hz quadrature pair whose phase is the server
    /// time, so each output frame says which server time it plays.
    const HZ: f64 = 5.0;
    const PERIOD_US: f64 = 1e6 / HZ;
    const AMP: f64 = 20_000.0;
    /// The speaker mixes a chunk this long before it is heard (its lead and
    /// the player's buffer).
    const MIX_AHEAD: f64 = 0.25;
    /// The server sends audio this long before it is due.
    const SEND_AHEAD: f64 = 0.5;

    /// A server clock `offset_us` ahead of local time at `origin`, running
    /// `drift` fast. It stands for the time filter: the error is measured
    /// against what the filter predicts. Tests move its offset as a filter
    /// update would.
    struct FakeClock {
        origin: Instant,
        offset_us: Mutex<f64>,
        drift: f64,
    }

    impl FakeClock {
        fn at(&self, t: Instant) -> f64 {
            let local = if t >= self.origin {
                (t - self.origin).as_secs_f64()
            } else {
                -(self.origin - t).as_secs_f64()
            };
            *lock(&self.offset_us) + local * 1e6 * (1.0 + self.drift)
        }

        fn local(&self, s_us: f64) -> Instant {
            let secs = (s_us - *lock(&self.offset_us)) / (1.0 + self.drift) / 1e6;
            self.origin + Duration::from_secs_f64(secs.max(0.0))
        }

        fn shift(&self, us: f64) {
            *lock(&self.offset_us) += us;
        }
    }

    impl ServerClock for FakeClock {
        fn server_us(&self, local: Instant) -> Option<i64> {
            Some(self.at(local).round() as i64)
        }

        fn rate(&self) -> f64 {
            1.0 + self.drift
        }
    }

    /// The server's next chunk: due at `ts` (µs, exact), arriving at `arrival`.
    struct Pending {
        arrival: Instant,
        ts: f64,
        frames: usize,
    }

    /// A lane with a timed source, mixed chunk by chunk by a speaker whose
    /// clock runs at `spk_true` frames per second (it reports 48 kHz, as
    /// before its rate is measured), fed by a server that sends chunks of
    /// 20 to 150 ms, `SEND_AHEAD` before they are due, each delayed by up
    /// to 30 ms of network jitter.
    struct Sim {
        lane: MediaLane,
        sink: TimedSink,
        clock: Arc<FakeClock>,
        /// When the first frame of the first chunk mixed is heard.
        t0: Instant,
        spk_true: f64,
        spk_told: f64,
        mixed: u64,
        rng: u64,
        rate: u32,
        next: Option<Pending>,
        next_ts: f64,
        streaming: bool,
        started_at: Instant,
        last_arrival: Instant,
        /// The signal's level (a factor) from server time `.1` on.
        level_from: Option<(f64, f64)>,
        /// The timestamps of the chunks delivered.
        sent: Vec<f64>,
        /// Per output frame: the error in µs (positive: late), or NaN where
        /// the audio isn't at full level.
        err: Vec<f32>,
        /// Per output frame: the signal's amplitude.
        amp: Vec<f32>,
    }

    impl Sim {
        fn new(spk_ppm: f64, drift_ppm: f64, rate: u32) -> Sim {
            let origin = Instant::now();
            let clock = Arc::new(FakeClock {
                origin,
                offset_us: Mutex::new(4.2e9),
                drift: drift_ppm * 1e-6,
            });
            let q = TimedQueue::new(clock.clone());
            let mut lane = MediaLane::default();
            lane.begin_timed(1, "sendspin".into(), Decoder::timed(q.clone()));
            let t0 = origin + Duration::from_secs(1);
            Sim {
                lane,
                sink: TimedSink::new(q),
                clock,
                t0,
                spk_true: 48_000.0 * (1.0 + spk_ppm * 1e-6),
                spk_told: 48_000.0,
                mixed: 0,
                rng: 0x9E37_79B9_7F4A_7C15,
                rate,
                next: None,
                next_ts: 0.0,
                streaming: false,
                started_at: t0,
                last_arrival: t0,
                level_from: None,
                sent: Vec::new(),
                err: Vec::new(),
                amp: Vec::new(),
            }
        }

        fn random(&mut self) -> f64 {
            self.rng ^= self.rng << 13;
            self.rng ^= self.rng >> 7;
            self.rng ^= self.rng << 17;
            (self.rng >> 11) as f64 / (1u64 << 53) as f64
        }

        fn queue(&self) -> Arc<TimedQueue> {
            self.sink.q.clone()
        }

        /// When frame `n` of the mix is heard.
        fn heard(&self, n: u64) -> Instant {
            self.t0 + Duration::from_secs_f64(n as f64 / self.spk_true)
        }

        /// When the speaker mixes its next chunk.
        fn now(&self) -> Instant {
            self.heard(self.mixed * CHUNK as u64) - Duration::from_secs_f64(MIX_AHEAD)
        }

        /// The server time heard at the next chunk mixed.
        fn heard_us(&self) -> f64 {
            self.clock.at(self.heard(self.mixed * CHUNK as u64))
        }

        /// The server starts a stream now, its first audio due `lead` later.
        fn start_stream(&mut self, lead: f64) {
            self.streaming = true;
            self.started_at = self.now();
            self.last_arrival = self.started_at;
            self.next_ts = self.clock.at(self.started_at) + lead * 1e6;
            self.next = None;
        }

        fn signal(&self, s_us: f64) -> [i16; 2] {
            let phase = TAU * (s_us % PERIOD_US) / PERIOD_US;
            let amp = match self.level_from {
                Some((level, from)) if s_us >= from => AMP * level,
                _ => AMP,
            };
            [
                (amp * phase.sin()).round() as i16,
                (amp * phase.cos()).round() as i16,
            ]
        }

        fn chunk_pcm(&self, ts: f64, frames: usize) -> Vec<i16> {
            let upf = 1e6 / f64::from(self.rate);
            (0..frames)
                .flat_map(|k| self.signal(ts + k as f64 * upf))
                .collect()
        }

        /// Deliver what has arrived by `until`.
        fn deliver(&mut self, until: Instant) {
            while self.streaming {
                if self.next.is_none() {
                    let ms = 20.0 + 130.0 * self.random();
                    let frames = (f64::from(self.rate) * ms / 1000.0) as usize;
                    let send = self.clock.local(self.next_ts - SEND_AHEAD * 1e6);
                    let jitter = Duration::from_secs_f64(0.030 * self.random());
                    // TCP keeps the order.
                    let arrival = send.max(self.started_at).max(self.last_arrival) + jitter;
                    self.next = Some(Pending {
                        arrival,
                        ts: self.next_ts,
                        frames,
                    });
                    self.next_ts += frames as f64 * 1e6 / f64::from(self.rate);
                }
                if self.next.as_ref().is_some_and(|p| p.arrival > until) {
                    return;
                }
                let Some(p) = self.next.take() else { return };
                self.last_arrival = p.arrival;
                self.sent.push(p.ts);
                let pcm = self.chunk_pcm(p.ts, p.frames);
                self.sink
                    .push(p.ts.round() as i64, self.rate, pcm)
                    .expect("queued");
            }
        }

        /// Mix one chunk, recording against each frame the server time it
        /// plays and the server time due when it is heard.
        fn step(&mut self) {
            let now = self.now();
            self.deliver(now);
            let first = self.mixed * CHUNK as u64;
            self.lane.set_timing(
                Timing {
                    first: self.heard(first),
                    rate: self.spk_told,
                    exact: true,
                },
                CHUNK,
            );
            for j in 0..CHUNK as u64 {
                let (l, r) = self.lane.pull();
                let (l, r) = (f64::from(l), f64::from(r));
                let amp = l.hypot(r);
                self.amp.push(amp as f32);
                let err = if amp > 0.95 * AMP {
                    let played = (l.atan2(r) / TAU).rem_euclid(1.0) * PERIOD_US;
                    let due = self.clock.at(self.heard(first + j)) % PERIOD_US;
                    let d = due - played;
                    (d - PERIOD_US * (d / PERIOD_US).round()) as f32
                } else {
                    f32::NAN
                };
                self.err.push(err);
            }
            self.mixed += 1;
        }

        fn run(&mut self, secs: f64) {
            for _ in 0..(secs * 50.0).round() as usize {
                self.step();
            }
        }

        fn first_sound(&self) -> usize {
            self.err
                .iter()
                .position(|e| !e.is_nan())
                .expect("it played")
        }

        /// The largest error from frame `from` on.
        fn max_err(&self, from: usize) -> f32 {
            self.err[from..]
                .iter()
                .filter(|e| !e.is_nan())
                .fold(0.0, |m, e| m.max(e.abs()))
        }

        /// The largest speed deviation over any 150 ms of full-level audio
        /// from frame `from` on (a one-shot jump is exempt: it fades).
        fn max_speed(&self, from: usize) -> f64 {
            let w = 7_200;
            let span = w as f64 / self.spk_true * 1e6;
            let mut worst = 0.0f64;
            let mut j = from;
            while j + w < self.err.len() {
                if self.err[j..=j + w].iter().all(|e| !e.is_nan()) {
                    // Off-speed audio drifts against the clock by as much.
                    let drift = f64::from(self.err[j + w]) - f64::from(self.err[j]);
                    worst = worst.max(drift.abs() / span);
                }
                j += 240;
            }
            worst
        }

        fn state(&self) -> TimedState {
            *self.sink.state().borrow()
        }
    }

    /// Frames in `secs` at 48 kHz.
    fn frames(secs: f64) -> usize {
        (secs * 48_000.0) as usize
    }

    /// A stream that starts 300 ms after the speaker began mixing.
    fn playing(spk_ppm: f64, drift_ppm: f64, rate: u32, secs: f64) -> Sim {
        let mut sim = Sim::new(spk_ppm, drift_ppm, rate);
        sim.run(0.5);
        sim.start_stream(0.3);
        sim.run(secs);
        sim
    }

    /// 20 s of a stream against a speaker `spk_ppm` off 48 kHz (which it
    /// doesn't know yet) and a server clock `drift_ppm` fast.
    fn assert_follows(spk_ppm: f64, drift_ppm: f64, rate: u32) {
        let sim = playing(spk_ppm, drift_ppm, rate, 20.0);
        let first = sim.first_sound();
        // From 10 s after the start, within 1 ms.
        let steady = first + frames(10.0);
        assert!(sim.max_err(steady) < 1_000.0, "{} µs", sim.max_err(steady));
        // Within 0.5 % over any 150 ms, and not even 0.1 % from the start:
        // it jumped to the right sample rather than correcting its way
        // there, so there is no warble.
        assert!(
            sim.max_speed(first) < 0.001,
            "speed {}",
            sim.max_speed(first)
        );
        assert!(
            sim.err[first..first + frames(0.1)]
                .iter()
                .all(|e| e.abs() < 50.0),
            "started off by {} µs",
            sim.max_err(first)
        );
        let st = sim.sink.stats();
        assert!(st.steady && st.exact_timing, "{st:?}");
        assert_eq!((st.jumps, st.late_drops), (1, 0));
        assert!(
            st.sync_error_us.is_some_and(|e| e.abs() < 1_000.0),
            "{st:?}"
        );
        assert_eq!(sim.state(), TimedState::Playing);
    }

    #[test]
    fn follows_a_fast_speaker_and_a_fast_server() {
        assert_follows(320.0, 100.0, 48_000);
    }

    #[test]
    fn follows_a_slow_speaker_and_a_slow_server() {
        assert_follows(-320.0, -100.0, 48_000);
    }

    #[test]
    fn follows_44k_audio_from_a_slow_server_on_a_fast_speaker() {
        assert_follows(320.0, -100.0, 44_100);
    }

    #[test]
    fn follows_44k_audio_from_a_fast_server_on_a_slow_speaker() {
        assert_follows(-320.0, 100.0, 44_100);
    }

    #[test]
    fn without_the_correction_the_error_leaves_a_millisecond() {
        // The tests above need the correction: the feed-forward ratio alone
        // misses the speaker's 320 ppm until its rate is measured.
        let mut sim = Sim::new(320.0, 100.0, 48_000);
        sim.queue().correct.store(false, Ordering::Relaxed);
        sim.run(0.5);
        sim.start_stream(0.3);
        sim.run(20.0);
        let steady = sim.first_sound() + frames(10.0);
        assert!(sim.max_err(steady) > 3_000.0, "{} µs", sim.max_err(steady));
    }

    #[test]
    fn a_small_filter_step_is_slewed_and_a_large_one_jumped() {
        let mut sim = playing(0.0, 0.0, 48_000, 3.0);
        // The filter moves 3 ms: within what 10 s of correction can do.
        sim.clock.shift(3_000.0);
        let mark = sim.err.len();
        sim.run(8.0);
        assert!(sim.max_speed(mark) < 0.005, "speed {}", sim.max_speed(mark));
        assert!(sim.err[mark..].iter().all(|e| !e.is_nan()), "never faded");
        assert!(sim.max_err(sim.err.len() - frames(1.0)) < 200.0);
        assert_eq!(sim.sink.stats().jumps, 1);

        // It moves 80 ms: a jump, faded out and in, back in time.
        sim.clock.shift(80_000.0);
        let mark = sim.err.len();
        sim.run(1.0);
        assert_eq!(sim.sink.stats().jumps, 2);
        assert!(!sim.sink.stats().steady);
        let faded = sim.err[mark..].iter().position(|e| e.is_nan()).unwrap();
        let back = mark
            + faded
            + sim.err[mark + faded..]
                .iter()
                .position(|e| !e.is_nan())
                .unwrap();
        assert!(sim.max_err(back) < 300.0, "{} µs", sim.max_err(back));
        assert!(
            back - mark < frames(0.1),
            "silent for {} frames",
            back - mark
        );
    }

    #[test]
    fn the_largest_step_slewed_is_within_a_millisecond_10_s_later() {
        // Without a missed rate, and with one that works against the
        // correction: a slow speaker for a step forward, a fast one for a
        // step back.
        for (spk_ppm, sign) in [(0.0, 1.0), (-320.0, 1.0), (320.0, -1.0)] {
            let mut sim = playing(spk_ppm, 0.0, 48_000, 3.0);
            // The error the step leaves, not the step, is what counts.
            let err = |sim: &Sim| sim.sink.stats().sync_error_us.unwrap();
            sim.clock.shift(sign * (jump_us() - 200.0) - err(&sim));
            let mark = sim.err.len();
            sim.run(10.5);
            assert_eq!(sim.sink.stats().jumps, 1, "{spk_ppm} ppm: jumped");
            assert!(sim.max_speed(mark) < 0.005, "{spk_ppm} ppm");
            // The filter moved as the chunk before `mark` was mixed, which
            // is heard MIX_AHEAD later.
            let steady = mark + frames(10.0 - MIX_AHEAD);
            assert!(
                sim.max_err(steady) < 1_000.0,
                "{spk_ppm} ppm: {} µs",
                sim.max_err(steady)
            );

            // A little more is jumped.
            sim.clock.shift(sign * (jump_us() + 200.0) - err(&sim));
            sim.run(0.5);
            assert_eq!(sim.sink.stats().jumps, 2, "{spk_ppm} ppm: slewed");
        }
    }

    #[test]
    fn late_audio_is_dropped() {
        let mut sim = Sim::new(0.0, 0.0, 48_000);
        sim.run(0.5);
        // A chunk already heard by the time the speaker mixes.
        let past = sim.heard_us() - 200_000.0;
        sim.sink
            .push(past as i64, 48_000, sim.chunk_pcm(past, 4_800))
            .unwrap();
        sim.start_stream(0.3);
        sim.run(2.0);
        let st = sim.sink.stats();
        assert_eq!((st.late_drops, st.jumps), (1, 1), "{st:?}");
        assert!(sim.max_err(sim.first_sound()) < 300.0);

        // While it plays, a chunk whose audio has already been played (a
        // resend) is dropped as late too.
        let old = sim.heard_us() - 500_000.0;
        sim.sink
            .push(old as i64, 48_000, sim.chunk_pcm(old, 960))
            .unwrap();
        sim.run(0.5);
        assert_eq!(sim.sink.stats().late_drops, 2);
        assert!(sim.max_err(sim.first_sound()) < 300.0);
    }

    #[test]
    fn a_late_start_skips_to_the_sample_due() {
        let mut sim = Sim::new(0.0, 0.0, 48_000);
        sim.run(0.5);
        // The first chunk's start is 40 ms past when the source starts.
        let ts = sim.heard_us() - 40_000.0;
        sim.sink
            .push(ts as i64, 48_000, sim.chunk_pcm(ts, 9_600))
            .unwrap();
        sim.run(0.1);
        let first = sim.first_sound();
        assert!(first < frames(0.55), "started at once: {first}");
        assert!(sim.max_err(first) < 100.0, "{} µs", sim.max_err(first));
        assert_eq!(sim.sink.stats().late_drops, 0);
    }

    #[test]
    fn a_full_clear_fades_out_and_the_next_audio_jumps_in() {
        let mut sim = playing(0.0, 0.0, 48_000, 2.0);
        sim.sink.clear(None);
        sim.streaming = false;
        let mark = sim.err.len();
        sim.run(0.5);
        // Fades out within a chunk and a fade, then silence.
        let quiet = sim.amp[mark..].iter().position(|&a| a < 1.0).unwrap();
        assert!(quiet <= 2 * CHUNK, "faded out after {quiet} frames");
        assert!(
            sim.amp[mark..mark + quiet]
                .windows(2)
                .all(|w| w[1] <= w[0] + 20.0),
            "a fade, not a cut"
        );
        assert!(sim.amp[mark + quiet..].iter().all(|&a| a < 1.0));
        assert_eq!(sim.state(), TimedState::Waiting);
        assert_eq!(sim.lane.phase(), MediaPhase::Buffering);
        // The server starts again: a jump, in time.
        sim.start_stream(0.3);
        let mark = sim.err.len();
        sim.run(1.0);
        let back = mark + sim.err[mark..].iter().position(|e| !e.is_nan()).unwrap();
        assert!(sim.max_err(back) < 300.0, "{} µs", sim.max_err(back));
        let st = sim.sink.stats();
        assert_eq!(st.jumps, 2);
        assert!(!st.steady);
        assert_eq!(sim.state(), TimedState::Playing);
    }

    #[test]
    fn a_clear_from_a_time_keeps_playing_what_comes_before() {
        let mut sim = playing(0.0, 0.0, 48_000, 2.0);
        // The server replaces what follows a chunk it sent, at least 50 ms
        // ahead, with other audio (3 % quieter).
        let heard = sim.heard_us();
        let from = sim
            .sent
            .iter()
            .copied()
            .find(|&t| t >= heard + 50_000.0)
            .unwrap();
        assert!(from < heard + 250_000.0, "sent and queued");
        let from_frame = sim.err.len() as f64 + (from - heard) / 1e6 * 48_000.0;
        sim.sink.clear(Some(from.round() as i64));
        sim.next = None;
        sim.next_ts = from;
        sim.level_from = Some((0.97, from));
        let mark = sim.err.len();
        sim.run(1.0);
        // No break in the sound, and the timeline runs on: every frame at
        // full level and on time.
        assert!(
            sim.err[mark..].iter().all(|e| !e.is_nan()),
            "a gap or a fade"
        );
        assert!(sim.max_err(mark) < 300.0, "{} µs", sim.max_err(mark));
        // The new audio sounds from the frame heard at `from`.
        let switched = mark
            + sim.amp[mark..]
                .iter()
                .position(|&a| a < 0.985 * AMP as f32)
                .expect("the new audio plays");
        assert!(
            (switched as f64 - from_frame).abs() < 48.0,
            "switched at frame {switched}, due at {from_frame}"
        );
        assert!(
            sim.amp[switched + 100..]
                .iter()
                .all(|&a| a < 0.975 * AMP as f32)
        );
        assert_eq!(sim.sink.stats().jumps, 1, "no jump");
    }

    #[test]
    fn a_gap_in_the_timeline_fades_out_and_jumps_back_in() {
        let mut sim = playing(0.0, 0.0, 48_000, 2.0);
        // The server skips 100 ms.
        sim.deliver(sim.now() + Duration::from_secs(1));
        let gap_at = sim.next_ts;
        sim.next_ts += 100_000.0;
        let mark = sim.err.len();
        sim.run(2.0);
        let heard_gap =
            mark + ((gap_at - sim.clock.at(sim.heard(mark as u64))) / 1e6 * 48_000.0) as usize;
        // Faded out by the gap, silent through it, then back on time.
        let quiet = mark + sim.amp[mark..].iter().position(|&a| a < 1.0).unwrap();
        assert!(
            quiet <= heard_gap && heard_gap - quiet < 2 * CHUNK,
            "{quiet} vs {heard_gap}"
        );
        assert!(
            sim.amp[quiet - CHUNK / 2..quiet]
                .windows(2)
                .all(|w| w[1] <= w[0] + 30.0)
        );
        let back = quiet + sim.err[quiet..].iter().position(|e| !e.is_nan()).unwrap();
        assert!(back > heard_gap + frames(0.1), "silent through the gap");
        assert!(sim.max_err(back) < 300.0, "{} µs", sim.max_err(back));
        assert_eq!(sim.sink.stats().jumps, 2);
    }

    #[test]
    fn end_fades_out_and_says_so() {
        let mut sim = playing(0.0, 0.0, 48_000, 2.0);
        sim.sink.end();
        sim.streaming = false;
        let mark = sim.amp.len();
        sim.run(0.2);
        let quiet = sim.amp[mark..].iter().position(|&a| a < 1.0).unwrap();
        assert!(quiet <= 2 * CHUNK, "faded out after {quiet} frames");
        assert!(
            sim.amp[mark..mark + quiet]
                .windows(2)
                .all(|w| w[1] <= w[0] + 20.0)
        );
        assert_eq!(sim.state(), TimedState::Ended(EndReason::Ended));
        assert_eq!(sim.lane.phase(), MediaPhase::Idle);
        assert!(!sim.lane.wants_session());
        assert_eq!(sim.sink.push(0, 48_000, vec![0; 2]), Err(Refused::Ended));
        assert!(lock(&sim.sink.q.buf).chunks.is_empty(), "queue emptied");
    }

    #[test]
    fn dropping_the_sink_ends_it() {
        let mut sim = playing(0.0, 0.0, 48_000, 1.0);
        let state = sim.sink.state();
        let q = sim.queue();
        let sink = std::mem::replace(
            &mut sim.sink,
            TimedSink::new(TimedQueue::new(sim.clock.clone())),
        );
        drop(sink);
        sim.streaming = false;
        sim.run(0.1);
        assert_eq!(*state.borrow(), TimedState::Ended(EndReason::Ended));
        assert!(q.ended());
        assert_eq!(sim.lane.phase(), MediaPhase::Idle);
    }

    #[test]
    fn the_gain_ramps() {
        let mut sim = playing(0.0, 0.0, 48_000, 1.0);
        sim.sink.set_gain(0.5);
        let mark = sim.amp.len();
        sim.run(0.1);
        let amp = &sim.amp[mark..];
        let half = AMP as f32 / 2.0;
        assert!((amp[amp.len() - 1] - half).abs() < 5.0);
        // Over 10 ms (half the full-scale ramp), never a step.
        let reached = amp.iter().position(|a| (a - half).abs() < 5.0).unwrap();
        assert!((440..=520).contains(&reached), "{reached}");
        assert!(amp.windows(2).all(|w| (w[1] - w[0]).abs() < 30.0));
        // Timing is untouched.
        assert!(sim.max_err(mark) < 300.0);
        sim.sink.set_gain(f32::NAN);
        sim.sink.set_gain(7.0);
        sim.run(0.1);
        assert!(
            (sim.amp[sim.amp.len() - 1] - AMP as f32).abs() < 5.0,
            "clamped to 1"
        );
    }

    #[test]
    fn audio_too_far_ahead_and_bad_formats_are_refused() {
        let sim = playing(0.0, 0.0, 48_000, 0.5);
        let ahead = sim.heard_us() + 31e6;
        assert_eq!(
            sim.sink.push(ahead as i64, 48_000, vec![0; 2]),
            Err(Refused::TooFarAhead)
        );
        let soon = sim.heard_us() + 29e6;
        assert_eq!(sim.sink.push(soon as i64, 48_000, vec![0; 2]), Ok(()));
        // Due soon enough, but lasting past 30 s.
        let soon = sim.heard_us() + 29.5e6;
        assert_eq!(
            sim.sink.push(soon as i64, 48_000, vec![0; 2 * 48_000]),
            Err(Refused::TooFarAhead)
        );
        assert_eq!(
            sim.sink.push(soon as i64, 48_000, vec![0; 2 * 4_800]),
            Ok(())
        );
        assert_eq!(
            sim.sink.push(0, 32_000, vec![0; 2]),
            Err(Refused::Rate(32_000))
        );
        assert_eq!(sim.sink.push(0, 48_000, vec![0; 3]), Err(Refused::Torn));
    }

    #[tokio::test]
    async fn a_speaker_plays_a_timed_source_until_stopped() {
        use crate::{Backend, Speaker, SpeakerConfig};
        let mut cfg = SpeakerConfig::new(Backend::WavFile("unused".into()));
        cfg.idle_timeout = Some(Duration::from_secs(5));
        cfg.initial_volume = 100;
        let sp = Speaker::new(cfg);
        let clock = Arc::new(FakeClock {
            origin: Instant::now(),
            offset_us: Mutex::new(1e9),
            drift: 0.0,
        });
        let sink = sp.media().begin_timed("sendspin", clock.clone());
        // 2 s of audio due from 300 ms on, in 50 ms chunks.
        let start = clock.at(Instant::now()) + 300_000.0;
        for i in 0..40 {
            let ts = start + f64::from(i) * 50_000.0;
            sink.push(ts as i64, 48_000, vec![1_000; 2 * 2_400])
                .unwrap();
        }
        let mut state = sink.state();
        tokio::time::timeout(
            Duration::from_secs(2),
            state.wait_for(|s| *s == TimedState::Playing),
        )
        .await
        .expect("plays")
        .unwrap();
        let media = sp.media().state().borrow().clone();
        assert_eq!(
            (media.phase, media.url.as_deref(), media.source.as_str()),
            (MediaPhase::Playing, Some(STREAM_URL), "sendspin")
        );
        // The file backend has no hardware pointer: paced, not exact.
        assert!(!sink.stats().exact_timing);
        sp.media().stop();
        tokio::time::timeout(
            Duration::from_secs(1),
            state.wait_for(|s| *s == TimedState::Ended(EndReason::Stopped)),
        )
        .await
        .expect("ends")
        .unwrap();
        assert_eq!(sink.push(0, 48_000, vec![0; 2]), Err(Refused::Ended));
    }

    // -- sharing the lane ----------------------------------------------------

    /// A lane playing a URL at a constant level, and a timed source that
    /// begins with its first audio due 300 ms after the next chunk mixed.
    fn url_then_timed() -> (Sim, std::sync::mpsc::SyncSender<Item>) {
        let mut sim = Sim::new(0.0, 0.0, 48_000);
        let mut lane = MediaLane::default();
        let (tx, rx) = sync_channel(200);
        for _ in 0..150 {
            tx.send(Item::Data(vec![3_000; 2 * CHUNK])).unwrap();
        }
        lane.begin(1, "http://x/a.mp3".into());
        lane.attach(1, Decoder::detached(rx));
        lane.set_stream_track(
            "sendspin".into(),
            Track {
                title: "Song".into(),
                ..Default::default()
            },
        );
        // The source the simulation began goes with its lane; this one
        // begins after the URL.
        let q = TimedQueue::new(sim.clock.clone());
        lane.begin_timed(2, "sendspin".into(), Decoder::timed(q.clone()));
        sim.lane = lane;
        sim.sink = TimedSink::new(q);
        (sim, tx)
    }

    #[test]
    fn it_replaces_what_plays_once_its_first_audio_is_due() {
        let (mut sim, _tx) = url_then_timed();
        sim.start_stream(0.6);
        let mut st = MediaState::default();
        sim.run(0.3);
        sim.lane.publish(&mut st);
        assert_eq!(
            (st.phase, st.url.as_deref()),
            (MediaPhase::Playing, Some("http://x/a.mp3")),
            "the URL plays on"
        );
        assert!(sim.amp.iter().all(|&a| (a - 4_243.0).abs() < 2.0));
        assert_eq!(sim.state(), TimedState::Waiting);
        sim.run(0.5);
        sim.lane.publish(&mut st);
        assert_eq!(
            (
                st.phase,
                st.url.as_deref(),
                st.source.as_str(),
                st.track.title.as_str()
            ),
            (MediaPhase::Playing, Some(STREAM_URL), "sendspin", "Song")
        );
        assert_eq!(sim.state(), TimedState::Playing);
        // Replaced when due, not before, and in time: its first audio is due
        // 350 ms into the mix (600 ms after the server started, which was
        // 250 ms before the first chunk is heard), then fades in.
        let first = sim.first_sound();
        let due = frames(0.6 - MIX_AHEAD) + 912;
        assert!((first as i64 - due as i64).abs() < 50, "{first} vs {due}");
        assert!(sim.max_err(first) < 300.0);
        // The URL faded out as the timed source faded in: no click.
        let around = &sim.amp[first - 3 * CHUNK..first + CHUNK];
        assert!(around.windows(2).all(|w| (w[1] - w[0]).abs() < 60.0));
    }

    #[test]
    fn another_source_a_stop_a_pause_or_a_failure_end_it_and_say_why() {
        // A URL replaces it.
        let mut sim = playing(0.0, 0.0, 48_000, 0.6);
        assert_eq!(sim.state(), TimedState::Playing);
        let (_tx, rx) = sync_channel(4);
        sim.lane.begin(5, "http://x/b.mp3".into());
        assert_eq!(
            sim.state(),
            TimedState::Playing,
            "only once the URL has audio"
        );
        sim.lane.attach(5, Decoder::detached(rx));
        assert_eq!(sim.state(), TimedState::Ended(EndReason::Replaced));
        // It fades out rather than stopping dead.
        let mark = sim.amp.len();
        sim.run(0.1);
        assert!(sim.amp[mark] > 0.9 * AMP as f32);
        assert!(
            sim.amp[mark..mark + 2 * CHUNK]
                .windows(2)
                .all(|w| w[1] <= w[0] + 20.0)
        );

        let mut sim = playing(0.0, 0.0, 48_000, 0.6);
        sim.lane.stop(9);
        assert_eq!(sim.state(), TimedState::Ended(EndReason::Stopped));

        let mut sim = playing(0.0, 0.0, 48_000, 0.6);
        sim.lane.pause();
        assert_eq!(sim.state(), TimedState::Ended(EndReason::Paused));
        assert_eq!(sim.lane.phase(), MediaPhase::Idle);

        let mut sim = playing(0.0, 0.0, 48_000, 0.6);
        sim.lane.fail("speaker process died");
        assert_eq!(sim.state(), TimedState::Ended(EndReason::SpeakerFailed));

        // Still waiting for its first audio: a newer play replaces it, a
        // pause ends it.
        let (mut sim, _tx) = url_then_timed();
        sim.lane.begin(7, "http://x/c.mp3".into());
        assert_eq!(sim.state(), TimedState::Ended(EndReason::Replaced));
        let (mut sim, _tx) = url_then_timed();
        sim.lane.pause();
        assert_eq!(sim.state(), TimedState::Ended(EndReason::Paused));
        let mut st = MediaState::default();
        sim.lane.publish(&mut st);
        assert_eq!(
            (st.phase, st.url.as_deref()),
            (MediaPhase::Paused, Some("http://x/a.mp3")),
            "the URL pauses"
        );

        // A pause ends a waiting timed source whatever plays, also a timed
        // source or a live stream: it doesn't start when its audio is due.
        for live in [false, true] {
            let mut sim = playing(0.0, 0.0, 48_000, 0.6);
            sim.streaming = false;
            if live {
                let q = Arc::new(crate::media::LiveQueue::default());
                sim.lane.begin_stream(4, "airplay".into());
                sim.lane.attach(4, Decoder::live_detached(q));
            }
            let q = TimedQueue::new(sim.clock.clone());
            sim.lane
                .begin_timed(8, "sendspin".into(), Decoder::timed(q.clone()));
            let waiting = TimedSink::new(q);
            sim.lane.pause();
            assert_eq!(
                *waiting.state().borrow(),
                TimedState::Ended(EndReason::Paused),
                "live: {live}"
            );
            assert_eq!(sim.lane.phase(), MediaPhase::Idle);
            let due = sim.heard_us() + 100_000.0;
            assert_eq!(
                waiting.push(due as i64, 48_000, sim.chunk_pcm(due, 14_400)),
                Err(Refused::Ended)
            );
            let mark = sim.amp.len();
            sim.run(0.3);
            assert!(
                sim.amp[mark + 2 * CHUNK..].iter().all(|&a| a < 1.0),
                "live: {live}: it played after the pause"
            );
            assert_eq!(sim.lane.phase(), MediaPhase::Idle);
        }
    }
}
