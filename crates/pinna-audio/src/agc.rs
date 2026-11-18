//! Automatic gain on the captured mono stream, after echo cancellation and
//! before the wake word and Home Assistant (ADR 0004).
//!
//! Tracks the noise floor and the level of speech, and aims for the lowest of:
//! speech at [`TARGET_SPEECH_DBFS`], room noise at [`NOISE_CEILING_DBFS`], and
//! [`MAX_GAIN_DB`]. The noise ceiling sits under the wake word detector's
//! energy VAD, which needs more than about -58.5 dBFS whatever the noise:
//! lifted noise never opens it, so its speech-over-noise rule decides. Levels
//! are dBFS of a frame's mean square (a full-scale sine is -3 dBFS).

use crate::dsp::SAMPLE_RATE;

/// Speech level the gain aims for.
pub const TARGET_SPEECH_DBFS: f32 = -24.0;
/// Room noise is lifted no higher than this.
pub const NOISE_CEILING_DBFS: f32 = -65.0;
pub const MIN_GAIN_DB: f32 = 0.0;
pub const MAX_GAIN_DB: f32 = 36.0;

/// Output peaks stay at or below -1 dBFS while the gain is above 0 dB.
const PEAK_CEILING: f32 = 29_204.0;
/// A frame this far above the noise floor is speech.
const SPEECH_OVER_NOISE_DB: f32 = 9.0;
/// The noise floor falls at once and rises this fast, as the beamformer's.
const NOISE_RISE_DB_PER_S: f32 = 3.0;
/// Speech level smoothing: towards louder frames fast, quieter ones slowly.
const SPEECH_ATTACK_S: f32 = 0.1;
const SPEECH_RELEASE_S: f32 = 1.0;
/// Without speech the speech level decays, so the gain comes back after a
/// loud talker leaves, but too slowly to lift the noise between words.
const SPEECH_DECAY_DB_PER_S: f32 = 1.0;
const GAIN_RISE_DB_PER_S: f32 = 3.0;
const GAIN_FALL_DB_PER_S: f32 = 30.0;
/// Digital silence counts as this level, so the arithmetic stays finite.
const SILENCE_DBFS: f32 = -100.0;

#[derive(Clone, Debug)]
pub struct Agc {
    gain_db: f32,
    noise_db: Option<f32>,
    speech_db: Option<f32>,
}

impl Agc {
    /// Starts at `initial_gain_db`, clamped to the gain range.
    pub fn new(initial_gain_db: f32) -> Self {
        Agc {
            gain_db: initial_gain_db.clamp(MIN_GAIN_DB, MAX_GAIN_DB),
            noise_db: None,
            speech_db: None,
        }
    }

    /// Gain applied at the end of the last frame, dB.
    pub fn gain_db(&self) -> f32 {
        self.gain_db
    }

    /// Measure the frame, move the gain and apply it in place, ramped from
    /// the previous frame's gain so that no step is audible.
    pub fn process(&mut self, frame: &mut [i16]) {
        if frame.is_empty() {
            return;
        }
        let dt = frame.len() as f32 / SAMPLE_RATE as f32;
        let level = level_dbfs(frame);

        let speech = self
            .noise_db
            .is_some_and(|n| level >= n + SPEECH_OVER_NOISE_DB);
        let noise = match self.noise_db {
            Some(n) => (n + NOISE_RISE_DB_PER_S * dt).min(level),
            None => level,
        };
        self.noise_db = Some(noise);
        if speech {
            let s = self.speech_db.unwrap_or(level);
            let tau = if level > s {
                SPEECH_ATTACK_S
            } else {
                SPEECH_RELEASE_S
            };
            self.speech_db = Some(s + (level - s) * (1.0 - (-dt / tau).exp()));
        } else if let Some(s) = self.speech_db {
            let s = s - SPEECH_DECAY_DB_PER_S * dt;
            self.speech_db = (s > SILENCE_DBFS).then_some(s);
        }

        let mut desired = MAX_GAIN_DB.min(NOISE_CEILING_DBFS - noise);
        if let Some(s) = self.speech_db {
            desired = desired.min(TARGET_SPEECH_DBFS - s);
        }
        let desired = desired.max(MIN_GAIN_DB);
        let prev = self.gain_db;
        let mut next = if desired > prev {
            (prev + GAIN_RISE_DB_PER_S * dt).min(desired)
        } else {
            (prev - GAIN_FALL_DB_PER_S * dt).max(desired)
        };

        // A loud onset: drop within this frame instead of clipping it. At
        // 0 dB the input passes unchanged, so it can't clip either.
        let peak = f32::from(frame.iter().map(|s| s.unsigned_abs()).max().unwrap_or(0));
        let mut start = prev;
        if peak > 0.0 {
            let cap = 20.0 * (PEAK_CEILING / peak).log10();
            next = next.min(cap).max(MIN_GAIN_DB);
            if prev > cap {
                start = next;
            }
        }
        // Both ends are at or below the cap, so every ramped value is too.
        let (g0, g1) = (db_to_linear(start), db_to_linear(next));
        let n = frame.len() as f32;
        for (i, s) in frame.iter_mut().enumerate() {
            let g = g0 + (g1 - g0) * (i + 1) as f32 / n;
            *s = (f32::from(*s) * g).round().clamp(-32768.0, 32767.0) as i16;
        }
        self.gain_db = next;
    }
}

