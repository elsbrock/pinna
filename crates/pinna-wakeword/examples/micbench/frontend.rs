//! The front ends compared: what turns the seven microphone lanes into one
//! signal. Each runs on 16-bit samples, as the LADSPA plugins do after
//! PipeWire's float, and the beamformer is the device's
//! (`pinna_audio::beamformer`, LibreEcho's superdirective design).

use anyhow::{Result, anyhow};
use pinna_audio::beamformer::{Beamformer, FFT as BEAM_DELAY, HOP as BEAM_HOP};
use pinna_audio::dsp::{LOGICAL_CHANNELS, MIC_MASK_ALL, MIC_MASK_BEAM};

pub const CENTRE: usize = 6;
pub const BEAMS: usize = 12;
/// Every lane but 3 and 4, which hear 5-6 dB more of the Dot's speaker than
/// the others (echo-ref and beam-hold takes, Echo Dot (2nd gen), 2026-11-21).
pub const MIC_MASK_QUIET5: u8 = MIC_MASK_ALL & !(1 << 3) & !(1 << 4);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Front {
    /// Lane 6 alone.
    Centre,
    /// The mean of the lanes in the mask.
    Mean(u8),
    /// The beamformer on the lanes in the mask, choosing its beam freely, as
    /// `pinna_beam` does.
    Mvdr(u8),
    /// One microphone, chosen per turn.
    Lane(usize),
    /// The beamformer held on one azimuth, chosen per turn.
    MvdrAt(u8, i32),
}

fn mask_name(mask: u8) -> &'static str {
    match mask {
        MIC_MASK_ALL => "7",
        MIC_MASK_BEAM => "4",
        MIC_MASK_QUIET5 => "5",
        _ => "?",
    }
}

impl Front {
    pub fn name(&self) -> String {
        match *self {
            Front::Centre => "centre".into(),
            Front::Mean(m) => format!("mean{}", mask_name(m)),
            Front::Mvdr(m) => format!("mvdr{}", mask_name(m)),
            Front::Lane(l) => format!("lane {l}"),
            Front::MvdrAt(m, az) => format!("mvdr{} at {az}", mask_name(m)),
        }
    }

    /// Samples the output lags the microphones.
    pub fn latency(&self) -> usize {
        match self {
            Front::Mvdr(_) | Front::MvdrAt(..) => BEAM_DELAY,
            _ => 0,
        }
    }
}

/// Lanes on the 16-bit scale, as `pinna_ladspa`'s `to_i16` rounds them.
pub fn logical(lanes: &[[f32; 9]]) -> Vec<[i16; LOGICAL_CHANNELS]> {
    lanes
        .iter()
        .map(|f| std::array::from_fn(|c| (f[c] * 32768.0).round().clamp(-32768.0, 32767.0) as i16))
        .collect()
}

fn lanes_of(mask: u8) -> Vec<usize> {
    (0..LOGICAL_CHANNELS)
        .filter(|c| mask & (1 << c) != 0)
        .collect()
}

/// The front end's output on the 16-bit scale. `alive` masks out dead lanes.
pub fn run(front: Front, logical: &[[i16; LOGICAL_CHANNELS]], alive: u8) -> Result<Vec<f32>> {
    Ok(match front {
        Front::Centre => logical.iter().map(|f| f32::from(f[CENTRE])).collect(),
        Front::Lane(l) => logical.iter().map(|f| f32::from(f[l])).collect(),
        Front::Mean(mask) => {
            let lanes = lanes_of(mask & alive);
            let n = lanes.len().max(1) as f32;
            logical
                .iter()
                .map(|f| lanes.iter().map(|&c| f32::from(f[c])).sum::<f32>() / n)
                .collect()
        }
        Front::Mvdr(mask) | Front::MvdrAt(mask, _) => {
            let mut bf =
                Beamformer::new(mask & alive, BEAMS).map_err(|e| anyhow!("beamformer: {e}"))?;
            if let Front::MvdrAt(_, az) = front {
                bf.lock(Some(az));
            }
            logical.iter().map(|f| f32::from(bf.sample(f))).collect()
        }
    })
}

/// The beam whose speech-band power stands out most during `talk` against
/// `quiet` (both say, per sample, whether it counts), as a per-turn lock
/// would choose it from the wake word. Returns its azimuth.
pub fn best_beam(
    mask: u8,
    logical: &[[i16; LOGICAL_CHANNELS]],
    alive: u8,
    talk: impl Fn(usize) -> bool,
    quiet: impl Fn(usize) -> bool,
) -> Result<i32> {
    let mut bf = Beamformer::new(mask & alive, BEAMS).map_err(|e| anyhow!("beamformer: {e}"))?;
    let (mut t, mut q) = (vec![0f64; BEAMS], vec![0f64; BEAMS]);
    let (mut nt, mut nq) = (0usize, 0usize);
    for (i, f) in logical.iter().enumerate() {
        bf.sample(f);
        if (i + 1) % BEAM_HOP != 0 {
            continue;
        }
        // The powers describe audio one beamformer delay earlier.
        let at = (i + 1).saturating_sub(BEAM_DELAY);
        let (is_talk, is_quiet) = (talk(at), quiet(at));
        for b in 0..BEAMS {
            let p = f64::from(bf.beam_power(b));
            if is_talk {
                t[b] += p;
            }
            if is_quiet {
                q[b] += p;
            }
        }
        nt += usize::from(is_talk);
        nq += usize::from(is_quiet);
    }
    let ratio = |b: usize| (t[b] / nt.max(1) as f64) / (q[b] / nq.max(1) as f64 + 1e-12);
    let best = (0..BEAMS)
        .max_by(|&a, &b| ratio(a).total_cmp(&ratio(b)))
        .unwrap_or(0);
    Ok(bf.beam_azimuth(best))
}

/// Lanes whose level sits 10 dB or more under the median lane's: dead, like
/// lane 1 after boot before commit e6c9fda (15 dB down in a quiet room). The
/// live lanes agree within 1.5 dB on a talker (TECHO5) and within 6 dB on the
/// Dot's own echo. Returns the mask of live lanes.
pub fn alive_lanes(logical: &[[i16; LOGICAL_CHANNELS]]) -> u8 {
    let level: Vec<f64> = (0..LOGICAL_CHANNELS)
        .map(|c| {
            let ms = logical.iter().map(|f| f64::from(f[c]).powi(2)).sum::<f64>()
                / logical.len().max(1) as f64;
            10.0 * (ms + 1e-3).log10()
        })
        .collect();
    let mut sorted = level.clone();
    sorted.sort_by(f64::total_cmp);
    let median = sorted[LOGICAL_CHANNELS / 2];
    (0..LOGICAL_CHANNELS)
        .filter(|&c| level[c] > median - 10.0)
        .fold(0u8, |m, c| m | 1 << c)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dead_lane_is_left_out() {
        let frames: Vec<[i16; 7]> = (0..16_000)
            .map(|i| {
                let v = ((i * 7919) % 2001) as i16 - 1000;
                [v, v / 100, v, v, v, v, v]
            })
            .collect();
        assert_eq!(alive_lanes(&frames), MIC_MASK_ALL & !(1 << 1));
        let mean = run(
            Front::Mean(MIC_MASK_BEAM),
            &frames[..3],
            alive_lanes(&frames),
        )
        .unwrap();
        assert_eq!(mean[0], f32::from(frames[0][0]));
    }
}
