//! Microphone-array front end: decode (S24_3LE or S32_LE), per-mic
//! calibration, mixdown, decimation to 16 kHz when the PCM runs faster,
//! 80 Hz high-pass and digital gain. A port of LibreEcho's voice_dsp.c and the
//! gain stage of micd.c. Beamforming / AEC would replace [`SimpleFrontEnd`]
//! behind the [`FrontEnd`] trait.

/// Capture sample rate.
pub const SAMPLE_RATE: u32 = 16_000;
/// Samples per output frame (20 ms).
pub const FRAME_SAMPLES: usize = 320;
/// Channels on the MT8163's capture PCM (7 logical mics + 2 reference lanes).
pub const RAW_CHANNELS: usize = 9;
pub const LOGICAL_CHANNELS: usize = 7;
/// Bytes per sample on the MT8163's capture PCM.
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

/// 4 bytes little-endian, the 16 bits that count left-aligned: the top 16.
pub fn unpack_s32le(b: [u8; 4]) -> i16 {
    (i32::from_le_bytes(b) >> 16) as i16
}

/// How the capture PCM stores a sample.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SampleFormat {
    /// 24 bits in 3 bytes (the MT8163's FPGA).
    S24_3Le,
    /// 32 bits, the samples left-aligned (the MT8519's converter).
    S32Le,
}

impl SampleFormat {
    pub const fn bytes(self) -> usize {
        match self {
            SampleFormat::S24_3Le => 3,
            SampleFormat::S32Le => 4,
        }
    }

    /// The format as `arecord -f` names it.
    pub const fn alsa_name(self) -> &'static str {
        match self {
            SampleFormat::S24_3Le => "S24_3LE",
            SampleFormat::S32Le => "S32_LE",
        }
    }

    /// The top 16 bits of the sample at the start of `b`.
    fn unpack(self, b: &[u8]) -> i16 {
        match self {
            SampleFormat::S24_3Le => unpack_s24_3le([b[0], b[1], b[2]]),
            SampleFormat::S32Le => unpack_s32le([b[0], b[1], b[2], b[3]]),
        }
    }
}

/// What the capture PCM delivers: its rate, lanes and sample format, and
/// which lanes are the microphones. The default is the MT8163's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaptureFormat {
    pub rate: u32,
    pub channels: usize,
    pub sample: SampleFormat,
    /// Lane of logical microphone `i`, at most [`LOGICAL_CHANNELS`].
    pub mic_lanes: Vec<usize>,
}

impl Default for CaptureFormat {
    fn default() -> Self {
        CaptureFormat {
            rate: SAMPLE_RATE,
            channels: RAW_CHANNELS,
            sample: SampleFormat::S24_3Le,
            mic_lanes: (0..LOGICAL_CHANNELS).collect(),
        }
    }
}

impl CaptureFormat {
    /// Bytes in one frame of all lanes.
    pub fn frame_bytes(&self) -> usize {
        self.channels * self.sample.bytes()
    }

    /// Input frames per output sample at [`SAMPLE_RATE`].
    pub fn decimation(&self) -> usize {
        (self.rate / SAMPLE_RATE) as usize
    }

