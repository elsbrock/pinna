//! The standalone configuration owner (ADR 0017, architecture §4 and §6).
//!
//! The service owns `standalone.toml` and `secrets.toml`. An edit is validated,
//! checked against its provider outside every lock, then committed with a
//! compare-and-set on the connection revision. Commits are serialised, written
//! off the runtime, and each one publishes a new [`AssistLive`] for the Brain.
//!
//! The owner's MCP servers are kept the same way. Their addresses may carry a
//! token, so they are secrets: stored in `secrets.toml` and shown by host only.

use std::collections::{BTreeMap, VecDeque};
use std::fmt;
use std::io;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use futures::future::join_all;
use pinna_assist::check::{self, CHECK_TIMEOUT, CheckSetup};
use pinna_assist::config::{
    self as assist, Ack, AssistConfig, Connection, MAX_MCP_SERVERS, McpServer, Place, SlotConfig,
    Station,
};
use pinna_assist::error::Slot;
use pinna_assist::http::Auth;
use pinna_assist::pipeline::{self, Pipeline};
use pinna_assist::presets::{
    self, ChatOverride, ChatProtocol, Delegation, HistoryPolicy, PresetId, SpeechProtocol,
};
use pinna_assist::secrets::{Secrets, SecretsFile};
use pinna_assist::tts::Tts;
use pinna_http::{ClientKind, Secret, reqwest};
use tokio::sync::{Mutex as AsyncMutex, watch};

use super::ack::{self, AckCache};
use super::{AssistLive, OwnerMcp};
use crate::checks;
use crate::settings::{self, Mode};
use crate::store::Store;
use crate::tools::radio;
use crate::tools::weather::{OpenMeteo, PlaceError};
use crate::voice::Snapshot;

/// At most this many connections.
pub const MAX_CONNECTIONS: usize = 16;
/// The dashboard lists this many MCP agents, most recent first (architecture §8).
pub const MAX_AGENTS: usize = 8;
/// An agent name is cut to this many characters.
pub const AGENT_NAME_MAX: usize = 40;
/// A model or voice id is at most this many characters.
const MODEL_MAX: usize = 200;
/// A key is at most this many bytes.
const KEY_MAX: usize = 1024;
/// The acknowledgment phrase is at most this many characters.
const ACK_MAX: usize = 100;
/// At most this many per-tool switches.
const MAX_SWITCHES: usize = 128;
/// A station name is at most this many characters.
const STATION_NAME_MAX: usize = 60;
/// A tool name is at most this many characters (the model's limit).
const TOOL_NAME_MAX: usize = 64;
/// The Advanced first-text limit, in seconds.
const FIRST_TEXT_S: std::ops::RangeInclusive<u64> = 1..=300;
/// The Advanced history caps (architecture §7, size caps).
const HISTORY_TURNS_MAX: usize = 10;
const HISTORY_BYTES_MAX: usize = 64 * 1024;
/// An MCP server address is at most this many bytes.
const MCP_URL_MAX: usize = 2048;

const CLOCK_SYNCED: &str = "/run/systemd/timesync/synchronized";

const NOT_SAVED: &str = "The assistant settings couldn't be saved. Try again.";
const CHANGED: &str = "This connection was changed meanwhile. Reload and try again.";
const REMOVED: &str = "This connection was removed meanwhile. Reload and try again.";
const NO_SUCH_PLACE: &str = "No place by that name was found. Try the nearest town or city.";
const MCP_CHANGED: &str = "This MCP server was changed meanwhile. Reload and try again.";
const MCP_REMOVED: &str = "This MCP server was removed meanwhile. Reload and try again.";
const MCP_URL: &str = "Enter the MCP server's whole address, starting with http:// or https://.";

/// Where the service keeps its files: beside `settings.toml`.
#[derive(Clone, Debug)]
pub struct Paths {
    pub config: PathBuf,
    pub secrets: PathBuf,
    /// The acknowledgment files.
    pub cache: PathBuf,
}

impl Paths {
    pub fn beside(settings_file: &Path) -> Self {
        Self {
            config: settings_file.with_file_name("standalone.toml"),
            secrets: settings_file.with_file_name("secrets.toml"),
            cache: settings_file.with_file_name("cache"),
        }
    }
}

/// Why an edit was not applied. Each carries one owner sentence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The edit itself is wrong.
    Invalid(String),
    /// The edit was based on a revision that changed meanwhile.
    Conflict(String),
    /// The edit is valid, but the provider or the device refuses it now.
    Precondition(String),
}

impl Refusal {
    pub fn sentence(&self) -> &str {
        match self {
            Self::Invalid(s) | Self::Conflict(s) | Self::Precondition(s) => s,
        }
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.sentence())
    }
}

impl std::error::Error for Refusal {}

/// The three provider slots.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlotKind {
    Stt,
    Chat,
    Tts,
}

impl SlotKind {
    pub const ALL: [SlotKind; 3] = [Self::Stt, Self::Chat, Self::Tts];

    fn slot(self) -> Slot {
        match self {
            Self::Stt => Slot::Stt,
            Self::Chat => Slot::Chat,
            Self::Tts => Slot::Tts,
        }
    }

    /// The slot as the owner reads it.
    fn name(self) -> &'static str {
        match self {
            Self::Stt => "speech-to-text",
            Self::Chat => "brain",
            Self::Tts => "text-to-speech",
        }
    }

    /// What the slot's `model` field holds.
    fn holds(self) -> &'static str {
        match self {
            Self::Tts => "voice",
            Self::Stt | Self::Chat => "model",
        }
    }

    fn get(self, cfg: &AssistConfig) -> Option<&SlotConfig> {
        match self {
            Self::Stt => cfg.stt.as_ref(),
            Self::Chat => cfg.chat.as_ref(),
            Self::Tts => cfg.tts.as_ref(),
        }
    }

    fn get_mut(self, cfg: &mut AssistConfig) -> &mut Option<SlotConfig> {
        match self {
            Self::Stt => &mut cfg.stt,
            Self::Chat => &mut cfg.chat,
            Self::Tts => &mut cfg.tts,
        }
    }

    fn served_by(self, preset: PresetId) -> bool {
        let slots = presets::preset(preset).slots;
        match self {
            Self::Stt => slots.stt,
            Self::Chat => slots.chat,
            Self::Tts => slots.tts,
        }
    }
}

/// What a connection edit does with the key.
#[derive(Clone, Debug)]
pub enum KeyEdit {
    /// Keep the key, unless the URL changes: a key belongs to its URL.
    Keep,
    /// A key typed for the edit's URL.
    Set(Secret),
    Clear,
}

/// Create (`id: None`) or change a connection.
#[derive(Clone, Debug)]
pub struct ConnectionEdit {
    pub id: Option<String>,
    /// The revision the owner edited; ignored when creating.
    pub revision: u64,
    pub preset: PresetId,
    /// Empty means the preset's address. For a local server preset, a bare
    /// host is enough: the preset adds the port and path.
    pub base_url: String,
    pub key: KeyEdit,
}

/// Point a slot at a connection with a model, or for text-to-speech a voice.
#[derive(Clone, Debug)]
pub struct SlotEdit {
    pub connection: String,
    pub model: String,
}

/// Add (`id: None`) or change one of the owner's MCP servers. There is no
/// key: a server that needs one takes it in its address.
#[derive(Clone, Debug)]
pub struct McpEdit {
    pub id: Option<String>,
    /// The revision the owner edited; ignored when adding.
    pub revision: u64,
    pub name: String,
    /// The whole address. `None` keeps the stored one; needed when adding.
    pub url: Option<Secret>,
    pub enabled: bool,
}

/// Settings that need no provider check. `None` leaves a field unchanged; for
/// the optional fields, `Some(None)` clears it.
#[derive(Clone, Debug, Default)]
pub struct AssistantPatch {
    pub language: Option<Option<String>>,
    /// `Some(None)` restores the voice rules; an empty prompt keeps only the
    /// context.
    pub prompt: Option<Option<String>>,
    pub follow_ups: Option<bool>,
    pub ack: Option<Ack>,
    /// The whole switch table; an absent name means on.
    pub tools: Option<BTreeMap<String, bool>>,
    pub chat_override: Option<ChatOverride>,
    /// A place name to look up; the coordinates are stored with it.
    pub place: Option<Option<String>>,
    /// The whole station list. Names must differ, as `play_station` finds a
    /// station by its name.
    pub stations: Option<Vec<Station>>,
}

