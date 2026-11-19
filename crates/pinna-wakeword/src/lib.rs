//! On-device wake word detection.
//!
//! openWakeWord v0.5.1 ONNX models (mel spectrogram -> embedding -> keyword
//! classifier) run on the pure-Rust `tract` engine. The framing, model
//! chaining, startup behaviour and detection rule reproduce the previous firmware's
//! `wake_engine_onnx.cpp` / `wake_worker.c` exactly.
//!
//! The models are CC BY-NC-SA 4.0 and are never part of this repository; see
//! `scripts/fetch-models.sh`.
//!
//! The mel spectrogram and embedding are shared by every wake word; each word
//! is a [`Keyword`], a small classifier on the last 16 embeddings, loaded from
//! `.onnx` or `.tflite` (ADR 0013).

use std::io::Cursor;
use std::path::Path;

use anyhow::{Context, Result, anyhow, bail, ensure};
use pinna_vad::Vad;
use tract_onnx::prelude::*;

/// Samples per inference block (80 ms at 16 kHz).
pub const BLOCK_SAMPLES: usize = 1280;
/// Samples of the previous block prepended to each mel input.
pub const OVERLAP_SAMPLES: usize = 480;
const MEL_FRAMES: usize = 76;
const MEL_BINS: usize = 32;
/// Blocks the keyword classifier scores at once: one embedding per block,
/// the last 1.28 s.
pub const FEATURE_FRAMES: usize = 16;
const FEATURE_BINS: usize = 96;
/// Scores that must be at least this high to corroborate a peak.
const SUPPORT_THRESHOLD: f32 = 0.20;
/// Scores suppressed after startup/reset (the classifier is still settling).
const STARTUP_SUPPRESSED: u32 = 5;
/// VAD frame size (10 ms at 16 kHz), as in the previous firmware's AFE path.
const VAD_FRAME_SAMPLES: usize = 160;

type Plan = TypedRunnableModel<TypedModel>;

/// Detection tuning. `Default` is the previous firmware's configuration.
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
    /// Map a 0..=100 sensitivity to an accept threshold (the previous firmware's
    /// `sensitivity_threshold`): `0.85 - 0.0044 * s`, clamped to 0.30..=0.85.
    pub fn threshold_for_sensitivity(sensitivity: u8) -> f32 {
        (0.85 - f32::from(sensitivity) * 0.0044).clamp(0.30, 0.85)
    }

    /// The lowest sensitivity whose threshold `score` reaches, or `None` when
    /// even 100's is higher.
    pub fn min_sensitivity_for(score: f32) -> Option<u8> {
        (0..=100).find(|&s| score >= Self::threshold_for_sensitivity(s))
    }
}

/// A completed wake word detection.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Detection {
    /// Which keyword fired: an index into the detector's keywords
    /// ([`Detector::keyword`]).
    pub keyword: usize,
    /// Score of the peak block.
    pub score: f32,
    /// Stream position (samples since load/reset) at the end of the peak block.
    pub sample_index: u64,
}

/// Per-block debugging record, reported by [`Detector::push_with`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BlockScore {
    /// The highest raw classifier score of this block in 0..=1 (0 while startup
    /// suppression is active, or without keywords).
    pub score: f32,
    /// The keyword with that score; `None` without keywords.
    pub keyword: Option<usize>,
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

/// What a wake word may cost per block: a tenth of the 80 ms it covers, so
/// that two words leave the audio path enough time (ADR 0013).
const BUDGET_MS: f32 = 8.0;
/// Milliseconds of audio in one block.
const BLOCK_MS: f32 = 80.0;
/// Runs of the classifier timed; the fastest counts.
const TIMING_RUNS: usize = 20;

/// What [`test_keyword`] measured on a model it accepted.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TestReport {
    /// The highest raw block score on the test's silence and noise.
    pub peak_score: f32,
    /// The classifier's time for one block, in milliseconds.
    pub block_ms: f32,
}

/// How a classifier is stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelFormat {
    Onnx,
    Tflite,
}

