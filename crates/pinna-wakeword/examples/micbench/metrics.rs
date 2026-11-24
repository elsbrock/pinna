//! What the bench measures, on 20 ms frames. As TECHO5 measured it
//! (`techo5-dot/docs/microphones.md`): the talker's frames against the
//! frames between talkers, in three bands. During playback those frames
//! hold the residual echo, so the same number is the talker's margin over
//! the residual, which is what wake word detection tracked.

use crate::fft::{SPEECH, db, frame_centre};

/// A talker's utterance, in seconds.
pub type Cue = (f64, f64);

/// The loopback counts as playing above this mean square (-75 dBFS).
const PLAYING: f64 = 3.16e-8;
/// After playback stops, the room still rings; keep counting it as playback.
const PLAYBACK_HOLD_FRAMES: usize = 15;
/// Frames this close to a talker are neither talker nor residual.
const MARGIN_BEFORE: f64 = 0.5;
const MARGIN_AFTER: f64 = 1.0;
/// Skipped at the start of the take, and after playback starts, while
/// cancellers adapt.
const SETTLE: f64 = 1.0;
const CONVERGE: f64 = 3.0;

pub struct Frames {
    /// Inside an utterance.
    pub talk: Vec<bool>,
    /// The talker's utterance each frame belongs to.
    pub turn: Vec<Option<usize>>,
    /// The loopback plays (with a hold).
    pub playback: Vec<bool>,
    /// Away from talkers, settled, and during playback if the take has any.
    pub residual: Vec<bool>,
    /// Playback without a talker, after the canceller had time to adapt.
    pub echo: Vec<bool>,
    pub has_playback: bool,
}

/// Classify `n` analysis frames. `reference_ms` is the loopback's mean
/// square per frame.
pub fn classify(n: usize, cues: &[Cue], reference_ms: &[f64]) -> Frames {
    let active: Vec<bool> = (0..n)
        .map(|k| reference_ms.get(k).is_some_and(|&p| p > PLAYING))
        .collect();
    let playback: Vec<bool> = (0..n)
        .map(|k| {
            active[k.saturating_sub(PLAYBACK_HOLD_FRAMES)..=k]
                .iter()
                .any(|&a| a)
        })
        .collect();
    let has_playback = playback.iter().filter(|&&p| p).count() * 10 > n;
    let start = active.iter().position(|&a| a).map(frame_centre);
    let mut f = Frames {
        talk: vec![false; n],
        turn: vec![None; n],
        playback,
        residual: vec![false; n],
        echo: vec![false; n],
        has_playback,
    };
    for k in 0..n {
        let t = frame_centre(k);
        let inside = cues.iter().position(|&(s, e)| t >= s && t <= e);
        f.talk[k] = inside.is_some();
        f.turn[k] = inside;
        let near = cues
            .iter()
            .any(|&(s, e)| t >= s - MARGIN_BEFORE && t <= e + MARGIN_AFTER);
        let settled = start.is_some_and(|s| t >= s + CONVERGE);
        f.echo[k] = f.playback[k] && !near && settled;
        f.residual[k] = !near && t >= SETTLE && if has_playback { f.echo[k] } else { true };
    }
    f
}

fn mean_where(p: &[f64], mask: impl Fn(usize) -> bool) -> Option<f64> {
    let (mut s, mut n) = (0.0, 0usize);
    for (k, &v) in p.iter().enumerate() {
        if mask(k) {
            s += v;
            n += 1;
        }
    }
    (n > 0).then(|| s / n as f64)
}

/// The talker over what surrounds it, dB, power-subtracted so that noise in
/// the talker's frames doesn't count as talker. Floors at -30 dB.
pub fn snr(
    p: &[f64],
    talk: impl Fn(usize) -> bool,
    residual: impl Fn(usize) -> bool,
) -> Option<f64> {
    let t = mean_where(p, talk)?;
    let r = mean_where(p, residual)?;
    Some(db((t - r).max(r * 1e-3) / r))
}

/// Echo removed in the speech band over the echo frames: overall, and the
/// median of each second's.
pub fn erle(before: &[Vec<f64>; 4], after: &[Vec<f64>; 4], echo: &[bool]) -> Option<(f64, f64)> {
    let (b, a) = (&before[SPEECH], &after[SPEECH]);
    let (mut sb, mut sa) = (0.0, 0.0);
    let mut per_second: Vec<(f64, f64)> = Vec::new();
    for k in 0..echo.len().min(b.len()).min(a.len()) {
        if !echo[k] {
            continue;
        }
        sb += b[k];
        sa += a[k];
        let s = frame_centre(k) as usize;
        if per_second.len() <= s {
            per_second.resize(s + 1, (0.0, 0.0));
        }
        per_second[s].0 += b[k];
        per_second[s].1 += a[k];
    }
    if sb == 0.0 {
        return None;
    }
    let mut each: Vec<f64> = per_second
        .iter()
        .filter(|(b, _)| *b > 0.0)
        .map(|&(b, a)| db(b) - db(a))
        .collect();
    each.sort_by(f64::total_cmp);
    Some((db(sb) - db(sa), each[each.len() / 2]))
}

/// Mean of `p` over `mask`, dB.
pub fn level(p: &[f64], mask: &[bool]) -> Option<f64> {
    mean_where(p, |k| mask.get(k).copied().unwrap_or(false)).map(db)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fft::{BANDS, band_powers};

    struct Noise(u64);
    impl Noise {
        fn next(&mut self) -> f32 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (self.0 >> 40) as f32 / (1u64 << 24) as f32 - 0.5
        }
    }

    /// White noise at -60 dB, a white "talker" 20 dB louder from 2 s to 3 s:
    /// every band reads 20 dB, because both are flat.
    #[test]
    fn snr_reads_back_the_ratio_put_in() {
        let mut n = Noise(3);
        let x: Vec<f32> = (0..5 * 16_000)
            .map(|i| {
                let talker = if (32_000..48_000).contains(&i) {
                    10.0
                } else {
                    0.0
                };
                n.next() * 0.0035 * (1.0 + talker)
            })
            .collect();
        // Amplitude 11x is 20.8 dB more power; minus the noise, 20.7 dB.
        let want = db(11f64.powi(2) - 1.0);
        let p = band_powers(&x);
        let f = classify(p[0].len(), &[(2.0, 3.0)], &[]);
        for (b, band) in BANDS.iter().enumerate() {
            let got = snr(&p[b], |k| f.talk[k], |k| f.residual[k]).unwrap();
            assert!((got - want).abs() < 1.0, "{}: {got:.1} dB", band.name);
        }
    }

    #[test]
    fn playback_frames_follow_the_loopback() {
        let mut reference = vec![0.0; 300];
        for v in &mut reference[100..200] {
            *v = 1e-4;
        }
        let f = classify(300, &[], &reference);
        assert!(f.has_playback);
        assert!(!f.playback[99] && f.playback[100] && f.playback[214] && !f.playback[216]);
        // The echo frames start once the canceller has had 3 s (150 frames).
        assert!(!f.echo[150] && !f.echo[200]);
    }
}
