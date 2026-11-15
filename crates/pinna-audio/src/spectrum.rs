//! Music spectrum for the LED ring: band levels of the media lane, timed to
//! the moment each chunk sounds.
//!
//! The speaker actor runs every media chunk (before ducking and the master
//! volume) through a bank of band-pass filters and queues the levels with the
//! time the chunk reaches the speaker; the ring renderer asks for the levels
//! sounding now. Levels are relative to the loudest band's recent peak, so
//! quiet tracks still reach full brightness, and silence stays dark.

use crate::refsink::Biquad;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Bands, bass first: one per ring pixel.
pub const BANDS: usize = 12;
/// Levels 0..=1 per band, bass first.
pub type Bands = [f32; BANDS];

/// Centre of the lowest and highest band; the rest are log-spaced between
/// (~0.7 octave apart).
const LOW_HZ: f32 = 50.0;
const HIGH_HZ: f32 = 10_000.0;
/// Q of each of the two cascaded stages per band (4th order: steep enough
/// that a tone does not light bands far from it).
const STAGE_Q: f32 = 1.3;
/// Added per band, bass to treble: music has less energy per band in the
/// treble, which would otherwise always look dim.
const TILT_DB: f32 = 1.5;
/// Range shown below the ceiling.
const RANGE_DB: f32 = 24.0;
/// How fast the ceiling falls back after a peak.
const CEILING_FALL_DB_S: f32 = 6.0;
/// The ceiling never falls below this: quiet audio stays dim, silence dark.
const FLOOR_DBFS: f32 = -50.0;
/// Time constant of a level's fall (it rises at once).
const RELEASE: Duration = Duration::from_millis(150);

/// Levels queued ahead of the speaker (~0.6 s of 20 ms chunks).
const QUEUE: usize = 32;
/// The newest due levels are shown for this long; after that no music is sounding.
const STALE: Duration = Duration::from_millis(100);

/// Band levels from mono audio, one call per chunk.
pub struct Analyzer {
    rate: f32,
    filters: [[Biquad; 2]; BANDS],
    ceiling_db: f32,
    level: Bands,
}

impl Analyzer {
    pub fn new(rate: u32) -> Self {
        let rate = rate as f32;
        let step = (HIGH_HZ / LOW_HZ).powf(1.0 / (BANDS - 1) as f32);
        Analyzer {
            rate,
            filters: std::array::from_fn(|b| {
                let f = Biquad::bandpass(rate, LOW_HZ * step.powi(b as i32), STAGE_Q);
                [f.clone(), f]
            }),
            ceiling_db: FLOOR_DBFS,
            level: [0.0; BANDS],
        }
    }

    /// Feed one chunk of mono samples (s16 scale) and return the levels.
    pub fn process(&mut self, mono: &[f32]) -> Bands {
        if mono.is_empty() {
            return self.level;
        }
        let dt = mono.len() as f32 / self.rate;
        let mut db = [0.0f32; BANDS];
        for (b, ([f1, f2], d)) in self.filters.iter_mut().zip(&mut db).enumerate() {
            let mut sum = 0.0f32;
            for &x in mono {
                let y = f2.process(f1.process(x));
                sum += y * y;
            }
            // dBFS of the band's RMS (full scale 32768), tilted.
            *d = 10.0 * (sum / mono.len() as f32).max(1e-6).log10() - 90.309 + TILT_DB * b as f32;
        }
        let loudest = db.iter().copied().fold(f32::MIN, f32::max);
        self.ceiling_db = loudest
            .max(self.ceiling_db - CEILING_FALL_DB_S * dt)
            .max(FLOOR_DBFS);
        let fall = (-dt / RELEASE.as_secs_f32()).exp();
        for (l, d) in self.level.iter_mut().zip(db) {
            let target = ((d - self.ceiling_db + RANGE_DB) / RANGE_DB).clamp(0.0, 1.0);
            *l = target.max(*l * fall);
        }
        self.level
    }
}

/// Levels of the music as it sounds. Cheap to clone.
#[derive(Clone, Default)]
pub struct Spectrum {
    queue: Arc<Mutex<VecDeque<(Instant, Bands)>>>,
}

impl Spectrum {
    /// Queue `bands` for the chunk that starts sounding at `at`.
    pub(crate) fn push(&self, at: Instant, bands: Bands) {
        let mut q = self.queue.lock().unwrap();
        if q.len() >= QUEUE {
            q.pop_front();
        }
        q.push_back((at, bands));
    }

