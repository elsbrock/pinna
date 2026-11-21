//! Linear echo canceller for the wake word's stream (ADR 0033).
//!
//! Partitioned-block frequency-domain NLMS (overlap-save): 128-sample blocks
//! and 16 partitions, so 2048 taps or 128 ms at 16 kHz. Two filters run on
//! the same reference: the background one adapts on every block, and the
//! foreground one produces the output and takes the background's
//! coefficients only after they have cancelled more for several blocks in a
//! row. Double talk can then disturb the adapting filter but not the output.
//! There is no suppressor: what the linear filter leaves stays in the output,
//! and so does the talker.
//!
//! The parameters are the offline prototype's that measured, on an Echo Dot
//! (2nd gen) on 2026-11-21, 14-20 dB less echo at volumes 50 to 80 with the
//! microphones' loopback lane as the reference.
// Index loops over partitions and bins read like the algorithm; keep them.
#![allow(clippy::needless_range_loop)]

use crate::beamformer::{FFT, fft, twiddles};

/// Samples per block: half the FFT. The output lags the input by one block.
pub const BLOCK: usize = FFT / 2;
const BINS: usize = FFT / 2 + 1;
/// Partitions of one block each: 2048 taps.
pub const PARTITIONS: usize = 16;
/// NLMS step size.
const MU: f32 = 0.5;
/// Smoothing of the reference's power per bin, per block.
const POWER_SMOOTHING: f32 = 0.9;
/// Keeps the step finite while the reference is silent (the loopback lane is
/// exactly zero then). Power on the 16-bit scale.
const REGULARISATION: f32 = 0.1;
/// Smoothing of the block energies the two-filter rule compares.
const LEVEL_SMOOTHING: f32 = 0.7;
/// The adapting filter takes over when its error is this much smaller than
/// the output's...
const TAKE_OVER_RATIO: f32 = 0.7;
/// ...for this many blocks in a row.
const TAKE_OVER_BLOCKS: u32 = 4;
/// The adapting filter restarts from the output's when its error grows this
/// far above the microphone signal.
const DIVERGED_RATIO: f32 = 4.0;

type Spectra = [[f32; BINS]; PARTITIONS];

/// A filter: one complex spectrum per partition.
#[derive(Clone)]
struct Filter {
    re: Spectra,
    im: Spectra,
}

impl Filter {
    fn zero() -> Self {
        Filter {
            re: [[0.0; BINS]; PARTITIONS],
            im: [[0.0; BINS]; PARTITIONS],
        }
    }
}

pub struct LinearEchoCanceller {
    twiddle_re: [f32; FFT / 2],
    twiddle_im: [f32; FFT / 2],
    /// Reference spectra of the last blocks; partition `p` is at
    /// `(newest + p) % PARTITIONS`.
    x_re: Spectra,
    x_im: Spectra,
    newest: usize,
    background: Filter,
    foreground: Filter,
    power: [f32; BINS],
    previous_reference: [f32; BLOCK],
    mic: [f32; BLOCK],
    reference: [f32; BLOCK],
    output: [f32; BLOCK],
    fill: usize,
    foreground_error: f32,
    background_error: f32,
    mic_level: f32,
    better_blocks: u32,
}

impl Default for LinearEchoCanceller {
    fn default() -> Self {
        Self::new()
    }
}

impl LinearEchoCanceller {
    pub fn new() -> Self {
        let (twiddle_re, twiddle_im) = twiddles();
        LinearEchoCanceller {
            twiddle_re,
            twiddle_im,
            x_re: [[0.0; BINS]; PARTITIONS],
            x_im: [[0.0; BINS]; PARTITIONS],
            newest: 0,
            background: Filter::zero(),
            foreground: Filter::zero(),
            power: [0.0; BINS],
            previous_reference: [0.0; BLOCK],
            mic: [0.0; BLOCK],
            reference: [0.0; BLOCK],
            output: [0.0; BLOCK],
            fill: 0,
            foreground_error: 1e-12,
            background_error: 1e-12,
            mic_level: 1e-12,
            better_blocks: 0,
        }
    }

    /// One sample of the microphone signal and of the reference, on the
    /// 16-bit scale. Returns the microphone signal minus the echo estimate,
    /// [`BLOCK`] samples late.
    pub fn process(&mut self, mic: f32, reference: f32) -> f32 {
        let out = self.output[self.fill];
        self.mic[self.fill] = mic;
        self.reference[self.fill] = reference;
        self.fill += 1;
        if self.fill == BLOCK {
            self.fill = 0;
            self.block();
        }
        out
    }

