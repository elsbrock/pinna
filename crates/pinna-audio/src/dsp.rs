//! Microphone-array front end: S24_3LE decode, per-mic calibration, mixdown,
//! 80 Hz high-pass and digital gain. A port of LibreEcho's voice_dsp.c and the
//! gain stage of micd.c. Beamforming / AEC would replace [`SimpleFrontEnd`]
//! behind the [`FrontEnd`] trait.

/// Capture sample rate.
pub const SAMPLE_RATE: u32 = 16_000;
/// Samples per output frame (20 ms).
pub const FRAME_SAMPLES: usize = 320;
/// Channels on the capture PCM (7 logical mics + 2 reference lanes).
pub const RAW_CHANNELS: usize = 9;
pub const LOGICAL_CHANNELS: usize = 7;
pub const BYTES_PER_SAMPLE: usize = 3;
pub const RAW_FRAME_BYTES: usize = RAW_CHANNELS * BYTES_PER_SAMPLE;
pub const MICCAL_UNITY: i32 = 16384;

/// The stock speech microphones: lanes 0, 1, 3, 4 (rear-left, rear-right,
/// front-right, front-left).
pub const MIC_MASK_BEAM: u8 = (1 << 0) | (1 << 1) | (1 << 3) | (1 << 4);
pub const MIC_MASK_ALL: u8 = (1 << LOGICAL_CHANNELS) - 1;

pub const DIGITAL_GAIN_UNITY: u32 = 100;
pub const DIGITAL_GAIN_DEFAULT: u32 = 400;
pub const DIGITAL_GAIN_MAX: u32 = 1600;

pub fn saturate_s16(v: i64) -> i16 {
    v.clamp(i64::from(i16::MIN), i64::from(i16::MAX)) as i16
}

/// 3 bytes little-endian, 16 valid bits left-justified: sign-extend 24 bits
/// then take the top 16.
pub fn unpack_s24_3le(b: [u8; 3]) -> i16 {
    let mut v = i32::from(b[0]) | i32::from(b[1]) << 8 | i32::from(b[2]) << 16;
    if v & 0x0080_0000 != 0 {
        v -= 0x0100_0000;
    }
    saturate_s16(i64::from(v >> 8))
}

fn divide_rounded(num: i64, den: i64) -> i64 {
    if den <= 0 {
        0
    } else if num >= 0 {
        (num + den / 2) / den
    } else {
        -((-num + den / 2) / den)
    }
}

/// Per-mic Q14 calibration (multiply, round half away from zero, saturate).
/// Invalid factors (<= 0 or > 65535) are treated as unity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Calibration {
    q14: [u16; LOGICAL_CHANNELS],
}

impl Calibration {
    pub fn new(q14: [i32; LOGICAL_CHANNELS]) -> Self {
        let mut out = [MICCAL_UNITY as u16; LOGICAL_CHANNELS];
        for (o, g) in out.iter_mut().zip(q14) {
            if g > 0 && g <= i32::from(u16::MAX) {
                *o = g as u16;
            }
        }
        Calibration { q14: out }
    }

    pub fn unity() -> Self {
        Self::new([MICCAL_UNITY; LOGICAL_CHANNELS])
    }

    pub fn apply(&self, channel: usize, sample: i16) -> i16 {
        let scaled = divide_rounded(
            i64::from(sample) * i64::from(self.q14[channel]),
            i64::from(MICCAL_UNITY),
        );
        saturate_s16(scaled)
    }
}

/// Rounded mean of the channels selected by `mask`; 0 if none selected.
pub fn mix_selected(logical: &[i16; LOGICAL_CHANNELS], mask: u8) -> i16 {
    let mut sum = 0i64;
    let mut count = 0i64;
    for (c, &s) in logical.iter().enumerate() {
        if mask & (1 << c) != 0 {
            sum += i64::from(s);
            count += 1;
        }
    }
    if count == 0 {
        return 0;
    }
    saturate_s16(divide_rounded(sum, count))
}

/// First-order 80 Hz high-pass at 16 kHz, integer-only:
/// `y[n] = 0.9691 * (y[n-1] + x[n] - x[n-1])`, Q15 coefficient, truncating
/// toward zero so the state decays fully after a DC step.
#[derive(Clone, Debug, Default)]
pub struct HighPass {
    prev_in: i32,
    prev_out: i32,
}

impl HighPass {
    const COEFF_Q15: i64 = 31755;

    pub fn new() -> Self {
        Self::default()
    }

    pub fn sample(&mut self, input: i16) -> i16 {
        let diff = i64::from(self.prev_out) + i64::from(input) - i64::from(self.prev_in);
        let out = ((diff * Self::COEFF_Q15) / 32768) as i32;
        self.prev_in = i32::from(input);
        self.prev_out = out;
        saturate_s16(i64::from(out))
    }
}

