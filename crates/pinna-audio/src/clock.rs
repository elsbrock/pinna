//! Hardware clock tracking for the Echo Dot's two audio clock domains.
//!
//! The speaker (AFE, nominally 48 kHz) and the microphones (SPI/FPGA, nominally
//! 16 kHz) run from different clocks, neither of which is the CPU's. pinnad paces
//! the speaker by the CPU clock and feeds the echo canceller a reference that
//! PipeWire consumes at the microphone clock, so both rates are measured from
//! the PCMs' hardware pointers (`/proc/asound/.../status`) and corrected for.
//!
//! The speaker's pointer and its timestamp also say *when* a frame sounds,
//! not only how fast: [`PointerAnchor`] corrects the offset of the speaker
//! session's frame-to-time mapping from them (ADR 0027 §10), so a timed
//! source can play each sample at the time its clock sets.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::time::Instant;

/// Least-squares rate of a hardware pointer over a sliding window.
#[derive(Debug, Clone)]
pub struct PcmClock {
    path: PathBuf,
    nominal: f64,
    origin: Instant,
    /// (seconds, frames)
    samples: VecDeque<(f64, f64)>,
    window_s: f64,
    min_span_s: f64,
}

/// One sample of a PCM status file.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PcmStatus {
    /// `tstamp` (0 when the driver does not provide one).
    pub tstamp: f64,
    pub hw_ptr: u64,
}

/// Parse `/proc/asound/cardN/pcmXY/subZ/status`; `None` when the PCM is closed.
pub fn parse_status(text: &str) -> Option<PcmStatus> {
    let mut tstamp = 0.0;
    let mut hw_ptr = None;
    for line in text.lines() {
        let mut kv = line.splitn(2, ':');
        let (k, v) = (kv.next()?.trim(), kv.next().unwrap_or("").trim());
        match k {
            "tstamp" => tstamp = v.parse().unwrap_or(0.0),
            "hw_ptr" => hw_ptr = v.parse().ok(),
            _ => {}
        }
    }
    hw_ptr.map(|hw_ptr| PcmStatus { tstamp, hw_ptr })
}

/// What the pointer anchor reads from a PCM status file.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PointerSample {
    /// `state: RUNNING`: the pointer advances.
    pub running: bool,
    /// `tstamp` (0 when the file has none).
    pub tstamp: f64,
    pub hw_ptr: u64,
    pub appl_ptr: Option<u64>,
}

/// The value of `key` in a `/proc/asound` file of `key: value` lines.
fn field<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    text.lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        (k.trim() == key).then(|| v.trim())
    })
}

/// Parse a status file for [`PointerAnchor`]; `None` when the PCM is closed.
pub fn parse_pointer(text: &str) -> Option<PointerSample> {
    let st = parse_status(text)?;
    Some(PointerSample {
        running: field(text, "state") == Some("RUNNING"),
        tstamp: st.tstamp,
        hw_ptr: st.hw_ptr,
        appl_ptr: field(text, "appl_ptr").and_then(|v| v.parse().ok()),
    })
}

/// The `boundary` of a PCM's `sw_params` file: where its pointers wrap.
pub fn parse_boundary(text: &str) -> Option<u64> {
    field(text, "boundary")?.parse().ok()
}

/// `CLOCK_MONOTONIC` in seconds: the clock ALSA stamps pointers with, and
/// the one [`Instant`] reads on Linux.
pub fn monotonic_now() -> f64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `ts` is a valid, writable timespec for the duration of the call.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    ts.tv_sec as f64 + ts.tv_nsec as f64 * 1e-9
}

/// Status samples the anchor keeps (2 s at one per 200 ms). A pointer that
/// moves in bursts reads behind the truth, never ahead, so a sample can only
/// make the mapping look early: the largest error among them is the closest
/// to the truth.
const ANCHOR_WINDOW: usize = 10;
/// A mapping error beyond this is re-anchored, not slewed: at once when the
/// mapping is late, once the whole window agrees when it is early (a burst
/// can make a few samples look early), or at once when one sample shows it
/// early by more than any burst could ([`ANCHOR_BIG`]).
const ANCHOR_JUMP: f64 = 2e-3;
const ANCHOR_BIG: f64 = 0.05;
/// A small error is slewed: a quarter of it per sample, at most 100 µs, so a
/// timed source sees the time a frame is heard move gently.
const ANCHOR_SLEW_GAIN: f64 = 0.25;
const ANCHOR_SLEW_MAX: f64 = 100e-6;
/// A `tstamp` this far from `CLOCK_MONOTONIC` is on another clock.
const ANCHOR_CLOCK_MAX: f64 = 5.0;