    /// Spectrum of a real signal of `FFT` samples, bins 0..=FFT/2.
    fn real_spectrum(
        &self,
        signal: &[f32; FFT],
        re_out: &mut [f32; BINS],
        im_out: &mut [f32; BINS],
    ) {
        let mut re = *signal;
        let mut im = [0f32; FFT];
        fft(&self.twiddle_re, &self.twiddle_im, &mut re, &mut im);
        re_out.copy_from_slice(&re[..BINS]);
        im_out.copy_from_slice(&im[..BINS]);
    }

    /// Inverse of [`real_spectrum`](Self::real_spectrum), by the conjugate
    /// trick on the Hermitian spectrum, as the beamformer does it.
    fn real_signal(&self, re_in: &[f32; BINS], im_in: &[f32; BINS]) -> [f32; FFT] {
        let mut re = [0f32; FFT];
        let mut im = [0f32; FFT];
        for k in 0..BINS {
            re[k] = re_in[k];
            im[k] = -im_in[k];
        }
        im[0] = 0.0;
        im[FFT / 2] = 0.0;
        for k in BINS..FFT {
            re[k] = re_in[FFT - k];
            im[k] = im_in[FFT - k];
        }
        fft(&self.twiddle_re, &self.twiddle_im, &mut re, &mut im);
        for v in re.iter_mut() {
            *v /= FFT as f32;
        }
        re
    }

    /// The echo estimate for the current block: the last half of the
    /// filter's output (overlap-save).
    fn estimate(&self, w: &Filter) -> [f32; BLOCK] {
        let mut yr = [0f32; BINS];
        let mut yi = [0f32; BINS];
        for p in 0..PARTITIONS {
            let slot = (self.newest + p) % PARTITIONS;
            let (xr, xi) = (&self.x_re[slot], &self.x_im[slot]);
            let (wr, wi) = (&w.re[p], &w.im[p]);
            for k in 0..BINS {
                yr[k] += wr[k] * xr[k] - wi[k] * xi[k];
                yi[k] += wr[k] * xi[k] + wi[k] * xr[k];
            }
        }
        let y = self.real_signal(&yr, &yi);
        let mut out = [0f32; BLOCK];
        out.copy_from_slice(&y[BLOCK..]);
        out
    }