/// Everything the Assistant pages show. Keys appear only as "set" and their
/// last four characters, and MCP servers only by name and host.
#[derive(Clone, Debug, PartialEq)]
pub struct AssistantView {
    pub ready: bool,
    pub connections: Vec<ConnectionView>,
    pub stt: Option<SlotView>,
    pub chat: Option<SlotView>,
    pub tts: Option<SlotView>,
    pub chat_override: ChatOverride,
    pub language: Option<String>,
    pub place: Option<Place>,
    pub prompt: Option<String>,
    pub follow_ups: bool,
    pub ack: Ack,
    /// The phrase the acknowledgment says in the current language.
    pub ack_spoken: String,
    pub tools: BTreeMap<String, bool>,
    pub stations: Vec<Station>,
    pub mcp_servers: Vec<McpServerView>,
    pub agents: Vec<AgentSeen>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConnectionView {
    pub id: String,
    pub preset: PresetId,
    pub base_url: String,
    pub revision: u64,
    pub key_set: bool,
    pub key_last4: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SlotView {
    pub connection: String,
    pub model: String,
    /// The slot passed its check at the connection's current revision.
    pub ready: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpServerView {
    pub id: String,
    pub name: String,
    pub host: String,
    pub enabled: bool,
    pub revision: u64,
}

/// An MCP client that called `/mcp` (architecture §8). Never a metric label.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentSeen {
    pub name: String,
    pub peer: IpAddr,
    pub last_seen: SystemTime,
}

/// The configuration owner. See the module docs.
pub struct Service {
    files: Arc<Files>,
    status: watch::Receiver<Snapshot>,
    /// The client for save-time checks.
    http: reqwest::Client,
    /// The place search.
    places: OpenMeteo,
    /// Serialises commits, including their file writes.
    writer: Arc<AsyncMutex<()>>,
    committed: Arc<Committed>,
    agents: Mutex<VecDeque<AgentSeen>>,
}

/// The applied configuration and the channel it is published on.
struct Committed {
    state: Mutex<State>,
    live: watch::Sender<Arc<AssistLive>>,
    ack: AckCache,
}

struct State {
    config: AssistConfig,
    secrets: Secrets,
    generation: u64,
    chat_gen: u64,
    brain: Option<BrainId>,
    /// The MCP servers last published, reused while unchanged.
    mcp: Vec<Arc<OwnerMcp>>,
}

/// What makes a brain a different brain: changing any of it forgets the
/// conversation (`chat_gen`). A key rotation does not.
#[derive(Clone, Debug, PartialEq)]
struct BrainId {
    connection: String,
    preset: PresetId,
    base_url: String,
    model: String,
    overrides: ChatOverride,
}

/// The configuration an edit works on before it is committed.
struct Draft {
    config: AssistConfig,
    secrets: Secrets,
}

impl Service {
    /// Load both files, salvaging them key by key, and publish the result.
    pub fn load(paths: Paths, status: watch::Receiver<Snapshot>) -> Arc<Self> {
        Self::load_with(paths, status, OpenMeteo::default())
    }

    fn load_with(paths: Paths, status: watch::Receiver<Snapshot>, places: OpenMeteo) -> Arc<Self> {
        let (config_store, config, _) =
            Store::load(&paths.config, settings::FILE_MODE, AssistConfig::default());
        let (secrets_store, secrets, _) = crate::secrets::load(&paths.secrets);
        let files = Arc::new(Files {
            config: config_store,
            secrets: secrets_store,
        });
        let mut state = State {
            config,
            secrets,
            generation: 0,
            chat_gen: 0,
            brain: None,
            mcp: Vec::new(),
        };
        let ack = AckCache::new(paths.cache.clone());
        let (live, _) = watch::channel(Arc::new(state.publish(&ack)));
        let service = Arc::new(Self {
            files,
            status,
            http: pinna_http::client(ClientKind::Provider {
                read: Some(CHECK_TIMEOUT),
            }),
            places,
            writer: Arc::new(AsyncMutex::new(())),
            committed: Arc::new(Committed {
                state: Mutex::new(state),
                live,
                ack,
            }),
            agents: Mutex::new(VecDeque::new()),
        });
        service.committed.refresh_ack();
        service
    }

    /// The snapshot the Brain reads; a new value follows every commit.
    pub fn live(&self) -> watch::Receiver<Arc<AssistLive>> {
        self.committed.live.subscribe()
    }

    pub fn view(&self) -> AssistantView {
        let state = self.committed.state.lock().unwrap();
        // Published under the same lock, so it matches the configuration.
        let ready = self.committed.live.borrow().ready;
        let agents = self.agents.lock().unwrap().iter().cloned().collect();
        view(&state.config, &state.secrets, ready, agents)
    }

    /// Create or change a connection. Slots that use it are checked again
    /// with the new address and key; in standalone mode every one must pass.
    pub async fn set_connection(&self, e: ConnectionEdit) -> Result<AssistantView, Refusal> {
        let url = connection_url(e.preset, &e.base_url)?;
        let key = match &e.key {
            KeyEdit::Set(key) => KeyEdit::Set(key_value(key)?),
            other => other.clone(),
        };
        let Some(id) = e.id else {
            return self.create_connection(e.preset, url, key).await;
        };

        let (current, current_key, used_by) = {
            let state = self.committed.state.lock().unwrap();
            let current = find(&state.config, &id)
                .cloned()
                .ok_or_else(|| Refusal::Conflict(REMOVED.into()))?;
            let current_key = state.secrets.key_for(&id, &current.base_url).cloned();
            (current, current_key, slots_on(&state.config, &id))
        };
        if current.revision != e.revision {
            return Err(Refusal::Conflict(CHANGED.into()));
        }
        let url_changed = current.base_url != url;
        let new_key = match &key {
            KeyEdit::Set(key) => Some(key.clone()),
            KeyEdit::Clear => None,
            KeyEdit::Keep if url_changed => None,
            KeyEdit::Keep => current_key.clone(),
        };
        let key_changed = match &key {
            KeyEdit::Set(_) => true,
            KeyEdit::Clear | KeyEdit::Keep => current_key.is_some() && new_key.is_none(),
        };
        if !url_changed && !key_changed && current.preset == e.preset {
            return Ok(self.view());
        }

        let checks = join_all(
            used_by
                .iter()
                .map(|(kind, model)| self.check(*kind, e.preset, &url, model, new_key.as_ref())),
        )
        .await;
        if self.standalone() {
            let failed: Vec<(SlotKind, &String)> = used_by
                .iter()
                .zip(&checks)
                .filter_map(|((kind, _), result)| result.as_ref().err().map(|e| (*kind, e)))
                .collect();
            if let Some((_, sentence)) = failed.first() {
                let kinds: Vec<SlotKind> = failed.iter().map(|(kind, _)| *kind).collect();
                return Err(Refusal::Precondition(format!(
                    "Not saved: {} {sentence} To save it anyway, switch to Home Assistant \
                     mode first.",
                    checks_failed(&kinds)
                )));
            }
        }

        self.commit(|d| {
            let connection =
                find(&d.config, &id).ok_or_else(|| Refusal::Conflict(REMOVED.into()))?;
            if connection.revision != current.revision {
                return Err(Refusal::Conflict(CHANGED.into()));
            }
            // A slot moved onto, off or within this connection while the
            // checks ran wasn't checked with the new address and key.
            if slots_on(&d.config, &id) != used_by {
                return Err(Refusal::Conflict(CHANGED.into()));
            }
            let connection =
                find_mut(&mut d.config, &id).ok_or_else(|| Refusal::Conflict(REMOVED.into()))?;
            connection.preset = e.preset;
            connection.base_url.clone_from(&url);
            connection.revision += 1;
            let revision = connection.revision;
            match &key {
                // The key is stored with the URL it was typed for.
                KeyEdit::Set(key) => d.secrets.set_key(&id, &url, key.clone()),
                KeyEdit::Clear => d.secrets.clear(&id),
                KeyEdit::Keep if url_changed => d.secrets.clear(&id),
                KeyEdit::Keep => {}
            }
            for ((kind, _), result) in used_by.iter().zip(&checks) {
                if let (Some(slot), Ok(())) = (kind.get_mut(&mut d.config), result) {
                    slot.checked = revision;
                }
            }
            Ok(())
        })
        .await
    }

    async fn create_connection(
        &self,
        preset: PresetId,
        url: String,
        key: KeyEdit,
    ) -> Result<AssistantView, Refusal> {
        self.commit(|d| {
            if d.config.connections.len() >= MAX_CONNECTIONS {
                return Err(Refusal::Invalid(format!(
                    "At most {MAX_CONNECTIONS} connections."
                )));
            }
            let id = next_id(&d.config, preset);
            // A stale entry written by another image must not lend its key.
            d.secrets.clear(&id);
            if let KeyEdit::Set(key) = key {
                d.secrets.set_key(&id, &url, key);
            }
            d.config.connections.push(Connection {
                id,
                preset,
                base_url: url,
                revision: 1,
            });
            Ok(())
        })
        .await
    }

    /// Remove a connection. In standalone mode one that a slot uses can't be
    /// removed; in Home Assistant mode those slots are cleared with it.
    pub async fn delete_connection(
        &self,
        id: &str,
        revision: u64,
    ) -> Result<AssistantView, Refusal> {
        let standalone = self.standalone();
        self.commit(|d| {
            let Some(connection) = find(&d.config, id) else {
                return Ok(());
            };
            if connection.revision != revision {
                return Err(Refusal::Conflict(CHANGED.into()));
            }
            let used: Vec<SlotKind> = slots_on(&d.config, id)
                .into_iter()
                .map(|(kind, _)| kind)
                .collect();
            if standalone && !used.is_empty() {
                return Err(Refusal::Precondition(format!(
                    "{} this connection. Choose another connection there first.",
                    slots_using(&used)
                )));
            }
            for kind in used {
                *kind.get_mut(&mut d.config) = None;
            }
            d.config
                .connections
                .retain(|connection| connection.id != id);
            d.secrets.clear(id);
            Ok(())
        })
        .await
    }

    /// Point a slot at a connection. The slot is saved only when its check
    /// passes, and is then ready.
    pub async fn set_slot(&self, kind: SlotKind, e: SlotEdit) -> Result<AssistantView, Refusal> {
        let model = model_value(kind, &e.model)?;
        let (connection, key) = {
            let state = self.committed.state.lock().unwrap();
            let connection = find(&state.config, &e.connection)
                .cloned()
                .ok_or_else(|| Refusal::Conflict(REMOVED.into()))?;
            let key = state
                .secrets
                .key_for(&connection.id, &connection.base_url)
                .cloned();
            (connection, key)
        };
        let fixed = kind == SlotKind::Chat && presets::preset(connection.preset).model.is_some();
        if model.is_empty() && !fixed {
            return Err(Refusal::Invalid(format!(
                "Enter the {} {}.",
                kind.name(),
                kind.holds()
            )));
        }
        self.check(
            kind,
            connection.preset,
            &connection.base_url,
            &model,
            key.as_ref(),
        )
        .await
        .map_err(Refusal::Precondition)?;

        self.commit(|d| {
            let current =
                find(&d.config, &connection.id).ok_or_else(|| Refusal::Conflict(REMOVED.into()))?;
            // The check proved this revision only.
            if current.revision != connection.revision {
                return Err(Refusal::Conflict(CHANGED.into()));
            }
            *kind.get_mut(&mut d.config) = Some(SlotConfig {
                connection: connection.id.clone(),
                model,
                checked: connection.revision,
            });
            Ok(())
        })
        .await
    }

    /// Change the settings that need no provider check.
    pub async fn update(&self, p: AssistantPatch) -> Result<AssistantView, Refusal> {
        let language = p
            .language
            .map(|language| match language {
                Some(tag) if !tag.trim().is_empty() => assist::language(&tag).map(Some),
                _ => Ok(None),
            })
            .transpose()
            .map_err(Refusal::Invalid)?;
        let prompt = p
            .prompt
            .map(|prompt| prompt.map(|text| assist::prompt(&text)).transpose())
            .transpose()
            .map_err(Refusal::Invalid)?;
        let ack = p.ack.map(ack_value).transpose()?;
        let tools = p.tools.map(switches_value).transpose()?;
        let chat_override = p.chat_override.map(override_value).transpose()?;
        let stations = p.stations.map(stations_value).transpose()?;
        let place = match p.place {
            Some(Some(name)) if !name.trim().is_empty() => {
                let language = match &language {
                    Some(language) => language.clone(),
                    None => self.committed.state.lock().unwrap().config.language.clone(),
                };
                match self.places.geocode(&name, language.as_deref()).await {
                    Ok(place) => Some(Some(place)),
                    Err(PlaceError::NotFound) => {
                        return Err(Refusal::Invalid(NO_SUCH_PLACE.into()));
                    }
                    Err(PlaceError::Unavailable(sentence)) => {
                        return Err(Refusal::Precondition(sentence));
                    }
                }
            }
            Some(_) => Some(None),
            None => None,
        };

        self.commit(|d| {
            let c = &mut d.config;
            if let Some(language) = language {
                c.language = language;
            }
            if let Some(prompt) = prompt {
                c.prompt = prompt;
            }
            if let Some(follow_ups) = p.follow_ups {
                c.follow_ups = follow_ups;
            }
            if let Some(ack) = ack {
                c.ack = ack;
            }
            if let Some(tools) = tools {
                c.tools = tools;
            }
            if let Some(chat_override) = chat_override {
                c.chat_override = chat_override;
            }
            if let Some(place) = place {
                c.place = place;
            }
            if let Some(stations) = stations {
                c.stations = stations;
            }
            Ok(())
        })
        .await
    }

    /// Add or change one of the owner's MCP servers. Its name becomes the
    /// prefix of its tools' names and a metric label, so names are unique.
    pub async fn set_mcp_server(&self, e: McpEdit) -> Result<AssistantView, Refusal> {
        let name = assist::mcp_name(&e.name).map_err(Refusal::Invalid)?;
        let url = e.url.as_ref().map(mcp_url).transpose()?;
        self.commit(|d| {
            let taken =
                |server: &McpServer| server.name == name && Some(&server.id) != e.id.as_ref();
            if d.config.mcp_servers.iter().any(taken) {
                return Err(Refusal::Invalid(format!(
                    "Another MCP server is called {name}."
                )));
            }
            let Some(id) = e.id else {
                if d.config.mcp_servers.len() >= MAX_MCP_SERVERS {
                    return Err(Refusal::Invalid(format!(
                        "At most {MAX_MCP_SERVERS} MCP servers."
                    )));
                }
                let url = url.ok_or_else(|| Refusal::Invalid(MCP_URL.into()))?;
                let id = next_mcp_id(&d.config);
                // A stale entry written by another image must not linger.
                d.secrets.clear_mcp(&id);
                d.secrets.set_mcp(&id, url, None);
                d.config.mcp_servers.push(McpServer {
                    host: d.secrets.mcp_host(&id).unwrap_or_default(),
                    id,
                    name,
                    enabled: e.enabled,
                    revision: 1,
                });
                return Ok(());
            };
            let server = d
                .config
                .mcp_servers
                .iter_mut()
                .find(|server| server.id == id)
                .ok_or_else(|| Refusal::Conflict(MCP_REMOVED.into()))?;
            if server.revision != e.revision {
                return Err(Refusal::Conflict(MCP_CHANGED.into()));
            }
            let stored = d.secrets.mcp(&id).map(|(url, _)| url.clone());
            let url = match (url, stored) {
                (Some(url), stored) => (stored.as_ref() != Some(&url)).then_some(url),
                // secrets.toml lost the address: the server can't be on without one.
                (None, None) if e.enabled => {
                    return Err(Refusal::Invalid(
                        "Enter the MCP server's address again.".into(),
                    ));
                }
                (None, _) => None,
            };
            if server.name == name && server.enabled == e.enabled && url.is_none() {
                return Ok(());
            }
            if let Some(url) = url {
                d.secrets.set_mcp(&id, url, None);
                server.host = d.secrets.mcp_host(&id).unwrap_or_default();
            }
            server.name = name;
            server.enabled = e.enabled;
            server.revision += 1;
            Ok(())
        })
        .await
    }

    /// Remove one of the owner's MCP servers and its address.
    pub async fn delete_mcp_server(
        &self,
        id: &str,
        revision: u64,
    ) -> Result<AssistantView, Refusal> {
        self.commit(|d| {
            let Some(server) = d.config.mcp_servers.iter().find(|s| s.id == id) else {
                return Ok(());
            };
            if server.revision != revision {
                return Err(Refusal::Conflict(MCP_CHANGED.into()));
            }
            d.config.mcp_servers.retain(|server| server.id != id);
            d.secrets.clear_mcp(id);
            Ok(())
        })
        .await
    }

    /// Remember an MCP client for the dashboard. The name is the client's own
    /// `clientInfo.name`, cut short and stripped of control characters.
    /// The `/mcp` handler records its callers here.
    pub fn agent_seen(&self, name: &str, peer: IpAddr) {
        let name: String = name
            .chars()
            .filter(|c| !c.is_control())
            .take(AGENT_NAME_MAX)
            .collect();
        let mut agents = self.agents.lock().unwrap();
        agents.retain(|agent| agent.name != name || agent.peer != peer);
        agents.push_front(AgentSeen {
            name,
            peer,
            last_seen: SystemTime::now(),
        });
        agents.truncate(MAX_AGENTS);
    }

    fn standalone(&self) -> bool {
        self.status.borrow().mode == Mode::Standalone
    }

    /// Make the acknowledgment the configuration now asks for, off the
    /// caller's path, and publish it once it is on disk.
    /// The save-time check for one slot, or the sentence why it failed.
    async fn check(
        &self,
        kind: SlotKind,
        preset: PresetId,
        base_url: &str,
        model: &str,
        key: Option<&Secret>,
    ) -> Result<(), String> {
        let setup = self.check_setup(kind, preset, base_url, model, key)?;
        check::check(&setup).await
    }

    fn check_setup(
        &self,
        kind: SlotKind,
        preset: PresetId,
        base_url: &str,
        model: &str,
        key: Option<&Secret>,
    ) -> Result<CheckSetup, String> {
        if !kind.served_by(preset) {
            return Err(format!(
                "The {} preset cannot provide {}.",
                preset.label(),
                kind.name()
            ));
        }
        let row = presets::preset(preset);
        // The text-to-speech slot holds a voice; the model is the pipeline's.
        let (model, voice) = match kind {
            SlotKind::Stt => (model.to_owned(), None),
            SlotKind::Chat => (row.model.unwrap_or(model).to_owned(), None),
            SlotKind::Tts => (
                pipeline::tts_model(row.speech_protocol).to_owned(),
                Some(model.to_owned()),
            ),
        };
        let auth = match (kind, key.cloned()) {
            (_, None) => Auth::None,
            (SlotKind::Chat, Some(key)) => match row.chat_protocol {
                ChatProtocol::OpenAi => Auth::Bearer(key),
                ChatProtocol::Anthropic => Auth::ApiKey("x-api-key", key),
                ChatProtocol::Gemini => Auth::ApiKey("x-goog-api-key", key),
            },
            (SlotKind::Stt | SlotKind::Tts, Some(key)) => match row.speech_protocol {
                SpeechProtocol::OpenAi => Auth::Bearer(key),
                SpeechProtocol::ElevenLabs => Auth::ApiKey("xi-api-key", key),
            },
        };
        let hand_off = kind == SlotKind::Chat && {
            let state = self.committed.state.lock().unwrap();
            row.behaviour
                .overridden(&state.config.chat_override)
                .delegation
                == Delegation::HandOff
        };
        Ok(CheckSetup {
            slot: kind.slot(),
            http: self.http.clone(),
            preset,
            base_url: base_url.to_owned(),
            model,
            voice,
            auth,
            hand_off,
            clock_synced,
        })
    }

    /// Apply `edit` to the committed configuration, write what changed and
    /// publish. Commits run one at a time. Once the edit is accepted, the
    /// write and the publish finish even if the caller goes away. A write
    /// that fails is rolled back, so memory and disk agree.
    async fn commit(
        &self,
        edit: impl FnOnce(&mut Draft) -> Result<(), Refusal>,
    ) -> Result<AssistantView, Refusal> {
        let writer = self.writer.clone().lock_owned().await;
        let mut draft = {
            let state = self.committed.state.lock().unwrap();
            Draft {
                config: state.config.clone(),
                secrets: state.secrets.clone(),
            }
        };
        edit(&mut draft)?;
        let (config, secrets) = {
            let state = self.committed.state.lock().unwrap();
            (
                (draft.config != state.config).then(|| draft.config.clone()),
                (draft.secrets.to_file() != state.secrets.to_file()).then(|| draft.secrets.clone()),
            )
        };
        if config.is_none() && secrets.is_none() {
            return Ok(self.view());
        }

        let files = self.files.clone();
        let committed = self.committed.clone();
        let saved = tokio::task::spawn_blocking(move || {
            let _writer = writer;
            // A key is bound to its URL, so a crash between the two writes
            // can lose a key but never pair it with another address.
            if let Some(secrets) = &secrets {
                crate::secrets::save(&files.secrets, secrets)?;
            }
            if let Some(config) = &config
                && let Err(e) = files.config.try_save(config)
            {
                if let Some(secrets) = secrets {
                    committed.roll_back_secrets(&files, secrets);
                }
                return Err(e);
            }
            committed.replace(draft);
            committed.refresh_ack();
            Ok::<_, io::Error>(())
        })
        .await;
        match saved {
            Ok(Ok(())) => Ok(self.view()),
            Ok(Err(e)) => {
                tracing::warn!("saving the assistant settings: {e}");
                Err(Refusal::Precondition(NOT_SAVED.into()))
            }
            Err(e) => {
                tracing::warn!("saving the assistant settings: {e}");
                Err(Refusal::Precondition(NOT_SAVED.into()))
            }
        }
    }
}

impl Committed {
    /// Synthesise the acknowledgment again if what it says or sounds like
    /// changed, then publish it.
    fn refresh_ack(self: &Arc<Self>) {
        // Without a runtime (plain unit tests) there is nothing to run it on.
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let committed = self.clone();
        runtime.spawn(async move {
            if committed.ack.refresh(|| committed.ack_wanted()).await {
                committed.republish();
            }
        });
    }

    /// The keys were written but the configuration wasn't: put the committed
    /// keys back on disk. If that fails too, memory takes the written keys,
    /// so the dashboard shows what the next start will read.
    fn roll_back_secrets(&self, files: &Files, written: Secrets) {
        let committed = self.state.lock().unwrap().secrets.clone();
        if let Err(e) = crate::secrets::save(&files.secrets, &committed) {
            tracing::warn!("putting the previous keys back after a failed save: {e}");
            let mut state = self.state.lock().unwrap();
            state.secrets = written;
            let live = state.publish(&self.ack);
            self.live.send_replace(Arc::new(live));
        }
    }

    fn replace(&self, draft: Draft) {
        let mut state = self.state.lock().unwrap();
        state.config = draft.config;
        state.secrets = draft.secrets;
        let live = state.publish(&self.ack);
        self.live.send_replace(Arc::new(live));
    }

    /// Publish again, with the acknowledgment that is now on disk.
    fn republish(&self) {
        let mut state = self.state.lock().unwrap();
        let live = state.publish(&self.ack);
        self.live.send_replace(Arc::new(live));
    }

    /// The acknowledgment the configuration asks for, and the voice to make
    /// it with.
    fn ack_wanted(&self) -> Option<(ack::Source, Arc<dyn Tts>)> {
        let state = self.state.lock().unwrap();
        let live = self.live.borrow();
        let pipeline = live.pipeline.as_ref()?;
        let tts_ready = slot_ready(&state.config, SlotKind::Tts);
        let source = ack::Source::wanted(&state.config, Some(pipeline), tts_ready)?;
        Some((source, pipeline.tts.clone()))
    }
}

impl State {
    /// The next snapshot for the Brain.
    fn publish(&mut self, cache: &AckCache) -> AssistLive {
        let brain = brain_id(&self.config);
        if brain != self.brain {
            self.chat_gen += 1;
            self.brain = brain;
        }
        self.generation += 1;
        let c = &self.config;
        let slots = SlotKind::ALL.map(|kind| slot_ready(c, kind));
        let pipeline = Pipeline::build(c, &self.secrets).ok().map(Arc::new);
        let ack = ack::Source::wanted(c, pipeline.as_deref(), slot_ready(c, SlotKind::Tts))
            .and_then(|source| cache.path(&source));
        AssistLive {
            generation: self.generation,
            chat_gen: self.chat_gen,
            ready: pipeline.is_some() && slots.iter().all(|ready| *ready),
            slots,
            label: pipeline
                .as_ref()
                .map(|pipeline| pipeline.label.clone())
                .unwrap_or_default(),
            pipeline,
            prompt: c.prompt.clone(),
            language: c.language.clone(),
            place: c.place.clone(),
            follow_ups: c.follow_ups,
            ack,
            switches: c.tools.clone(),
            stations: Arc::new(c.stations.clone()),
            mcp: self.owner_mcp(),
        }
    }

    /// The enabled MCP servers with an address, each kept from the last
    /// snapshot while its name and address are the same.
    fn owner_mcp(&mut self) -> Vec<Arc<OwnerMcp>> {
        let servers: Vec<Arc<OwnerMcp>> = self
            .config
            .mcp_servers
            .iter()
            .filter(|server| server.enabled)
            .filter_map(|server| {
                let (url, _) = self.secrets.mcp(&server.id)?;
                let kept = self
                    .mcp
                    .iter()
                    .find(|live| live.is(&server.id, &server.name, url))
                    .cloned();
                Some(kept.unwrap_or_else(|| {
                    Arc::new(OwnerMcp::new(&server.id, &server.name, url.clone()))
                }))
            })
            .take(MAX_MCP_SERVERS)
            .collect();
        self.mcp.clone_from(&servers);
        servers
    }
}

/// The two files the service writes, each through the store it was loaded
/// with, so entries this build couldn't read are written back.
struct Files {
    config: Store<AssistConfig>,
    secrets: Store<SecretsFile>,
}

fn view(c: &AssistConfig, secrets: &Secrets, ready: bool, agents: Vec<AgentSeen>) -> AssistantView {
    let slot = |kind: SlotKind| {
        kind.get(c).map(|slot| SlotView {
            connection: slot.connection.clone(),
            model: slot.model.clone(),
            ready: slot_ready(c, kind),
        })
    };
    AssistantView {
        ready,
        connections: c
            .connections
            .iter()
            .map(|connection| {
                let key_set = secrets
                    .key_for(&connection.id, &connection.base_url)
                    .is_some();
                ConnectionView {
                    id: connection.id.clone(),
                    preset: connection.preset,
                    base_url: connection.base_url.clone(),
                    revision: connection.revision,
                    key_set,
                    key_last4: key_set.then(|| secrets.last4(&connection.id)).flatten(),
                }
            })
            .collect(),
        stt: slot(SlotKind::Stt),
        chat: slot(SlotKind::Chat),
        tts: slot(SlotKind::Tts),
        chat_override: c.chat_override,
        language: c.language.clone(),
        place: c.place.clone(),
        prompt: c.prompt.clone(),
        follow_ups: c.follow_ups,
        ack: c.ack.clone(),
        ack_spoken: ack::phrase(&c.ack, c.language.as_deref()),
        tools: c.tools.clone(),
        stations: c.stations.clone(),
        mcp_servers: c
            .mcp_servers
            .iter()
            .map(|server| McpServerView {
                id: server.id.clone(),
                name: server.name.clone(),
                host: server.host.clone(),
                enabled: server.enabled,
                revision: server.revision,
            })
            .collect(),
        agents,
    }
}

/// The slots that use a connection, with their models.
fn slots_on(c: &AssistConfig, id: &str) -> Vec<(SlotKind, String)> {
    SlotKind::ALL
        .into_iter()
        .filter_map(|kind| {
            let slot = kind.get(c)?;
            (slot.connection == id).then(|| (kind, slot.model.clone()))
        })
        .collect()
}

/// "the brain check failed.", "the speech-to-text and brain checks failed.", ...
fn checks_failed(kinds: &[SlotKind]) -> String {
    let names: Vec<&str> = kinds.iter().map(|kind| kind.name()).collect();
    match names.as_slice() {
        [one] => format!("the {one} check failed."),
        [first @ .., last] => format!("the {} and {last} checks failed.", first.join(", ")),
        [] => String::new(),
    }
}

/// "The brain slot uses", "The speech-to-text and brain slots use", ...
fn slots_using(kinds: &[SlotKind]) -> String {
    let names: Vec<&str> = kinds.iter().map(|kind| kind.name()).collect();
    match names.as_slice() {
        [one] => format!("The {one} slot uses"),
        [first @ .., last] => format!("The {} and {last} slots use", first.join(", ")),
        [] => String::new(),
    }
}

fn find<'a>(c: &'a AssistConfig, id: &str) -> Option<&'a Connection> {
    c.connections.iter().find(|connection| connection.id == id)
}