/// Corrects when a playback session's frames sound, from the hardware
/// pointer and its timestamp (ADR 0027 §10).
///
/// Session frame `f` is pointer position `f`: the pointer counts from 0 when
/// the player opens the PCM, and the player plays every frame the session
/// writes, in order. Each status sample says frame `hw_ptr` was being played
/// at `tstamp`; the anchor compares that with where the session's mapping
/// puts it and says how far to move the mapping. An underrun stalls the
/// pointer while time goes on: the samples after it show the mapping early
/// by the stall, and it is re-anchored.
///
/// Two events break the identity of frame and pointer. The pointer wraps at
/// the PCM's `boundary` (on a 32-bit kernel, every ~9 h at 48 kHz): that is
/// undone exactly. And a player that recovers from an underrun re-prepares
/// the PCM, which restarts the pointer from 0: the session frame it counts
/// from is then estimated from the application pointer (the frames handed
/// to the player, less those it still holds: at most that), and the timing
/// is no longer exact until the session reopens.
#[derive(Debug, Clone)]
pub struct PointerAnchor {
    nominal: f64,
    /// Session frame of pointer position 0.
    base: u64,
    /// The last running sample: (timestamp, pointer).
    last: Option<(f64, u64)>,
    boundary: Option<u64>,
    boundary_read: bool,
    /// The pointer restarted: `base` is an estimate.
    restarted: bool,
    /// Recent errors, mapping minus pointer, in seconds (positive: the
    /// mapping puts the frame later than the pointer did).
    recent: VecDeque<f64>,
    /// The timestamps are on another clock: logged once, never used.
    foreign_clock: bool,
    anchored: bool,
}

impl PointerAnchor {
    /// For a PCM that runs at `nominal` frames per second.
    pub fn new(nominal: f64) -> Self {
        Self {
            nominal,
            base: 0,
            last: None,
            boundary: None,
            boundary_read: false,
            restarted: false,
            recent: VecDeque::with_capacity(ANCHOR_WINDOW + 1),
            foreign_clock: false,
            anchored: false,
        }
    }

    /// Where the PCM's pointers wrap (its `sw_params`), or that it is not
    /// known: a wrap then counts as a restart.
    pub fn set_boundary(&mut self, boundary: Option<u64>) {
        self.boundary = boundary.filter(|&b| b > 0);
        self.boundary_read = true;
    }

    /// Whether the boundary is still to be read.
    pub fn needs_boundary(&self) -> bool {
        !self.boundary_read
    }

    /// The mapping follows the pointer, and the pointer still counts the
    /// session's frames.
    pub fn exact(&self) -> bool {
        self.anchored && !self.restarted && !self.foreign_clock
    }

