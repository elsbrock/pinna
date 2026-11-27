//! Kaldi-compatible log-mel filterbank, as kaldi-native-fbank computes it with the settings
//! sherpa-onnx uses for speaker embedding models: 16 kHz, 25 ms frames every 10 ms, no snip-edges
//! (frames centred, edges reflected), DC offset removed, pre-emphasis 0.97, Povey window, 512-point
//! FFT, power spectrum, 80 triangular mel bins from 20 Hz to 7600 Hz, natural log floored at f32::EPSILON.
use std::f32::consts::PI;
use std::sync::Arc;

use rustfft::num_complex::Complex32;
use rustfft::{Fft, FftPlanner};

pub const SAMPLE_RATE: usize = 16_000;
pub const SHIFT: usize = 160;
pub const LENGTH: usize = 400;
pub const PADDED: usize = 512;
pub const BINS: usize = 80;
const LOW_HZ: f32 = 20.0;
const HIGH_HZ: f32 = 7600.0;
const PREEMPH: f32 = 0.97;

pub struct Fbank {
    window: Vec<f32>,
    /// Per mel bin: first FFT bin and the weights from there.
    banks: Vec<(usize, Vec<f32>)>,
    fft: Arc<dyn Fft<f32>>,
}

fn mel(hz: f32) -> f32 {
    1127.0 * (1.0 + hz / 700.0).ln()
}

impl Default for Fbank {
    fn default() -> Self {
        Self::new()
    }
}

impl Fbank {
    pub fn new() -> Self {
        let a = 2.0 * PI / (LENGTH as f32 - 1.0);
        let window = (0..LENGTH)
            .map(|i| (0.5 - 0.5 * (a * i as f32).cos()).powf(0.85))
            .collect();
        let fft_bins = PADDED / 2;
        let bin_hz = SAMPLE_RATE as f32 / PADDED as f32;
        let (lo, hi) = (mel(LOW_HZ), mel(HIGH_HZ));
        let delta = (hi - lo) / (BINS as f32 + 1.0);
        let banks = (0..BINS)
            .map(|b| {
                let left = lo + b as f32 * delta;
                let centre = lo + (b as f32 + 1.0) * delta;
                let right = lo + (b as f32 + 2.0) * delta;
                let mut first = None;
                let mut weights = Vec::new();
                for i in 0..fft_bins {
                    let m = mel(bin_hz * i as f32);
                    if m > left && m < right {
                        first.get_or_insert(i);
                        weights.push(if m <= centre {
                            (m - left) / (centre - left)
                        } else {
                            (right - m) / (right - centre)
                        });
                    }
                }
                (first.unwrap_or(0), weights)
            })
            .collect();
        let fft = FftPlanner::new().plan_fft_forward(PADDED);
        Self { window, banks, fft }
    }

    pub fn frames(samples: usize) -> usize {
        (samples + SHIFT / 2) / SHIFT
    }

    /// Features for a whole utterance, `frames(samples.len())` rows of `BINS`.
    pub fn compute(&self, samples: &[f32]) -> Vec<f32> {
        let n = samples.len() as isize;
        let frames = Self::frames(samples.len());
        let mut out = Vec::with_capacity(frames * BINS);
        let mut w = vec![0f32; LENGTH];
        let mut buf = vec![Complex32::default(); PADDED];
        let mut scratch = vec![Complex32::default(); self.fft.get_inplace_scratch_len()];
        for f in 0..frames {
            let start = (f * SHIFT + SHIFT / 2) as isize - (LENGTH / 2) as isize;
            for (s, slot) in w.iter_mut().enumerate() {
                let mut i = start + s as isize;
                while i < 0 || i >= n {
                    i = if i < 0 { -i - 1 } else { 2 * n - 1 - i };
                }
                *slot = samples[i as usize];
            }
            let mean = w.iter().sum::<f32>() / LENGTH as f32;
            w.iter_mut().for_each(|x| *x -= mean);
            for i in (1..LENGTH).rev() {
                w[i] -= PREEMPH * w[i - 1];
            }
            w[0] -= PREEMPH * w[0];
            for (i, c) in buf.iter_mut().enumerate() {
                *c = Complex32::new(
                    if i < LENGTH {
                        w[i] * self.window[i]
                    } else {
                        0.0
                    },
                    0.0,
                );
            }
            self.fft.process_with_scratch(&mut buf, &mut scratch);
            for (first, weights) in &self.banks {
                let e: f32 = weights
                    .iter()
                    .enumerate()
                    .map(|(k, wt)| wt * buf[first + k].norm_sqr())
                    .sum();
                out.push(e.max(f32::EPSILON).ln());
            }
        }
        out
    }
}

/// Subtracts each bin's mean over the utterance (3D-Speaker's "global-mean").
pub fn subtract_mean(feats: &mut [f32]) {
    let frames = feats.len() / BINS;
    if frames == 0 {
        return;
    }
    let mut mean = [0f32; BINS];
    for row in feats.chunks(BINS) {
        mean.iter_mut().zip(row).for_each(|(m, x)| *m += x);
    }
    mean.iter_mut().for_each(|m| *m /= frames as f32);
    for row in feats.chunks_mut(BINS) {
        row.iter_mut().zip(&mean).for_each(|(x, m)| *x -= m);
    }
}
