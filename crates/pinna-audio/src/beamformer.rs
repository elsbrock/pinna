//! Fixed-beam frequency-domain superdirective beamformer: a port of
//! the previous firmware's `mic_beamformer.c`.
//!
//! 256-point STFT, 128-sample hop, up to 12 beams spread evenly in azimuth
//! (0 degrees = front, clockwise from above). Each beam is MVDR against a
//! spherically diffuse noise field with per-frequency diagonal loading chosen
//! so white-noise gain stays at or above -3 dB; the selector follows the beam
//! with the best speech-band power over its noise floor, with hysteresis.
//! Output is delayed by [`FFT`] samples (16 ms). The arithmetic (f32 where the
//! C uses float, f64 where it uses double, same operation order) follows the C
//! so outputs match it.
// The loops mirror the C index for index; keep them that way.
#![allow(clippy::needless_range_loop)]

use crate::dsp::{LOGICAL_CHANNELS, MIC_MASK_ALL, MIC_MASK_BEAM, SAMPLE_RATE, saturate_s16};

pub const FFT: usize = 256;
pub const HOP: usize = 128;
pub const BINS: usize = FFT / 2 + 1;
pub const MAX_BEAMS: usize = 12;
pub const DEFAULT_BEAMS: usize = 12;
const WNG_MIN_DB: f64 = -3.0;

const PI: f64 = std::f64::consts::PI;
/// Speech band used to score beams: bins 5..=64 are 312 Hz to 4 kHz.
const SCORE_FIRST_BIN: usize = 5;
const SCORE_LAST_BIN: usize = 64;
/// Per-hop power smoothing (one hop is 8 ms).
const POWER_SMOOTHING: f32 = 0.8;
/// Each beam's noise floor falls at once and rises 3 dB per second; beams are
/// scored by power over floor, smoothed over ~200 ms.
const FLOOR_RISE: f32 = 1.00554;
const SCORE_SMOOTHING: f32 = 0.96;
/// A rival beam must score 1.5 dB higher for 12 hops (96 ms) to take over.
const SWITCH_RATIO: f32 = 1.4125;
const SWITCH_HOPS: u32 = 12;
/// Only a beam clearly above its floor (3 dB) may take over.
const ACTIVE_SCORE: f32 = 2.0;

// Geometry of the seven-microphone ring (see the previous firmware's mic_array_geometry.h).
const CENTRE_LANE: usize = 6;
const LANE0_AZIMUTH_DEG: f64 = 150.0;
const RING_STEP_DEG: f64 = 60.0;
const RING_RADIUS_M: f64 = 36000.0 * 1e-6;
const SPEED_OF_SOUND: f64 = 343000.0 * 1e-3;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum BeamError {
    #[error("beam count must be 1..={MAX_BEAMS}")]
    BeamCount,
    #[error("at least two microphones must be selected")]
    MicMask,
    #[error("beam design failed (singular system)")]
    Design,
}

#[derive(Clone, Copy, Default)]
struct Cplx {
    re: f64,
    im: f64,
}

type Vec7 = [Cplx; LOGICAL_CHANNELS];
type Mat7 = [[Cplx; LOGICAL_CHANNELS]; LOGICAL_CHANNELS];

fn mic_position(lane: usize) -> (f64, f64) {
    if lane == CENTRE_LANE {
        return (0.0, 0.0);
    }
    let angle = (LANE0_AZIMUTH_DEG + lane as f64 * RING_STEP_DEG) * PI / 180.0;
    (RING_RADIUS_M * angle.cos(), RING_RADIUS_M * angle.sin())
}

