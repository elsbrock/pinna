//! Loads the CAM++ model and turns a 1.5 s window into a voiceprint (ADR 0038).

use std::path::Path;

use sha2::{Digest, Sha256};
use tract_onnx::prelude::*;

use crate::fbank::{BINS, Fbank, subtract_mean};
use crate::{Error, Voiceprint, WINDOW_SAMPLES};

/// 150 frames: 1.5 s with a front end that doesn't snip edges.
const FRAMES: usize = WINDOW_SAMPLES / crate::fbank::SHIFT;

pub struct Model {
    plan: Arc<TypedRunnableModel>,
    fbank: Fbank,
    /// Turns `i16` samples into what the model was trained on.
    scale: f32,
    cmn: bool,
    sha256: String,
}

impl Model {
    pub fn load(path: &Path) -> Result<Model, Error> {
        let shown = path.display().to_string();
        let bytes = std::fs::read(path).map_err(|e| Error::Read {
            path: shown.clone(),
            cause: e.to_string(),
        })?;
        let sha256 = Sha256::digest(&bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let load = |cause: &dyn ToString| Error::Load {
            path: shown.clone(),
            cause: cause.to_string(),
        };
        // Each copy of the 28 MB weights is dropped as soon as the next one exists: holding the
        // file, two parses and the model at once took pinnad's memory from 45 MB to 162 MB
        // while loading, near its 200 MB cap (Echo Dot (2nd gen), 2026-11-28).
        let proto = onnx()
            .proto_model_for_read(&mut std::io::Cursor::new(&bytes))
            .map_err(|e| load(&e))?;
        drop(bytes);
        let meta = |key: &str| {
            proto
                .metadata_props
                .iter()
                .find(|p| p.key == key)
                .map(|p| p.value.as_str())
        };
        // sherpa-onnx's exports say how the model's features were made.
        let scale = if meta("normalize_samples").unwrap_or("1") == "1" {
            1.0 / 32768.0
        } else {
            1.0
        };
        let cmn = meta("feature_normalize_type") == Some("global-mean");
        // One fixed length: a symbolic one made tract mis-slice CAM++'s pooling (seen 2026-11-24,
        // tract 0.22.5), and a single plan keeps memory bounded.
        let model = onnx().model_for_proto_model(&proto).map_err(|e| load(&e))?;
        drop(proto);
        let plan = model
            .with_input_fact(0, f32::fact([1, FRAMES, BINS]).into())
            .and_then(|m| m.into_optimized())
            .and_then(|m| m.into_runnable())
            .map_err(|e| load(&e))?;
        Ok(Model {
            plan,
            fbank: Fbank::new(),
            scale,
            cmn,
            sha256,
        })
    }

    /// Lowercase hex of the model file, to tell which model made a stored voiceprint.
    pub fn sha256(&self) -> &str {
        &self.sha256
    }

    pub fn voiceprint(&self, window: &[i16]) -> Result<Voiceprint, Error> {
        if window.len() != WINDOW_SAMPLES {
            return Err(Error::WindowLength(window.len()));
        }
        let samples: Vec<f32> = window.iter().map(|&s| f32::from(s) * self.scale).collect();
        let mut feats = self.fbank.compute(&samples);
        if self.cmn {
            subtract_mean(&mut feats);
        }
        let run = |e: &dyn ToString| Error::Run(e.to_string());
        let input = Tensor::from_shape(&[1, FRAMES, BINS], &feats).map_err(|e| run(&e))?;
        let out = self
            .plan
            .run(tvec!(input.into_tvalue()))
            .map_err(|e| run(&e))?;
        let plain = out[0].try_as_plain_ram().map_err(|e| run(&e))?;
        let v = plain.as_slice::<f32>().map_err(|e| run(&e))?.to_vec();
        Voiceprint::from_raw(v)
            .ok_or_else(|| Error::Run("the model gave no usable embedding".into()))
    }
}