/// Digital gain in hundredths (100 = unity), truncating, saturating.
pub fn apply_digital_gain(samples: &mut [i16], gain_hundredths: u32) {
    if gain_hundredths == DIGITAL_GAIN_UNITY {
        return;
    }
    for s in samples {
        let scaled = i64::from(*s) * i64::from(gain_hundredths) / i64::from(DIGITAL_GAIN_UNITY);
        *s = saturate_s16(scaled);
    }
}

/// Turns raw multi-channel capture bytes into mono 16 kHz samples.
pub trait FrontEnd: Send {
    /// Consume whole raw frames from `raw` (a multiple of [`RAW_FRAME_BYTES`])
    /// and append mono samples to `out`.
    fn process(&mut self, raw: &[u8], out: &mut Vec<i16>);
}

#[derive(Clone, Debug)]
pub struct FrontEndConfig {
    pub calibration: Calibration,
    /// Bit n selects logical mic n (0-5 ring, 6 centre).
    pub mic_mask: u8,
    /// In hundredths; clamped to 100..=1600.
    pub digital_gain: u32,
    pub highpass: bool,
}

impl Default for FrontEndConfig {
    fn default() -> Self {
        FrontEndConfig {
            calibration: Calibration::unity(),
            mic_mask: MIC_MASK_BEAM,
            digital_gain: DIGITAL_GAIN_DEFAULT,
            highpass: true,
        }
    }
}

/// Calibrate -> mean mixdown -> high-pass -> digital gain.
pub struct SimpleFrontEnd {
    cfg: FrontEndConfig,
    hpf: HighPass,
}

impl SimpleFrontEnd {
    pub fn new(mut cfg: FrontEndConfig) -> Self {
        cfg.digital_gain = cfg.digital_gain.clamp(DIGITAL_GAIN_UNITY, DIGITAL_GAIN_MAX);
        SimpleFrontEnd {
            cfg,
            hpf: HighPass::new(),
        }
    }
}