/// Solve A x = b by Gaussian elimination with partial pivoting; A and b are
/// overwritten. Returns false when singular.
fn solve(a: &mut Mat7, b: &mut Vec7, n: usize) -> bool {
    for col in 0..n {
        let mut pivot = col;
        let mut best = a[col][col].re * a[col][col].re + a[col][col].im * a[col][col].im;
        for row in col + 1..n {
            let mag = a[row][col].re * a[row][col].re + a[row][col].im * a[row][col].im;
            if mag > best {
                best = mag;
                pivot = row;
            }
        }
        if best < 1e-30 {
            return false;
        }
        if pivot != col {
            for k in 0..n {
                let t = a[col][k];
                a[col][k] = a[pivot][k];
                a[pivot][k] = t;
            }
            b.swap(col, pivot);
        }
        let norm = a[col][col].re * a[col][col].re + a[col][col].im * a[col][col].im;
        let inv = Cplx {
            re: a[col][col].re / norm,
            im: -a[col][col].im / norm,
        };
        for row in col + 1..n {
            let f = Cplx {
                re: a[row][col].re * inv.re - a[row][col].im * inv.im,
                im: a[row][col].re * inv.im + a[row][col].im * inv.re,
            };
            for k in col..n {
                let (cr, ci) = (a[col][k].re, a[col][k].im);
                a[row][k].re -= f.re * cr - f.im * ci;
                a[row][k].im -= f.re * ci + f.im * cr;
            }
            let (cr, ci) = (b[col].re, b[col].im);
            b[row].re -= f.re * cr - f.im * ci;
            b[row].im -= f.re * ci + f.im * cr;
        }
    }
    for row in (0..n).rev() {
        let mut s = b[row];
        for k in row + 1..n {
            s.re -= a[row][k].re * b[k].re - a[row][k].im * b[k].im;
            s.im -= a[row][k].re * b[k].im + a[row][k].im * b[k].re;
        }
        let norm = a[row][row].re * a[row][row].re + a[row][row].im * a[row][row].im;
        b[row].re = (s.re * a[row][row].re + s.im * a[row][row].im) / norm;
        b[row].im = (s.im * a[row][row].re - s.re * a[row][row].im) / norm;
    }
    true
}

/// MVDR weights against diffuse noise loaded by `mu`, for steering vector `d`:
/// w = (G + mu I)^-1 d / (d^H (G + mu I)^-1 d). Returns the white-noise gain
/// 1/|w|^2, or a negative value when the system is singular.
fn mvdr(
    gamma: &[[f64; LOGICAL_CHANNELS]; LOGICAL_CHANNELS],
    d: &Vec7,
    n: usize,
    mu: f64,
    w: &mut Vec7,
) -> f64 {
    let mut a = Mat7::default();
    let mut denom = Cplx::default();
    let mut energy = 0.0f64;
    for i in 0..n {
        for j in 0..n {
            a[i][j].re = gamma[i][j] + if i == j { mu } else { 0.0 };
            a[i][j].im = 0.0;
        }
        w[i] = d[i];
    }
    if !solve(&mut a, w, n) {
        return -1.0;
    }
    for i in 0..n {
        // d^H x
        denom.re += d[i].re * w[i].re + d[i].im * w[i].im;
        denom.im += d[i].re * w[i].im - d[i].im * w[i].re;
    }
    let norm = denom.re * denom.re + denom.im * denom.im;
    if norm < 1e-30 {
        return -1.0;
    }
    for i in 0..n {
        let v = w[i];
        w[i].re = (v.re * denom.re + v.im * denom.im) / norm;
        w[i].im = (v.im * denom.re - v.re * denom.im) / norm;
        energy += w[i].re * w[i].re + w[i].im * w[i].im;
    }
    if energy > 0.0 { 1.0 / energy } else { -1.0 }
}

fn steering(px: &[f64], py: &[f64], n: usize, frequency: f64, azimuth: f64, d: &mut Vec7) {
    let ux = azimuth.cos();
    let uy = azimuth.sin();
    for m in 0..n {
        // A plane wave from the look direction reaches a microphone further
        // along it earlier: arrival delay is -(p . u) / c.
        let tau = -(px[m] * ux + py[m] * uy) / SPEED_OF_SOUND;
        let phase = -2.0 * PI * frequency * tau;
        d[m].re = phase.cos();
        d[m].im = phase.sin();
    }
}

