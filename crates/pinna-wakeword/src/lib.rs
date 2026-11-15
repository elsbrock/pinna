//! On-device wake word detection.
//!
//! openWakeWord v0.5.1 ONNX models (mel spectrogram -> embedding -> keyword
//! classifier) run on the pure-Rust `tract` engine. The framing, model
//! chaining, startup behaviour and detection rule reproduce LibreEcho's
//! `wake_engine_onnx.cpp` / `wake_worker.c` exactly.
//!
//! The models are CC BY-NC-SA 4.0 and are never part of this repository; see
//! `scripts/fetch-models.sh`.

mod vad;

use std::path::Path;

use anyhow::{Context, Result, ensure};
use tract_onnx::prelude::*;

use vad::Vad;

/// Samples per inference block (80 ms at 16 kHz).
pub const BLOCK_SAMPLES: usize = 1280;
/// Samples of the previous block prepended to each mel input.
pub const OVERLAP_SAMPLES: usize = 480;
const MEL_FRAMES: usize = 76;
const MEL_BINS: usize = 32;
const FEATURE_FRAMES: usize = 16;
const FEATURE_BINS: usize = 96;
/// Scores that must be at least this high to corroborate a peak.
const SUPPORT_THRESHOLD: f32 = 0.20;
/// Scores suppressed after startup/reset (the classifier is still settling).
const STARTUP_SUPPRESSED: u32 = 5;
/// VAD frame size (10 ms at 16 kHz), as in LibreEcho's AFE path.
const VAD_FRAME_SAMPLES: usize = 160;

type Plan = TypedRunnableModel<TypedModel>;

/// Detection tuning. `Default` is LibreEcho's configuration.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WakeConfig {
    /// Accept threshold for the peak score of the last three blocks.
    pub threshold: f32,
    /// Samples after a detection during which further detections are ignored.
    pub lockout_samples: u32,
    /// Initial playback state (see [`Detector::set_playback`]). While playing,
    /// one supporting score is enough instead of two of three.
    pub during_playback: bool,
}

impl Default for WakeConfig {
    fn default() -> Self {
        Self {
            threshold: 0.55,
            lockout_samples: 28_000,
            during_playback: false,
        }
    }
}

impl WakeConfig {
    /// Map a 0..=100 sensitivity to an accept threshold (LibreEcho's
    /// `sensitivity_threshold`): `0.85 - 0.0044 * s`, clamped to 0.30..=0.85.
    pub fn threshold_for_sensitivity(sensitivity: u8) -> f32 {
        (0.85 - f32::from(sensitivity) * 0.0044).clamp(0.30, 0.85)
    }
}

/// A completed wake word detection.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Detection {
    /// Score of the peak block.
    pub score: f32,
    /// Stream position (samples since load/reset) at the end of the peak block.
    pub sample_index: u64,
}

/// Per-block debugging record, reported by [`Detector::push_with`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BlockScore {
    /// Raw classifier score in 0..=1 (0 while startup suppression is active).
    pub score: f32,
    /// Stream position at the end of this block.
    pub sample_index: u64,
    /// Whether the VAD was active at any point during the block.
    pub vad_active: bool,
    /// Whether playback was active at any point during the block.
    pub playback: bool,
}

#[derive(Clone, Copy, Default)]
struct Observation {
    sample_index: u64,
    vad_active: bool,
    playback: bool,
}

/// Streaming wake word detector.
pub struct Detector {
    cfg: WakeConfig,
    mel_first: Plan,
    mel: Plan,
    embedding: Plan,
    classifier: Plan,

    block: [i16; BLOCK_SAMPLES],
    block_len: usize,
    overlap: [i16; OVERLAP_SAMPLES],
    overlap_len: usize,
    audio: Vec<f32>,
    mel_out: Vec<f32>,
    mel_history: Vec<f32>,
    feature_history: Vec<f32>,
    /// Feature history left behind by the silent warm-up; restored by `reset`.
    warm_features: Vec<f32>,
    last_embedding: Vec<f32>,
    feature_count: usize,
    prediction_count: u32,