    /// Whether the front end can decode it.
    pub fn check(&self) -> Result<(), String> {
        if self.rate < SAMPLE_RATE || !self.rate.is_multiple_of(SAMPLE_RATE) {
            return Err(format!(
                "the capture rate {} Hz must be a multiple of {SAMPLE_RATE} Hz",
                self.rate
            ));
        }
        if self.mic_lanes.len() > LOGICAL_CHANNELS {
            return Err(format!(
                "{} microphones are more than the front end's {LOGICAL_CHANNELS}",
                self.mic_lanes.len()
            ));
        }
        if let Some(l) = self.mic_lanes.iter().find(|&&l| l >= self.channels) {
            return Err(format!(
                "microphone lane {l} is not on a {}-channel capture",
                self.channels
            ));
        }
        Ok(())
    }
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

/// Anti-alias filter and decimation to [`SAMPLE_RATE`] by an integer factor:
/// a Kaiser-windowed sinc, flat to 7 kHz and at least 70 dB down from 8 kHz,
/// where what is left folds into the band. One output for every `ratio`
/// inputs; the filter delays the signal by half its length.
#[derive(Clone, Debug)]
pub struct Decimator {
    taps: Vec<f32>,
    /// The last `taps.len()` inputs, written twice so that the newest-first
    /// window is one slice.
    hist: Vec<f32>,
    pos: usize,
    ratio: usize,
    phase: usize,
}

impl Decimator {
    pub fn new(ratio: usize) -> Self {
        let fs = (ratio as u32 * SAMPLE_RATE) as f64;
        let (cutoff, transition, atten) = (7_500.0, 1_000.0, 70.0);
        let n = (((atten - 7.95) / (2.285 * 2.0 * std::f64::consts::PI * transition / fs)).ceil()
            as usize)
            | 1;
        let beta = 0.1102 * (atten - 8.7);
        let mid = (n / 2) as f64;
        let wc = 2.0 * cutoff / fs;
        let mut taps: Vec<f64> = (0..n)
            .map(|k| {
                let m = k as f64 - mid;
                let sinc = if m == 0.0 {
                    wc
                } else {
                    (std::f64::consts::PI * wc * m).sin() / (std::f64::consts::PI * m)
                };
                let r = m / mid;
                let w = crate::upsample::bessel_i0(beta * (1.0 - r * r).max(0.0).sqrt())
                    / crate::upsample::bessel_i0(beta);
                sinc * w
            })
            .collect();
        let sum: f64 = taps.iter().sum();
        taps.iter_mut().for_each(|t| *t /= sum);
        Decimator {
            taps: taps.into_iter().map(|t| t as f32).collect(),
            hist: vec![0.0; 2 * n],
            pos: 0,
            ratio,
            phase: 0,
        }
    }

    /// Take one input; every `ratio`th gives an output.
    pub fn push(&mut self, x: i16) -> Option<i16> {
        let n = self.taps.len();
        self.pos = (self.pos + n - 1) % n;
        self.hist[self.pos] = f32::from(x);
        self.hist[self.pos + n] = f32::from(x);
        self.phase += 1;
        if self.phase < self.ratio {
            return None;
        }
        self.phase = 0;
        let acc: f32 = self
            .taps
            .iter()
            .zip(&self.hist[self.pos..self.pos + n])
            .map(|(t, x)| t * x)
            .sum();
        Some(saturate_s16(acc.round() as i64))
    }
}

/// Turns raw multi-channel capture bytes into mono 16 kHz samples.
pub trait FrontEnd: Send {
    /// Consume whole raw frames from `raw` (a multiple of the capture
    /// format's frame size, [`RAW_FRAME_BYTES`] for the MT8163) and append
    /// mono samples to `out`.
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

/// Calibrate -> mean mixdown -> decimate to 16 kHz (if the PCM is faster)
/// -> high-pass -> digital gain.
pub struct SimpleFrontEnd {
    cfg: FrontEndConfig,
    format: CaptureFormat,
    /// `cfg.mic_mask` less the microphones the board doesn't have.
    mask: u8,
    hpf: HighPass,
    decimator: Option<Decimator>,
}

impl SimpleFrontEnd {
    /// For the MT8163's capture format.
    pub fn new(cfg: FrontEndConfig) -> Self {
        Self::with_format(cfg, CaptureFormat::default()).expect("the default format decodes")
    }

