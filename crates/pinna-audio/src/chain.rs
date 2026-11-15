//! The Echo Dot's speaker chain, the last stage before the codec: the speaker
//! EQ, extra gain at the top of the volume range, and a look-ahead peak
//! limiter that keeps the result under full scale.

use crate::refsink::SpeakerEq;

/// Look-ahead length: 1.3 ms at 48 kHz. The output is delayed by this much.
pub const LOOKAHEAD: usize = 64;
/// 10 ms. Longer than the look-ahead, so inside one look-ahead window the gain
/// only rises (release) and then falls (new peaks): its minimum is at an end.
const HOLD: usize = 480;

/// Look-ahead peak limiter on stereo frames (s16 scale, one gain for both
/// channels).
///
/// Each frame's target gain is `ceiling / peak`, or 1 below the ceiling. The
/// gain drops at once, holds, then releases exponentially. The output is the
/// frame from [`LOOKAHEAD`] frames ago, scaled by the average over the
/// look-ahead of the gain's running minimum. Every value in that average is at
/// most the delayed frame's target, so peaks are held at the ceiling without
/// clipping them.
pub struct Limiter {
    ceiling: f32,
    release: f32,
    gain: f32,
    hold: usize,
    pos: usize,
    delay: [(f32, f32); LOOKAHEAD],
    gains: [f32; LOOKAHEAD],
    window: [f32; LOOKAHEAD],
    window_sum: f64,
}

impl Limiter {
    pub fn new(ceiling_dbfs: f32, release_ms: f32, rate: f32) -> Self {
        Limiter {
            ceiling: 32768.0 * 10f32.powf(ceiling_dbfs / 20.0),
            release: (-1000.0 / (rate * release_ms)).exp(),
            gain: 1.0,
            hold: 0,
            pos: 0,
            delay: [(0.0, 0.0); LOOKAHEAD],
            gains: [1.0; LOOKAHEAD],
            window: [1.0; LOOKAHEAD],
            window_sum: LOOKAHEAD as f64,
        }
    }

    pub fn process(&mut self, l: f32, r: f32) -> (f32, f32) {
        let peak = l.abs().max(r.abs());
        let target = if peak > self.ceiling {
            self.ceiling / peak
        } else {
            1.0
        };
        if target <= self.gain {
            self.gain = target;
            self.hold = HOLD;
        } else if self.hold > 0 {
            self.hold -= 1;
        } else {
            self.gain = (1.0 - (1.0 - self.gain) * self.release).min(target);
        }
        let i = self.pos;
        // Minimum over the look-ahead window: one of its ends (see HOLD).
        let min = self.gains[i].min(self.gain);
        self.gains[i] = self.gain;
        self.window_sum += f64::from(min) - f64::from(self.window[i]);
        self.window[i] = min;
        let g = (self.window_sum / LOOKAHEAD as f64) as f32;
        let (dl, dr) = self.delay[i];
        self.delay[i] = (l, r);
        self.pos = (i + 1) % LOOKAHEAD;
        let c = self.ceiling;
        ((dl * g).clamp(-c, c), (dr * g).clamp(-c, c))
    }
}

/// The Echo Dot speaker chain, in place on s16le stereo chunks.
pub struct SpeakerChain {
    eq: SpeakerEq,
    boost: f32,
    limiter: Limiter,
}

impl SpeakerChain {
    pub const CEILING_DBFS: f32 = -0.1;
    pub const RELEASE_MS: f32 = 200.0;

    pub fn new() -> Self {
        SpeakerChain {
            eq: SpeakerEq::new(),
            boost: 1.0,
            limiter: Limiter::new(Self::CEILING_DBFS, Self::RELEASE_MS, 48_000.0),
        }
    }

    /// EQ, then the linear gain `boost` (ramped across the chunk from the
    /// previous chunk's), then the limiter.
    pub fn process(&mut self, bytes: &mut [u8], boost: f32) {
        let frames = (bytes.len() / 4).max(1) as f32;
        let start = self.boost;
        for (n, f) in bytes.chunks_exact_mut(4).enumerate() {
            let l = f32::from(i16::from_le_bytes([f[0], f[1]]));
            let r = f32::from(i16::from_le_bytes([f[2], f[3]]));
            let (l, r) = self.eq.process_frame(l, r);
            let g = start + (boost - start) * (n as f32 / frames);
            let (l, r) = self.limiter.process(l * g, r * g);
            let l = l.round().clamp(-32768.0, 32767.0) as i16;
            let r = r.round().clamp(-32768.0, 32767.0) as i16;
            f[..2].copy_from_slice(&l.to_le_bytes());
            f[2..].copy_from_slice(&r.to_le_bytes());
        }
        self.boost = boost;
    }
}