    vad: Vad,
    vad_frame: [i16; VAD_FRAME_SAMPLES],
    vad_len: usize,
    playing: bool,
    observation: Observation,
    samples_seen: u64,

    scores: [f32; 3],
    observations: [Observation; 3],
    score_count: usize,
    lockout_until: u64,
}

fn load_plan(path: &Path, shape: &[usize]) -> Result<Plan> {
    onnx()
        .model_for_path(path)
        .with_context(|| format!("loading {}", path.display()))?
        .with_input_fact(0, f32::fact(shape).into())?
        .into_optimized()?
        .into_runnable()
        .with_context(|| format!("optimizing {}", path.display()))
}

fn run(plan: &Plan, input: &[f32], shape: &[usize]) -> Result<TValue> {
    let tensor = Tensor::from_shape(shape, input)?;
    let mut out = plan.run(tvec!(tensor.into_tvalue()))?;
    Ok(out.remove(0))
}

impl Detector {
    /// Load `melspectrogram.onnx`, `embedding_model.onnx` and `keyword_model`
    /// (for example `alexa_v0.1.onnx`) from `model_dir`, then warm up on
    /// silence exactly as LibreEcho does.
    pub fn load(model_dir: &Path, keyword_model: &str, cfg: WakeConfig) -> Result<Self> {
        let mel_path = model_dir.join("melspectrogram.onnx");
        let mut det = Self {
            cfg,
            mel_first: load_plan(&mel_path, &[1, BLOCK_SAMPLES])?,
            mel: load_plan(&mel_path, &[1, BLOCK_SAMPLES + OVERLAP_SAMPLES])?,
            embedding: load_plan(
                &model_dir.join("embedding_model.onnx"),
                &[1, MEL_FRAMES, MEL_BINS, 1],
            )?,
            classifier: load_plan(
                &model_dir.join(keyword_model),
                &[1, FEATURE_FRAMES, FEATURE_BINS],
            )?,
            block: [0; BLOCK_SAMPLES],
            block_len: 0,
            overlap: [0; OVERLAP_SAMPLES],
            overlap_len: 0,
            audio: vec![0.0; BLOCK_SAMPLES + OVERLAP_SAMPLES],
            mel_out: Vec::new(),
            mel_history: vec![1.0; MEL_FRAMES * MEL_BINS],
            feature_history: vec![0.0; FEATURE_FRAMES * FEATURE_BINS],
            warm_features: Vec::new(),
            last_embedding: vec![0.0; FEATURE_BINS],
            feature_count: 0,
            prediction_count: 0,
            vad: Vad::new(),
            vad_frame: [0; VAD_FRAME_SAMPLES],
            vad_len: 0,
            playing: cfg.during_playback,
            observation: Observation::default(),
            samples_seen: 0,
            scores: [0.0; 3],
            observations: [Observation::default(); 3],
            score_count: 0,
            lockout_until: 0,
        };
        // Warm every graph and establish a neutral feature history. The warm-up
        // blocks are not suppressed and their scores are discarded.
        let silence = [0i16; BLOCK_SAMPLES];
        for _ in 0..FEATURE_FRAMES {
            det.process_block(&silence, false)?;
        }
        det.warm_features = det.feature_history.clone();
        det.reset_stream();
        Ok(det)
    }

    /// Feed 16 kHz mono samples of any length; returns detections that
    /// completed in this call. Inference errors are treated as silence for the
    /// affected block (they cannot occur with fixed shapes once loaded).
    pub fn push(&mut self, samples: &[i16]) -> Vec<Detection> {
        self.push_with(samples, |_| {})
    }