fn find_mut<'a>(c: &'a mut AssistConfig, id: &str) -> Option<&'a mut Connection> {
    c.connections
        .iter_mut()
        .find(|connection| connection.id == id)
}

/// A slot is ready when its connection serves it and it passed its check at
/// the connection's current revision.
fn slot_ready(c: &AssistConfig, kind: SlotKind) -> bool {
    kind.get(c).is_some_and(|slot| {
        find(c, &slot.connection).is_some_and(|connection| {
            kind.served_by(connection.preset) && slot.checked == connection.revision
        })
    })
}

fn brain_id(c: &AssistConfig) -> Option<BrainId> {
    let slot = c.chat.as_ref()?;
    let connection = find(c, &slot.connection)?;
    Some(BrainId {
        connection: connection.id.clone(),
        preset: connection.preset,
        base_url: connection.base_url.clone(),
        model: slot.model.clone(),
        overrides: c.chat_override,
    })
}

/// The first free `mcp-<n>`.
fn next_mcp_id(c: &AssistConfig) -> String {
    (1..)
        .map(|n| format!("mcp-{n}"))
        .find(|id| c.mcp_servers.iter().all(|server| server.id != *id))
        .expect("an unbounded range has a free id")
}

/// The first free `<preset>-<n>`.
fn next_id(c: &AssistConfig, preset: PresetId) -> String {
    (1..)
        .map(|n| format!("{}-{n}", preset.label()))
        .find(|id| find(c, id).is_none())
        .expect("an unbounded range has a free id")
}

