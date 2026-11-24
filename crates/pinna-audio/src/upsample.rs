//! Upsampling by two for a speaker PCM that runs at twice Pinna's rate
//! (ADR 0032): the Echo Dot (5th gen)'s playback PCM is fixed at 96 kHz by
//! its eTDM2 clock, while Pinna mixes and times everything at 48 kHz.
//!
//! A half-band FIR in its polyphase form, one per channel. The filter is a
//! Kaiser-windowed sinc of 127 taps, cut at a quarter of the output rate (24
//! kHz). Half of a half-band's taps are zero and the centre one is 1, so
//! every other output is the input delayed, and the rest is a 64-tap sum.
//! The transition runs from about 22 kHz to 26 kHz and the image above it is
//! at least 60 dB down (see the tests).

/// Group delay in output frames: the filter is symmetric over 127 taps.
pub const GROUP_DELAY: usize = 63;

/// Taps of the sum branch; the other branch is the single centre tap.
const BRANCH: usize = 64;
/// The input frame the centre tap reads: `(GROUP_DELAY - 1) / 2` behind.
const CENTRE: usize = (GROUP_DELAY - 1) / 2;
const BETA: f64 = 8.0;
const CHANNELS: usize = 2;

/// Zeroth-order modified Bessel function, by its power series.
pub(crate) fn bessel_i0(x: f64) -> f64 {
    let (mut sum, mut term) = (1.0, 1.0);
    for k in 1..60 {
        term *= (x / (2.0 * f64::from(k))).powi(2);
        sum += term;
        if term < sum * 1e-17 {
            break;
        }
    }
    sum
}

/// The sum branch's taps, newest input first, with unity gain at DC.
fn branch_taps() -> [f32; BRANCH] {
    let c = GROUP_DELAY as f64;
    let mut taps = [0f64; BRANCH];
    for (j, t) in taps.iter_mut().enumerate() {
        // Tap `n = 2j` of the 127; its offset from the centre is odd.
        let m = (2 * j) as f64 - c;
        let sinc = (std::f64::consts::FRAC_PI_2 * m).sin() / (std::f64::consts::PI * m);
        let window = bessel_i0(BETA * (1.0 - (m / c).powi(2)).max(0.0).sqrt()) / bessel_i0(BETA);
        *t = sinc * window;
    }
    let sum: f64 = taps.iter().sum();
    let mut out = [0f32; BRANCH];
    for (o, t) in out.iter_mut().zip(taps) {
        *o = (t / sum) as f32;
    }
    out
}

/// Interleaved 16-bit stereo, doubled in rate.
#[derive(Clone, Debug)]
pub struct Upsampler2x {
    taps: [f32; BRANCH],
    /// Per channel the last `BRANCH` inputs, written twice so that the
    /// newest-first window is one slice.
    hist: [Vec<f32>; CHANNELS],
    pos: usize,
}

impl Default for Upsampler2x {
    fn default() -> Self {
        Self::new()
    }
}

impl Upsampler2x {
    pub fn new() -> Self {
        Upsampler2x {
            taps: branch_taps(),
            hist: [vec![0.0; 2 * BRANCH], vec![0.0; 2 * BRANCH]],
            pos: 0,
        }
    }

