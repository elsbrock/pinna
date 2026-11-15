//! Hardware clock tracking for the Echo Dot's two audio clock domains.
//!
//! The speaker (AFE, nominally 48 kHz) and the microphones (SPI/FPGA, nominally
//! 16 kHz) run from different clocks, neither of which is the CPU's. pinnad paces
//! the speaker by the CPU clock and feeds the echo canceller a reference that
//! PipeWire consumes at the microphone clock, so both rates are measured from
//! the PCMs' hardware pointers (`/proc/asound/.../status`) and corrected for.

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
        let Ok(text) = std::fs::read_to_string(&self.path) else {
            return;
        };
        let Some(st) = parse_status(&text) else {
            self.samples.clear();
            return;
        };
        self.push(now.saturating_duration_since(self.origin).as_secs_f64(), st);
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
}