    /// One status sample, read when `CLOCK_MONOTONIC` read `mono_now`
    /// seconds, with `written` frames handed to the player so far.
    /// `predicted(f)` is where the session's mapping puts frame `f`, in
    /// `CLOCK_MONOTONIC` seconds. Returns how far to move the mapping, in
    /// seconds (positive: later), if at all.
    pub fn update(
        &mut self,
        s: &PointerSample,
        mono_now: f64,
        written: u64,
        predicted: impl Fn(u64) -> f64,
    ) -> Option<f64> {
        if !s.running || self.foreign_clock {
            return None;
        }
        // Without a driver timestamp, the read time: later than the pointer,
        // so it errs the same way as a bursty pointer.
        let t = if s.tstamp > 0.0 { s.tstamp } else { mono_now };
        if (t - mono_now).abs() > ANCHOR_CLOCK_MAX {
            self.foreign_clock = true;
            tracing::warn!(
                "speaker: PCM timestamps are not on the monotonic clock ({t:.3} s, monotonic \
                 {mono_now:.3} s): the speaker's timing is not exact"
            );
            return None;
        }
        if let Some((lt, lhw)) = self.last
            && s.hw_ptr < lhw
        {
            self.pointer_went_back(lt, lhw, t, s, written);
        }
        self.last = Some((t, s.hw_ptr));
        if self.restarted
            && let Some(appl) = s.appl_ptr
        {
            // The player holds fewer frames at some samples than at others:
            // the smallest estimate is the tightest.
            self.base = self.base.min(written.saturating_sub(appl));
        }
        let e = predicted(self.base + s.hw_ptr) - t;
        self.recent.push_back(e);
        if self.recent.len() > ANCHOR_WINDOW {
            self.recent.pop_front();
        }
        let m = self.recent.iter().fold(f64::MIN, |a, &b| a.max(b));
        let shift = if e > ANCHOR_JUMP {
            // The mapping is late: no pointer reads ahead of the truth.
            -e
        } else if e < -ANCHOR_BIG {
            // Frames sound later than mapped (an underrun stalled the
            // pointer): the samples before describe the old timeline.
            self.recent.drain(..self.recent.len() - 1);
            -e
        } else if self.recent.len() == ANCHOR_WINDOW && m < -ANCHOR_JUMP {
            -m
        } else {
            -(m * ANCHOR_SLEW_GAIN).clamp(-ANCHOR_SLEW_MAX, ANCHOR_SLEW_MAX)
        };
        for r in &mut self.recent {
            *r += shift;
        }
        self.anchored = true;
        (shift != 0.0).then_some(shift)
    }

    /// The pointer is behind its last sample: it wrapped at the boundary, or
    /// the PCM restarted.
    fn pointer_went_back(&mut self, lt: f64, lhw: u64, t: f64, s: &PointerSample, written: u64) {
        let elapsed = (t - lt).max(0.0);
        let wrapped = self.boundary.is_some_and(|b| {
            let forward = b.saturating_sub(lhw) + s.hw_ptr;
            lhw < b && (forward as f64) <= (elapsed * 1.01 + 0.1) * self.nominal
        });
        if let (true, Some(b)) = (wrapped, self.boundary) {
            self.base += b;
            return;
        }
        if !self.restarted {
            tracing::warn!(
                "speaker: the PCM restarted after an underrun: the time each frame sounds is an \
                 estimate from now on"
            );
        }
        self.restarted = true;
        self.base = written.saturating_sub(s.appl_ptr.unwrap_or(0));
        self.recent.clear();
    }
}

impl PcmClock {
    pub fn new(path: impl AsRef<Path>, nominal: f64) -> Self {
        Self {
            path: path.as_ref().to_owned(),
            nominal,
            origin: Instant::now(),
            samples: VecDeque::new(),
            window_s: 120.0,
            min_span_s: 20.0,
        }
    }

    /// Forget all samples (the PCM was reopened: its pointer restarted).
    pub fn reset(&mut self) {
        self.samples.clear();
    }

    /// Read the status file once.
    pub fn sample(&mut self, now: Instant) {
        self.sample_text(now);
    }

    /// Read the status file once, and return what it said.
    pub fn sample_text(&mut self, now: Instant) -> Option<String> {
        let text = std::fs::read_to_string(&self.path).ok()?;
        match parse_status(&text) {
            Some(st) => self.push(now.saturating_duration_since(self.origin).as_secs_f64(), st),
            None => self.samples.clear(),
        }
        Some(text)
    }

    /// The status file this clock reads.
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn push(&mut self, cpu_s: f64, st: PcmStatus) {
        // Prefer the driver's timestamp of the pointer update: exact even for a
        // bursty pointer. Without it, the read time (noisy, averaged out by the fit).
        let t = if st.tstamp > 0.0 { st.tstamp } else { cpu_s };
        if let Some(&(lt, lp)) = self.samples.back()
            && ((st.hw_ptr as f64) < lp || t < lt)
        {
            self.samples.clear(); // pointer or clock went backwards: PCM restarted
        }
        self.samples.push_back((t, st.hw_ptr as f64));
        while self
            .samples
            .front()
            .is_some_and(|&(t0, _)| t - t0 > self.window_s)
        {
            self.samples.pop_front();
        }
    }