    /// Like [`push`](Self::push), additionally reporting every completed block
    /// (raw score, VAD and playback state) to `on_block`.
    pub fn push_with(
        &mut self,
        samples: &[i16],
        mut on_block: impl FnMut(&BlockScore),
    ) -> Vec<Detection> {
        let mut detections = Vec::new();
        let mut offset = 0;
        while offset < samples.len() {
            let n = (BLOCK_SAMPLES - self.block_len).min(samples.len() - offset);
            let chunk = &samples[offset..offset + n];
            self.block[self.block_len..self.block_len + n].copy_from_slice(chunk);
            self.feed_vad(chunk);
            self.block_len += n;
            offset += n;
            if self.block_len == BLOCK_SAMPLES {
                self.block_len = 0;
                let block = self.block;
                let score = self.process_block(&block, true).unwrap_or(0.0);
                let obs = std::mem::take(&mut self.observation);
                let obs = Observation {
                    sample_index: self.samples_seen,
                    ..obs
                };
                on_block(&BlockScore {
                    score,
                    sample_index: obs.sample_index,
                    vad_active: obs.vad_active,
                    playback: obs.playback,
                });
                if let Some(d) = self.decode(score, obs) {
                    detections.push(d);
                }
            }
        }
        detections
    }

    /// Tell the detector whether audio is currently playing out of the device.
    /// While playing, a peak needs only one supporting score instead of two.
    pub fn set_playback(&mut self, playing: bool) {
        self.playing = playing;
    }

    /// Change the accept threshold (see [`WakeConfig::threshold_for_sensitivity`]).
    pub fn set_threshold(&mut self, threshold: f32) {
        self.cfg.threshold = threshold;
    }

    /// The current accept threshold.
    pub fn threshold(&self) -> f32 {
        self.cfg.threshold
    }

    /// Return to the freshly loaded state (stream position 0, no lockout).
    pub fn reset(&mut self) {
        self.reset_stream();
    }

    /// Mel output (after the x/10+2 transform) of the most recent block.
    pub fn last_mel(&self) -> &[f32] {
        &self.mel_out
    }

    /// 96-float embedding of the most recent block.
    pub fn last_embedding(&self) -> &[f32] {
        &self.last_embedding
    }

    fn reset_stream(&mut self) {
        self.mel_history.fill(1.0);
        self.feature_history.copy_from_slice(&self.warm_features);
        self.feature_count = FEATURE_FRAMES;
        self.block_len = 0;
        self.overlap_len = 0;
        self.prediction_count = 0;
        self.vad = Vad::new();
        self.vad_len = 0;
        self.playing = self.cfg.during_playback;
        self.observation = Observation::default();
        self.samples_seen = 0;
        self.scores = [0.0; 3];
        self.observations = [Observation::default(); 3];
        self.score_count = 0;
        self.lockout_until = 0;
    }

    fn feed_vad(&mut self, mut samples: &[i16]) {
        while !samples.is_empty() {
            let n = (VAD_FRAME_SAMPLES - self.vad_len).min(samples.len());
            self.vad_frame[self.vad_len..self.vad_len + n].copy_from_slice(&samples[..n]);
            self.vad_len += n;
            self.samples_seen += n as u64;
            samples = &samples[n..];
            if self.vad_len == VAD_FRAME_SAMPLES {
                self.vad_len = 0;
                if self.vad.process(&self.vad_frame) {
                    self.observation.vad_active = true;
                }
                if self.playing {
                    self.observation.playback = true;
                }
            }
        }
    }

