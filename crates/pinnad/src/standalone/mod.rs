//! Live standalone configuration shared with the Brain (ADR 0017).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use pinna_assist::config::{Place, Station};
use pinna_assist::pipeline::Pipeline;
use pinna_assist::presets::{Behaviour, PresetId, preset};
use pinna_mcp::client::McpToolSet;

/// The standalone snapshot the Brain reads. The service owns and replaces it.
#[allow(dead_code)]
pub struct AssistLive {
    pub generation: u64,
    pub chat_gen: u64,
    pub ready: bool,
    pub slots: [bool; 3],
    pub label: String,
    pub pipeline: Option<Arc<Pipeline>>,
    pub prompt: Option<String>,
    pub language: Option<String>,
    pub place: Option<Place>,
    pub follow_ups: bool,
    pub ack: Option<PathBuf>,
    pub switches: BTreeMap<String, bool>,
    pub stations: Arc<Vec<Station>>,
    pub mcp: Vec<Arc<McpToolSet>>,
}

impl AssistLive {
    /// No configured pipeline: standalone mode is not ready.
    pub fn empty() -> Self {
        Self {
            generation: 0,
            chat_gen: 0,
            ready: false,
            slots: [false; 3],
            label: String::new(),
            pipeline: None,
            prompt: None,
            language: None,
            place: None,
            follow_ups: true,
            ack: None,
            switches: BTreeMap::new(),
            stations: Arc::new(Vec::new()),
            mcp: Vec::new(),
        }
    }

    /// The configured brain's limits and history; a model's without a pipeline.
    pub fn behaviour(&self) -> Behaviour {
        self.pipeline
            .as_ref()
            .map_or(preset(PresetId::Custom).behaviour, |p| p.behaviour)
    }
}