    /// Measured rate in frames per second, once the window spans long enough.
    pub fn rate(&self) -> Option<f64> {
        let (&(t0, _), &(t1, _)) = (self.samples.front()?, self.samples.back()?);
        if t1 - t0 < self.min_span_s || self.samples.len() < 10 {
            return None;
        }
        let n = self.samples.len() as f64;
        let (mt, mp) = self
            .samples
            .iter()
            .fold((0.0, 0.0), |(a, b), &(t, p)| (a + t / n, b + p / n));
        let (num, den) = self.samples.iter().fold((0.0, 0.0), |(a, b), &(t, p)| {
            (a + (t - mt) * (p - mp), b + (t - mt) * (t - mt))
        });
        (den > 0.0).then(|| num / den)
    }

    /// Relative deviation from nominal (e.g. 3.2e-4 = +320 ppm), clamped to ±2000 ppm.
    pub fn deviation(&self) -> Option<f64> {
        self.rate()
            .map(|r| (r / self.nominal - 1.0).clamp(-2e-3, 2e-3))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_status() {
        let s = "state: RUNNING\nowner_pid   : 290\ntrigger_time: 1.0\ntstamp      : 1204.641889163\ndelay       : 227\navail       : 0\navail_max   : 640\n-----\nhw_ptr      : 9035107\nappl_ptr    : 9034880\n";
        assert_eq!(
            parse_status(s),
            Some(PcmStatus {
                tstamp: 1204.641889163,
                hw_ptr: 9035107
            })
        );
        assert_eq!(parse_status("closed\n"), None);
    }

    #[test]
    fn fits_bursty_pointer_with_read_times() {
        // 48 kHz +320 ppm, pointer advancing in 1024-frame bursts, sampled every 200 ms.
        let mut c = PcmClock::new("/nonexistent", 48_000.0);
        let rate = 48_000.0 * (1.0 + 320e-6);
        for i in 0..400 {
            let t = i as f64 * 0.2;
            let p = ((t * rate) as u64 / 1024) * 1024;
            c.push(
                t,
                PcmStatus {
                    tstamp: 0.0,
                    hw_ptr: p,
                },
            );
        }
        let d = c.deviation().unwrap();
        assert!((d - 320e-6).abs() < 30e-6, "{d}");
    }

    #[test]
    fn needs_a_long_enough_window_and_resets_on_restart() {
        let mut c = PcmClock::new("/nonexistent", 16_000.0);
        for i in 0..50 {
            let t = i as f64 * 0.2;
            c.push(
                t,
                PcmStatus {
                    tstamp: t,
                    hw_ptr: (t * 16_000.0) as u64,
                },
            );
        }
        assert!(c.rate().is_none(), "10 s is not enough");
        for i in 50..200 {
            let t = i as f64 * 0.2;
            c.push(
                t,
                PcmStatus {
                    tstamp: t,
                    hw_ptr: (t * 16_000.0) as u64,
                },
            );
        }
        assert!((c.rate().unwrap() - 16_000.0).abs() < 0.01);
        c.push(
            41.0,
            PcmStatus {
                tstamp: 41.0,
                hw_ptr: 5,
            },
        );
        assert!(c.rate().is_none(), "pointer restart clears the fit");
    }

    // -- the pointer anchor --------------------------------------------------

    const RATE: f64 = 48_000.0 * (1.0 + 320e-6);

    /// A speaker whose session frame `f` truly sounds at `t0 + f / RATE`,
    /// and a session mapping (`map_t0`, at the same rate) that the anchor
    /// corrects. Status samples come every 200 ms of monotonic time.
    struct Sim {
        anchor: PointerAnchor,
        t0: f64,
        map_t0: f64,
        t: f64,
        /// Every shift the anchor asked for.
        shifts: Vec<f64>,
    }

    impl Sim {
        fn new(map_error: f64) -> Sim {
            Sim {
                anchor: PointerAnchor::new(48_000.0),
                t0: 100.0,
                map_t0: 100.0 + map_error,
                t: 100.5,
                shifts: Vec::new(),
            }
        }

        /// Session frame sounding at `t`.
        fn frame_at(&self, t: f64) -> u64 {
            ((t - self.t0) * RATE) as u64
        }

        fn sample(&mut self, s: PointerSample, written: u64) {
            let map_t0 = self.map_t0;
            let predicted = |f: u64| map_t0 + f as f64 / RATE;
            // The read itself is a little later than the driver's stamp.
            if let Some(d) = self.anchor.update(&s, self.t + 30e-6, written, predicted) {
                self.map_t0 += d;
                self.shifts.push(d);
            }
            self.t += 0.2;
        }

        /// A precise pointer stamped when read.
        fn step(&mut self) {
            let hw = self.frame_at(self.t);
            self.sample(running(self.t, hw, None), hw + 10_000);
        }

        fn error(&self) -> f64 {
            self.map_t0 - self.t0
        }
    }

    fn running(tstamp: f64, hw_ptr: u64, appl_ptr: Option<u64>) -> PointerSample {
        PointerSample {
            running: true,
            tstamp,
            hw_ptr,
            appl_ptr,
        }
    }

    #[test]
    fn a_late_mapping_is_reanchored_at_once() {
        // The PCM was seen running 8 ms after it started (the open polls).
        let mut sim = Sim::new(8e-3);
        sim.step();
        assert!(sim.error().abs() < 30e-6, "{}", sim.error());
        for _ in 0..50 {
            sim.step();
        }
        assert!(sim.error().abs() < 30e-6, "{}", sim.error());
        assert!(sim.anchor.exact());
    }

    #[test]
    fn a_small_error_is_slewed_gently() {
        let mut sim = Sim::new(-1.5e-3);
        for _ in 0..60 {
            sim.step();
        }
        assert!(sim.error().abs() < 50e-6, "{}", sim.error());
        assert!(
            sim.shifts
                .iter()
                .all(|d| d.abs() <= ANCHOR_SLEW_MAX + 1e-12),
            "{:?}",
            sim.shifts
        );
    }

    #[test]
    fn an_underrun_stall_moves_the_mapping_later() {
        let mut sim = Sim::new(0.0);
        for _ in 0..20 {
            sim.step();
        }
        // The pointer stops for 300 ms (the player underran), then goes on:
        // every frame from there sounds 300 ms later than mapped.
        let stalled = sim.frame_at(sim.t);
        sim.t0 += 0.3;
        sim.sample(running(sim.t, stalled, None), stalled);
        // Mid-stall, the pointer still names the same frame.
        sim.sample(running(sim.t, stalled, None), stalled);
        assert!(sim.map_t0 > sim.t0 - 0.11, "moved by the stall so far");
        sim.step();
        assert!(sim.error().abs() < 30e-6, "{}", sim.error());
        for _ in 0..20 {
            sim.step();
        }
        assert!(sim.error().abs() < 30e-6, "{}", sim.error());
        assert!(sim.anchor.exact(), "the pointer still counts the frames");

        // A short stall could be a bursty pointer reading late: the mapping
        // jumps once the whole window agrees.
        sim.t0 += 0.004;
        for _ in 0..ANCHOR_WINDOW - 1 {
            sim.step();
        }
        assert!(
            (sim.error() + 0.004).abs() < 1e-4,
            "not yet: {}",
            sim.error()
        );
        sim.step();
        assert!(sim.error().abs() < 30e-6, "{}", sim.error());
    }

    #[test]
    fn a_restarted_pointer_is_reidentified_from_the_application_pointer() {
        let mut sim = Sim::new(0.0);
        for _ in 0..20 {
            sim.step();
        }
        // An underrun: the player re-prepares the PCM, both pointers restart
        // from 0, and playback resumes 150 ms later from session frame
        // `played`. The player holds part of an aplay period (1536 frames)
        // of what the session wrote in 960-frame chunks, plus what arrived
        // since.
        let played = sim.frame_at(sim.t);
        sim.t0 += 0.15;
        sim.t += 0.2;
        for i in 0..40u64 {
            let hw = sim.frame_at(sim.t) - played;
            let written = played + hw + 6_000;
            let held = (i * 960) % 1536 + (i * 37) % 90;
            let appl = written - played - held;
            sim.sample(running(sim.t, hw, Some(appl)), written);
        }
        // The estimate is high by the fewest frames the player held when
        // sampled: the mapping is early by that, here under 2 ms.
        assert!(
            sim.error() <= 30e-6 && sim.error() > -2e-3,
            "{}",
            sim.error()
        );
        assert!(!sim.anchor.exact(), "an estimate is not exact");
    }

    #[test]
    fn a_wrapping_pointer_keeps_counting() {
        let boundary = 1u64 << 21; // ~44 s at 48 kHz
        let mut sim = Sim::new(0.0);
        sim.anchor.set_boundary(Some(boundary));
        for _ in 0..600 {
            let f = sim.frame_at(sim.t);
            sim.sample(running(sim.t, f % boundary, Some(f % boundary)), f + 10_000);
        }
        assert!(
            sim.t - sim.t0 > 2.0 * boundary as f64 / RATE,
            "it wrapped twice"
        );
        assert!(sim.error().abs() < 30e-6, "{}", sim.error());
        assert!(sim.anchor.exact());
    }

    #[test]
    fn a_bursty_pointer_with_driver_stamps_is_exact() {
        // The pointer moves 1024 frames at a time; the driver stamps each
        // move, so every sample is exact for the frame it names.
        let mut sim = Sim::new(5e-3);
        for _ in 0..50 {
            let hw = sim.frame_at(sim.t) / 1024 * 1024;
            let stamp = sim.t0 + hw as f64 / RATE;
            sim.sample(running(stamp, hw, None), hw + 10_000);
        }
        assert!(sim.error().abs() < 30e-6, "{}", sim.error());
    }

    #[test]
    fn a_bursty_pointer_read_without_stamps_stays_within_a_burst() {
        // No driver stamp: the read time stands in, so a sample names a
        // frame up to a burst before the one playing. The mapping is never
        // early, and late by no more than the closest sample in the window.
        let mut sim = Sim::new(5e-3);
        let mut errors = Vec::new();
        for _ in 0..200 {
            let hw = sim.frame_at(sim.t) / 1024 * 1024;
            sim.sample(running(0.0, hw, None), hw + 10_000);
            errors.push(sim.error());
        }
        let burst = 1024.0 / RATE;
        let settled = &errors[20..];
        assert!(
            settled.iter().all(|&e| e > -1e-5 && e < burst / 4.0),
            "{:?}",
            &settled[..20]
        );
    }

    #[test]
    fn wall_clock_stamps_and_a_stopped_pcm_are_not_used() {
        let mut sim = Sim::new(5e-3);
        let hw = sim.frame_at(sim.t);
        // A wall-clock stamp, as a PCM opened without monotonic stamps has.
        sim.sample(running(1.763e9, hw, None), hw);
        assert!(sim.shifts.is_empty());
        sim.step();
        assert!(sim.shifts.is_empty(), "ignored for good");
        assert!(!sim.anchor.exact());

        let mut sim = Sim::new(5e-3);
        let hw = sim.frame_at(sim.t);
        sim.sample(
            PointerSample {
                running: false,
                ..running(sim.t, hw, None)
            },
            hw,
        );
        assert!(sim.shifts.is_empty());
        assert!(!sim.anchor.exact());
    }

    #[test]
    fn parses_pointer_samples_and_the_boundary() {
        let s = "state: RUNNING\nowner_pid   : 290\ntrigger_time: 1.0\ntstamp      : 1204.641889163\ndelay       : 227\navail       : 0\navail_max   : 640\n-----\nhw_ptr      : 9035107\nappl_ptr    : 9034880\n";
        assert_eq!(
            parse_pointer(s),
            Some(PointerSample {
                running: true,
                tstamp: 1204.641889163,
                hw_ptr: 9035107,
                appl_ptr: Some(9034880),
            })
        );
        let xrun = s.replace("RUNNING", "XRUN");
        assert!(!parse_pointer(&xrun).unwrap().running);
        assert_eq!(parse_pointer("closed\n"), None);
        let sw = "tstamp_mode: NONE\nperiod_step: 1\navail_min: 1536\nstart_threshold: 6144\nstop_threshold: 6144\nsilence_threshold: 0\nsilence_size: 0\nboundary: 1610612736\n";
        assert_eq!(parse_boundary(sw), Some(1_610_612_736));
        assert_eq!(parse_boundary("closed\n"), None);
    }

    #[test]
    fn monotonic_now_is_the_clock_instant_reads() {
        let (a, i0) = (monotonic_now(), Instant::now());
        std::thread::sleep(std::time::Duration::from_millis(20));
        let (b, i1) = (monotonic_now(), Instant::now());
        let d = (b - a) - (i1 - i0).as_secs_f64();
        assert!(d.abs() < 2e-3, "{d}");
        assert!(a > 0.0);
    }
}
