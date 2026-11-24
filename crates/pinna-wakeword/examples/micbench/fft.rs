//! A radix-2 FFT, and power per band on 20 ms frames.

use std::f64::consts::PI;

pub const RATE: f64 = 16_000.0;
/// Analysis frames: a 512-point Hann window every 320 samples (20 ms), so
/// frame `k` starts where pinnad's 20 ms frame `k` starts.
pub const HOP: usize = 320;
pub const WIN: usize = 512;

/// In place; `re.len()` must be a power of two.
pub fn fft(re: &mut [f64], im: &mut [f64]) {
    let n = re.len();
    assert!(n.is_power_of_two() && im.len() == n);
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
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
    while len <= n {
        let half = len / 2;
        for k in 0..half {
            let a = -2.0 * PI * k as f64 / len as f64;
            let (wr, wi) = (a.cos(), a.sin());
            let mut start = 0;
            while start < n {
                let (p, q) = (start + k, start + k + half);
                let tr = re[q] * wr - im[q] * wi;
                let ti = re[q] * wi + im[q] * wr;
                re[q] = re[p] - tr;
                im[q] = im[p] - ti;
                re[p] += tr;
                im[p] += ti;
                start += len;
            }
        }
        len <<= 1;
    }
}

pub fn ifft(re: &mut [f64], im: &mut [f64]) {
    for v in im.iter_mut() {
        *v = -*v;
    }
    fft(re, im);
    let n = re.len() as f64;
    for (r, i) in re.iter_mut().zip(im.iter_mut()) {
        *r /= n;
        *i = -*i / n;
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Band {
    pub name: &'static str,
    pub lo: f64,
    pub hi: f64,
}

/// TECHO5's three bands (`techo5-dot/docs/microphones.md`): below 1.5 kHz the
/// mics hear almost the same diffuse noise, 1.5-4.7 kHz is where spatial
/// filtering can work, and 4.7 kHz is where the 36 mm spacing aliases. The
/// fourth is the speech band the hardware gate measures echo in (ADR 0004).
pub const BANDS: [Band; 4] = [
    Band {
        name: "<1.5k",
        lo: 100.0,
        hi: 1500.0,
    },
    Band {
        name: "1.5-4.7k",
        lo: 1500.0,
        hi: 4700.0,
    },
    Band {
        name: ">4.7k",
        lo: 4700.0,
        hi: 7900.0,
    },
    Band {
        name: "300-3400",
        lo: 300.0,
        hi: 3400.0,
    },
];
pub const SPEECH: usize = 3;

pub fn db(p: f64) -> f64 {
    10.0 * p.max(1e-20).log10()
}

pub fn frame_count(len: usize) -> usize {
    if len < WIN { 0 } else { (len - WIN) / HOP + 1 }
}

/// The middle of frame `k`, in seconds.
pub fn frame_centre(k: usize) -> f64 {
    (k * HOP + WIN / 2) as f64 / RATE
}

/// Mean square per band (`[band][frame]`) on the signal's own scale: a sine
/// of amplitude 1 inside a band reads 0.5 (-3 dB), as the AGC counts levels.
pub fn band_powers(x: &[f32]) -> [Vec<f64>; 4] {
    let n = frame_count(x.len());
    let w: Vec<f64> = (0..WIN)
        .map(|i| 0.5 - 0.5 * (2.0 * PI * i as f64 / WIN as f64).cos())
        .collect();
    let wsum: f64 = w.iter().map(|v| v * v).sum();
    let bins: Vec<(usize, usize)> = BANDS
        .iter()
        .map(|b| {
            (
                (b.lo * WIN as f64 / RATE).ceil() as usize,
                (b.hi * WIN as f64 / RATE).floor() as usize,
            )
        })
        .collect();
    let mut p: [Vec<f64>; 4] = std::array::from_fn(|_| vec![0.0; n]);
    let (mut re, mut im) = (vec![0.0; WIN], vec![0.0; WIN]);
    for k in 0..n {
        for i in 0..WIN {
            re[i] = f64::from(x[k * HOP + i]) * w[i];
            im[i] = 0.0;
        }
        fft(&mut re, &mut im);
        for (b, &(lo, hi)) in bins.iter().enumerate() {
            let s: f64 = (lo..=hi).map(|j| re[j] * re[j] + im[j] * im[j]).sum();
            // One-sided: each bin below Nyquist stands for two.
            p[b][k] = 2.0 * s / (WIN as f64 * wsum);
        }
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inverse_undoes_forward() {
        let x: Vec<f64> = (0..64).map(|i| ((i * 37) % 11) as f64 - 5.0).collect();
        let (mut re, mut im) = (x.clone(), vec![0.0; 64]);
        fft(&mut re, &mut im);
        ifft(&mut re, &mut im);
        for (a, b) in re.iter().zip(&x) {
            assert!((a - b).abs() < 1e-9);
        }
    }

    #[test]
    fn a_full_scale_sine_reads_minus_3_db_in_its_band() {
        let x: Vec<f32> = (0..16_000)
            .map(|i| (2.0 * PI * 1000.0 * i as f64 / RATE).sin() as f32)
            .collect();
        let p = band_powers(&x);
        let mean = |v: &Vec<f64>| v.iter().sum::<f64>() / v.len() as f64;
        assert!((db(mean(&p[0])) + 3.01).abs() < 0.1, "{}", db(mean(&p[0])));
        assert!(db(mean(&p[2])) < -60.0);
    }
}
