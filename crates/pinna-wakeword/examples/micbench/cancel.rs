//! The echo cancellers compared, each where the device would put it:
//!
//! - Pinna's linear canceller as `pinna_wake` runs it (ADR 0033): on the
//!   front end's output, then the 80 Hz high-pass and +12.04 dB;
//! - WebRTC's AEC3 as `module-echo-cancel` runs it (ADR 0004): after the
//!   high-pass and +12.04 dB that `pinna_beam` applies, through
//!   `tools/aec-replay` (with `--linear`: the linear filter's output, before
//!   the suppressor that PipeWire 1.4.2 doesn't let Pinna turn off).
//!
//! The reference is the loopback lanes, delayed by the front end's latency.

use std::path::Path;
use std::process::Command;
use std::time::Instant;

use anyhow::{Context, Result, ensure};
use pinna_audio::dsp::{DIGITAL_GAIN_DEFAULT, HighPass, saturate_s16};
use pinna_audio::echocancel::{BLOCK, LinearEchoCanceller, STEP_SIZE};

use crate::wav;

/// Both plugins' digital gain: +12.04 dB.
pub const GAIN_DB: f64 = 12.0412;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Cancel {
    None,
    /// Pinna's linear canceller with this step size.
    Pinna(f32),
    WebrtcLinear,
    Webrtc,
}

impl Cancel {
    pub fn name(&self) -> String {
        match *self {
            Cancel::None => "none".into(),
            Cancel::Pinna(mu) if mu == STEP_SIZE => "pinna".into(),
            Cancel::Pinna(mu) => format!("pinna mu {mu}"),
            Cancel::WebrtcLinear => "webrtc linear".into(),
            Cancel::Webrtc => "webrtc".into(),
        }
    }
}

/// One front end through one canceller, on the front end's timeline.
pub struct Chain {
    /// The canceller's input and output, on one scale, for ERLE.
    pub before: Vec<f32>,
    pub after: Vec<f32>,
    /// What pinnad receives, before its automatic gain.
    pub out: Vec<i16>,
    /// Samples the +12.04 dB stage saturated.
    pub gain_clips: usize,
    /// How much louder `after` is than the microphones, dB.
    pub after_gain_db: f64,
    /// Seconds of host CPU the canceller took.
    pub cpu: f64,
}

/// The high-pass and +12.04 dB, as both plugins apply them.
fn highpass_gain(x: &[f32], clips: &mut usize) -> Vec<i16> {
    let mut hp = HighPass::new();
    x.iter()
        .map(|&v| {
            let s = hp.sample(saturate_s16(v.round() as i64));
            let g = i64::from(s) * i64::from(DIGITAL_GAIN_DEFAULT) / 100;
            if !(-32768..=32767).contains(&g) {
                *clips += 1;
            }
            saturate_s16(g)
        })
        .collect()
}

/// `front` and `reference` are on the 16-bit scale and line up in time.
/// `warm` runs the take through the canceller once first, like one that has
/// been running since boot. Returns `None` for WebRTC without `aec_replay`.
pub fn run(
    cancel: Cancel,
    front: &[f32],
    reference: &[f32],
    warm: bool,
    aec_replay: Option<&Path>,
    scratch: &Path,
) -> Result<Option<Chain>> {
    let mut gain_clips = 0;
    Ok(Some(match cancel {
        Cancel::None => Chain {
            before: front.to_vec(),
            after: front.to_vec(),
            out: highpass_gain(front, &mut gain_clips),
            gain_clips,
            after_gain_db: 0.0,
            cpu: 0.0,
        },
        Cancel::Pinna(mu) => {
            let mut c = LinearEchoCanceller::with_step_size(mu);
            if warm {
                for (&m, &r) in front.iter().zip(reference) {
                    c.process(m, r);
                }
            }
            let t = Instant::now();
            let mut e: Vec<f32> = front
                .iter()
                .zip(reference)
                .map(|(&m, &r)| c.process(m, r))
                .collect();
            let cpu = t.elapsed().as_secs_f64();
            // The output is one block late; line it up with the input.
            e.drain(..BLOCK.min(e.len()));
            e.resize(front.len(), 0.0);
            Chain {
                out: highpass_gain(&e, &mut gain_clips),
                before: front.to_vec(),
                after: e,
                gain_clips,
                after_gain_db: 0.0,
                cpu,
            }
        }
        Cancel::WebrtcLinear | Cancel::Webrtc => {
            let Some(exe) = aec_replay else {
                return Ok(None);
            };
            let pre = highpass_gain(front, &mut gain_clips);
            let cap = scratch.join("capture.wav");
            let refp = scratch.join("reference.wav");
            let outp = scratch.join("out.wav");
            let as_float = |x: &[f32]| x.iter().map(|v| v / 32768.0).collect::<Vec<_>>();
            let pre_f: Vec<f32> = pre.iter().map(|&s| f32::from(s)).collect();
            wav::write(&cap, 16_000, 1, &as_float(&pre_f))?;
            wav::write(&refp, 16_000, 1, &as_float(reference))?;
            let mut cmd = Command::new(exe);
            cmd.arg(&cap).arg(&refp).arg(&outp);
            if warm {
                cmd.arg("--warm");
            }
            if cancel == Cancel::WebrtcLinear {
                cmd.arg("--linear");
            }
            let t = Instant::now();
            let o = cmd
                .output()
                .with_context(|| format!("running {}", exe.display()))?;
            let cpu = t.elapsed().as_secs_f64() / if warm { 2.0 } else { 1.0 };
            ensure!(
                o.status.success(),
                "aec_replay failed: {}",
                String::from_utf8_lossy(&o.stderr)
            );
            let mut after: Vec<f32> = wav::read(&outp)?
                .samples
                .iter()
                .map(|v| v * 32768.0)
                .collect();
            after.resize(front.len(), 0.0);
            let out = after
                .iter()
                .map(|&v| saturate_s16(v.round() as i64))
                .collect();
            Chain {
                before: pre_f,
                after,
                out,
                gain_clips,
                after_gain_db: GAIN_DB,
                cpu,
            }
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fft::{SPEECH, band_powers, db};

    /// A linear echo of white noise with a 52-sample inverted lag, as TECHO5
    /// measured on a Dot.
    #[test]
    fn pinna_cancels_a_linear_echo_and_reports_it_aligned() {
        let mut seed = 9u64;
        let mut noise = || {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((seed >> 40) as f32 / (1u64 << 24) as f32 - 0.5) * 8000.0
        };
        let reference: Vec<f32> = (0..6 * 16_000).map(|_| noise()).collect();
        let mic: Vec<f32> = (0..reference.len())
            .map(|i| {
                if i >= 52 {
                    -0.3 * reference[i - 52]
                } else {
                    0.0
                }
            })
            .collect();
        let c = run(
            Cancel::Pinna(STEP_SIZE),
            &mic,
            &reference,
            true,
            None,
            Path::new("."),
        )
        .unwrap()
        .unwrap();
        let tail = |x: &[f32]| {
            let p = band_powers(&x[3 * 16_000..]);
            p[SPEECH].iter().sum::<f64>()
        };
        let erle = db(tail(&c.before)) - db(tail(&c.after));
        assert!(erle > 30.0, "ERLE {erle:.1} dB");
        assert_eq!(c.out.len(), mic.len());
    }
}