    pub fn with_format(mut cfg: FrontEndConfig, format: CaptureFormat) -> Result<Self, String> {
        format.check()?;
        cfg.digital_gain = cfg.digital_gain.clamp(DIGITAL_GAIN_UNITY, DIGITAL_GAIN_MAX);
        let ratio = format.decimation();
        Ok(SimpleFrontEnd {
            mask: cfg.mic_mask & ((1u16 << format.mic_lanes.len()) - 1) as u8,
            cfg,
            decimator: (ratio > 1).then(|| Decimator::new(ratio)),
            format,
            hpf: HighPass::new(),
        })
    }
}

impl FrontEnd for SimpleFrontEnd {
    fn process(&mut self, raw: &[u8], out: &mut Vec<i16>) {
        let start = out.len();
        let width = self.format.sample.bytes();
        for frame in raw.chunks_exact(self.format.frame_bytes()) {
            let mut logical = [0i16; LOGICAL_CHANNELS];
            for (c, (l, &lane)) in logical.iter_mut().zip(&self.format.mic_lanes).enumerate() {
                let s = self.format.sample.unpack(&frame[lane * width..]);
                *l = self.cfg.calibration.apply(c, s);
            }
            let mut mixed = mix_selected(&logical, self.mask);
            if let Some(d) = &mut self.decimator {
                match d.push(mixed) {
                    Some(y) => mixed = y,
                    None => continue,
                }
            }
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

    /// The capture format a board's PCM delivers, as pinnad hands it to the
    /// front end (ADR 0032).
    fn board_format(b: &pinna_board::Board) -> CaptureFormat {
        let c = b.audio.capture;
        CaptureFormat {
            rate: c.rate,
            channels: c.channels,
            sample: match c.format {
                pinna_board::SampleFormat::S24_3Le => SampleFormat::S24_3Le,
                pinna_board::SampleFormat::S32Le => SampleFormat::S32Le,
                f => panic!("the front end can't decode {f:?}"),
            },
            mic_lanes: (0..b.mics.count).collect(),
        }
    }

    /// Raw capture frames with `mic` on the microphone lanes and `other` on
    /// every other lane, as the board's PCM lays them out.
    fn raw_tone(fmt: &CaptureFormat, frames: usize, mic: (f64, f64), other: (f64, f64)) -> Vec<u8> {
        let mut raw = Vec::new();
        for n in 0..frames {
            for lane in 0..fmt.channels {
                let (freq, amp) = if fmt.mic_lanes.contains(&lane) {
                    mic
                } else {
                    other
                };
                let t = n as f64 / f64::from(fmt.rate);
                let s = (amp * (2.0 * std::f64::consts::PI * freq * t).sin()).round() as i16;
                // Garbage below the 16 bits that count.
                match fmt.sample {
                    SampleFormat::S24_3Le => {
                        raw.push(0x55);
                        raw.extend_from_slice(&s.to_le_bytes());
                    }
                    SampleFormat::S32Le => {
                        raw.extend_from_slice(&[0x55, 0x55]);
                        raw.extend_from_slice(&s.to_le_bytes());
                    }
                }
            }
        }
        raw
    }

    /// Amplitude of `freq` in `x` at 16 kHz, by correlation.
    fn amplitude(x: &[i16], freq: f64) -> f64 {
        let (mut re, mut im) = (0.0, 0.0);
        for (n, &s) in x.iter().enumerate() {
            let w = 2.0 * std::f64::consts::PI * freq * n as f64 / f64::from(SAMPLE_RATE);
            re += f64::from(s) * w.cos();
            im += f64::from(s) * w.sin();
        }
        2.0 * (re * re + im * im).sqrt() / x.len() as f64
    }

    fn front_end(fmt: &CaptureFormat, mask: u8) -> SimpleFrontEnd {
        SimpleFrontEnd::with_format(
            FrontEndConfig {
                mic_mask: mask,
                highpass: false,
                digital_gain: DIGITAL_GAIN_UNITY,
                ..Default::default()
            },
            fmt.clone(),
        )
        .unwrap()
    }

    /// For every board, the front end accepts the board's capture PCM and
    /// microphone lanes, and a tone on the microphones comes out as that
    /// tone at 16 kHz, with what is on the other lanes (the echo reference)
    /// left out. A board whose PCM the front end can't decode (ADR 0032)
    /// fails here.
    #[test]
    fn every_board_captures_what_the_front_end_decodes() {
        for m in pinna_board::Model::ALL {
            let b = m.board();
            let fmt = board_format(b);
            fmt.check().unwrap_or_else(|e| panic!("{m:?}: {e}"));
            assert_eq!(fmt.mic_lanes.len(), b.mics.count, "{m:?}");
            // One second of 1 kHz at 12000 on the microphones, 3 kHz at
            // 20000 on the other lanes.
            let raw = raw_tone(
                &fmt,
                fmt.rate as usize,
                (1_000.0, 12_000.0),
                (3_000.0, 20_000.0),
            );
            let mut fe = front_end(&fmt, b.mics.speech_mask);
            let mut out = Vec::new();
            fe.process(&raw, &mut out);
            assert_eq!(out.len(), SAMPLE_RATE as usize, "{m:?}");
            // Past the decimator's start-up (half its length).
            let y = &out[1_000..];
            let tone = amplitude(y, 1_000.0);
            assert!((tone / 12_000.0 - 1.0).abs() < 0.01, "{m:?}: {tone}");
            let leak = amplitude(y, 3_000.0);
            assert!(leak < 12.0, "{m:?}: the reference lanes leak {leak}");
        }
    }

    #[test]
    fn s32_takes_top_16_bits() {
        assert_eq!(unpack_s32le([0xff, 0xff, 0x34, 0x12]), 0x1234);
        assert_eq!(unpack_s32le([0x00, 0x00, 0x00, 0x80]), i16::MIN);
        assert_eq!(unpack_s32le([0x00, 0x00, 0xff, 0xff]), -1);
        assert_eq!(unpack_s32le([0xff, 0xff, 0xff, 0x7f]), 0x7fff);
    }

    #[test]
    fn a_microphone_is_the_lane_the_format_names() {
        let fmt = CaptureFormat {
            rate: 16_000,
            channels: 6,
            sample: SampleFormat::S32Le,
            mic_lanes: vec![4, 1],
        };
        let mut fe = front_end(&fmt, 0b01);
        let mut raw = Vec::new();
        for lane in 0..6i16 {
            raw.extend_from_slice(&[0, 0]);
            raw.extend_from_slice(&((lane + 1) * 100).to_le_bytes());
        }
        let mut out = Vec::new();
        fe.process(&raw, &mut out);
        // Microphone 0 is lane 4: 500.
        assert_eq!(out, vec![500]);
        // A microphone the board doesn't have is not in the mean.
        let mut fe = front_end(&fmt, 0b1111_1111);
        out.clear();
        fe.process(&raw, &mut out);
        assert_eq!(out, vec![350]);
    }

    #[test]
    fn the_format_is_checked() {
        let ok = CaptureFormat::default();
        assert!(ok.check().is_ok());
        let with = |f: &dyn Fn(&mut CaptureFormat)| {
            let mut c = ok.clone();
            f(&mut c);
            c.check().unwrap_err()
        };
        assert!(with(&|c| c.rate = 44_100).contains("44100"));
        assert!(with(&|c| c.rate = 8_000).contains("8000"));
        assert!(with(&|c| c.mic_lanes = vec![0; 8]).contains("8 microphones"));
        assert!(with(&|c| c.mic_lanes = vec![9]).contains("lane 9"));
    }

    /// The anti-alias filter passes the speech band and keeps what is above
    /// 8 kHz from folding into it.
    #[test]
    fn decimation_keeps_speech_and_rejects_aliases() {
        let fmt = CaptureFormat {
            rate: 96_000,
            channels: 1,
            sample: SampleFormat::S32Le,
            mic_lanes: vec![0],
        };
        let run = |freq: f64| {
            let raw = raw_tone(&fmt, 96_000, (freq, 10_000.0), (0.0, 0.0));
            let mut out = Vec::new();
            front_end(&fmt, 1).process(&raw, &mut out);
            out[1_000..].to_vec()
        };
        // 6.5 kHz is in the passband.
        let y = run(6_500.0);
        let pass = amplitude(&y, 6_500.0) / 10_000.0;
        assert!((pass - 1.0).abs() < 0.05, "{pass}");
        // 9 kHz would fold to 7 kHz, 10 kHz to 6 kHz, 20 kHz to 4 kHz.
        for (freq, folded) in [(9_000.0, 7_000.0), (10_000.0, 6_000.0), (20_000.0, 4_000.0)] {
            let alias = amplitude(&run(freq), folded) / 10_000.0;
            assert!(20.0 * alias.log10() < -60.0, "{freq} Hz: {alias}");
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