    fn block(&mut self) {
        // The newest reference spectrum: the previous block and this one.
        let mut frame = [0f32; FFT];
        frame[..BLOCK].copy_from_slice(&self.previous_reference);
        frame[BLOCK..].copy_from_slice(&self.reference);
        self.previous_reference = self.reference;
        self.newest = (self.newest + PARTITIONS - 1) % PARTITIONS;
        let (mut xr, mut xi) = ([0f32; BINS], [0f32; BINS]);
        self.real_spectrum(&frame, &mut xr, &mut xi);
        self.x_re[self.newest] = xr;
        self.x_im[self.newest] = xi;
        for k in 0..BINS {
            self.power[k] = POWER_SMOOTHING * self.power[k]
                + (1.0 - POWER_SMOOTHING) * (xr[k] * xr[k] + xi[k] * xi[k]);
        }

        let yf = self.estimate(&self.foreground);
        let yb = self.estimate(&self.background);
        let mut eb = [0f32; BLOCK];
        let (mut ef_energy, mut eb_energy, mut mic_energy) = (0f32, 0f32, 0f32);
        for i in 0..BLOCK {
            let ef = self.mic[i] - yf[i];
            eb[i] = self.mic[i] - yb[i];
            self.output[i] = ef;
            ef_energy += ef * ef;
            eb_energy += eb[i] * eb[i];
            mic_energy += self.mic[i] * self.mic[i];
        }

        // Background update, the gradient constrained to one block of taps
        // per partition.
        let mut error_frame = [0f32; FFT];
        error_frame[BLOCK..].copy_from_slice(&eb);
        let (mut er, mut ei) = ([0f32; BINS], [0f32; BINS]);
        self.real_spectrum(&error_frame, &mut er, &mut ei);
        let mut norm = [0f32; BINS];
        for k in 0..BINS {
            norm[k] = 1.0 / (PARTITIONS as f32 * self.power[k] + REGULARISATION);
        }
        for p in 0..PARTITIONS {
            let slot = (self.newest + p) % PARTITIONS;
            let (mut gr, mut gi) = ([0f32; BINS], [0f32; BINS]);
            for k in 0..BINS {
                // conj(X) * E, normalised.
                let (xr, xi) = (self.x_re[slot][k], self.x_im[slot][k]);
                gr[k] = (xr * er[k] + xi * ei[k]) * norm[k];
                gi[k] = (xr * ei[k] - xi * er[k]) * norm[k];
            }
            let mut g = self.real_signal(&gr, &gi);
            g[BLOCK..].fill(0.0);
            self.real_spectrum(&g, &mut gr, &mut gi);
            for k in 0..BINS {
                self.background.re[p][k] += MU * gr[k];
                self.background.im[p][k] += MU * gi[k];
            }
        }

        // Two filters: the output takes the adapting filter's coefficients
        // only while they cancel clearly more.
        let n = BLOCK as f32;
        let a = LEVEL_SMOOTHING;
        self.foreground_error = a * self.foreground_error + (1.0 - a) * ef_energy / n;
        self.background_error = a * self.background_error + (1.0 - a) * eb_energy / n;
        self.mic_level = a * self.mic_level + (1.0 - a) * mic_energy / n;
        if self.background_error < TAKE_OVER_RATIO * self.foreground_error
            && self.background_error < self.mic_level
        {
            self.better_blocks += 1;
            if self.better_blocks >= TAKE_OVER_BLOCKS {
                self.foreground = self.background.clone();
                self.better_blocks = 0;
            }
        } else {
            self.better_blocks = 0;
        }
        if self.background_error > DIVERGED_RATIO * self.mic_level {
            self.background = self.foreground.clone();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic white noise on the 16-bit scale.
    struct Noise(u64);
    impl Noise {
        fn next(&mut self, amp: f32) -> f32 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((self.0 >> 40) as f32 / (1u64 << 24) as f32 - 0.5) * 2.0 * amp
        }
    }

    /// A room-like echo path: a delay, a few reflections and a decaying tail.
    fn echo_path() -> Vec<f32> {
        let mut h = vec![0f32; 1200];
        h[40] = 0.5;
        h[41] = -0.3;
        h[95] = 0.2;
        h[300] = -0.1;
        let mut n = Noise(7);
        for (i, v) in h.iter_mut().enumerate().skip(100) {
            *v += n.next(0.02) * (-(i as f32) / 300.0).exp();
        }
        h
    }

    fn convolve(x: &[f32], h: &[f32]) -> Vec<f32> {
        (0..x.len())
            .map(|n| (0..h.len().min(n + 1)).map(|k| h[k] * x[n - k]).sum())
            .collect()
    }

    fn db(v: &[f32]) -> f32 {
        10.0 * (v.iter().map(|x| x * x).sum::<f32>() / v.len() as f32 + 1e-9).log10()
    }

    #[test]
    fn without_a_reference_the_microphone_passes_one_block_late() {
        let mut aec = LinearEchoCanceller::new();
        let mut n = Noise(1);
        let mic: Vec<f32> = (0..4 * BLOCK).map(|_| n.next(1000.0)).collect();
        let out: Vec<f32> = mic.iter().map(|&m| aec.process(m, 0.0)).collect();
        assert!(out[..BLOCK].iter().all(|&v| v == 0.0));
        assert_eq!(&out[BLOCK..], &mic[..3 * BLOCK]);
    }

    #[test]
    fn it_converges_on_an_echo_path() {
        let mut aec = LinearEchoCanceller::new();
        let mut n = Noise(2);
        let reference: Vec<f32> = (0..3 * 16_000).map(|_| n.next(8000.0)).collect();
        let echo = convolve(&reference, &echo_path());
        let out: Vec<f32> = echo
            .iter()
            .zip(&reference)
            .map(|(&m, &x)| aec.process(m, x))
            .collect();
        // The last second, against the echo it replaced (one block earlier).
        let removed = db(&echo[2 * 16_000 - BLOCK..3 * 16_000 - BLOCK]) - db(&out[2 * 16_000..]);
        assert!(removed > 25.0, "removed {removed:.1} dB");
    }

    #[test]
    fn double_talk_keeps_the_talker_and_the_learned_echo_path() {
        let mut aec = LinearEchoCanceller::new();
        let (mut n, mut t) = (Noise(3), Noise(4));
        let len = 5 * 16_000;
        let reference: Vec<f32> = (0..len).map(|_| n.next(8000.0)).collect();
        let echo = convolve(&reference, &echo_path());
        // A talker as loud as the echo, from 3 s to 4 s.
        let talker: Vec<f32> = (0..len)
            .map(|i| {
                if (48_000..64_000).contains(&i) {
                    t.next(4000.0)
                } else {
                    0.0
                }
            })
            .collect();
        let out: Vec<f32> = (0..len)
            .map(|i| aec.process(echo[i] + talker[i], reference[i]))
            .collect();
        // While the talker speaks, the output is the talker.
        let during = db(&out[48_000 + BLOCK + 1600..64_000 + BLOCK]);
        let talk = db(&talker[48_000 + 1600..64_000]);
        assert!(
            (during - talk).abs() < 1.0,
            "talker {talk:.1} dB, output {during:.1} dB"
        );
        // Afterwards the echo is still cancelled.
        let removed = db(&echo[64_000 + 1600..len - BLOCK]) - db(&out[64_000 + BLOCK + 1600..]);
        assert!(removed > 20.0, "removed {removed:.1} dB after double talk");
    }
}
