//! The wake word detector as pinnad runs it: 20 ms frames through the
//! automatic gain (from +12 dB, as on the Echo's PipeWire graph), then the
//! detector. Each keyword gets a detector of its own, so every block's score
//! is known per keyword; [`replay`] then applies the detection rule at any
//! threshold without running the models again.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use pinna_audio::Agc;
use pinna_audio::dsp::FRAME_SAMPLES;
use pinna_wakeword::{BlockScore, Detector, Keyword, WakeConfig};

/// `pinna_wakeword`'s rule: a peak needs two scores of at least this (one
/// during playback). Checked against the detector in `replay_matches_the_detector`.
const SUPPORT: f32 = 0.20;
const LOCKOUT: u64 = 28_000;

pub struct Scored {
    pub blocks: Vec<BlockScore>,
    /// The detector's own detections, with playback flagged and its default
    /// threshold.
    pub detections: Vec<u64>,
}

/// The model file for a keyword name such as `alexa` or `hey_jarvis`.
pub fn model(dir: &Path, name: &str) -> Result<PathBuf> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name().and_then(|f| f.to_str()).is_some_and(|f| {
                f.starts_with(name) && (f.ends_with(".onnx") || f.ends_with(".tflite"))
            })
        })
        .collect();
    found.sort();
    match found.into_iter().next() {
        Some(p) => Ok(p),
        None => bail!("no model for {name} in {}", dir.display()),
    }
}

/// Run `pcm` (pinnad's input, before its gain) through the gain and one
/// detector per keyword. `playback` says per 20 ms frame whether the
/// satellite plays.
pub fn score(
    dir: &Path,
    keywords: &[String],
    pcm: &[i16],
    playback: &[bool],
) -> Result<Vec<Scored>> {
    let mut dets = keywords
        .iter()
        .map(|k| {
            let kw = Keyword::load(&model(dir, k)?)?;
            Detector::new(dir, vec![kw], WakeConfig::default())
        })
        .collect::<Result<Vec<_>>>()?;
    let mut out: Vec<Scored> = keywords
        .iter()
        .map(|_| Scored {
            blocks: Vec::new(),
            detections: Vec::new(),
        })
        .collect();
    let mut agc = Agc::new(12.0);
    for (i, chunk) in pcm.chunks_exact(FRAME_SAMPLES).enumerate() {
        let mut frame = [0i16; FRAME_SAMPLES];
        frame.copy_from_slice(chunk);
        agc.process(&mut frame);
        let playing = playback.get(i).copied().unwrap_or(false);
        for (d, o) in dets.iter_mut().zip(out.iter_mut()) {
            d.set_playback(playing);
            let found = d.push_with(&frame, |b| o.blocks.push(*b));
            o.detections.extend(found.iter().map(|x| x.sample_index));
        }
    }
    Ok(out)
}

/// The detections `blocks` give at `threshold`: `Detector::decode`'s rule,
/// with the playback relief (one supporting score instead of two) only when
/// `playback_rule` is set. pinnad doesn't call `Detector::set_playback`, so
/// on the device the relief is off.
pub fn replay(blocks: &[BlockScore], threshold: f32, playback_rule: bool) -> Vec<(u64, f32)> {
    let mut scores = [0f32; 3];
    let mut obs = [(0u64, false, false); 3];
    let mut count = 0usize;
    let mut lockout_until = 0u64;
    let mut out = Vec::new();
    for b in blocks {
        obs.rotate_left(1);
        obs[2] = (b.sample_index, b.vad_active, b.playback);
        count = (count + 1).min(3);
        let first = 3 - count;
        scores.rotate_left(1);
        scores[2] = b.score;
        let support = scores[first..].iter().filter(|&&s| s >= SUPPORT).count();
        let mut peak = 0;
        for i in first..3 {
            if scores[i] > scores[peak] {
                peak = i;
            }
        }
        let (at, vad, playing) = obs[peak];
        let needed = if playing && playback_rule { 1 } else { 2 };
        if at < lockout_until || !vad || scores[peak] < threshold || support < needed {
            continue;
        }
        out.push((at, scores[peak]));
        lockout_until = at + LOCKOUT;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn models() -> Option<PathBuf> {
        let dir =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../blobs/models/openwakeword-0.5.1");
        dir.join("melspectrogram.onnx").exists().then_some(dir)
    }

    /// The replayed rule finds what the detector found, on a fixture with two
    /// wake words, with and without playback flagged.
    #[test]
    fn replay_matches_the_detector() {
        let Some(dir) = models() else {
            eprintln!("SKIP replay_matches_the_detector: no models (just models)");
            return;
        };
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/alexa-twice.wav");
        let pcm: Vec<i16> = hound::WavReader::open(fixture)
            .unwrap()
            .samples::<i16>()
            .map(|s| s.unwrap())
            .collect();
        for playing in [false, true] {
            let flags = vec![playing; pcm.len() / FRAME_SAMPLES];
            let s = score(&dir, &["alexa".into()], &pcm, &flags).unwrap();
            let replayed: Vec<u64> = replay(&s[0].blocks, WakeConfig::default().threshold, true)
                .iter()
                .map(|d| d.0)
                .collect();
            assert!(!s[0].detections.is_empty());
            assert_eq!(replayed, s[0].detections, "playback {playing}");
        }
    }
}