    /// The levels sounding at `now`, or `None` when no music is.
    pub fn at(&self, now: Instant) -> Option<Bands> {
        let mut q = self.queue.lock().unwrap();
        while q.len() > 1 && q[1].0 <= now {
            q.pop_front();
        }
        let &(t, bands) = q.front()?;
        (t <= now && now - t <= STALE).then_some(bands)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;
    /// One 20 ms chunk.
    const N: usize = 960;

    /// `chunks` chunks of a sine at `hz`, amplitude `amp`.
    fn run(a: &mut Analyzer, hz: f32, amp: f32, chunks: usize) -> Bands {
        let mut out = [0.0; BANDS];
        let mut i = 0usize;
        for _ in 0..chunks {
            let buf: Vec<f32> = (0..N)
                .map(|_| {
                    i += 1;
                    amp * (std::f32::consts::TAU * hz * i as f32 / RATE as f32).sin()
                })
                .collect();
            out = a.process(&buf);
        }
        out
    }

    fn loudest(b: &Bands) -> usize {
        (0..BANDS).max_by(|&x, &y| b[x].total_cmp(&b[y])).unwrap()
    }

    #[test]
    fn a_tone_lights_its_band() {
        for (hz, band) in [(50.0, 0), (1000.0, 6), (10_000.0, 11)] {
            let b = run(&mut Analyzer::new(RATE), hz, 8000.0, 25);
            assert_eq!(loudest(&b), band, "{hz} Hz: {b:?}");
            assert!(b[band] > 0.9, "{hz} Hz: {b:?}");
            // Far bands stay dark.
            let far = if band < 6 { 11 } else { 0 };
            assert!(b[far] < 0.1, "{hz} Hz: {b:?}");
        }
    }

    #[test]
    fn silence_is_dark() {
        let b = run(&mut Analyzer::new(RATE), 440.0, 0.0, 10);
        assert!(b.iter().all(|&l| l == 0.0), "{b:?}");
        // Very quiet noise floor: still dark.
        let b = run(&mut Analyzer::new(RATE), 440.0, 0.5, 10);
        assert!(b.iter().all(|&l| l == 0.0), "{b:?}");
    }

    #[test]
    fn quiet_music_still_reaches_full_level() {
        // -44 dBFS: the ceiling follows it down.
        let b = run(&mut Analyzer::new(RATE), 1000.0, 300.0, 25);
        assert!(b[6] > 0.9, "{b:?}");
    }

    #[test]
    fn a_tone_lights_only_bands_near_it() {
        let b = run(&mut Analyzer::new(RATE), 1000.0, 8000.0, 25);
        for band in [0, 1, 2, 3, 10, 11] {
            assert!(b[band] < 0.05, "band {band}: {b:?}");
        }
    }

    #[test]
    fn levels_rise_at_once_and_fall_smoothly() {
        let mut a = Analyzer::new(RATE);
        let on = run(&mut a, 1000.0, 8000.0, 25)[6];
        let next = a.process(&[0.0; N])[6];
        assert!(next < on && next > 0.5 * on, "{on} -> {next}");
        for _ in 0..50 {
            a.process(&[0.0; N]);
        }
        assert!(a.process(&[0.0; N])[6] < 0.01);
    }

    #[test]
    fn spectrum_follows_the_clock() {
        let s = Spectrum::default();
        let t0 = Instant::now();
        let ms = |n| t0 + Duration::from_millis(n);
        assert_eq!(s.at(t0), None);
        s.push(ms(100), [0.1; BANDS]);
        s.push(ms(120), [0.2; BANDS]);
        s.push(ms(140), [0.3; BANDS]);
        assert_eq!(s.at(ms(50)), None, "not sounding yet");
        assert_eq!(s.at(ms(105)), Some([0.1; BANDS]));
        assert_eq!(s.at(ms(125)), Some([0.2; BANDS]));
        assert_eq!(s.at(ms(200)), Some([0.3; BANDS]));
        assert_eq!(s.at(ms(260)), None, "stale: the music stopped");
    }

    #[test]
    fn spectrum_queue_is_bounded() {
        let s = Spectrum::default();
        let t0 = Instant::now();
        for i in 0..100 {
            s.push(t0 + Duration::from_millis(20 * i), [i as f32; BANDS]);
        }
        assert_eq!(s.queue.lock().unwrap().len(), QUEUE);
    }
}