impl Default for SpeakerChain {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: f32 = 48_000.0;

    fn sine(hz: f32, amp: f32, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| amp * (std::f32::consts::TAU * hz * i as f32 / RATE).sin())
            .collect()
    }

    fn limit(lim: &mut Limiter, x: &[f32]) -> Vec<f32> {
        x.iter().map(|&s| lim.process(s, s).0).collect()
    }

    fn peak(x: &[f32]) -> f32 {
        x.iter().fold(0.0, |m, s| m.max(s.abs()))
    }

    #[test]
    fn quiet_signal_passes_unchanged_after_the_lookahead() {
        let mut lim = Limiter::new(-0.1, 200.0, RATE);
        let x = sine(1_000.0, 20_000.0, 4_800);
        let y = limit(&mut lim, &x);
        assert!(y[..LOOKAHEAD].iter().all(|&s| s == 0.0));
        for (a, b) in x.iter().zip(&y[LOOKAHEAD..]) {
            assert!((a - b).abs() < 1e-3, "{a} vs {b}");
        }
    }

    #[test]
    fn loud_signal_is_held_at_the_ceiling() {
        let mut lim = Limiter::new(-0.1, 200.0, RATE);
        let ceiling = 32768.0 * 10f32.powf(-0.1 / 20.0);
        // +8 dB over full scale, a step from silence (worst case for attack).
        let mut x = vec![0.0; 480];
        x.extend(sine(1_000.0, 32768.0 * 2.5, 48_000));
        let y = limit(&mut lim, &x);
        assert!(peak(&y) <= ceiling + 1e-3, "peak {}", peak(&y));
        // Steady state is limited, not crushed: close to the ceiling.
        let tail = peak(&y[24_000..]);
        assert!(tail > 0.95 * ceiling, "steady peak {tail}");
    }

    #[test]
    fn gain_recovers_after_a_burst() {
        let mut lim = Limiter::new(-0.1, 200.0, RATE);
        let mut x = sine(1_000.0, 32768.0 * 4.0, 4_800);
        x.extend(sine(1_000.0, 10_000.0, 96_000));
        let y = limit(&mut lim, &x);
        // A second after the burst the quiet tone is back to unity gain.
        let tail = peak(&y[y.len() - 4_800..]);
        assert!((tail - 10_000.0).abs() < 50.0, "tail peak {tail}");
    }

    fn stereo(x: &[f32]) -> Vec<u8> {
        x.iter()
            .flat_map(|&s| {
                let s = s.round() as i16;
                [s.to_le_bytes(), s.to_le_bytes()].concat()
            })
            .collect()
    }

    fn left(b: &[u8]) -> Vec<f32> {
        b.chunks_exact(4)
            .map(|f| f32::from(i16::from_le_bytes([f[0], f[1]])))
            .collect()
    }

    #[test]
    fn chain_boosts_and_never_clips() {
        // A 1 kHz tone at -6 dBFS boosted by +14.5 dB, in 20 ms chunks.
        let x = sine(1_000.0, 16_384.0, 48_000);
        let mut chain = SpeakerChain::new();
        let boost = 10f32.powf(14.54 / 20.0);
        let mut out = Vec::new();
        for c in x.chunks(960) {
            let mut b = stereo(c);
            chain.process(&mut b, boost);
            out.extend(left(&b));
        }
        let ceiling = 32768.0 * 10f32.powf(-0.1 / 20.0);
        assert!(peak(&out) <= ceiling.ceil(), "peak {}", peak(&out));
        assert!(peak(&out[24_000..]) > 0.95 * ceiling);
    }

    #[test]
    fn chain_at_unity_is_the_eq() {
        let x = sine(1_000.0, 8_000.0, 9_600);
        let mut chain = SpeakerChain::new();
        let mut eq = SpeakerEq::new();
        let mut a = stereo(&x);
        let mut b = stereo(&x);
        chain.process(&mut a, 1.0);
        eq.process(&mut b);
        let (a, b) = (left(&a), left(&b));
        for (p, q) in a[LOOKAHEAD..].iter().zip(&b) {
            assert!((p - q).abs() <= 1.0, "{p} vs {q}");
        }
    }
}