/// The connection's base URL as it is stored.
fn connection_url(preset: PresetId, input: &str) -> Result<String, Refusal> {
    let template = presets::preset(preset).base_url;
    let input = input.trim();
    let url = if input.is_empty() {
        if template.is_empty() || template.contains('<') {
            return Err(Refusal::Invalid("Enter the server's address.".into()));
        }
        template.to_owned()
    } else if !input.contains("://") && template.contains("<host>") {
        // A local server: the owner enters the host, the preset adds the rest.
        if template.contains("<port>") {
            template.replace("<host>:<port>", input)
        } else {
            template.replace("<host>", input)
        }
    } else {
        input.to_owned()
    };
    assist::base_url(&url).map_err(Refusal::Invalid)
}

/// An MCP server address as it is stored. Unlike a provider's, it may carry a
/// query, which is where a server that needs a key often takes it. A refusal
/// never repeats the address.
fn mcp_url(url: &Secret) -> Result<Secret, Refusal> {
    let url = url.expose().trim();
    let refuse = |sentence: &str| Err(Refusal::Invalid(sentence.into()));
    if url.len() > MCP_URL_MAX {
        return refuse("That address is too long.");
    }
    if url.chars().any(char::is_whitespace) {
        return refuse("An address has no spaces.");
    }
    let Ok(parsed) = url::Url::parse(url) else {
        return refuse(MCP_URL);
    };
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return refuse(MCP_URL);
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return refuse("Leave out the user name and password.");
    }
    if parsed.fragment().is_some() {
        return refuse("Leave out the # and everything after it.");
    }
    Ok(Secret::new(url))
}

fn key_value(key: &Secret) -> Result<Secret, Refusal> {
    let key = key.expose().trim();
    if key.is_empty() {
        return Err(Refusal::Invalid("Enter the key.".into()));
    }
    if key.len() > KEY_MAX {
        return Err(Refusal::Invalid("That key is too long.".into()));
    }
    // It goes into a header as it is.
    if !key.chars().all(|c| c.is_ascii_graphic()) {
        return Err(Refusal::Invalid(
            "A key is letters, digits and symbols, with no spaces.".into(),
        ));
    }
    Ok(Secret::new(key))
}

fn model_value(kind: SlotKind, model: &str) -> Result<String, Refusal> {
    let model = model.trim();
    if model.chars().count() > MODEL_MAX || model.chars().any(char::is_control) {
        return Err(Refusal::Invalid(format!(
            "A {} is at most {MODEL_MAX} characters on one line.",
            kind.holds()
        )));
    }
    Ok(model.to_owned())
}

fn ack_value(ack: Ack) -> Result<Ack, Refusal> {
    let phrase = ack.phrase.trim();
    if ack.enabled && phrase.is_empty() {
        return Err(Refusal::Invalid("Enter the acknowledgment.".into()));
    }
    if phrase.chars().count() > ACK_MAX {
        return Err(Refusal::Invalid(format!(
            "The acknowledgment is at most {ACK_MAX} characters."
        )));
    }
    Ok(Ack {
        enabled: ack.enabled,
        phrase: phrase.to_owned(),
    })
}

fn switches_value(tools: BTreeMap<String, bool>) -> Result<BTreeMap<String, bool>, Refusal> {
    if tools.len() > MAX_SWITCHES {
        return Err(Refusal::Invalid(format!(
            "At most {MAX_SWITCHES} tool switches."
        )));
    }
    let valid = |name: &str| {
        (1..=TOOL_NAME_MAX).contains(&name.len())
            && name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    };
    if let Some(name) = tools.keys().find(|name| !valid(name)) {
        return Err(Refusal::Invalid(format!("{name:?} isn't a tool name.")));
    }
    Ok(tools)
}

/// The owner's stations as they are stored. An address is checked as the
/// media lane's (`checks::media_url`): stream addresses often carry a query.
fn stations_value(stations: Vec<Station>) -> Result<Vec<Station>, Refusal> {
    if stations.len() > assist::MAX_STATIONS {
        return Err(Refusal::Invalid(format!(
            "At most {} stations.",
            assist::MAX_STATIONS
        )));
    }
    let mut names = std::collections::HashSet::new();
    stations
        .into_iter()
        .map(|station| {
            let name = station.name.trim();
            if name.is_empty() || radio::key(name).is_empty() {
                return Err(Refusal::Invalid("Enter a station name.".into()));
            }
            if name.chars().count() > STATION_NAME_MAX || name.chars().any(char::is_control) {
                return Err(Refusal::Invalid(format!(
                    "A station name is at most {STATION_NAME_MAX} characters on one line."
                )));
            }
            if !names.insert(radio::key(name)) {
                return Err(Refusal::Invalid(format!(
                    "Two stations are named {name}. Give each its own name."
                )));
            }
            let url = checks::media_url(&station.url).map_err(|_| {
                Refusal::Invalid(format!(
                    "Enter the address of {name}, starting with http:// or https://."
                ))
            })?;
            Ok(Station {
                name: name.to_owned(),
                url,
            })
        })
        .collect()
}

fn override_value(o: ChatOverride) -> Result<ChatOverride, Refusal> {
    if let Some(s) = o.first_text_s
        && !FIRST_TEXT_S.contains(&s)
    {
        return Err(Refusal::Invalid(format!(
            "The first-text limit is {} to {} seconds.",
            FIRST_TEXT_S.start(),
            FIRST_TEXT_S.end()
        )));
    }
    match o.history {
        Some(HistoryPolicy::Turns(n)) if !(1..=HISTORY_TURNS_MAX).contains(&n) => Err(
            Refusal::Invalid(format!("Keep 1 to {HISTORY_TURNS_MAX} turns.")),
        ),
        Some(HistoryPolicy::Bytes(n)) if !(1..=HISTORY_BYTES_MAX).contains(&n) => {
            Err(Refusal::Invalid("Keep at most 64 KiB of history.".into()))
        }
        _ => Ok(o),
    }
}

