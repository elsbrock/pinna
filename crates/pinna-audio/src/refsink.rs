//! Speaker EQ for the Echo Dot's speaker chain, and the biquad it is built
//! from. The echo canceller no longer gets a reference from here: it reads
//! what the speaker plays from the microphones' loopback lanes (ADR 0004).

/// Second-order filter (RBJ cookbook), one per channel.
#[derive(Clone, Debug)]
pub struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    x1: f32,
    x2: f32,
    y1: f32,
    y2: f32,
}

impl Biquad {
    pub fn highpass(rate: f32, freq: f32, q: f32) -> Self {
        let w0 = 2.0 * std::f32::consts::PI * freq / rate;
        let (sin, cos) = w0.sin_cos();
        let alpha = sin / (2.0 * q);
        let a0 = 1.0 + alpha;
        Self {
            b0: (1.0 + cos) / 2.0 / a0,
            b1: -(1.0 + cos) / a0,
            b2: (1.0 + cos) / 2.0 / a0,
            a1: -2.0 * cos / a0,
            a2: (1.0 - alpha) / a0,
            x1: 0.0,
            x2: 0.0,
            y1: 0.0,
            y2: 0.0,
        }
    }

    /// Band-pass with 0 dB gain at `freq`.
    pub fn bandpass(rate: f32, freq: f32, q: f32) -> Self {
        let w0 = 2.0 * std::f32::consts::PI * freq / rate;
        let (sin, cos) = w0.sin_cos();
        let alpha = sin / (2.0 * q);
        let a0 = 1.0 + alpha;
        Self {
            b0: alpha / a0,
            b1: 0.0,
            b2: -alpha / a0,
            a1: -2.0 * cos / a0,
            a2: (1.0 - alpha) / a0,
            x1: 0.0,
            x2: 0.0,
            y1: 0.0,
            y2: 0.0,
        }
    }

    pub fn process(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.b1 * self.x1 + self.b2 * self.x2
            - self.a1 * self.y1
            - self.a2 * self.y2;
        self.x2 = self.x1;
        self.x1 = x;
        self.y2 = self.y1;
        self.y1 = y;
        y
    }
}

/// Speaker EQ: keep energy the 40 mm driver cannot reproduce out of it.
#[derive(Clone, Debug)]
pub struct SpeakerEq {
    l: Biquad,
    r: Biquad,
}

impl SpeakerEq {
    pub const HIGHPASS_HZ: f32 = 150.0;

    pub fn new() -> Self {
        let hp = Biquad::highpass(48_000.0, Self::HIGHPASS_HZ, std::f32::consts::FRAC_1_SQRT_2);
        Self {
            l: hp.clone(),
            r: hp,
        }
    }

    /// One stereo frame, unclipped.
    pub fn process_frame(&mut self, l: f32, r: f32) -> (f32, f32) {
        (self.l.process(l), self.r.process(r))
    }

    /// In place on interleaved S16LE stereo bytes.
    pub fn process(&mut self, bytes: &mut [u8]) {
        for f in bytes.chunks_exact_mut(4) {
            let l = f32::from(i16::from_le_bytes([f[0], f[1]]));
            let r = f32::from(i16::from_le_bytes([f[2], f[3]]));
            let (l, r) = self.process_frame(l, r);
            let l = l.round().clamp(-32768.0, 32767.0) as i16;
            let r = r.round().clamp(-32768.0, 32767.0) as i16;
            f[..2].copy_from_slice(&l.to_le_bytes());
            f[2..].copy_from_slice(&r.to_le_bytes());
        }
    }
}

impl Default for SpeakerEq {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stereo_tone(hz: f32, frames: usize, amp: f32) -> Vec<u8> {
        let mut v = Vec::with_capacity(frames * 4);
        for i in 0..frames {
            let s = (amp * (2.0 * std::f32::consts::PI * hz * i as f32 / 48_000.0).sin()) as i16;
            v.extend_from_slice(&s.to_le_bytes());
            v.extend_from_slice(&s.to_le_bytes());
        }
        v
    }

    fn rms(b: &[u8], skip: usize) -> f32 {
        let s: Vec<f32> = b
            .chunks_exact(2)
            .skip(skip)
            .map(|c| f32::from(i16::from_le_bytes([c[0], c[1]])))
            .collect();
        (s.iter().map(|x| x * x).sum::<f32>() / s.len() as f32).sqrt()
    }

    #[test]
    fn eq_cuts_bass_and_passes_mids() {
        let mut eq = SpeakerEq::new();
        let mut low = stereo_tone(50.0, 48_000, 10_000.0);
        eq.process(&mut low);
        let mut eq = SpeakerEq::new();
        let mut mid = stereo_tone(1_000.0, 48_000, 10_000.0);
        eq.process(&mut mid);
        let (l, m) = (rms(&low, 4_800), rms(&mid, 4_800));
        assert!(l < 0.15 * 7071.0, "50 Hz rms {l}");
        assert!((m - 7071.0).abs() < 150.0, "1 kHz rms {m}");
    }
}