/// The frame's mean square, full scale being 1; an empty frame's is 0.
/// Callers that average several frames add these, never dB values.
pub fn mean_square(frame: &[i16]) -> f64 {
    if frame.is_empty() {
        return 0.0;
    }
    let sum: f64 = frame.iter().map(|&s| f64::from(s) * f64::from(s)).sum();
    sum / frame.len() as f64 / (32768.0 * 32768.0)
}

/// A [`mean_square`] in dBFS, the AGC's levels: a full-scale sine is
/// -3 dBFS, and digital silence counts as -100 dBFS.
pub fn mean_square_dbfs(mean_square: f64) -> f32 {
    if mean_square > 0.0 {
        ((10.0 * mean_square.log10()) as f32).max(SILENCE_DBFS)
    } else {
        SILENCE_DBFS
    }
}

fn level_dbfs(frame: &[i16]) -> f32 {
    mean_square_dbfs(mean_square(frame))
}

fn db_to_linear(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::FRAME_SAMPLES;

    const FRAMES_PER_S: usize = SAMPLE_RATE as usize / FRAME_SAMPLES;

    /// Sine frames at `dbfs` (mean square), continuing the phase.
    struct Sine {
        amp: f32,
        n: u64,
    }

    impl Sine {
        fn new(dbfs: f32) -> Self {
            Sine {
                amp: 32768.0 * 10f32.powf(dbfs / 20.0) * std::f32::consts::SQRT_2,
                n: 0,
            }
        }
        fn frame(&mut self) -> [i16; FRAME_SAMPLES] {
            let mut f = [0i16; FRAME_SAMPLES];
            for s in f.iter_mut() {
                let t = self.n as f32 / SAMPLE_RATE as f32;
                *s = (self.amp * (2.0 * std::f32::consts::PI * 440.0 * t).sin()).round() as i16;
                self.n += 1;
            }
            f
        }
    }

    /// Uniform noise frames, `amp` LSB peak (xorshift, fixed seed).
    struct Noise {
        amp: i32,
        state: u32,
    }

    impl Noise {
        fn new(amp: i32) -> Self {
            Noise {
                amp,
                state: 0x1234_5678,
            }
        }
        fn frame(&mut self) -> [i16; FRAME_SAMPLES] {
            let mut f = [0i16; FRAME_SAMPLES];
            for s in f.iter_mut() {
                self.state ^= self.state << 13;
                self.state ^= self.state >> 17;
                self.state ^= self.state << 5;
                *s = ((self.state % (2 * self.amp as u32 + 1)) as i32 - self.amp) as i16;
            }
            f
        }
    }

    fn silence() -> [i16; FRAME_SAMPLES] {
        [0; FRAME_SAMPLES]
    }

    #[test]
    fn starts_at_the_initial_gain_clamped() {
        assert_eq!(Agc::new(12.0).gain_db(), 12.0);
        assert_eq!(Agc::new(-6.0).gain_db(), MIN_GAIN_DB);
        assert_eq!(Agc::new(60.0).gain_db(), MAX_GAIN_DB);
    }

    #[test]
    fn quiet_speech_in_a_quiet_room_reaches_the_target() {
        // Far speech: 1 s words at -55 dBFS, 0.5 s of silence between.
        let mut agc = Agc::new(12.0);
        let mut word = Sine::new(-55.0);
        let mut out = Vec::new();
        for _ in 0..12 {
            out.clear();
            for _ in 0..FRAMES_PER_S {
                let mut f = word.frame();
                agc.process(&mut f);
                out.push(level_dbfs(&f));
            }
            for _ in 0..FRAMES_PER_S / 2 {
                agc.process(&mut silence());
            }
        }
        let last = out[out.len() / 2..].iter().sum::<f32>() / (out.len() / 2) as f32;
        assert!((last - TARGET_SPEECH_DBFS).abs() < 2.0, "{last} dBFS");
    }

    #[test]
    fn room_noise_is_lifted_no_higher_than_the_ceiling() {
        let mut noise = Noise::new(6);
        let input = level_dbfs(&noise.frame());
        assert!(input < NOISE_CEILING_DBFS - 6.0, "{input} dBFS");
        let mut agc = Agc::new(0.0);
        let mut f = silence();
        for _ in 0..20 * FRAMES_PER_S {
            f = noise.frame();
            agc.process(&mut f);
        }
        let out = level_dbfs(&f);
        assert!(out <= NOISE_CEILING_DBFS + 1.0, "{out} dBFS");
        assert!(
            out >= NOISE_CEILING_DBFS - 3.0,
            "noise should be lifted: {out} dBFS"
        );
    }

    #[test]
    fn digital_silence_stays_silent_and_bounded() {
        let mut agc = Agc::new(12.0);
        for _ in 0..60 * FRAMES_PER_S {
            let mut f = silence();
            agc.process(&mut f);
            assert_eq!(f, silence());
        }
        assert!(agc.gain_db() <= MAX_GAIN_DB);
        assert!(agc.gain_db() > 30.0, "{}", agc.gain_db());
    }

    #[test]
    fn a_loud_word_after_silence_never_clips() {
        let mut agc = Agc::new(12.0);
        for _ in 0..20 * FRAMES_PER_S {
            agc.process(&mut silence());
        }
        assert!(agc.gain_db() > 30.0);
        // Near speech, peaks at -7 dBFS: +30 dB would be far over full scale.
        let mut word = Sine::new(-10.0);
        for i in 0..2 * FRAMES_PER_S {
            let mut f = word.frame();
            agc.process(&mut f);
            let peak = f.iter().map(|s| s.unsigned_abs()).max().unwrap();
            assert!(
                f32::from(peak) <= PEAK_CEILING + 1.0,
                "frame {i}: peak {peak}"
            );
        }
        // Speech this loud needs no gain at all.
        assert_eq!(agc.gain_db(), MIN_GAIN_DB);
    }

    #[test]
    fn the_gain_moves_in_ramps_not_steps() {
        // A constant input shows the gain itself: it falls from +12 dB to
        // 0 dB (the noise ceiling) without a jump between samples.
        let mut agc = Agc::new(12.0);
        let mut prev: Option<i16> = None;
        for _ in 0..FRAMES_PER_S {
            let mut f = [100i16; FRAME_SAMPLES];
            agc.process(&mut f);
            for s in f {
                if let Some(p) = prev {
                    assert!((i32::from(s) - i32::from(p)).abs() <= 1, "{p} -> {s}");
                }
                prev = Some(s);
            }
        }
        assert_eq!(agc.gain_db(), MIN_GAIN_DB);
        assert_eq!(prev, Some(100));
    }

    #[test]
    fn pauses_between_words_do_not_pump_the_gain() {
        // Settle on words at -40 dBFS with 1 s pauses, then watch one pause.
        let mut agc = Agc::new(12.0);
        let mut word = Sine::new(-40.0);
        for _ in 0..15 {
            for _ in 0..FRAMES_PER_S {
                agc.process(&mut word.frame());
            }
            for _ in 0..FRAMES_PER_S {
                agc.process(&mut silence());
            }
        }
        for _ in 0..FRAMES_PER_S {
            agc.process(&mut word.frame());
        }
        let before = agc.gain_db();
        assert!(
            (before - (TARGET_SPEECH_DBFS + 40.0)).abs() < 2.0,
            "{before} dB"
        );
        for _ in 0..FRAMES_PER_S {
            agc.process(&mut silence());
        }
        let rise = agc.gain_db() - before;
        assert!(rise <= 1.5, "rose {rise} dB in a 1 s pause");
    }

    #[test]
    fn the_gain_comes_back_after_a_loud_talker_leaves() {
        let mut agc = Agc::new(12.0);
        for _ in 0..2 * FRAMES_PER_S {
            agc.process(&mut silence());
        }
        let mut loud = Sine::new(-20.0);
        for _ in 0..3 * FRAMES_PER_S {
            agc.process(&mut loud.frame());
        }
        assert_eq!(agc.gain_db(), MIN_GAIN_DB);
        for _ in 0..30 * FRAMES_PER_S {
            agc.process(&mut silence());
        }
        assert!(agc.gain_db() > 20.0, "{} dB", agc.gain_db());
    }
}
