//! Live standalone configuration shared with the Brain (ADR 0017).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use pinna_assist::config::{Place, Station};
use pinna_assist::pipeline::{Pipeline, Speech};
use pinna_assist::presets::{Behaviour, PresetId, Slots, preset};
use pinna_http::Secret;
use pinna_mcp::McpCredential;
use pinna_mcp::client::{McpClient, McpToolSet};

use crate::group::Persisted;

pub mod ack;
mod runlog;
pub mod service;
pub mod spawn;

/// The standalone snapshot the Brain reads. The service owns and replaces it.
pub struct AssistLive {
    pub chat_gen: u64,
    pub ready: bool,
    pub slots: Slots,
    pub label: String,
    pub pipeline: Option<Arc<Pipeline>>,
    /// The text-to-speech slot, whenever it is set up: Speak needs no more.
    pub speech: Option<Speech>,
    pub prompt: Option<String>,
    pub language: Option<String>,
    pub place: Option<Place>,
    pub follow_ups: bool,
    pub ack: Option<PathBuf>,
    pub switches: BTreeMap<String, bool>,
    pub stations: Arc<Vec<Station>>,
    /// The owner's enabled MCP servers that have an address.
    pub mcp: Vec<Arc<OwnerMcp>>,
    /// The satellite group as saved (ADR 0026). Its Debug output never
    /// shows the key.
    pub group: Persisted,
}

impl AssistLive {
    /// No configured pipeline: standalone mode is not ready.
    #[cfg(test)]
    pub fn empty() -> Self {
        Self {
            chat_gen: 0,
            ready: false,
            slots: Slots::all(false),
            label: String::new(),
            pipeline: None,
            speech: None,
            prompt: None,
            language: None,
            place: None,
            follow_ups: true,
            ack: None,
            switches: BTreeMap::new(),
            stations: Arc::new(Vec::new()),
            mcp: Vec::new(),
            group: Persisted::default(),
        }
    }

    /// The configured brain's limits and history; a model's without a pipeline.
    pub fn behaviour(&self) -> Behaviour {
        self.pipeline
            .as_ref()
            .map_or(preset(PresetId::Custom).behaviour, |p| p.behaviour)
    }
}

/// A down server is logged at most this often.
const LEFT_OUT_EVERY: Duration = Duration::from_secs(5 * 60);

/// One of the owner's MCP servers, whose tools a tool-loop brain may call as
/// `<name>_<tool>`. The service keeps it across snapshots while its name and
/// address stay the same, so its session and tool list carry over.
pub struct OwnerMcp {
    pub name: String,
    pub tools: McpToolSet,
    id: String,
    url: Secret,
    credential: Option<McpCredential>,
    left_out: Mutex<Option<Instant>>,
}

impl OwnerMcp {
    pub fn new(id: &str, name: &str, url: Secret, credential: Option<McpCredential>) -> Self {
        Self {
            name: name.to_owned(),
            tools: McpToolSet::new(
                name,
                Arc::new(McpClient::new(url.clone(), credential.as_ref())),
            ),
            id: id.to_owned(),
            url,
            credential,
            left_out: Mutex::new(None),
        }
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    /// The same server under the same name, address and credential.
    pub fn is(
        &self,
        id: &str,
        name: &str,
        url: &Secret,
        credential: Option<&McpCredential>,
    ) -> bool {
        self.id == id
            && self.name == name
            && self.url == *url
            && self.credential.as_ref() == credential
    }

    /// The server listed no tools at `now`, so the brain goes without it.
    /// True when that is worth a warning: once per five minutes.
    pub fn left_out(&self, now: Instant) -> bool {
        let mut last = self.left_out.lock().unwrap();
        if last.is_some_and(|at| now.saturating_duration_since(at) < LEFT_OUT_EVERY) {
            return false;
        }
        *last = Some(now);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_left_out_server_warns_once_every_five_minutes() {
        let server = OwnerMcp::new("mcp-1", "home", Secret::new("http://127.0.0.1:1/mcp"), None);
        let start = Instant::now();
        assert!(server.left_out(start));
        assert!(!server.left_out(start + Duration::from_secs(60)));
        assert!(!server.left_out(start + LEFT_OUT_EVERY - Duration::from_millis(1)));
        assert!(server.left_out(start + LEFT_OUT_EVERY));
        assert!(!server.left_out(start + LEFT_OUT_EVERY + Duration::from_secs(1)));
    }

    #[test]
    fn a_server_is_the_same_only_under_its_name_address_and_credential() {
        let url = Secret::new("https://mcp.example/token-1");
        let token = McpCredential::bearer("tok-1").unwrap();
        let server = OwnerMcp::new("mcp-1", "home", url.clone(), Some(token.clone()));
        assert!(server.is("mcp-1", "home", &url, Some(&token)));
        assert!(!server.is("mcp-2", "home", &url, Some(&token)));
        assert!(!server.is("mcp-1", "house", &url, Some(&token)));
        let other = Secret::new("https://mcp.example/token-2");
        assert!(!server.is("mcp-1", "home", &other, Some(&token)));
        let new = McpCredential::bearer("tok-2").unwrap();
        assert!(!server.is("mcp-1", "home", &url, Some(&new)));
        assert!(!server.is("mcp-1", "home", &url, None));
    }
}