impl ModelFormat {
    /// Recognise a model by its content: a TFLite flatbuffer carries the file
    /// identifier `TFL3` at offset 4; anything else is tried as ONNX.
    pub fn detect(bytes: &[u8]) -> Self {
        if bytes.get(4..8) == Some(b"TFL3") {
            Self::Tflite
        } else {
            Self::Onnx
        }
    }
}

/// One wake word: an openWakeWord classifier from the last 16 embeddings
/// (`[1, 16, 96]`) to one score.
pub struct Keyword {
    name: String,
    plan: Plan,
}

impl Keyword {
    /// Load a classifier file; the name is the file stem (`alexa_v0.1`).
    pub fn load(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        let name = path
            .file_stem()
            .map_or_else(String::new, |s| s.to_string_lossy().into_owned());
        Self::from_bytes(name, &bytes).with_context(|| format!("loading {}", path.display()))
    }

    /// Load and check a classifier from memory, `.onnx` or `.tflite`
    /// ([`ModelFormat::detect`]). It must take `[1, 16, 96]` floats, give one
    /// score, and run once. A model that makes tract panic is an error too.
    pub fn from_bytes(name: impl Into<String>, bytes: &[u8]) -> Result<Self> {
        let plan = std::panic::catch_unwind(|| classifier_plan(bytes))
            .map_err(|_| anyhow!("the model crashed the loader"))??;
        Ok(Self {
            name: name.into(),
            plan,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    fn score(&self, features: &[f32]) -> Result<f32> {
        let out = run(&self.plan, features, &[1, FEATURE_FRAMES, FEATURE_BINS])?;
        Ok(out.as_slice::<f32>()?[0])
    }
}

impl std::fmt::Debug for Keyword {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Keyword").field("name", &self.name).finish()
    }
}

fn classifier_plan(bytes: &[u8]) -> Result<Plan> {
    let shape = [1, FEATURE_FRAMES, FEATURE_BINS];
    let model = match ModelFormat::detect(bytes) {
        ModelFormat::Onnx => {
            let model = onnx().model_for_read(&mut Cursor::new(bytes)).context(
                "not an ONNX or TFLite model; choose the wake word's .onnx or .tflite file",
            )?;
            ensure!(
                model.input_outlets()?.len() == 1,
                "a wake word model has one input, this one has {}",
                model.input_outlets()?.len()
            );
            // openWakeWord's downloads put the shared models beside the
            // words: the melspectrogram takes audio ([1, samples]), the
            // embedding mel frames ([1, 76, 32, 1]). A word takes rank 3.
            if let Some(2 | 4) = tract_onnx::tract_hir::infer::Factoid::concretize(
                &model.input_fact(0)?.shape.rank(),
            ) {
                bail!(
                    "this is one of openWakeWord's shared models (the melspectrogram or the \
                     embedding), not a wake word; choose the file named after the word"
                );
            }
            model
                .with_input_fact(0, f32::fact(shape).into())
                .and_then(|m| m.into_typed())
                .context("not an openWakeWord classifier: it doesn't take [1, 16, 96] floats")?
        }
        ModelFormat::Tflite => {
            // Before tract reads it: tract panics on the streaming
            // microWakeWord models, whose CALL_ONCE operator has no outputs.
            if is_micro_wake_word(bytes) {
                bail!(
                    "this looks like a microWakeWord model (for ESPHome devices such as Voice PE); \
                     Pinna runs openWakeWord models, .onnx or .tflite"
                );
            }
            let model = tract_tflite::tflite()
                .model_for_read(&mut Cursor::new(bytes))
                .context("not a readable TFLite model")?;
            ensure!(
                model.input_outlets()?.len() == 1,
                "a wake word model has one input, this one has {}",
                model.input_outlets()?.len()
            );
            let input = model.input_fact(0)?;
            let got = input.shape.to_tvec();
            let want: TVec<TDim> = shape.iter().map(|&d| d.into()).collect();
            if input.datum_type != f32::datum_type() || got != want {
                bail!(
                    "not an openWakeWord classifier: it takes {:?} {:?}, not [1, 16, 96] floats",
                    input.datum_type,
                    got
                );
            }
            model
        }
    };
    let plan = model
        .into_optimized()
        .and_then(|m| m.into_runnable())
        .context("the model can't be prepared to run")?;
    let out = plan
        .run(tvec!(Tensor::zero::<f32>(&shape)?.into_tvalue()))
        .context("the model doesn't run on openWakeWord features")?;
    let out = out.first().context("the model has no output")?;
    let scores = out
        .as_slice::<f32>()
        .context("the model's output isn't a float")?;
    ensure!(
        scores.len() == 1,
        "not an openWakeWord classifier: it gives {} values, not one score",
        scores.len()
    );
    ensure!(scores[0].is_finite(), "the model's score isn't a number");
    Ok(plan)
}

/// Whether a TFLite file's input is a microWakeWord model's (ESPHome, Voice
/// PE): quantized 40-bin spectrogram slices, `[1, 3, 40]` int8 in the models
/// from microwakeword.com, where openWakeWord takes float embeddings. Read
/// from the flatbuffer itself, checked, so it also works where tract can't
/// read the model. A file it can't read is left to tract.
fn is_micro_wake_word(bytes: &[u8]) -> bool {
    use tract_tflite::tflite::{TensorType, root_as_model};
    let Ok(model) = root_as_model(bytes) else {
        return false;
    };
    let input = (|| {
        let graph = model.subgraphs()?.iter().next()?;
        let tensors = graph.tensors()?;
        let id = usize::try_from(graph.inputs()?.iter().next()?).ok()?;
        (id < tensors.len()).then(|| tensors.get(id))
    })();
    let Some(input) = input else {
        return false;
    };
    let float = matches!(
        input.type_(),
        TensorType::FLOAT32 | TensorType::FLOAT16 | TensorType::FLOAT64
    );
    !float || input.shape().and_then(|s| s.iter().next_back()) == Some(40)
}

/// Test an uploaded classifier before the satellite keeps it (ADR 0013): it
/// must load, cost at most 8 ms per block, and stay below `threshold` on
/// generated silence and noise. An error is a refusal; its message is the
/// reason an owner reads.
pub fn test_keyword(model_dir: &Path, bytes: &[u8], threshold: f32) -> Result<TestReport> {
    test_keyword_within(model_dir, bytes, threshold, BUDGET_MS)
}

/// [`test_keyword`] with the time budget as a parameter, for tests.
pub fn test_keyword_within(
    model_dir: &Path,
    bytes: &[u8],
    threshold: f32,
    budget_ms: f32,
) -> Result<TestReport> {
    let keyword = Keyword::from_bytes("upload", bytes)?;

    // The test runs at low priority, so the fastest run is the model's own cost.
    let features = vec![0.1_f32; FEATURE_FRAMES * FEATURE_BINS];
    let mut fastest = f32::MAX;
    for _ in 0..TIMING_RUNS {
        let start = std::time::Instant::now();
        keyword.score(&features)?;
        fastest = fastest.min(start.elapsed().as_secs_f32() * 1000.0);
    }
    ensure!(
        fastest <= budget_ms,
        "This model needs {fastest:.1} ms for each {BLOCK_MS:.0} ms of audio on this satellite; \
         a wake word may take at most {budget_ms:.0} ms."
    );

    let mut detector = Detector::new(
        model_dir,
        vec![keyword],
        WakeConfig {
            threshold,
            ..WakeConfig::default()
        },
    )?;
    let mut peak = 0.0_f32;
    let mut noise = Noise(0x9E37_79B9_7F4A_7C15);
    for (seconds, dbfs) in [(2, None), (3, Some(-30.0)), (3, Some(-12.0))] {
        let samples = noise.samples(seconds * 16_000, dbfs);
        for chunk in samples.chunks(BLOCK_SAMPLES) {
            detector.push_with(chunk, |b| peak = peak.max(b.score));
        }
    }
    ensure!(
        peak < threshold,
        "This model wakes on silence or noise (score {peak:.2}; the satellite wakes at \
         {threshold:.2}), so the satellite would wake by itself."
    );
    Ok(TestReport {
        peak_score: peak,
        block_ms: fastest,
    })
}

/// Deterministic white noise (xorshift64), so a test gives the same answer
/// every time without a dependency.
struct Noise(u64);

impl Noise {
    /// Uniform samples at `dbfs` RMS, or digital silence for `None`.
    fn samples(&mut self, count: usize, dbfs: Option<f32>) -> Vec<i16> {
        let Some(dbfs) = dbfs else {
            return vec![0; count];
        };
        // A uniform signal in +-a has an RMS of a / sqrt(3).
        let amplitude = 32768.0 * 10f32.powf(dbfs / 20.0) * 3f32.sqrt();
        (0..count)
            .map(|_| {
                self.0 ^= self.0 << 13;
                self.0 ^= self.0 >> 7;
                self.0 ^= self.0 << 17;
                let unit = (self.0 >> 11) as f32 / (1u64 << 53) as f32 * 2.0 - 1.0;
                (unit * amplitude).clamp(-32768.0, 32767.0) as i16
            })
            .collect()
    }
}

struct Head {
    keyword: Keyword,
    /// This block's raw score (0 while startup suppression is active).
    current: f32,
    /// The last three scores, oldest first.
    scores: [f32; 3],
}

impl Head {
    fn new(keyword: Keyword) -> Self {
        Self {
            keyword,
            current: 0.0,
            scores: [0.0; 3],
        }
    }
}

/// Streaming wake word detector.
pub struct Detector {
    cfg: WakeConfig,
    mel_first: Plan,
    mel: Plan,
    embedding: Plan,
    heads: Vec<Head>,

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

    observations: [Observation; 3],
    score_count: usize,
    /// Shared by every keyword: one utterance gives at most one detection.
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
    /// silence exactly as the previous firmware does.
    pub fn load(model_dir: &Path, keyword_model: &str, cfg: WakeConfig) -> Result<Self> {
        let keyword = Keyword::load(&model_dir.join(keyword_model))?;
        Self::new(model_dir, vec![keyword], cfg)
    }

    /// Load the shared models from `model_dir` and listen for `keywords`
    /// (none is allowed: the detector then keeps its audio context and never
    /// fires).
    pub fn new(model_dir: &Path, keywords: Vec<Keyword>, cfg: WakeConfig) -> Result<Self> {
        let mel_path = model_dir.join("melspectrogram.onnx");
        let mut det = Self {
            cfg,
            mel_first: load_plan(&mel_path, &[1, BLOCK_SAMPLES])?,
            mel: load_plan(&mel_path, &[1, BLOCK_SAMPLES + OVERLAP_SAMPLES])?,
            embedding: load_plan(
                &model_dir.join("embedding_model.onnx"),
                &[1, MEL_FRAMES, MEL_BINS, 1],
            )?,
            heads: keywords.into_iter().map(Head::new).collect(),
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
                if self.process_block(&block, true).is_err() {
                    for head in &mut self.heads {
                        head.current = 0.0;
                    }
                }
                let obs = std::mem::take(&mut self.observation);
                let obs = Observation {
                    sample_index: self.samples_seen,
                    ..obs
                };
                let (keyword, score) = self.best_current();
                on_block(&BlockScore {
                    score,
                    keyword,
                    sample_index: obs.sample_index,
                    vad_active: obs.vad_active,
                    playback: obs.playback,
                });
                if let Some(d) = self.decode(obs) {
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

    /// The VAD's tracked noise energy, or `None` before it has seen a frame.
    /// Standalone mode seeds its endpointer with this, the noise before the
    /// wake word.
    pub fn noise_energy(&self) -> Option<u64> {
        self.vad.noise_energy()
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

    /// Listen for `keywords` from the next block on. The audio context is kept,
    /// so the new words work at once; each starts with an empty score window.
    pub fn set_keywords(&mut self, keywords: Vec<Keyword>) {
        self.heads = keywords.into_iter().map(Head::new).collect();
    }

    /// The keyword at `index` ([`Detection::keyword`], [`BlockScore::keyword`]).
    pub fn keyword(&self, index: usize) -> Option<&Keyword> {
        self.heads.get(index).map(|h| &h.keyword)
    }

    /// How many keywords the detector listens for.
    pub fn keyword_count(&self) -> usize {
        self.heads.len()
    }

    /// The keyword with this block's highest score, and that score.
    fn best_current(&self) -> (Option<usize>, f32) {
        let mut best = (None, 0.0);
        for (i, head) in self.heads.iter().enumerate() {
            if best.0.is_none() || head.current > best.1 {
                best = (Some(i), head.current);
            }
        }
        best
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
        for head in &mut self.heads {
            head.current = 0.0;
            head.scores = [0.0; 3];
        }
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
    ) -> Result<()> {
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

        let ready = self.feature_count >= FEATURE_FRAMES;
        let suppressed = suppress_startup && self.prediction_count < STARTUP_SUPPRESSED;
        for head in &mut self.heads {
            // A failing classifier scores 0 for this block; the others go on.
            head.current = if ready && !suppressed {
                head.keyword
                    .score(&self.feature_history)
                    .unwrap_or(0.0)
                    .clamp(0.0, 1.0)
            } else {
                0.0
            };
        }

        self.overlap
            .copy_from_slice(&block[BLOCK_SAMPLES - OVERLAP_SAMPLES..]);
        self.overlap_len = OVERLAP_SAMPLES;
        if suppress_startup {
            self.prediction_count += 1;
        }
        Ok(())
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

    /// the previous firmware's `decode_score`: the peak of the last three scores must reach
    /// the threshold, be VAD-active, outside the lockout, and be corroborated
    /// by two (one during playback) scores of at least 0.20.
    ///
    /// Each keyword is decided on its own scores. When several qualify, the
    /// highest peak wins; the lockout that follows applies to all of them.
    fn decode(&mut self, obs: Observation) -> Option<Detection> {
        self.observations.rotate_left(1);
        self.observations[2] = obs;
        self.score_count = (self.score_count + 1).min(3);
        let first = 3 - self.score_count;

        let mut best: Option<Detection> = None;
        for (k, head) in self.heads.iter_mut().enumerate() {
            head.scores.rotate_left(1);
            head.scores[2] = head.current;
            let scores = &head.scores;

            let support = scores[first..]
                .iter()
                .filter(|&&s| s >= SUPPORT_THRESHOLD)
                .count();
            let mut peak = 0;
            for i in first..3 {
                if scores[i] > scores[peak] {
                    peak = i;
                }
            }
            let o = self.observations[peak];
            let needed = if o.playback { 1 } else { 2 };
            if o.sample_index < self.lockout_until
                || !o.vad_active
                || scores[peak] < self.cfg.threshold
                || support < needed
            {
                continue;
            }
            if best.is_none_or(|b| scores[peak] > b.score) {
                best = Some(Detection {
                    keyword: k,
                    score: scores[peak],
                    sample_index: o.sample_index,
                });
            }
        }
        if let Some(d) = best {
            self.lockout_until = d.sample_index + u64::from(self.cfg.lockout_samples);
        }
        best
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
    fn min_sensitivity_for_a_score() {
        // 68 gives 0.5508, so 0.55 first passes at 69.
        assert_eq!(WakeConfig::min_sensitivity_for(0.55), Some(69));
        assert_eq!(WakeConfig::min_sensitivity_for(0.9), Some(0));
        assert_eq!(WakeConfig::min_sensitivity_for(0.42), Some(98));
        assert_eq!(WakeConfig::min_sensitivity_for(0.40), None);
        for score in [0.45, 0.5, 0.62, 0.7, 0.8] {
            let s = WakeConfig::min_sensitivity_for(score).unwrap();
            assert!(score >= WakeConfig::threshold_for_sensitivity(s));
            assert!(score < WakeConfig::threshold_for_sensitivity(s - 1));
        }
    }

    #[test]
    fn defaults_match_previous_firmware() {
        let c = WakeConfig::default();
        assert_eq!(c.threshold, 0.55);
        assert_eq!(c.lockout_samples, 28_000);
        assert!(!c.during_playback);
    }
}