fn fft(
    twiddle_re: &[f32; FFT / 2],
    twiddle_im: &[f32; FFT / 2],
    re: &mut [f32; FFT],
    im: &mut [f32; FFT],
) {
    let mut j = 0usize;
    for i in 1..FFT {
        let mut bit = FFT >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j ^= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= FFT {
        let half = len >> 1;
        let stride = FFT / len;
        let mut i = 0;
        while i < FFT {
            for j in 0..half {
                let wr = twiddle_re[j * stride];
                let wi = twiddle_im[j * stride];
                let p = i + j;
                let q = p + half;
                let tr = re[q] * wr - im[q] * wi;
                let ti = re[q] * wi + im[q] * wr;
                re[q] = re[p] - tr;
                im[q] = im[p] - ti;
                re[p] += tr;
                im[p] += ti;
            }
            i += len;
        }
        len <<= 1;
    }
}

pub struct Beamformer {
    mic_count: usize,
    mics: [u8; LOGICAL_CHANNELS],
    beam_count: usize,
    azimuth_deg: [i32; MAX_BEAMS],
    /// Conjugated weights, `[beam][bin][mic]` flattened:
    /// Y_b[k] = sum_m w[b][k][m] * X_m[k].
    w_re: Vec<f32>,
    w_im: Vec<f32>,
    window: [f32; FFT],
    twiddle_re: [f32; FFT / 2],
    twiddle_im: [f32; FFT / 2],
    input: [[f32; FFT]; LOGICAL_CHANNELS],
    xr: [[f32; BINS]; LOGICAL_CHANNELS],
    xi: [[f32; BINS]; LOGICAL_CHANNELS],
    overlap: [f32; HOP],
    output: [i16; HOP],
    fill: usize,
    power: [f32; MAX_BEAMS],
    floor: [f32; MAX_BEAMS],
    score: [f32; MAX_BEAMS],
    selected: usize,
    candidate: usize,
    candidate_frames: u32,
    locked: Option<usize>,
    primed: bool,
}

impl Beamformer {
    pub fn new(mic_mask: u8, beam_count: usize) -> Result<Self, BeamError> {
        if beam_count == 0 || beam_count > MAX_BEAMS {
            return Err(BeamError::BeamCount);
        }
        let mic_mask = mic_mask & MIC_MASK_ALL;
        let mut mics = [0u8; LOGICAL_CHANNELS];
        let mut mic_count = 0;
        for i in 0..LOGICAL_CHANNELS {
            if mic_mask & (1 << i) != 0 {
                mics[mic_count] = i as u8;
                mic_count += 1;
            }
        }
        if mic_count < 2 {
            return Err(BeamError::MicMask);
        }
        let mut azimuth_deg = [0i32; MAX_BEAMS];
        for (i, a) in azimuth_deg.iter_mut().enumerate().take(beam_count) {
            *a = (i * 360 / beam_count) as i32;
        }
        let mut window = [0f32; FFT];
        for (i, w) in window.iter_mut().enumerate() {
            // Periodic sqrt-Hann for analysis and synthesis: at 50% overlap the
            // product sums to exactly one, so a pass-through reconstructs.
            *w = (0.5 - 0.5 * (2.0 * PI * i as f64 / FFT as f64).cos()).sqrt() as f32;
        }
        let mut twiddle_re = [0f32; FFT / 2];
        let mut twiddle_im = [0f32; FFT / 2];
        for i in 0..FFT / 2 {
            twiddle_re[i] = (-2.0 * PI * i as f64 / FFT as f64).cos() as f32;
            twiddle_im[i] = (-2.0 * PI * i as f64 / FFT as f64).sin() as f32;
        }
        let mut bf = Beamformer {
            mic_count,
            mics,
            beam_count,
            azimuth_deg,
            w_re: vec![0.0; MAX_BEAMS * BINS * LOGICAL_CHANNELS],
            w_im: vec![0.0; MAX_BEAMS * BINS * LOGICAL_CHANNELS],
            window,
            twiddle_re,
            twiddle_im,
            input: [[0.0; FFT]; LOGICAL_CHANNELS],
            xr: [[0.0; BINS]; LOGICAL_CHANNELS],
            xi: [[0.0; BINS]; LOGICAL_CHANNELS],
            overlap: [0.0; HOP],
            output: [0; HOP],
            fill: 0,
            power: [0.0; MAX_BEAMS],
            floor: [0.0; MAX_BEAMS],
            score: [0.0; MAX_BEAMS],
            selected: 0,
            candidate: 0,
            candidate_frames: 0,
            locked: None,
            primed: false,
        };
        bf.design()?;
        Ok(bf)
    }

    /// The stock speech microphones (lanes 0, 1, 3, 4) with 12 beams.
    pub fn default_echo_dot() -> Self {
        Self::new(MIC_MASK_BEAM, DEFAULT_BEAMS).expect("default beamformer design")
    }

    fn wi(b: usize, k: usize) -> usize {
        (b * BINS + k) * LOGICAL_CHANNELS
    }

    fn design(&mut self) -> Result<(), BeamError> {
        let mut px = [0f64; LOGICAL_CHANNELS];
        let mut py = [0f64; LOGICAL_CHANNELS];
        let mut gamma = [[0f64; LOGICAL_CHANNELS]; LOGICAL_CHANNELS];
        let wng_min = 10f64.powf(WNG_MIN_DB / 10.0);
        let n = self.mic_count;
        for i in 0..n {
            (px[i], py[i]) = mic_position(self.mics[i] as usize);
        }
        for k in 0..BINS {
            let frequency = k as f64 * f64::from(SAMPLE_RATE) / FFT as f64;
            let mut d = Vec7::default();
            let mut w = Vec7::default();

            // Spherically diffuse coherence: sin(x)/x with x = 2 pi f r / c.
            for i in 0..n {
                for j in 0..n {
                    let r = (px[i] - px[j]).hypot(py[i] - py[j]);
                    let x = 2.0 * PI * frequency * r / SPEED_OF_SOUND;
                    gamma[i][j] = if x < 1e-9 { 1.0 } else { x.sin() / x };
                }
            }
            // For each beam, pick the smallest loading that keeps white-noise
            // gain above the floor.
            for b in 0..self.beam_count {
                let mut lo = 1e-6f64;
                let mut hi = 1e3f64;
                steering(
                    &px,
                    &py,
                    n,
                    frequency,
                    f64::from(self.azimuth_deg[b]) * PI / 180.0,
                    &mut d,
                );
                if mvdr(&gamma, &d, n, lo, &mut w) >= wng_min {
                    hi = lo;
                } else {
                    for _ in 0..40 {
                        let mid = (lo * hi).sqrt();
                        let wng = mvdr(&gamma, &d, n, mid, &mut w);
                        if wng >= wng_min {
                            hi = mid;
                        } else {
                            lo = mid;
                        }
                    }
                }
                if mvdr(&gamma, &d, n, hi, &mut w) < 0.0 {
                    return Err(BeamError::Design);
                }
                let base = Self::wi(b, k);
                for i in 0..n {
                    // Store conj(w) so the hot loop is a plain multiply-add.
                    self.w_re[base + i] = w[i].re as f32;
                    self.w_im[base + i] = (-w[i].im) as f32;
                }
            }
        }
        Ok(())
    }

    /// Beam output at bin `k` from the current spectra.
    fn combine(&self, b: usize, k: usize) -> (f32, f32) {
        let base = Self::wi(b, k);
        let mut sr = 0.0f32;
        let mut si = 0.0f32;
        for m in 0..self.mic_count {
            sr += self.w_re[base + m] * self.xr[m][k] - self.w_im[base + m] * self.xi[m][k];
            si += self.w_re[base + m] * self.xi[m][k] + self.w_im[base + m] * self.xr[m][k];
        }
        (sr, si)
    }

    fn process_hop(&mut self) {
        let mut re = [0f32; FFT];
        let mut im = [0f32; FFT];
        let mut yr = [0f32; BINS];
        let mut yi = [0f32; BINS];
        let mut best = 0usize;

        for m in 0..self.mic_count {
            for k in 0..FFT {
                re[k] = self.input[m][k] * self.window[k];
                im[k] = 0.0;
            }
            fft(&self.twiddle_re, &self.twiddle_im, &mut re, &mut im);
            self.xr[m].copy_from_slice(&re[..BINS]);
            self.xi[m].copy_from_slice(&im[..BINS]);
        }
        for b in 0..self.beam_count {
            let mut energy = 0.0f32;
            for k in SCORE_FIRST_BIN..=SCORE_LAST_BIN {
                let (sr, si) = self.combine(b, k);
                energy += sr * sr + si * si;
            }
            if !self.primed {
                // Start every beam at its first measurement, scored 0 dB.
                self.power[b] = energy;
                self.floor[b] = energy;
                self.score[b] = 1.0;
            }
            self.power[b] = POWER_SMOOTHING * self.power[b] + (1.0 - POWER_SMOOTHING) * energy;
            if self.floor[b] <= 0.0 || self.power[b] < self.floor[b] {
                self.floor[b] = self.power[b];
            } else {
                self.floor[b] *= FLOOR_RISE;
            }
            self.score[b] = SCORE_SMOOTHING * self.score[b]
                + (1.0 - SCORE_SMOOTHING) * (self.power[b] / (self.floor[b] + 1e-3));
            if self.score[b] > self.score[best] {
                best = b;
            }
        }
        self.primed = true;
        if let Some(l) = self.locked {
            self.selected = l;
            self.candidate_frames = 0;
        } else if best != self.selected
            && self.score[best] > ACTIVE_SCORE
            && self.score[best] > self.score[self.selected] * SWITCH_RATIO
        {
            if best == self.candidate {
                self.candidate_frames += 1;
                if self.candidate_frames >= SWITCH_HOPS {
                    self.selected = best;
                    self.candidate_frames = 0;
                }
            } else {
                self.candidate = best;
                self.candidate_frames = 1;
            }
        } else {
            self.candidate_frames = 0;
        }
        let b = self.selected;
        for k in 0..BINS {
            (yr[k], yi[k]) = self.combine(b, k);
        }
        // Inverse real FFT via the conjugate trick on the Hermitian spectrum.
        for k in 0..BINS {
            re[k] = yr[k];
            im[k] = -yi[k];
        }
        im[0] = 0.0;
        im[FFT / 2] = 0.0;
        for k in BINS..FFT {
            re[k] = yr[FFT - k];
            im[k] = yi[FFT - k];
        }
        fft(&self.twiddle_re, &self.twiddle_im, &mut re, &mut im);
        for k in 0..FFT {
            re[k] *= self.window[k] / FFT as f32;
        }
        for k in 0..HOP {
            let v = self.overlap[k] + re[k];
            self.output[k] = saturate_s16(v.round_ties_even() as i64);
            self.overlap[k] = re[k + HOP];
        }
    }

    /// One calibrated logical frame in, one beamformed sample out (delayed by
    /// [`FFT`] samples).
    pub fn sample(&mut self, logical: &[i16; LOGICAL_CHANNELS]) -> i16 {
        let pos = HOP + self.fill;
        let out = self.output[self.fill];
        for m in 0..self.mic_count {
            self.input[m][pos] = f32::from(logical[self.mics[m] as usize]);
        }
        self.fill += 1;
        if self.fill == HOP {
            self.process_hop();
            for m in 0..self.mic_count {
                self.input[m].copy_within(HOP.., 0);
            }
            self.fill = 0;
        }
        out
    }

    pub fn process(&mut self, frames: &[[i16; LOGICAL_CHANNELS]], out: &mut Vec<i16>) {
        out.reserve(frames.len());
        for f in frames {
            out.push(self.sample(f));
        }
    }

    /// Pin the output to the beam nearest `azimuth_deg`, or `None` (or a
    /// negative value) to return to automatic selection. Beam powers keep
    /// updating while pinned.
    pub fn lock(&mut self, azimuth_deg: Option<i32>) {
        let Some(az) = azimuth_deg.filter(|a| *a >= 0) else {
            self.locked = None;
            return;
        };
        let az = az % 360;
        let mut best = 0;
        let mut best_err = 360;
        for b in 0..self.beam_count {
            let mut err = (self.azimuth_deg[b] - az).abs();
            if err > 180 {
                err = 360 - err;
            }
            if err < best_err {
                best_err = err;
                best = b;
            }
        }
        self.locked = Some(best);
        self.selected = best;
    }

    /// Smoothed speech-band power of beam `b` (0 when out of range).
    pub fn beam_power(&self, b: usize) -> f32 {
        if b < self.beam_count {
            self.power[b]
        } else {
            0.0
        }
    }

    /// Azimuth of beam `b` in degrees (0 when out of range).
    pub fn beam_azimuth(&self, b: usize) -> i32 {
        if b < self.beam_count {
            self.azimuth_deg[b]
        } else {
            0
        }
    }

    /// Azimuth of the beam currently selected.
    pub fn selected_azimuth(&self) -> i32 {
        self.azimuth_deg[self.selected]
    }

    pub fn beam_count(&self) -> usize {
        self.beam_count
    }

    #[cfg(test)]
    fn weight(&self, b: usize, k: usize, m: usize) -> (f32, f32) {
        let i = Self::wi(b, k) + m;
        (self.w_re[i], self.w_im[i])
    }
}

/// Parses "0,1,3,4" or "all" into a microphone mask.
pub fn parse_mics(text: &str) -> Option<u8> {
    if text.is_empty() {
        return None;
    }
    if text == "all" {
        return Some(MIC_MASK_ALL);
    }
    let mut mask = 0u8;
    let mut it = text.bytes().peekable();
    while let Some(c) = it.next() {
        if c < b'0' || c >= b'0' + LOGICAL_CHANNELS as u8 {
            return None;
        }
        let bit = 1u8 << (c - b'0');
        if mask & bit != 0 {
            return None;
        }
        mask |= bit;
        match it.peek() {
            Some(b',') => {
                it.next();
            }
            Some(_) => return None,
            None => {}
        }
    }
    Some(mask)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    const RATE: f64 = SAMPLE_RATE as f64;

    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> f64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 >> 11) as f64 / (1u64 << 53) as f64
        }
    }

    type Frames = Vec<[i16; LOGICAL_CHANNELS]>;

    fn to_i16(v: f64) -> i16 {
        saturate_s16(v.round() as i64)
    }

    fn white_noise(n: usize, amp: f64, seed: u64) -> Frames {
        let mut r = Rng(seed);
        (0..n)
            .map(|_| std::array::from_fn(|_| to_i16((r.next() * 2.0 - 1.0) * amp)))
            .collect()
    }

    /// Source signal: a 1 kHz tone plus speech-band tones with fixed random phases.
    struct Source {
        tones: Vec<(f64, f64, f64)>, // freq, amp, phase
    }
    impl Source {
        fn new(seed: u64, noise_amp: f64) -> Self {
            let mut r = Rng(seed);
            let mut tones = vec![(1000.0, 3000.0, 0.0)];
            for _ in 0..40 {
                tones.push((
                    300.0 + r.next() * 3500.0,
                    noise_amp,
                    r.next() * 2.0 * std::f64::consts::PI,
                ));
            }
            Source { tones }
        }
        fn at(&self, t: f64) -> f64 {
            self.tones
                .iter()
                .map(|&(f, a, p)| a * (2.0 * std::f64::consts::PI * f * t + p).sin())
                .sum()
        }
    }

    /// Plane wave from `azimuth_deg` on all seven lanes, `gain(n)` scaling.
    fn plane_wave(src: &Source, azimuth_deg: f64, n: usize, gain: impl Fn(usize) -> f64) -> Frames {
        let az = azimuth_deg.to_radians();
        let delays: Vec<f64> = (0..LOGICAL_CHANNELS)
            .map(|lane| {
                let (x, y) = mic_position(lane);
                -(x * az.cos() + y * az.sin()) / SPEED_OF_SOUND
            })
            .collect();
        (0..n)
            .map(|i| std::array::from_fn(|m| to_i16(gain(i) * src.at(i as f64 / RATE - delays[m]))))
            .collect()
    }

    /// Plane wave from `azimuth_deg` over quiet diffuse-ish background noise;
    /// the wave switches on after `onset` samples.
    fn burst(src: &Source, azimuth_deg: f64, n: usize, onset: usize, seed: u64) -> Frames {
        let wave = plane_wave(src, azimuth_deg, n, |i| if i < onset { 0.0 } else { 1.0 });
        let bg = white_noise(n, 20.0, seed);
        wave.iter()
            .zip(&bg)
            .map(|(a, b)| std::array::from_fn(|m| a[m].saturating_add(b[m])))
            .collect()
    }

    fn run(bf: &mut Beamformer, frames: &Frames) -> Vec<i16> {
        let mut out = Vec::new();
        bf.process(frames, &mut out);
        out
    }

    #[test]
    fn parse_mics_cases() {
        assert_eq!(parse_mics("0,1,3,4"), Some(MIC_MASK_BEAM));
        assert_eq!(parse_mics("all"), Some(MIC_MASK_ALL));
        assert_eq!(parse_mics("6"), Some(1 << 6));
        assert_eq!(parse_mics("0,1,"), Some(3));
        assert_eq!(parse_mics(""), None);
        assert_eq!(parse_mics("7"), None);
        assert_eq!(parse_mics("0,0"), None);
        assert_eq!(parse_mics("0;1"), None);
        assert_eq!(parse_mics("01"), None);
        assert_eq!(parse_mics(",1"), None);
    }

    #[test]
    fn rejects_bad_config() {
        assert_eq!(
            Beamformer::new(MIC_MASK_BEAM, 0).err(),
            Some(BeamError::BeamCount)
        );
        assert_eq!(
            Beamformer::new(MIC_MASK_BEAM, 13).err(),
            Some(BeamError::BeamCount)
        );
        assert_eq!(Beamformer::new(1, 12).err(), Some(BeamError::MicMask));
        assert_eq!(Beamformer::new(0x80, 12).err(), Some(BeamError::MicMask));
    }

    #[test]
    fn distortionless_on_look_direction() {
        let src = Source::new(7, 0.0); // 1 kHz tone only
        let n = 16 * HOP;
        for az in [0, 30, 90, 150, 270, 330] {
            let mut bf = Beamformer::default_echo_dot();
            bf.lock(Some(az));
            let frames = plane_wave(&src, f64::from(az), n, |_| 1.0);
            let out = run(&mut bf, &frames);
            let mut worst = 0f64;
            for i in 4 * HOP..n - FFT {
                let want = src.at(i as f64 / RATE);
                worst = worst.max((f64::from(out[i + FFT]) - want).abs());
            }
            assert!(worst < 0.03 * 3000.0, "az {az}: worst error {worst}");
        }
    }

    #[test]
    fn selection_follows_moving_source() {
        let src = Source::new(11, 150.0);
        let mut bf = Beamformer::default_echo_dot();
        for az in [90, 270, 150] {
            // Quiet background, then a burst from `az`.
            let n = 16_000 * 3;
            let frames = burst(&src, f64::from(az), n, n / 2, 99 + az as u64);
            run(&mut bf, &frames);
            let err = (bf.selected_azimuth() - az).rem_euclid(360);
            let err = err.min(360 - err);
            assert!(err <= 30, "source {az}: selected {}", bf.selected_azimuth());
        }
    }

    // ---- parity against the original C ----

    fn c_source_dir() -> Option<PathBuf> {
        let dir = PathBuf::from(std::env::var_os("PINNA_BEAMFORMER_C_DIR")?);
        dir.join("mic_beamformer.c").exists().then_some(dir)
    }

    const HARNESS: &str = r#"