    fn process_block(
        &mut self,
        block: &[i16; BLOCK_SAMPLES],
        suppress_startup: bool,
    ) -> Result<f32> {
        let n = self.overlap_len + BLOCK_SAMPLES;
        for (dst, src) in self.audio.iter_mut().zip(&self.overlap[..self.overlap_len]) {
            *dst = f32::from(*src);
        }
        for (dst, src) in self.audio[self.overlap_len..n].iter_mut().zip(block) {
            *dst = f32::from(*src);
        }
        let plan = if self.overlap_len == 0 {
            &self.mel_first
        } else {
            &self.mel
        };
        let mel = run(plan, &self.audio[..n], &[1, n])?;
        let mel = mel.as_slice::<f32>()?;
        ensure!(
            !mel.is_empty() && mel.len() % MEL_BINS == 0,
            "invalid mel output shape"
        );
        self.mel_out.clear();
        self.mel_out.extend(mel.iter().map(|v| v / 10.0 + 2.0));
        self.append_mel();

        let emb = run(
            &self.embedding,
            &self.mel_history,
            &[1, MEL_FRAMES, MEL_BINS, 1],
        )?;
        let emb = emb.as_slice::<f32>()?;
        ensure!(emb.len() == FEATURE_BINS, "invalid embedding output shape");
        self.last_embedding.copy_from_slice(emb);
        self.feature_history.copy_within(FEATURE_BINS.., 0);
        self.feature_history[(FEATURE_FRAMES - 1) * FEATURE_BINS..].copy_from_slice(emb);
        self.feature_count = (self.feature_count + 1).min(FEATURE_FRAMES);

        let mut score = 0.0;
        if self.feature_count >= FEATURE_FRAMES {
            let out = run(
                &self.classifier,
                &self.feature_history,
                &[1, FEATURE_FRAMES, FEATURE_BINS],
            )?;
            score = out.as_slice::<f32>()?[0];
        }

        self.overlap
            .copy_from_slice(&block[BLOCK_SAMPLES - OVERLAP_SAMPLES..]);
        self.overlap_len = OVERLAP_SAMPLES;
        if suppress_startup {
            if self.prediction_count < STARTUP_SUPPRESSED {
                score = 0.0;
            }
            self.prediction_count += 1;
        }
        Ok(score.clamp(0.0, 1.0))
    }

    fn append_mel(&mut self) {
        let count = self.mel_out.len() / MEL_BINS;
        if count >= MEL_FRAMES {
            let start = (count - MEL_FRAMES) * MEL_BINS;
            self.mel_history.copy_from_slice(&self.mel_out[start..]);
            return;
        }
        let keep = (MEL_FRAMES - count) * MEL_BINS;
        self.mel_history.copy_within(count * MEL_BINS.., 0);
        self.mel_history[keep..].copy_from_slice(&self.mel_out);
    }

    /// LibreEcho's `decode_score`: the peak of the last three scores must reach
    /// the threshold, be VAD-active, outside the lockout, and be corroborated
    /// by two (one during playback) scores of at least 0.20.
    fn decode(&mut self, score: f32, obs: Observation) -> Option<Detection> {
        self.scores.rotate_left(1);
        self.scores[2] = score;
        self.observations.rotate_left(1);
        self.observations[2] = obs;
        self.score_count = (self.score_count + 1).min(3);

        let first = 3 - self.score_count;
        let support = self.scores[first..]
            .iter()
            .filter(|&&s| s >= SUPPORT_THRESHOLD)
            .count();
        let mut peak = 0;
        for i in first..3 {
            if self.scores[i] > self.scores[peak] {
                peak = i;
            }
        }
        let o = self.observations[peak];
        let needed = if o.playback { 1 } else { 2 };
        if o.sample_index < self.lockout_until
            || !o.vad_active
            || self.scores[peak] < self.cfg.threshold
            || support < needed
        {
            return None;
        }
        self.lockout_until = o.sample_index + u64::from(self.cfg.lockout_samples);
        Some(Detection {
            score: self.scores[peak],
            sample_index: o.sample_index,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sensitivity_mapping() {
        assert_eq!(WakeConfig::threshold_for_sensitivity(0), 0.85);
        assert!((WakeConfig::threshold_for_sensitivity(100) - 0.41).abs() < 1e-6);
        assert!((WakeConfig::threshold_for_sensitivity(75) - 0.52).abs() < 1e-6);
    }

    #[test]
    fn defaults_match_libreecho() {
        let c = WakeConfig::default();
        assert_eq!(c.threshold, 0.55);
        assert_eq!(c.lockout_samples, 28_000);
        assert!(!c.during_playback);
    }
}