    /// Append the upsampled `input` (S16_LE stereo) to `out`: two frames for
    /// each one, saturated. A trailing partial frame is ignored.
    pub fn process(&mut self, input: &[u8], out: &mut Vec<u8>) {
        out.reserve(input.len() * 2);
        for frame in input.chunks_exact(CHANNELS * 2) {
            self.pos = (self.pos + BRANCH - 1) % BRANCH;
            let mut even = [0i16; CHANNELS];
            let mut odd = [0i16; CHANNELS];
            for (c, h) in self.hist.iter_mut().enumerate() {
                let x = f32::from(i16::from_le_bytes([frame[2 * c], frame[2 * c + 1]]));
                h[self.pos] = x;
                h[self.pos + BRANCH] = x;
                let win = &h[self.pos..self.pos + BRANCH];
                let acc: f32 = self.taps.iter().zip(win).map(|(t, x)| t * x).sum();
                even[c] = acc.round().clamp(-32768.0, 32767.0) as i16;
                odd[c] = win[CENTRE] as i16;
            }
            for s in even.into_iter().chain(odd) {
                out.extend_from_slice(&s.to_le_bytes());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: f64 = 48_000.0;

    fn tone(freq: f64, amp: f64, frames: usize) -> Vec<u8> {
        let mut b = Vec::new();
        for n in 0..frames {
            let s =
                (amp * (2.0 * std::f64::consts::PI * freq * n as f64 / RATE).sin()).round() as i16;
            b.extend_from_slice(&s.to_le_bytes());
            b.extend_from_slice(&s.to_le_bytes());
        }
        b
    }

    /// The left channel of interleaved output.
    fn left(out: &[u8]) -> Vec<f64> {
        out.chunks_exact(4)
            .map(|f| f64::from(i16::from_le_bytes([f[0], f[1]])))
            .collect()
    }

    /// Amplitude of `freq` in `x` sampled at `rate`, by correlation.
    fn amplitude(x: &[f64], freq: f64, rate: f64) -> f64 {
        let (mut re, mut im) = (0.0, 0.0);
        for (n, s) in x.iter().enumerate() {
            let w = 2.0 * std::f64::consts::PI * freq * n as f64 / rate;
            re += s * w.cos();
            im += s * w.sin();
        }
        2.0 * (re * re + im * im).sqrt() / x.len() as f64
    }

    fn run(freq: f64) -> Vec<f64> {
        let mut up = Upsampler2x::new();
        let mut out = Vec::new();
        up.process(&tone(freq, 10_000.0, 9_600), &mut out);
        assert_eq!(out.len(), 9_600 * 8);
        // Skip the filter's start-up.
        left(&out)[4_000..].to_vec()
    }

    #[test]
    fn a_tone_passes_at_unity_gain_and_its_image_is_below_60_db() {
        let y = run(1_000.0);
        let gain = amplitude(&y, 1_000.0, 2.0 * RATE) / 10_000.0;
        assert!((gain - 1.0).abs() < 0.005, "gain {gain}");
        let image = amplitude(&y, 47_000.0, 2.0 * RATE) / 10_000.0;
        assert!(20.0 * image.log10() < -60.0, "image {image}");
    }

    #[test]
    fn the_band_to_22_khz_passes_and_the_image_above_26_khz_does_not() {
        let y = run(22_000.0);
        let gain = amplitude(&y, 22_000.0, 2.0 * RATE) / 10_000.0;
        assert!((gain - 1.0).abs() < 0.03, "gain {gain}");
        // The image of 20 kHz is at 28 kHz, just past the transition.
        let y = run(20_000.0);
        let image = amplitude(&y, 28_000.0, 2.0 * RATE) / 10_000.0;
        assert!(20.0 * image.log10() < -60.0, "image {image}");
    }

    #[test]
    fn dc_passes_and_loud_input_saturates_instead_of_wrapping() {
        let mut up = Upsampler2x::new();
        let mut out = Vec::new();
        let dc: Vec<u8> = (0..400).flat_map(|_| 20_000i16.to_le_bytes()).collect();
        let dc: Vec<u8> = dc.chunks(2).flat_map(|s| [s, s].concat()).collect();
        up.process(&dc, &mut out);
        let y = left(&out);
        assert!(y[600..].iter().all(|v| (v - 20_000.0).abs() < 2.0));
        // A full-scale square wave overshoots (Gibbs) but never wraps.
        let mut up = Upsampler2x::new();
        let mut out = Vec::new();
        let sq: Vec<u8> = (0..800)
            .flat_map(|n| {
                let s = if (n / 8) % 2 == 0 { i16::MAX } else { i16::MIN };
                [s, s].into_iter().flat_map(i16::to_le_bytes)
            })
            .collect();
        up.process(&sq, &mut out);
        let y = left(&out);
        assert!(y.iter().any(|&v| v == 32767.0 || v == -32768.0));
    }

    #[test]
    fn the_odd_outputs_are_the_input_delayed_by_the_group_delay() {
        let mut up = Upsampler2x::new();
        let mut out = Vec::new();
        let mut input = vec![0u8; 4 * 200];
        input[4 * 10..4 * 10 + 2].copy_from_slice(&5_000i16.to_le_bytes());
        up.process(&input, &mut out);
        let y = left(&out);
        // An impulse at input frame 10 is at output frame 20 + GROUP_DELAY.
        assert_eq!(y[20 + GROUP_DELAY], 5_000.0);
    }

    #[test]
    fn a_partial_frame_is_ignored_and_state_carries_across_calls() {
        let input = tone(1_000.0, 8_000.0, 1_000);
        let mut whole = Vec::new();
        Upsampler2x::new().process(&input, &mut whole);
        let mut parts = Vec::new();
        let mut up = Upsampler2x::new();
        let (a, b) = input.split_at(4 * 333);
        up.process(a, &mut parts);
        up.process(b, &mut parts);
        assert_eq!(whole, parts);
        let mut out = Vec::new();
        up.process(&[1, 2, 3], &mut out);
        assert!(out.is_empty());
    }
}