impl FrontEnd for SimpleFrontEnd {
    fn process(&mut self, raw: &[u8], out: &mut Vec<i16>) {
        let start = out.len();
        for frame in raw.chunks_exact(RAW_FRAME_BYTES) {
            let mut logical = [0i16; LOGICAL_CHANNELS];
            for (c, l) in logical.iter_mut().enumerate() {
                let o = c * BYTES_PER_SAMPLE;
                let s = unpack_s24_3le([frame[o], frame[o + 1], frame[o + 2]]);
                *l = self.cfg.calibration.apply(c, s);
            }
            let mixed = mix_selected(&logical, self.cfg.mic_mask);
            out.push(if self.cfg.highpass {
                self.hpf.sample(mixed)
            } else {
                mixed
            });
        }
        apply_digital_gain(&mut out[start..], self.cfg.digital_gain);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The front end decodes one capture format. A board whose capture PCM
    /// differs (ADR 0032) fails here until the front end takes it from the
    /// board.
    #[test]
    fn every_board_captures_what_the_front_end_decodes() {
        for m in pinna_board::Model::ALL {
            let b = m.board();
            assert_eq!(b.audio.capture.rate, SAMPLE_RATE, "{m:?}");
            assert_eq!(b.audio.capture.channels, RAW_CHANNELS, "{m:?}");
            assert_eq!(b.audio.capture.format, pinna_board::SampleFormat::S24_3Le);
            assert_eq!(BYTES_PER_SAMPLE, 3);
            assert_eq!(b.mics.count, LOGICAL_CHANNELS, "{m:?}");
        }
    }

    #[test]
    fn s24_takes_top_16_bits() {
        assert_eq!(unpack_s24_3le([0x00, 0x34, 0x12]), 0x1234);
        assert_eq!(unpack_s24_3le([0xff, 0x34, 0x12]), 0x1234); // low byte dropped
        assert_eq!(unpack_s24_3le([0x00, 0x00, 0x80]), i16::MIN);
        assert_eq!(unpack_s24_3le([0x00, 0xff, 0xff]), -1);
        assert_eq!(unpack_s24_3le([0x00, 0x00, 0x00]), 0);
        assert_eq!(unpack_s24_3le([0xff, 0xff, 0x7f]), 0x7fff);
        // Negative low byte only: arithmetic shift rounds toward -inf.
        assert_eq!(unpack_s24_3le([0x01, 0x00, 0x80]), i16::MIN);
        assert_eq!(unpack_s24_3le([0xff, 0xff, 0xff]), -1);
    }

    #[test]
    fn calibration_q14() {
        let mut q = [MICCAL_UNITY; 7];
        q[0] = 8192; // x0.5
        q[1] = 32768; // x2
        q[2] = 0; // invalid -> unity
        q[3] = -5; // invalid -> unity
        q[4] = 70000; // invalid -> unity
        let c = Calibration::new(q);
        assert_eq!(c.apply(0, 1001), 501); // 500.5 rounds away from zero
        assert_eq!(c.apply(0, -1001), -501);
        assert_eq!(c.apply(1, 20000), i16::MAX);
        assert_eq!(c.apply(1, -20000), i16::MIN);
        for ch in 2..7 {
            assert_eq!(c.apply(ch, 1234), 1234);
        }
    }

    #[test]
    fn mix_means_selected() {
        let l = [100, 200, 9999, 300, 400, -9999, 0];
        assert_eq!(mix_selected(&l, MIC_MASK_BEAM), 250);
        assert_eq!(mix_selected(&l, 0), 0);
        assert_eq!(mix_selected(&l, 1 << 2), 9999);
        // Rounded: (1 + 2) / 2 = 1.5 -> 2 ; (-1 - 2) / 2 -> -2
        assert_eq!(mix_selected(&[1, 2, 0, 0, 0, 0, 0], 0b11), 2);
        assert_eq!(mix_selected(&[-1, -2, 0, 0, 0, 0, 0], 0b11), -2);
        let all = [100, 200, 300, 400, 500, 600, 700];
        assert_eq!(mix_selected(&all, MIC_MASK_ALL), 400);
    }

    #[test]
    fn highpass_matches_reference_recurrence() {
        let mut h = HighPass::new();
        // First step: (0 + 1000 - 0) * 31755 / 32768 = 969 (truncated).
        assert_eq!(h.sample(1000), 969);
        // Second: (969 + 1000 - 1000) * 31755 / 32768 = 939.
        assert_eq!(h.sample(1000), 939);
    }

    #[test]
    fn highpass_removes_dc_completely() {
        let mut h = HighPass::new();
        let mut last = 0;
        for _ in 0..2000 {
            last = h.sample(5000);
        }
        assert_eq!(last, 0);
    }

    #[test]
    fn highpass_passes_1khz_and_attenuates_10hz() {
        fn amplitude(freq: f64) -> f64 {
            let mut h = HighPass::new();
            let mut peak = 0f64;
            for n in 0..32000 {
                let x = (10000.0 * (2.0 * std::f64::consts::PI * freq * n as f64 / 16000.0).sin())
                    as i16;
                let y = f64::from(h.sample(x));
                if n > 16000 {
                    peak = peak.max(y.abs());
                }
            }
            peak
        }
        assert!(amplitude(1000.0) > 9500.0);
        assert!(amplitude(10.0) < 1500.0);
    }

    #[test]
    fn gain_saturates_and_truncates() {
        let mut s = [10000, -10000, 100, -3, 7];
        apply_digital_gain(&mut s, 400);
        assert_eq!(s, [i16::MAX, i16::MIN, 400, -12, 28]);
        let mut s = [5, -5];
        apply_digital_gain(&mut s, 150);
        assert_eq!(s, [7, -7]); // 7.5 truncates toward zero
        let mut s = [123];
        apply_digital_gain(&mut s, DIGITAL_GAIN_UNITY);
        assert_eq!(s, [123]);
    }

    fn raw_frame(vals: [i16; 9]) -> Vec<u8> {
        let mut v = Vec::new();
        for s in vals {
            v.push(0x55); // garbage low byte
            v.extend_from_slice(&s.to_le_bytes());
        }
        v
    }

    #[test]
    fn front_end_end_to_end() {
        let mut fe = SimpleFrontEnd::new(FrontEndConfig {
            highpass: false,
            digital_gain: 100,
            ..Default::default()
        });
        let mut raw = raw_frame([100, 200, 7000, 300, 400, 7000, 7000, 1, 2]);
        raw.extend(raw_frame([-100, -200, 0, -300, -400, 0, 0, 0, 0]));
        let mut out = Vec::new();
        fe.process(&raw, &mut out);
        assert_eq!(out, vec![250, -250]);
    }

    #[test]
    fn front_end_ignores_partial_trailing_frame() {
        let mut fe = SimpleFrontEnd::new(FrontEndConfig::default());
        let mut raw = raw_frame([0; 9]);
        raw.extend_from_slice(&[1, 2, 3]);
        let mut out = Vec::new();
        fe.process(&raw, &mut out);
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn front_end_default_gain_is_4x_and_clamped() {
        let mut fe = SimpleFrontEnd::new(FrontEndConfig {
            highpass: false,
            mic_mask: 1,
            ..Default::default()
        });
        let mut out = Vec::new();
        fe.process(&raw_frame([1000, 0, 0, 0, 0, 0, 0, 0, 0]), &mut out);
        assert_eq!(out, vec![4000]);
        let fe = SimpleFrontEnd::new(FrontEndConfig {
            digital_gain: 99999,
            ..Default::default()
        });
        assert_eq!(fe.cfg.digital_gain, DIGITAL_GAIN_MAX);
    }
}