#include <stdio.h>
#include <stdlib.h>
#include "mic_beamformer.h"
static struct le_beamformer bf;
int main(int argc, char **argv) {
    unsigned mask = (unsigned)atoi(argv[1]), beams = (unsigned)atoi(argv[2]);
    int lock = atoi(argv[3]);
    FILE *in = fopen(argv[4], "rb"), *out = fopen(argv[5], "wb"), *wf = fopen(argv[6], "wb");
    int16_t frame[LE_VOICE_LOGICAL_CHANNELS];
    unsigned b, k, m;
    if (le_beamformer_init(&bf, mask, beams)) return 2;
    for (b = 0; b < beams; ++b) for (k = 0; k < LE_BEAM_BINS; ++k) for (m = 0; m < LE_VOICE_LOGICAL_CHANNELS; ++m) {
        fwrite(&bf.w_re[b][k][m], 4, 1, wf); fwrite(&bf.w_im[b][k][m], 4, 1, wf);
    }
    if (lock >= 0) le_beamformer_lock(&bf, lock);
    while (fread(frame, sizeof frame, 1, in) == 1) {
        int16_t s = le_beamformer_sample(&bf, frame);
        fwrite(&s, 2, 1, out);
    }
    printf("%d", le_beamformer_selected_azimuth(&bf));
    for (b = 0; b < beams; ++b) printf(" %.9g", le_beamformer_beam_power(&bf, b));
    printf("\n");
    return 0;
}
"#;

    struct CRun {
        out: Vec<i16>,
        weights: Vec<f32>,
        azimuth: i32,
        powers: Vec<f32>,
    }

    fn build_harness(src_dir: &Path, work: &Path) -> Option<PathBuf> {
        let c = work.join("harness.c");
        std::fs::write(&c, HARNESS).unwrap();
        let exe = work.join("harness");
        let st = Command::new("cc")
            .args([
                "-std=c99",
                "-O2",
                "-ffp-contract=off",
                "-D_POSIX_C_SOURCE=200809L",
            ])
            .arg("-I")
            .arg(src_dir)
            .arg(&c)
            .arg(src_dir.join("mic_beamformer.c"))
            .arg("-lm")
            .arg("-o")
            .arg(&exe)
            .status()
            .ok()?;
        assert!(st.success(), "compiling C harness failed");
        Some(exe)
    }

    fn run_c(exe: &Path, work: &Path, mask: u8, beams: usize, lock: i32, frames: &Frames) -> CRun {
        let inp = work.join("in.bin");
        let outp = work.join("out.bin");
        let wp = work.join("w.bin");
        let bytes: Vec<u8> = frames
            .iter()
            .flatten()
            .flat_map(|s| s.to_le_bytes())
            .collect();
        std::fs::write(&inp, bytes).unwrap();
        let o = Command::new(exe)
            .args([mask.to_string(), beams.to_string(), lock.to_string()])
            .args([&inp, &outp, &wp])
            .output()
            .unwrap();
        assert!(o.status.success(), "C harness failed");
        let text = String::from_utf8(o.stdout).unwrap();
        let mut it = text.split_whitespace();
        let azimuth = it.next().unwrap().parse().unwrap();
        let powers = it.map(|p| p.parse().unwrap()).collect();
        CRun {
            out: std::fs::read(&outp)
                .unwrap()
                .chunks_exact(2)
                .map(|c| i16::from_le_bytes([c[0], c[1]]))
                .collect(),
            weights: std::fs::read(&wp)
                .unwrap()
                .chunks_exact(4)
                .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                .collect(),
            azimuth,
            powers,
        }
    }

    #[test]
    fn parity_with_c() {
        let Some(src_dir) = c_source_dir() else {
            eprintln!("SKIP parity_with_c: C sources not found (set PINNA_BEAMFORMER_C_DIR)");
            return;
        };
        let work =
            std::env::temp_dir().join(format!("pinna-beamformer-parity-{}", std::process::id()));
        std::fs::create_dir_all(&work).unwrap();
        let Some(exe) = build_harness(&src_dir, &work) else {
            eprintln!("SKIP parity_with_c: no host `cc`");
            return;
        };

        // Weights, for the default array and the full seven-mic array.
        for (mask, beams) in [(MIC_MASK_BEAM, 12usize), (MIC_MASK_ALL, 8)] {
            let bf = Beamformer::new(mask, beams).unwrap();
            let c = run_c(&exe, &work, mask, beams, -1, &white_noise(HOP, 1.0, 1));
            let mut worst = 0f64;
            let mut idx = 0;
            for b in 0..beams {
                for k in 0..BINS {
                    for m in 0..LOGICAL_CHANNELS {
                        let (re, im) = bf.weight(b, k, m);
                        let (cre, cim) = (c.weights[idx], c.weights[idx + 1]);
                        idx += 2;
                        let mag = f64::from(cre).hypot(f64::from(cim));
                        let err =
                            (f64::from(re) - f64::from(cre)).hypot(f64::from(im) - f64::from(cim));
                        worst = worst.max(err / mag.max(1e-12));
                    }
                }
            }
            println!("weights mask {mask:#x} beams {beams}: max relative error {worst:e}");
            assert!(worst <= 1e-4, "weights differ: {worst}");
        }

        // Signals.
        let n = 16_000 * 3;
        let mut cases: Vec<(String, Frames, i32)> = vec![
            ("white noise".into(), white_noise(n, 3000.0, 5), -1),
            ("silence".into(), vec![[0; LOGICAL_CHANNELS]; n], -1),
        ];
        let src = Source::new(21, 300.0);
        for az in [0.0, 45.0, 100.0, 180.0, 255.0, 330.0] {
            cases.push((
                format!("plane wave {az} deg"),
                burst(&src, az, n, n / 3, 31 + az as u64),
                -1,
            ));
        }
        cases.push((
            "locked beam 90".into(),
            plane_wave(&src, 200.0, n, |_| 1.0),
            90,
        ));
        // Clipping path.
        cases.push(("clipping".into(), plane_wave(&src, 30.0, n, |_| 12.0), -1));

        for (name, frames, lock) in &cases {
            let c = run_c(&exe, &work, MIC_MASK_BEAM, 12, *lock, frames);
            let mut bf = Beamformer::default_echo_dot();
            if *lock >= 0 {
                bf.lock(Some(*lock));
            }
            let r = run(&mut bf, frames);
            assert_eq!(r.len(), c.out.len());
            let exact = r.iter().zip(&c.out).filter(|(a, b)| a == b).count();
            let within = r
                .iter()
                .zip(&c.out)
                .filter(|(a, b)| (i32::from(**a) - i32::from(**b)).abs() <= 1)
                .count();
            let maxd = r
                .iter()
                .zip(&c.out)
                .map(|(a, b)| (i32::from(*a) - i32::from(*b)).abs())
                .max()
                .unwrap();
            println!(
                "{name}: exact {:.4}%, within 1 LSB {:.4}%, max diff {maxd}, azimuth rust {} c {}",
                100.0 * exact as f64 / r.len() as f64,
                100.0 * within as f64 / r.len() as f64,
                bf.selected_azimuth(),
                c.azimuth
            );
            assert!(
                within as f64 >= 0.999 * r.len() as f64,
                "{name}: outputs diverge"
            );
            assert_eq!(bf.selected_azimuth(), c.azimuth, "{name}: selector differs");
            for (b, cp) in c.powers.iter().enumerate() {
                let rp = bf.beam_power(b);
                assert!(
                    (rp - cp).abs() <= 1e-3 * cp.abs().max(1.0),
                    "{name}: beam {b} power {rp} vs {cp}"
                );
            }
            if name.starts_with("plane wave") {
                let az: f64 = name.split_whitespace().nth(2).unwrap().parse().unwrap();
                let err = (f64::from(bf.selected_azimuth()) - az).rem_euclid(360.0);
                assert!(
                    err.min(360.0 - err) <= 45.0,
                    "{name}: selected {}",
                    bf.selected_azimuth()
                );
            }
        }
        let _ = std::fs::remove_dir_all(&work);
    }
}
