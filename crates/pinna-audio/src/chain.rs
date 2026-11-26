//! The Echo Dot's speaker chain, the last stage before the codec: a mono mix
//! where the speaker plays one channel, the speaker EQ, extra gain at the top
//! of the volume range, and a look-ahead peak limiter that keeps the result
//! under full scale.

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
    mono: bool,
    eq: SpeakerEq,
    boost: f32,
    limiter: Limiter,
}

impl SpeakerChain {
    pub const CEILING_DBFS: f32 = -0.1;
    pub const RELEASE_MS: f32 = 200.0;

    /// `mono` mixes both channels into each, for a speaker that plays only
    /// one of them (`pinna_board::SpeakerChannels::Only`): the other channel
    /// would never sound, and the loopback lanes then match (ADR 0004).
    /// `highpass_hz` is the EQ's corner, the board's
    /// (`pinna_board::Audio::speaker_highpass_hz`).
    pub fn new(mono: bool, highpass_hz: f32) -> Self {
        SpeakerChain {
            mono,
            eq: SpeakerEq::new(highpass_hz),
            boost: 1.0,
            limiter: Limiter::new(Self::CEILING_DBFS, Self::RELEASE_MS, 48_000.0),
        }
    }

    /// The mono mix if set, the EQ, then the linear gain `boost` (ramped
    /// across the chunk from the previous chunk's), then the limiter.
    pub fn process(&mut self, bytes: &mut [u8], boost: f32) {
        let frames = (bytes.len() / 4).max(1) as f32;
        let start = self.boost;
        for (n, f) in bytes.chunks_exact_mut(4).enumerate() {
            let l = f32::from(i16::from_le_bytes([f[0], f[1]]));
            let r = f32::from(i16::from_le_bytes([f[2], f[3]]));
            let (l, r) = if self.mono {
                let m = 0.5 * (l + r);
                (m, m)
            } else {
                (l, r)
            };
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
        let mut chain = SpeakerChain::new(false, SpeakerEq::HIGHPASS_HZ);
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
        let mut chain = SpeakerChain::new(false, SpeakerEq::HIGHPASS_HZ);
        let mut eq = SpeakerEq::default();
        let mut a = stereo(&x);
        let mut b = stereo(&x);
        chain.process(&mut a, 1.0);
        eq.process(&mut b);
        let (a, b) = (left(&a), left(&b));
        for (p, q) in a[LOOKAHEAD..].iter().zip(&b) {
            assert!((p - q).abs() <= 1.0, "{p} vs {q}");
        }
    }

    /// A tone only on the left channel reaches both, at half its amplitude,
    /// and a right-only tone too: the speaker plays one channel, and nothing
    /// panned to the other is lost.
    #[test]
    fn mono_puts_both_channels_on_each() {
        let x = sine(1_000.0, 8_000.0, 9_600);
        let left_only: Vec<u8> = x
            .iter()
            .flat_map(|&s| [(s.round() as i16).to_le_bytes(), [0, 0]].concat())
            .collect();
        let mut chain = SpeakerChain::new(true, SpeakerEq::HIGHPASS_HZ);
        let mut b = left_only.clone();
        chain.process(&mut b, 1.0);
        let right: Vec<f32> = b
            .chunks_exact(4)
            .map(|f| f32::from(i16::from_le_bytes([f[2], f[3]])))
            .collect();
        assert_eq!(left(&b), right);
        let p = peak(&right[4_800..]);
        assert!((p - 4_000.0).abs() < 200.0, "peak {p}");
        // Without the mix, the right channel stays silent.
        let mut chain = SpeakerChain::new(false, SpeakerEq::HIGHPASS_HZ);
        let mut b = left_only;
        chain.process(&mut b, 1.0);
        assert!(b.chunks_exact(4).all(|f| f[2] == 0 && f[3] == 0));
    }

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|s| s * s).sum::<f32>() / x.len() as f32).sqrt()
    }

    /// RMS of a 60 Hz tone through a chain with this high-pass, after a
    /// second of settling.
    fn bass_through(highpass_hz: f32) -> f32 {
        let mut chain = SpeakerChain::new(false, highpass_hz);
        let mut b = stereo(&sine(60.0, 10_000.0, 96_000));
        chain.process(&mut b, 1.0);
        rms(&left(&b)[48_000..])
    }

    #[test]
    fn the_echo_2_keeps_the_woofer_range_the_echo_dot_cuts() {
        use pinna_board::{ECHO_2, ECHO_DOT_2};
        let full = 10_000.0 / 2f32.sqrt();
        let echo_2 = bass_through(ECHO_2.audio.speaker_highpass_hz);
        let dot = bass_through(ECHO_DOT_2.audio.speaker_highpass_hz);
        // 60 Hz is within 1 dB on the Echo (2nd gen), more than 10 dB down
        // on the Echo Dot (2nd gen).
        assert!(echo_2 > 0.89 * full, "Echo 2: 60 Hz rms {echo_2}");
        assert!(dot < 0.32 * full, "Echo Dot: 60 Hz rms {dot}");
    }
}