fn clock_synced() -> bool {
    Path::new(CLOCK_SYNCED).exists()
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::time::Duration;

    use pinna_assist::reply::AudioBody;
    use pinna_assist::testing::{FakeResponse, FakeServer};
    use pinna_http::reqwest::{Method, StatusCode};

    use super::*;

    const KEY: &str = "sk-test-abcdef123456";

    struct Fixture {
        service: Arc<Service>,
        status: watch::Sender<Snapshot>,
        dir: tempfile::TempDir,
    }

    impl Fixture {
        fn new(mode: Mode) -> Self {
            Self::with_places(mode, OpenMeteo::default())
        }

        fn with_places(mode: Mode, places: OpenMeteo) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let (status, rx) = watch::channel(Snapshot {
                mode,
                ..Snapshot::default()
            });
            let service =
                Service::load_with(Paths::beside(&dir.path().join("settings.toml")), rx, places);
            Self {
                service,
                status,
                dir,
            }
        }

        fn set_mode(&self, mode: Mode) {
            self.status.send_modify(|status| status.mode = mode);
        }

        fn reload(&self) -> Arc<Service> {
            Service::load(
                Paths::beside(&self.dir.path().join("settings.toml")),
                self.status.subscribe(),
            )
        }

        fn file(&self, name: &str) -> String {
            fs::read_to_string(self.dir.path().join(name)).unwrap_or_default()
        }

        fn saved(&self) -> AssistConfig {
            toml::from_str(&self.file("standalone.toml")).unwrap()
        }

        fn live(&self) -> Arc<AssistLive> {
            self.service.live().borrow().clone()
        }

        /// One OpenAI-compatible connection on `server`, with `KEY`.
        async fn connect(&self, server: &FakeServer) -> ConnectionView {
            let view = self
                .service
                .set_connection(new_connection(server, KeyEdit::Set(Secret::new(KEY))))
                .await
                .unwrap();
            view.connections.last().unwrap().clone()
        }

        /// All three slots on one connection to `server`, each checked.
        async fn ready(&self, server: &FakeServer) -> ConnectionView {
            let connection = self.connect(server).await;
            for (kind, model) in [
                (SlotKind::Stt, "whisper-1"),
                (SlotKind::Chat, "gpt-test"),
                (SlotKind::Tts, "nova"),
            ] {
                passing(server, kind);
                self.service
                    .set_slot(kind, slot_edit(&connection.id, model))
                    .await
                    .unwrap();
            }
            assert!(self.live().ready);
            connection
        }
    }

    fn new_connection(server: &FakeServer, key: KeyEdit) -> ConnectionEdit {
        ConnectionEdit {
            id: None,
            revision: 0,
            preset: PresetId::OpenAi,
            base_url: server.url(""),
            key,
        }
    }

    fn edit(connection: &ConnectionView, base_url: String, key: KeyEdit) -> ConnectionEdit {
        ConnectionEdit {
            id: Some(connection.id.clone()),
            revision: connection.revision,
            preset: connection.preset,
            base_url,
            key,
        }
    }

    fn slot_edit(connection: &str, model: &str) -> SlotEdit {
        SlotEdit {
            connection: connection.into(),
            model: model.into(),
        }
    }

    const MODELS: &str = r#"{"data":[{"id":"whisper-1"},{"id":"gpt-test"}]}"#;

    /// Script one passing check for `kind` on an OpenAI-compatible server.
    fn passing(server: &FakeServer, kind: SlotKind) {
        passing_after(server, kind, Duration::ZERO);
    }

    /// The same, with the response body arriving after `delay`.
    fn passing_after(server: &FakeServer, kind: SlotKind, delay: Duration) {
        let (method, path, body) = match kind {
            SlotKind::Stt | SlotKind::Chat => (Method::GET, "/models", MODELS),
            SlotKind::Tts => (Method::POST, "/audio/speech", "audio"),
        };
        let (head, tail) = body.split_at(1);
        server.script(
            method,
            path,
            FakeResponse::chunked(StatusCode::OK, [head.to_owned(), tail.to_owned()], delay),
        );
    }

    async fn speak_through_live_pipeline(live: &AssistLive) {
        let audio = live
            .pipeline
            .as_ref()
            .unwrap()
            .tts
            .speak("Hello.")
            .await
            .unwrap();
        assert!(matches!(audio.body, AudioBody::Stream(_)));
    }

    #[tokio::test]
    async fn a_failed_configuration_write_puts_the_previous_key_back() {
        let f = Fixture::new(Mode::HomeAssistant);
        let server = FakeServer::start().await;
        let connection = f.connect(&server).await;
        let keys_before = f.file("secrets.toml");
        // The configuration can't be replaced; the keys file still can.
        let config = f.dir.path().join("standalone.toml");
        fs::remove_file(&config).unwrap();
        fs::create_dir_all(config.join("blocked")).unwrap();

        let refused = f
            .service
            .set_connection(edit(
                &connection,
                server.url("/elsewhere"),
                KeyEdit::Set(Secret::new("sk-new-9999")),
            ))
            .await
            .unwrap_err();

        assert_eq!(refused, Refusal::Precondition(NOT_SAVED.into()));
        assert_eq!(f.file("secrets.toml"), keys_before);
        let view = f.service.view();
        assert_eq!(view.connections[0].base_url, connection.base_url);
        assert_eq!(view.connections[0].key_last4.as_deref(), Some("3456"));
    }

    #[tokio::test]
    async fn a_failed_check_persists_nothing() {
        let f = Fixture::new(Mode::HomeAssistant);
        let server = FakeServer::start().await;
        let connection = f.connect(&server).await;
        let before = f.file("standalone.toml");
        let generation = f.live().generation;
        server.script_status(Method::GET, "/models", StatusCode::UNAUTHORIZED);

        let refused = f
            .service
            .set_slot(SlotKind::Chat, slot_edit(&connection.id, "gpt-test"))
            .await
            .unwrap_err();

        assert_eq!(
            refused,
            Refusal::Precondition("The key is wrong. Enter it again.".into())
        );
        assert!(f.service.view().chat.is_none());
        assert_eq!(f.file("standalone.toml"), before);
        assert_eq!(f.live().generation, generation);
        assert!(!f.live().slots[1]);
    }

    #[tokio::test]
    async fn a_passing_check_makes_the_slot_ready() {
        let f = Fixture::new(Mode::HomeAssistant);
        let server = FakeServer::start().await;
        let connection = f.connect(&server).await;
        passing(&server, SlotKind::Chat);

        let view = f
            .service
            .set_slot(SlotKind::Chat, slot_edit(&connection.id, "gpt-test"))
            .await
            .unwrap();

        assert_eq!(
            view.chat,
            Some(SlotView {
                connection: connection.id.clone(),
                model: "gpt-test".into(),
                ready: true,
            })
        );
        assert!(!view.ready);
        assert_eq!(f.live().slots, [false, true, false]);
        assert!(!f.live().ready);
        let request = &server.requests()[0];
        assert_eq!(request.path, "/models");
        assert_eq!(
            request.header("authorization"),
            Some(format!("Bearer {KEY}").as_str())
        );

        for (kind, model) in [(SlotKind::Stt, "whisper-1"), (SlotKind::Tts, "nova")] {
            passing(&server, kind);
            f.service
                .set_slot(kind, slot_edit(&connection.id, model))
                .await
                .unwrap();
        }
        let live = f.live();
        assert!(live.ready);
        assert_eq!(live.slots, [true; 3]);
        assert_eq!(live.label, "openai: gpt-test");
        assert!(f.service.view().ready);
        // The text-to-speech check used the pipeline's model and the voice.
        let speech = server.requests().pop().unwrap();
        let sent: serde_json::Value = serde_json::from_slice(&speech.body).unwrap();
        assert_eq!(sent["model"], pipeline::tts_model(SpeechProtocol::OpenAi));
        assert_eq!(sent["voice"], "nova");

        // The checked revisions are stored: a restart is ready at once.
        let reloaded = f.reload();
        assert!(reloaded.live().borrow().ready);
        assert_eq!(reloaded.view(), f.service.view());
    }

    #[tokio::test]
    async fn a_url_change_clears_the_key_in_the_view_and_in_the_live_pipeline() {
        let f = Fixture::new(Mode::HomeAssistant);
        let old = FakeServer::start().await;
        let connection = f.ready(&old).await;
        assert_eq!(connection.key_last4.as_deref(), Some("3456"));
        let new = FakeServer::start().await;
        for kind in SlotKind::ALL {
            passing(&new, kind);
        }

        let view = f
            .service
            .set_connection(edit(&connection, new.url(""), KeyEdit::Keep))
            .await
            .unwrap();

        let changed = &view.connections[0];
        assert_eq!(changed.base_url, new.url(""));
        assert_eq!(changed.revision, connection.revision + 1);
        assert!(!changed.key_set);
        assert_eq!(changed.key_last4, None);
        assert!(!f.file("secrets.toml").contains(KEY));
        // The keyless server passed every check, so the slots stay ready.
        assert!(view.ready);
        let live = f.live();
        assert!(live.ready);
        new.take_requests();
        new.script(
            Method::POST,
            "/audio/speech",
            FakeResponse::new(StatusCode::OK, "audio"),
        );
        speak_through_live_pipeline(&live).await;
        let sent = new.requests();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].header("authorization"), None);
    }

    #[tokio::test]
    async fn a_new_key_is_bound_to_the_url_it_was_typed_with() {
        let f = Fixture::new(Mode::HomeAssistant);
        let old = FakeServer::start().await;
        let connection = f.connect(&old).await;
        let new = FakeServer::start().await;

        f.service
            .set_connection(edit(
                &connection,
                new.url(""),
                KeyEdit::Set(Secret::new("  sk-other-9876 ")),
            ))
            .await
            .unwrap();

        let (_, secrets, _) = crate::secrets::load(&f.dir.path().join("secrets.toml"));
        assert_eq!(
            secrets
                .key_for(&connection.id, &new.url(""))
                .unwrap()
                .expose(),
            "sk-other-9876"
        );
        assert!(secrets.key_for(&connection.id, &old.url("")).is_none());

        let changed = f.service.view().connections[0].clone();
        let view = f
            .service
            .set_connection(edit(&changed, changed.base_url.clone(), KeyEdit::Clear))
            .await
            .unwrap();
        assert!(!view.connections[0].key_set);
        assert_eq!(view.connections[0].revision, changed.revision + 1);
        assert!(!f.file("secrets.toml").contains("sk-other-9876"));
    }

    #[tokio::test]
    async fn in_home_assistant_mode_a_failing_connection_edit_is_saved_and_unreadies_its_slots() {
        let f = Fixture::new(Mode::HomeAssistant);
        let server = FakeServer::start().await;
        let connection = f.ready(&server).await;
        // Nothing is scripted on the new address, so every check fails.
        let unscripted = FakeServer::start().await;

        let view = f
            .service
            .set_connection(edit(&connection, unscripted.url(""), KeyEdit::Keep))
            .await
            .unwrap();

        assert_eq!(view.connections[0].base_url, unscripted.url(""));
        assert!(!view.ready);
        assert!(view.chat.as_ref().is_some_and(|slot| !slot.ready));
        assert_eq!(f.live().slots, [false; 3]);
        assert!(!f.live().ready);
        assert_eq!(f.saved().connections[0].base_url, unscripted.url(""));
    }

    #[tokio::test]
    async fn in_standalone_mode_a_connection_edit_must_pass_its_checks() {
        let f = Fixture::new(Mode::Standalone);
        let server = FakeServer::start().await;
        let connection = f.ready(&server).await;
        let before = (f.file("standalone.toml"), f.file("secrets.toml"));
        let generation = f.live().generation;
        server.script_status(Method::GET, "/models", StatusCode::UNAUTHORIZED);
        server.script_status(Method::GET, "/models", StatusCode::UNAUTHORIZED);
        passing(&server, SlotKind::Tts);

        let refused = f
            .service
            .set_connection(edit(
                &connection,
                connection.base_url.clone(),
                KeyEdit::Set(Secret::new("sk-wrong-0000")),
            ))
            .await
            .unwrap_err();

        assert_eq!(
            refused,
            Refusal::Precondition(
                "Not saved: the speech-to-text and brain checks failed. The key is wrong. \
                 Enter it again. To save it anyway, switch to Home Assistant mode first."
                    .into()
            )
        );
        assert_eq!((f.file("standalone.toml"), f.file("secrets.toml")), before);
        assert_eq!(f.live().generation, generation);
        assert!(f.live().ready);
        assert_eq!(
            f.service.view().connections[0].key_last4.as_deref(),
            Some("3456")
        );

        // A key that works is saved, and the slots stay ready.
        for kind in SlotKind::ALL {
            passing(&server, kind);
        }
        let view = f
            .service
            .set_connection(edit(
                &connection,
                connection.base_url.clone(),
                KeyEdit::Set(Secret::new("sk-right-1111")),
            ))
            .await
            .unwrap();
        assert_eq!(view.connections[0].key_last4.as_deref(), Some("1111"));
        assert!(view.ready);
        assert!(f.live().ready);
    }

    #[tokio::test]
    async fn a_connection_in_use_cannot_be_deleted_in_standalone_mode() {
        let f = Fixture::new(Mode::Standalone);
        let server = FakeServer::start().await;
        let connection = f.ready(&server).await;

        let refused = f
            .service
            .delete_connection(&connection.id, connection.revision)
            .await
            .unwrap_err();
        assert_eq!(
            refused,
            Refusal::Precondition(
                "The speech-to-text, brain and text-to-speech slots use this connection. Choose another connection there first."
                    .into()
            )
        );
        assert!(f.live().ready);

        // In Home Assistant mode it goes, and takes its slots and key along.
        f.set_mode(Mode::HomeAssistant);
        let view = f
            .service
            .delete_connection(&connection.id, connection.revision)
            .await
            .unwrap();
        assert!(view.connections.is_empty());
        assert!(view.stt.is_none() && view.chat.is_none() && view.tts.is_none());
        assert!(!f.live().ready);
        assert!(f.live().pipeline.is_none());
        assert!(!f.file("secrets.toml").contains(KEY));
    }

    #[tokio::test]
    async fn a_stale_revision_is_a_conflict() {
        let f = Fixture::new(Mode::HomeAssistant);
        let server = FakeServer::start().await;
        let first = f.connect(&server).await;
        let view = f
            .service
            .set_connection(edit(&first, server.url("/v2"), KeyEdit::Keep))
            .await
            .unwrap();
        assert_eq!(view.connections[0].revision, first.revision + 1);
        let saved = f.file("standalone.toml");

        let stale = f
            .service
            .set_connection(edit(&first, server.url("/v3"), KeyEdit::Keep))
            .await
            .unwrap_err();
        assert!(matches!(stale, Refusal::Conflict(_)), "{stale:?}");
        let stale = f
            .service
            .delete_connection(&first.id, first.revision)
            .await
            .unwrap_err();
        assert!(matches!(stale, Refusal::Conflict(_)), "{stale:?}");
        assert_eq!(f.file("standalone.toml"), saved);
    }

    #[tokio::test]
    async fn concurrent_edits_serialise() {
        let f = Fixture::new(Mode::HomeAssistant);
        let server = FakeServer::start().await;
        let connection = f.connect(&server).await;
        let slow = Duration::from_millis(100);
        passing_after(&server, SlotKind::Stt, slow);
        passing_after(&server, SlotKind::Tts, slow);

        // Both checks run at once; neither commit loses the other's slot.
        let (stt, tts) = tokio::join!(
            f.service
                .set_slot(SlotKind::Stt, slot_edit(&connection.id, "whisper-1")),
            f.service
                .set_slot(SlotKind::Tts, slot_edit(&connection.id, "nova")),
        );
        stt.unwrap();
        tts.unwrap();
        let saved = f.saved();
        assert_eq!(saved.stt.unwrap().model, "whisper-1");
        assert_eq!(saved.tts.unwrap().model, "nova");
        assert_eq!(f.live().slots, [true, false, true]);

        // Two key edits from the same revision: one wins, one is told.
        passing_after(&server, SlotKind::Stt, slow);
        passing_after(&server, SlotKind::Tts, slow);
        passing_after(&server, SlotKind::Stt, slow);
        passing_after(&server, SlotKind::Tts, slow);
        let (one, two) = tokio::join!(
            f.service.set_connection(edit(
                &connection,
                connection.base_url.clone(),
                KeyEdit::Set(Secret::new("sk-one-1111")),
            )),
            f.service.set_connection(edit(
                &connection,
                connection.base_url.clone(),
                KeyEdit::Set(Secret::new("sk-two-2222")),
            )),
        );
        let (won, lost) = match (one, two) {
            (Ok(_), Err(lost)) => ("1111", lost),
            (Err(lost), Ok(_)) => ("2222", lost),
            other => panic!("exactly one edit must win: {other:?}"),
        };
        assert_eq!(lost, Refusal::Conflict(CHANGED.into()));
        let view = f.service.view();
        assert_eq!(view.connections[0].revision, connection.revision + 1);
        assert_eq!(view.connections[0].key_last4.as_deref(), Some(won));
        let (_, secrets, _) = crate::secrets::load(&f.dir.path().join("secrets.toml"));
        assert_eq!(secrets.last4(&connection.id).as_deref(), Some(won));
    }

    #[tokio::test]
    async fn a_slot_changed_during_a_connection_check_refuses_the_edit() {
        let f = Fixture::new(Mode::Standalone);
        let server = FakeServer::start().await;
        let connection = f.ready(&server).await;
        let slow = Duration::from_millis(200);
        // The key edit checks all three slots slowly; the voice change in
        // between is checked at once and saved first.
        passing_after(&server, SlotKind::Stt, slow);
        passing_after(&server, SlotKind::Chat, slow);
        passing_after(&server, SlotKind::Tts, slow);
        passing(&server, SlotKind::Tts);
        let (key_edit, voice) = tokio::join!(
            f.service.set_connection(edit(
                &connection,
                connection.base_url.clone(),
                KeyEdit::Set(Secret::new("sk-new-9999")),
            )),
            async {
                tokio::time::sleep(Duration::from_millis(50)).await;
                f.service
                    .set_slot(SlotKind::Tts, slot_edit(&connection.id, "alloy"))
                    .await
            },
        );
        voice.unwrap();
        assert_eq!(key_edit.unwrap_err(), Refusal::Conflict(CHANGED.into()));
        assert_eq!(f.saved().tts.unwrap().model, "alloy");
        assert_eq!(f.live().slots, [true; 3]);
        assert!(f.live().ready);
    }

    #[tokio::test]
    async fn the_view_never_contains_a_key() {
        let f = Fixture::new(Mode::HomeAssistant);
        let server = FakeServer::start().await;
        let connection = f.ready(&server).await;

        let view = f.service.view();
        assert!(view.connections[0].key_set);
        assert_eq!(view.connections[0].key_last4.as_deref(), Some("3456"));
        assert!(!format!("{view:?}").contains(KEY));
        assert!(!format!("{connection:?}").contains(KEY));
        // The key lives only in the owner-only secrets file.
        assert!(!f.file("standalone.toml").contains(KEY));
        assert!(f.file("secrets.toml").contains(KEY));
        let mode = fs::metadata(f.dir.path().join("secrets.toml"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, crate::secrets::FILE_MODE);
    }

    #[tokio::test]
    async fn a_new_connection_takes_the_preset_address_and_a_free_id() {
        let f = Fixture::new(Mode::HomeAssistant);
        let preset = |preset, base_url: &str| ConnectionEdit {
            id: None,
            revision: 0,
            preset,
            base_url: base_url.into(),
            key: KeyEdit::Keep,
        };
        f.service
            .set_connection(preset(PresetId::OpenAi, ""))
            .await
            .unwrap();
        f.service
            .set_connection(preset(PresetId::Ollama, " 192.168.1.5 "))
            .await
            .unwrap();
        let view = f
            .service
            .set_connection(preset(PresetId::OpenAi, "https://proxy.example/v1"))
            .await
            .unwrap();
        let connections: Vec<(&str, &str, u64)> = view
            .connections
            .iter()
            .map(|c| (c.id.as_str(), c.base_url.as_str(), c.revision))
            .collect();
        assert_eq!(
            connections,
            [
                ("openai-1", "https://api.openai.com/v1", 1),
                ("ollama-1", "http://192.168.1.5:11434/v1", 1),
                ("openai-2", "https://proxy.example/v1", 1),
            ]
        );

        for (preset_id, base_url, sentence) in [
            (PresetId::Ollama, "", "Enter the server's address."),
            (PresetId::Custom, "", "Enter the server's address."),
            (
                PresetId::Custom,
                "https://user:pw@example.com/v1",
                "Leave out the user name and password.",
            ),
        ] {
            assert_eq!(
                f.service
                    .set_connection(preset(preset_id, base_url))
                    .await
                    .unwrap_err(),
                Refusal::Invalid(sentence.into())
            );
        }
        let mut with_key = preset(PresetId::OpenAi, "");
        with_key.key = KeyEdit::Set(Secret::new("sk with space"));
        assert!(matches!(
            f.service.set_connection(with_key).await,
            Err(Refusal::Invalid(_))
        ));
    }

    #[tokio::test]
    async fn a_slot_its_preset_cannot_serve_is_refused_without_a_request() {
        let f = Fixture::new(Mode::HomeAssistant);
        let server = FakeServer::start().await;
        let view = f
            .service
            .set_connection(ConnectionEdit {
                preset: PresetId::OpenRouter,
                ..new_connection(&server, KeyEdit::Keep)
            })
            .await
            .unwrap();

        let refused = f
            .service
            .set_slot(
                SlotKind::Stt,
                slot_edit(&view.connections[0].id, "whisper-1"),
            )
            .await
            .unwrap_err();

        assert_eq!(
            refused,
            Refusal::Precondition("The openrouter preset cannot provide speech-to-text.".into())
        );
        let refused = f
            .service
            .set_slot(SlotKind::Chat, slot_edit(&view.connections[0].id, " "))
            .await
            .unwrap_err();
        assert_eq!(refused, Refusal::Invalid("Enter the brain model.".into()));
        assert!(server.requests().is_empty());
    }

    #[tokio::test]
    async fn settings_without_checks_are_validated_and_published() {
        let f = Fixture::new(Mode::Standalone);
        let view = f
            .service
            .update(AssistantPatch {
                language: Some(Some("de".into())),
                prompt: Some(Some(String::new())),
                follow_ups: Some(false),
                ack: Some(Ack {
                    enabled: true,
                    phrase: " Moment. ".into(),
                }),
                tools: Some([("weather".to_owned(), false)].into()),
                chat_override: Some(ChatOverride {
                    first_text_s: Some(90),
                    ..ChatOverride::default()
                }),
                place: None,
                stations: None,
            })
            .await
            .unwrap();
        assert_eq!(view.language.as_deref(), Some("de"));
        assert_eq!(view.prompt.as_deref(), Some(""));
        assert!(!view.follow_ups);
        assert_eq!(view.ack.phrase, "Moment.");
        let live = f.live();
        assert_eq!(live.language.as_deref(), Some("de"));
        assert!(!live.follow_ups);
        assert_eq!(live.switches.get("weather"), Some(&false));
        assert_eq!(f.saved().chat_override.first_text_s, Some(90));

        let cleared = f
            .service
            .update(AssistantPatch {
                language: Some(Some(" ".into())),
                prompt: Some(None),
                ..AssistantPatch::default()
            })
            .await
            .unwrap();
        assert_eq!(cleared.language, None);
        assert_eq!(cleared.prompt, None);

        for (patch, sentence) in [
            (
                AssistantPatch {
                    language: Some(Some("englisch".into())),
                    ..AssistantPatch::default()
                },
                "That isn't a language like en or en-US.",
            ),
            (
                AssistantPatch {
                    prompt: Some(Some("a".repeat(4_001))),
                    ..AssistantPatch::default()
                },
                "The prompt is at most 4,000 characters.",
            ),
            (
                AssistantPatch {
                    ack: Some(Ack {
                        enabled: true,
                        phrase: " ".into(),
                    }),
                    ..AssistantPatch::default()
                },
                "Enter the acknowledgment.",
            ),
            (
                AssistantPatch {
                    chat_override: Some(ChatOverride {
                        history: Some(HistoryPolicy::Turns(11)),
                        ..ChatOverride::default()
                    }),
                    ..AssistantPatch::default()
                },
                "Keep 1 to 10 turns.",
            ),
        ] {
            assert_eq!(
                f.service.update(patch).await.unwrap_err(),
                Refusal::Invalid(sentence.into())
            );
        }
    }

    #[tokio::test]
    async fn the_view_says_the_acknowledgment_as_it_is_spoken() {
        let f = Fixture::new(Mode::HomeAssistant);
        assert_eq!(f.service.view().ack_spoken, "One moment.");

        let german = f
            .service
            .update(AssistantPatch {
                language: Some(Some("de-AT".into())),
                ..AssistantPatch::default()
            })
            .await
            .unwrap();
        assert_eq!(german.ack.phrase, Ack::default().phrase);
        assert_eq!(german.ack_spoken, "Einen Moment.");

        let own = f
            .service
            .update(AssistantPatch {
                ack: Some(Ack {
                    enabled: false,
                    phrase: "Moment mal.".into(),
                }),
                ..AssistantPatch::default()
            })
            .await
            .unwrap();
        assert_eq!(own.ack_spoken, "Moment mal.");
    }

    #[tokio::test]
    async fn the_conversation_is_forgotten_only_when_the_brain_changes() {
        let f = Fixture::new(Mode::HomeAssistant);
        let server = FakeServer::start().await;
        let connection = f.ready(&server).await;
        let chat_gen = f.live().chat_gen;

        // A new key is the same brain.
        for kind in SlotKind::ALL {
            passing(&server, kind);
        }
        let view = f
            .service
            .set_connection(edit(
                &connection,
                connection.base_url.clone(),
                KeyEdit::Set(Secret::new("sk-rotated-4444")),
            ))
            .await
            .unwrap();
        assert_eq!(f.live().chat_gen, chat_gen);

        // Another model, or other limits, is not.
        passing(&server, SlotKind::Chat);
        f.service
            .set_slot(
                SlotKind::Chat,
                slot_edit(&view.connections[0].id, "whisper-1"),
            )
            .await
            .unwrap();
        assert_eq!(f.live().chat_gen, chat_gen + 1);
        f.service
            .update(AssistantPatch {
                chat_override: Some(ChatOverride {
                    first_text_s: Some(90),
                    ..ChatOverride::default()
                }),
                ..AssistantPatch::default()
            })
            .await
            .unwrap();
        assert_eq!(f.live().chat_gen, chat_gen + 2);
    }

    #[tokio::test]
    async fn a_place_is_looked_up_in_the_owner_language_and_published() {
        let server = FakeServer::start().await;
        let f = Fixture::with_places(
            Mode::Standalone,
            crate::tools::weather::tests::open_meteo(&server),
        );
        server.script(
            Method::GET,
            "/v1/search?name=Munich&count=1&language=de&format=json",
            FakeResponse::new(
                StatusCode::OK,
                r#"{"results": [{"name": "München", "latitude": 48.13743,
                    "longitude": 11.57549, "admin1": "Bayern", "country": "Deutschland"}]}"#,
            ),
        );
        let munich = Place {
            name: "München, Bayern, Deutschland".into(),
            lat: 48.13743,
            lon: 11.57549,
        };

        let view = f
            .service
            .update(AssistantPatch {
                language: Some(Some("de".into())),
                place: Some(Some("Munich".into())),
                ..AssistantPatch::default()
            })
            .await
            .unwrap();
        assert_eq!(view.place.as_ref(), Some(&munich));
        assert_eq!(f.live().place.as_ref(), Some(&munich));
        assert_eq!(f.saved().place.as_ref(), Some(&munich));

        server.script(
            Method::GET,
            "/v1/search?name=Nowhere&count=1&language=de&format=json",
            FakeResponse::new(StatusCode::OK, "{}"),
        );
        let place = |name: &str| AssistantPatch {
            place: Some(Some(name.into())),
            ..AssistantPatch::default()
        };
        assert_eq!(
            f.service.update(place("Nowhere")).await.unwrap_err(),
            Refusal::Invalid(NO_SUCH_PLACE.into())
        );
        assert_eq!(
            f.service.update(place("Elsewhere")).await.unwrap_err(),
            Refusal::Precondition("The place search answered 404. Try again later.".into())
        );
        assert_eq!(f.service.view().place.as_ref(), Some(&munich));

        let view = f
            .service
            .update(AssistantPatch {
                place: Some(None),
                ..AssistantPatch::default()
            })
            .await
            .unwrap();
        assert_eq!(view.place, None);
        assert_eq!(f.live().place, None);
    }

    const MCP_ADDRESS: &str = "https://mcp.example:8443/private-token?key=s3cret";

    fn add_mcp(name: &str, url: Option<&str>) -> McpEdit {
        McpEdit {
            id: None,
            revision: 0,
            name: name.into(),
            url: url.map(Secret::new),
            enabled: true,
        }
    }

    fn change_mcp(server: &McpServerView, name: &str, url: Option<&str>, enabled: bool) -> McpEdit {
        McpEdit {
            id: Some(server.id.clone()),
            revision: server.revision,
            name: name.into(),
            url: url.map(Secret::new),
            enabled,
        }
    }

    #[tokio::test]
    async fn an_mcp_server_shows_its_host_and_never_its_address() {
        let f = Fixture::new(Mode::Standalone);

        let view = f
            .service
            .set_mcp_server(add_mcp("home", Some(MCP_ADDRESS)))
            .await
            .unwrap();

        let home = McpServerView {
            id: "mcp-1".into(),
            name: "home".into(),
            host: "mcp.example:8443".into(),
            enabled: true,
            revision: 1,
        };
        assert_eq!(view.mcp_servers, std::slice::from_ref(&home));
        for text in [format!("{view:?}"), f.file("standalone.toml")] {
            assert!(!text.contains("private-token"), "{text}");
            assert!(!text.contains("s3cret"), "{text}");
        }
        assert!(f.file("secrets.toml").contains(MCP_ADDRESS));
        let live = f.live();
        assert_eq!(live.mcp.len(), 1);
        assert_eq!(live.mcp[0].name, "home");
        assert_eq!(f.reload().live().borrow().mcp.len(), 1);

        // A rename keeps the address.
        let view = f
            .service
            .set_mcp_server(change_mcp(&home, "house", None, true))
            .await
            .unwrap();
        let house = view.mcp_servers[0].clone();
        assert_eq!((house.name.as_str(), house.revision), ("house", 2));
        assert_eq!(house.host, "mcp.example:8443");
        let (_, secrets, _) = crate::secrets::load(&f.dir.path().join("secrets.toml"));
        assert_eq!(secrets.mcp("mcp-1").unwrap().0.expose(), MCP_ADDRESS);
        assert_eq!(f.live().mcp[0].name, "house");

        // A new address is entered whole and shows its own host.
        let view = f
            .service
            .set_mcp_server(change_mcp(
                &house,
                "house",
                Some("http://192.168.1.9:9000/mcp"),
                true,
            ))
            .await
            .unwrap();
        let moved = view.mcp_servers[0].clone();
        assert_eq!(
            (moved.host.as_str(), moved.revision),
            ("192.168.1.9:9000", 3)
        );
        assert!(!f.file("secrets.toml").contains("private-token"));

        // An edit made on an older revision is a conflict.
        let stale = f
            .service
            .set_mcp_server(change_mcp(&house, "home", None, true))
            .await
            .unwrap_err();
        assert_eq!(stale, Refusal::Conflict(MCP_CHANGED.into()));
        assert_eq!(
            f.service
                .delete_mcp_server(&house.id, house.revision)
                .await
                .unwrap_err(),
            Refusal::Conflict(MCP_CHANGED.into())
        );

        // Switched off, it leaves the brain; removed, its address goes too.
        let view = f
            .service
            .set_mcp_server(change_mcp(&moved, "house", None, false))
            .await
            .unwrap();
        assert!(!view.mcp_servers[0].enabled);
        assert!(f.live().mcp.is_empty());
        let off = view.mcp_servers[0].clone();
        let view = f
            .service
            .delete_mcp_server(&off.id, off.revision)
            .await
            .unwrap();
        assert!(view.mcp_servers.is_empty());
        assert!(!f.file("secrets.toml").contains("192.168.1.9"));
    }

    #[tokio::test]
    async fn mcp_servers_are_at_most_eight_with_unique_names_and_whole_addresses() {
        let f = Fixture::new(Mode::HomeAssistant);
        for n in 0..MAX_MCP_SERVERS {
            let url = format!("https://mcp{n}.example/mcp");
            f.service
                .set_mcp_server(add_mcp(&format!("s{n}"), Some(&url)))
                .await
                .unwrap();
        }
        assert_eq!(f.live().mcp.len(), MAX_MCP_SERVERS);
        let saved = (f.file("standalone.toml"), f.file("secrets.toml"));
        let s1 = f.service.view().mcp_servers[1].clone();

        for (edit, sentence) in [
            (
                add_mcp("s8", Some("https://mcp8.example/mcp")),
                "At most 8 MCP servers.",
            ),
            (
                add_mcp("s0", Some("https://other.example/mcp")),
                "Another MCP server is called s0.",
            ),
            (
                change_mcp(&s1, "s0", None, true),
                "Another MCP server is called s0.",
            ),
            (
                add_mcp("Home", Some(MCP_ADDRESS)),
                "A name is 1 to 24 lowercase letters, digits and hyphens.",
            ),
            (
                add_mcp("home_lab", Some(MCP_ADDRESS)),
                "A name is 1 to 24 lowercase letters, digits and hyphens.",
            ),
            (
                add_mcp(" ", Some(MCP_ADDRESS)),
                "Enter a name for the MCP server.",
            ),
            (add_mcp("home", Some("ftp://mcp.example/mcp")), MCP_URL),
            (add_mcp("home", Some("mcp.example/mcp")), MCP_URL),
            (
                add_mcp("home", Some("https://user:pw@mcp.example/mcp")),
                "Leave out the user name and password.",
            ),
            (
                add_mcp("home", Some("https://mcp.example/mcp#token")),
                "Leave out the # and everything after it.",
            ),
        ] {
            assert_eq!(
                f.service.set_mcp_server(edit).await.unwrap_err(),
                Refusal::Invalid(sentence.into())
            );
        }
        assert_eq!((f.file("standalone.toml"), f.file("secrets.toml")), saved);

        // An address is needed when adding.
        f.service
            .delete_mcp_server(&s1.id, s1.revision)
            .await
            .unwrap();
        assert_eq!(
            f.service
                .set_mcp_server(add_mcp("s9", None))
                .await
                .unwrap_err(),
            Refusal::Invalid(MCP_URL.into())
        );
        assert_eq!(f.service.view().mcp_servers.len(), MAX_MCP_SERVERS - 1);
    }

    #[tokio::test]
    async fn an_mcp_server_whose_address_was_lost_is_off_until_entered_again() {
        let f = Fixture::new(Mode::HomeAssistant);
        f.service
            .set_mcp_server(add_mcp("home", Some(MCP_ADDRESS)))
            .await
            .unwrap();
        fs::write(f.dir.path().join("secrets.toml"), "").unwrap();
        let service = f.reload();
        let home = service.view().mcp_servers[0].clone();
        assert!(home.enabled);
        assert!(service.live().borrow().mcp.is_empty());

        assert_eq!(
            service
                .set_mcp_server(change_mcp(&home, "home", None, true))
                .await
                .unwrap_err(),
            Refusal::Invalid("Enter the MCP server's address again.".into())
        );
        let off = service
            .set_mcp_server(change_mcp(&home, "home", None, false))
            .await
            .unwrap();
        let home = off.mcp_servers[0].clone();
        assert!(!home.enabled);
        let on = service
            .set_mcp_server(change_mcp(&home, "home", Some(MCP_ADDRESS), true))
            .await
            .unwrap();
        assert!(on.mcp_servers[0].enabled);
        assert_eq!(service.live().borrow().mcp.len(), 1);
    }

    #[tokio::test]
    async fn an_unchanged_mcp_server_keeps_its_session_across_edits() {
        let f = Fixture::new(Mode::HomeAssistant);
        f.service
            .set_mcp_server(add_mcp("home", Some(MCP_ADDRESS)))
            .await
            .unwrap();
        let before = f.live().mcp[0].clone();

        f.service
            .update(AssistantPatch {
                language: Some(Some("de".into())),
                ..AssistantPatch::default()
            })
            .await
            .unwrap();
        assert!(Arc::ptr_eq(&before, &f.live().mcp[0]));

        let home = f.service.view().mcp_servers[0].clone();
        f.service
            .set_mcp_server(change_mcp(&home, "house", None, true))
            .await
            .unwrap();
        assert!(!Arc::ptr_eq(&before, &f.live().mcp[0]));
    }

    #[tokio::test]
    async fn an_agent_brain_gets_its_acknowledgment_in_its_language() {
        let f = Fixture::new(Mode::HomeAssistant);
        let server = FakeServer::start().await;
        f.service
            .update(AssistantPatch {
                language: Some(Some("de".into())),
                ..AssistantPatch::default()
            })
            .await
            .unwrap();
        let speech = f.connect(&server).await;
        let agent = f
            .service
            .set_connection(ConnectionEdit {
                preset: PresetId::Hermes,
                ..new_connection(&server, KeyEdit::Keep)
            })
            .await
            .unwrap()
            .connections
            .pop()
            .unwrap();
        for (kind, connection, model) in [
            (SlotKind::Stt, &speech.id, "whisper-1"),
            (SlotKind::Chat, &agent.id, "hermes-3"),
        ] {
            passing(&server, kind);
            f.service
                .set_slot(kind, slot_edit(connection, model))
                .await
                .unwrap();
        }
        assert_eq!(f.live().ack, None);
        // The voice's check, then the acknowledgment.
        passing(&server, SlotKind::Tts);
        server.script(
            Method::POST,
            "/audio/speech",
            FakeResponse::new(StatusCode::OK, "ack-audio"),
        );

        f.service
            .set_slot(SlotKind::Tts, slot_edit(&speech.id, "nova"))
            .await
            .unwrap();

        let mut live = f.service.live();
        let path = tokio::time::timeout(
            Duration::from_secs(10),
            live.wait_for(|live| live.ack.is_some()),
        )
        .await
        .expect("the acknowledgment was published")
        .unwrap()
        .ack
        .clone()
        .unwrap();
        assert_eq!(path.parent(), Some(f.dir.path().join("cache").as_path()));
        assert_eq!(fs::read(&path).unwrap(), b"ack-audio");
        let request = server.requests().pop().unwrap();
        let sent: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
        assert_eq!(sent["input"], "Einen Moment.");
        assert_eq!(sent["voice"], "nova");
        assert_eq!(
            request.header("authorization"),
            Some(format!("Bearer {KEY}").as_str())
        );

        // A restart finds it on disk.
        assert_eq!(f.reload().live().borrow().ack.as_ref(), Some(&path));
    }

    #[tokio::test]
    async fn stations_are_validated_and_published() {
        let f = Fixture::new(Mode::Standalone);
        let station = |name: &str, url: &str| Station {
            name: name.into(),
            url: url.into(),
        };
        let stations = |list: Vec<Station>| AssistantPatch {
            stations: Some(list),
            ..AssistantPatch::default()
        };

        let view = f
            .service
            .update(stations(vec![
                station(" Radio One ", " https://radio.test/one.mp3?listener=pinna "),
                station("Jazz FM", "http://jazz.test/live.pls"),
            ]))
            .await
            .unwrap();

        let saved = vec![
            station("Radio One", "https://radio.test/one.mp3?listener=pinna"),
            station("Jazz FM", "http://jazz.test/live.pls"),
        ];
        assert_eq!(view.stations, saved);
        assert_eq!(*f.live().stations, saved);
        assert_eq!(f.saved().stations, saved);

        let too_many = (0..=assist::MAX_STATIONS)
            .map(|i| station(&format!("Station {i}"), "https://radio.test/"))
            .collect();
        for (list, sentence) in [
            (
                vec![
                    station("Radio One", "https://radio.test/1"),
                    station("radio  ONE!", "https://radio.test/2"),
                ],
                "Two stations are named radio  ONE!. Give each its own name.",
            ),
            (
                vec![station(" ", "https://radio.test/")],
                "Enter a station name.",
            ),
            (
                vec![station("Radio\nOne", "https://radio.test/")],
                "A station name is at most 60 characters on one line.",
            ),
            (
                vec![station("Radio One", "ftp://radio.test/one")],
                "Enter the address of Radio One, starting with http:// or https://.",
            ),
            (too_many, "At most 50 stations."),
        ] {
            assert_eq!(
                f.service.update(stations(list)).await.unwrap_err(),
                Refusal::Invalid(sentence.into())
            );
        }
        assert_eq!(f.saved().stations, saved);

        let view = f.service.update(stations(Vec::new())).await.unwrap();
        assert!(view.stations.is_empty());
        assert!(f.live().stations.is_empty());
    }

    #[test]
    fn agents_seen_keep_the_latest_eight_without_control_characters() {
        let (_status, rx) = watch::channel(Snapshot::default());
        let dir = tempfile::tempdir().unwrap();
        let service = Service::load(Paths::beside(&dir.path().join("settings.toml")), rx);
        let peer: IpAddr = "192.168.1.20".parse().unwrap();
        for n in 0..10 {
            service.agent_seen(&format!("agent-{n}"), peer);
        }
        service.agent_seen("agent-3", peer);
        service.agent_seen(&format!("evil\u{1b}[31m{}", "x".repeat(60)), peer);

        let agents = service.view().agents;
        assert_eq!(agents.len(), MAX_AGENTS);
        assert_eq!(agents[0].name, format!("evil[31m{}", "x".repeat(32)));
        assert_eq!(agents[0].name.chars().count(), AGENT_NAME_MAX);
        assert_eq!(agents[1].name, "agent-3");
        let names: Vec<&str> = agents[2..].iter().map(|a| a.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "agent-9", "agent-8", "agent-7", "agent-6", "agent-5", "agent-4"
            ]
        );
    }

    #[test]
    fn a_bad_key_in_standalone_toml_loses_only_that_key() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("standalone.toml"),
            "language = 7\nfollow_ups = false\n",
        )
        .unwrap();
        let (_status, rx) = watch::channel(Snapshot::default());
        let service = Service::load(Paths::beside(&dir.path().join("settings.toml")), rx);
        let view = service.view();
        assert_eq!(view.language, None);
        assert!(!view.follow_ups);
        assert!(!service.live().borrow().ready);
    }

    #[tokio::test]
    async fn a_connection_from_a_newer_build_survives_this_one() {
        // One connection uses a preset this build doesn't know (a rollback):
        // the others stay usable, and the unknown one is written back.
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("standalone.toml"),
            "[[connections]]\nid = 'openai-1'\npreset = 'openai'\n\
             base_url = 'https://api.openai.com/v1'\nrevision = 1\n\
             [[connections]]\nid = 'future-1'\npreset = 'future'\n\
             base_url = 'https://future.example/v1'\nrevision = 1\n",
        )
        .unwrap();
        let (_status, rx) = watch::channel(Snapshot::default());
        let service = Service::load(Paths::beside(&dir.path().join("settings.toml")), rx);
        let ids: Vec<String> = service
            .view()
            .connections
            .into_iter()
            .map(|c| c.id)
            .collect();
        assert_eq!(ids, ["openai-1"]);

        service
            .update(AssistantPatch {
                follow_ups: Some(false),
                ..AssistantPatch::default()
            })
            .await
            .unwrap();
        let text = fs::read_to_string(dir.path().join("standalone.toml")).unwrap();
        assert!(text.contains("openai-1"), "{text}");
        assert!(text.contains("future-1"), "{text}");
    }
}
