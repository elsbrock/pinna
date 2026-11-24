//! Semi-synthetic takes: a talker, and optionally an interferer, rendered as
//! plane waves onto the seven microphone lanes of a real recording, at known
//! times and levels. The echo, the room noise and the loopback stay real, so
//! a take that nobody spoke into gets counted wake words. The talker has no
//! room reflections and reaches every microphone with the same level, which
//! flatters spatial filtering: rank front ends on real talkers.

use crate::fft::{fft, ifft};
use pinna_audio::dsp::LOGICAL_CHANNELS;

const SPEED_OF_SOUND: f64 = 343.0;
const RATE: f64 = 16_000.0;

/// Where the Echo Dot (2nd gen)'s microphones sit, in metres
/// (`pinna_board`, the same table the beamformer is designed from).
pub fn positions() -> [(f64, f64); LOGICAL_CHANNELS] {
    let g = pinna_board::ECHO_DOT_2.mics.geometry;
    std::array::from_fn(|lane| g.position(lane))
}

/// `signal` as each microphone hears a plane wave from `az_deg` (0 = front,
/// the beamformer's convention) and `el_deg` above the horizon. Fractional
/// delays are phase shifts over the whole signal.
pub fn plane_wave(signal: &[f32], az_deg: f64, el_deg: f64) -> [Vec<f32>; LOGICAL_CHANNELS] {
    let n = (signal.len() + 64).next_power_of_two();
    let (mut re, mut im) = (vec![0.0; n], vec![0.0; n]);
    for (r, &s) in re.iter_mut().zip(signal) {
        *r = f64::from(s);
    }
    fft(&mut re, &mut im);
    let (az, el) = (az_deg.to_radians(), el_deg.to_radians());
    let (ux, uy) = (az.cos() * el.cos(), az.sin() * el.cos());
    let pos = positions();
    std::array::from_fn(|m| {
        // A microphone further along the look direction hears the wave
        // earlier (beamformer.rs, `steering`).
        let tau = -(pos[m].0 * ux + pos[m].1 * uy) / SPEED_OF_SOUND;
        let (mut r, mut i) = (re.clone(), im.clone());
        for k in 0..n {
            let f = if k <= n / 2 {
                k as f64
            } else {
                k as f64 - n as f64
            } * RATE
                / n as f64;
            let ph = -2.0 * std::f64::consts::PI * f * tau;
            let (c, s) = (ph.cos(), ph.sin());
            (r[k], i[k]) = (r[k] * c - i[k] * s, r[k] * s + i[k] * c);
        }
        ifft(&mut r, &mut i);
        r[..signal.len()].iter().map(|&v| v as f32).collect()
    })
}

/// Mean square over the frames within 30 dB of the loudest one, and the
/// first and last of those frames (in samples): the speech, not its pauses.
pub fn active(signal: &[f32]) -> (f64, usize, usize) {
    let ms: Vec<f64> = signal
        .chunks(320)
        .map(|c| c.iter().map(|&v| f64::from(v).powi(2)).sum::<f64>() / c.len() as f64)
        .collect();
    let peak = ms.iter().copied().fold(0.0, f64::max);
    let on: Vec<usize> = (0..ms.len()).filter(|&k| ms[k] >= peak * 1e-3).collect();
    let mean = on.iter().map(|&k| ms[k]).sum::<f64>() / on.len().max(1) as f64;
    let first = on.first().copied().unwrap_or(0) * 320;
    let last = (on.last().copied().unwrap_or(0) + 1) * 320;
    (mean, first, last.min(signal.len()))
}

pub struct Source {
    pub signal: Vec<f32>,
    /// Active level at each microphone, dBFS.
    pub level_db: f64,
    pub az_deg: f64,
    pub el_deg: f64,
}

/// Add `source` to the microphones in `alive` of `lanes` (nine per frame),
/// starting at sample `at`. A dead lane stays deaf.
pub fn add(lanes: &mut [[f32; 9]], source: &Source, at: usize, alive: u8) {
    let (ms, _, _) = active(&source.signal);
    let gain = (10f64.powf(source.level_db / 10.0) / ms.max(1e-20)).sqrt() as f32;
    let scaled: Vec<f32> = source.signal.iter().map(|v| v * gain).collect();
    let waves = plane_wave(&scaled, source.az_deg, source.el_deg);
    for (m, wave) in waves.iter().enumerate() {
        if alive & (1 << m) == 0 {
            continue;
        }
        for (i, &v) in wave.iter().enumerate() {
            if let Some(f) = lanes.get_mut(at + i) {
                f[m] = (f[m] + v).clamp(-1.0, 1.0);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wave_from_above_reaches_every_microphone_at_once() {
        let s: Vec<f32> = (0..1000)
            .map(|i| ((i * 13) % 17) as f32 / 17.0 - 0.5)
            .collect();
        let w = plane_wave(&s, 0.0, 90.0);
        for lane in &w {
            for (a, b) in lane.iter().zip(&s) {
                assert!((a - b).abs() < 1e-4);
            }
        }
    }

    #[test]
    fn a_wave_from_a_lanes_bearing_reaches_it_first() {
        // Lane 0 sits at 150 degrees: it leads the centre by r/c, 1.68 samples.
        let mut s = vec![0f32; 2048];
        s[1000] = 1.0;
        let w = plane_wave(&s, 150.0, 0.0);
        let peak = |x: &Vec<f32>| {
            // Centroid of the main lobe, in samples.
            let (mut a, mut b) = (0.0, 0.0);
            for (i, &v) in x.iter().enumerate().skip(990).take(20) {
                let p = f64::from(v).max(0.0).powi(2);
                a += p * i as f64;
                b += p;
            }
            a / b
        };
        let lead = peak(&w[6]) - peak(&w[0]);
        assert!(
            (lead - 0.036 / SPEED_OF_SOUND * RATE).abs() < 0.3,
            "lead {lead}"
        );
    }
}
