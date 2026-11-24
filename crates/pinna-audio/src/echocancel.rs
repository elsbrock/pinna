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
//! microphones' loopback lane as the reference, except the step (ADR 0033,
//! amended 2026-11-24).
// Index loops over partitions and bins read like the algorithm; keep them.
#![allow(clippy::needless_range_loop)]

use crate::beamformer::{FFT, fft, twiddles};

/// Samples per block: half the FFT. The output lags the input by one block.
pub const BLOCK: usize = FFT / 2;
const BINS: usize = FFT / 2 + 1;
/// Partitions of one block each: 2048 taps.
pub const PARTITIONS: usize = 16;
/// NLMS step size. At 0.5 a talker over music derailed the filter: on the
/// Echo Dot (2nd gen)'s recordings of 2026-11-21, with a talker at -45 dBFS,
/// the output stayed 7-13 dB louder than the echo for the next 30 s. At 0.1 it
/// removed 22-24 dB (micbench, ADR 0033). TECHO5's time-domain NLMS caught 72
/// of 127 wake words over music at 0.1 and 36 at 0.5
/// (`echod/internal/hardware/mic/cancel.go`). The cost is speed: about 3 dB
/// more each second on noise, where 0.5 needs under 3 s for 25 dB.
pub const STEP_SIZE: f32 = 0.1;
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
    mu: f32,
}

impl Default for LinearEchoCanceller {
    fn default() -> Self {
        Self::new()
    }
}

impl LinearEchoCanceller {
    pub fn new() -> Self {
        Self::with_step_size(STEP_SIZE)
    }

    /// A canceller whose adapting filter takes steps of `mu` (0 < mu <= 1)
    /// instead of [`STEP_SIZE`].
    pub fn with_step_size(mu: f32) -> Self {
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
            mu: mu.clamp(f32::MIN_POSITIVE, 1.0),
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
                self.background.re[p][k] += self.mu * gr[k];
                self.background.im[p][k] += self.mu * gi[k];
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
        let reference: Vec<f32> = (0..10 * 16_000).map(|_| n.next(8000.0)).collect();
        let echo = convolve(&reference, &echo_path());
        let out: Vec<f32> = echo
            .iter()
            .zip(&reference)
            .map(|(&m, &x)| aec.process(m, x))
            .collect();
        // The last second, against the echo it replaced (one block earlier).
        let removed = db(&echo[9 * 16_000 - BLOCK..10 * 16_000 - BLOCK]) - db(&out[9 * 16_000..]);
        assert!(removed > 25.0, "removed {removed:.1} dB");
    }

    #[test]
    fn the_step_size_sets_how_fast_it_converges() {
        let mut n = Noise(5);
        let reference: Vec<f32> = (0..3 * 16_000).map(|_| n.next(8000.0)).collect();
        let echo = convolve(&reference, &echo_path());
        let third_second = |mut aec: LinearEchoCanceller| {
            let out: Vec<f32> = echo
                .iter()
                .zip(&reference)
                .map(|(&m, &x)| aec.process(m, x))
                .collect();
            db(&echo[2 * 16_000 - BLOCK..3 * 16_000 - BLOCK]) - db(&out[2 * 16_000..])
        };
        let fast = third_second(LinearEchoCanceller::with_step_size(0.5));
        let default = third_second(LinearEchoCanceller::new());
        assert!(fast > 25.0, "0.5: {fast:.1} dB");
        assert!(default < 20.0, "{STEP_SIZE}: {default:.1} dB");
    }

    /// Music-like: four tones, new ones every 250 ms, over faint noise, so
    /// each frequency bin is loud for a while and then nearly silent.
    fn chords(len: usize, seed: u64) -> Vec<f32> {
        let (mut n, mut pick) = (Noise(seed), Noise(seed + 1));
        let mut freqs = [0f32; 4];
        let mut phase = [0f32; 4];
        (0..len)
            .map(|i| {
                if i % 4000 == 0 {
                    for f in &mut freqs {
                        *f = 150.0 + (pick.next(1.0) + 1.0) * 1500.0;
                    }
                }
                let mut x = n.next(1.0);
                for k in 0..4 {
                    phase[k] += 2.0 * std::f32::consts::PI * freqs[k] / 16_000.0;
                    x += 2500.0 * phase[k].sin();
                }
                x
            })
            .collect()
    }

    /// What `mu` removes of the echo in the 2 s after a talker spoke over
    /// music from 6 s to 7 s, in dB (negative: the output is louder).
    fn after_double_talk_over_music(mu: f32, seed: u64) -> f32 {
        let len = 8 * 16_000;
        let reference = chords(len, seed);
        let mut h = vec![0f32; 96];
        (h[40], h[41], h[95]) = (0.5, -0.3, 0.2);
        let echo = convolve(&reference, &h);
        let mut t = Noise(23);
        let mut aec = LinearEchoCanceller::with_step_size(mu);
        let out: Vec<f32> = (0..len)
            .map(|i| {
                let talker = if (4 * 16_000..5 * 16_000).contains(&i) {
                    t.next(6000.0)
                } else {
                    0.0
                };
                aec.process(echo[i] + talker, reference[i])
            })
            .collect();
        db(&echo[6 * 16_000 - BLOCK..len - BLOCK]) - db(&out[6 * 16_000..])
    }

    /// Talking over music must not leave the filter making the echo louder.
    /// At a step of 0.5 it did, by 7-13 dB on an Echo Dot (2nd gen)'s
    /// recordings (ADR 0033); on these music-like references 0.5 leaves the
    /// echo 9.5 dB louder at worst and 4.4 dB louder in the median.
    #[test]
    fn talking_over_music_doesnt_derail_it() {
        let mut after: Vec<f32> = (0..8u64)
            .map(|k| after_double_talk_over_music(STEP_SIZE, 100 + 7 * k))
            .collect();
        after.sort_by(f32::total_cmp);
        assert!(after[0] > -3.0, "removed after the talker: {after:?}");
        assert!(after[3] > -1.0, "removed after the talker: {after:?}");
    }

    #[test]
    fn double_talk_keeps_the_talker_and_the_learned_echo_path() {
        let mut aec = LinearEchoCanceller::new();
        let (mut n, mut t) = (Noise(3), Noise(4));
        let len = 12 * 16_000;
        let reference: Vec<f32> = (0..len).map(|_| n.next(8000.0)).collect();
        let echo = convolve(&reference, &echo_path());
        // A talker as loud as the echo, from 9 s to 10 s.
        let talker: Vec<f32> = (0..len)
            .map(|i| {
                if (144_000..160_000).contains(&i) {
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
        let during = db(&out[144_000 + BLOCK + 1600..160_000 + BLOCK]);
        let talk = db(&talker[144_000 + 1600..160_000]);
        assert!(
            (during - talk).abs() < 1.0,
            "talker {talk:.1} dB, output {during:.1} dB"
        );
        // Afterwards the echo is still cancelled.
        let removed = db(&echo[160_000 + 1600..len - BLOCK]) - db(&out[160_000 + BLOCK + 1600..]);
        assert!(removed > 20.0, "removed {removed:.1} dB after double talk");
    }
}
