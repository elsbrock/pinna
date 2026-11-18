//! The standalone settings (`standalone.toml`): connections, the three slots,
//! the prompt and place, the owner's tools, MCP servers and stations
//! (ADR 0017 §"The pipeline").
//!
//! The codec holds no secrets; keys live in `secrets.toml`. Unknown keys are
//! kept in `extra`, so a rollback never drops what a newer image wrote.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::presets::{ChatOverride, PresetId};
use crate::prompt::PROMPT_MAX;

/// At most this many radio stations.
pub const MAX_STATIONS: usize = 50;
/// At most this many owner MCP servers.
pub const MAX_MCP_SERVERS: usize = 8;
/// An MCP server name is at most this many characters.
const MCP_NAME_MAX: usize = 24;

/// Everything in `standalone.toml`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AssistConfig {
    pub connections: Vec<Connection>,
    pub stt: Option<SlotConfig>,
    pub chat: Option<SlotConfig>,
    pub tts: Option<SlotConfig>,
    /// Advanced overrides for the brain slot.
    pub chat_override: ChatOverride,
    /// A BCP 47 tag such as `de` or `en-US`.
    pub language: Option<String>,
    pub place: Option<Place>,
    /// `None` means pinnad's voice rules, an empty string the context only.
    pub prompt: Option<String>,
    /// Open a follow-up window after every reply.
    pub follow_ups: bool,
    pub ack: Ack,
    /// Per-tool switches; an absent name means on.
    pub tools: BTreeMap<String, bool>,
    pub stations: Vec<Station>,
    pub mcp_servers: Vec<McpServer>,
    /// Keys a newer pinnad wrote, kept as they are.
    #[serde(flatten)]
    pub extra: toml::Table,
}

impl Default for AssistConfig {
    fn default() -> Self {
        Self {
            connections: Vec::new(),
            stt: None,
            chat: None,
            tts: None,
            chat_override: ChatOverride::default(),
            language: None,
            place: None,
            prompt: None,
            follow_ups: true,
            ack: Ack::default(),
            tools: BTreeMap::new(),
            stations: Vec::new(),
            mcp_servers: Vec::new(),
            extra: toml::Table::new(),
        }
    }
}

/// A base URL and an optional key (in `secrets.toml`), shared by slots.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Connection {
    pub id: String,
    pub preset: PresetId,
    pub base_url: String,
    /// Bumped when the URL or the key changes; a slot is re-checked when it no
    /// longer matches `checked`.
    pub revision: u64,
}

/// One slot: which connection, which model or voice, and the revision that
/// last passed its save-time check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlotConfig {
    pub connection: String,
    /// A model id, or the voice id for the text-to-speech slot.
    pub model: String,
    pub checked: u64,
}

/// A place for the weather tool and the prompt's context.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Place {
    pub name: String,
    pub lat: f64,
    pub lon: f64,
    /// The place's IANA time zone, as the search found it, so setup can
    /// offer it for the satellite.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_zone: Option<String>,
}

/// The acknowledgment played while a slow agent thinks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Ack {
    pub enabled: bool,
    pub phrase: String,
}

impl Default for Ack {
    fn default() -> Self {
        Self {
            enabled: true,
            phrase: "One moment.".into(),
        }
    }
}

/// A Radio station the owner added.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Station {
    pub name: String,
    pub url: String,
}

/// An owner MCP server. The URL is a secret; only the host is stored here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpServer {
    pub id: String,
    pub name: String,
    pub host: String,
    pub enabled: bool,
    pub revision: u64,
}

/// A BCP 47 tag as it is stored, or why it can't be one.
pub fn language(s: &str) -> Result<String, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("Enter a language like en or en-US.".into());
    }
    if !valid_language(s) {
        return Err("That isn't a language like en or en-US.".into());
    }
    Ok(s.to_string())
}

/// The primary subtag of a language tag, lowercase: `en` for `en-US`, `de`
/// for `de_DE`. Speech services take this, not the whole tag. Three-letter
/// primaries (`haw`, `yue`) stay as they are.
pub fn primary_language(tag: &str) -> String {
    tag.split(['-', '_'])
        .next()
        .unwrap_or(tag)
        .to_ascii_lowercase()
}

fn valid_language(s: &str) -> bool {
    let parts: Vec<&str> = s.split(['-', '_']).collect();
    let Some(first) = parts.first() else {
        return false;
    };
    if !(2..=3).contains(&first.len()) || !first.chars().all(|c| c.is_ascii_alphabetic()) {
        return false;
    }
    if parts.len() > 3 {
        return false;
    }
    parts[1..]
        .iter()
        .all(|p| (1..=8).contains(&p.len()) && p.chars().all(|c| c.is_ascii_alphanumeric()))
}

/// A connection base URL as it is stored, or why it can't be one.
pub fn base_url(s: &str) -> Result<String, String> {
    let s = s.trim();
    http_url(
        s,
        false,
        "Enter the full address, starting with http:// or https://.",
    )?;
    Ok(s.to_string())
}

/// An http(s) address with a host and no user name or password, or the
/// sentence why not. `start` asks for the whole address; a query is allowed
/// only where `query` says so. No sentence repeats the address.
pub fn http_url(s: &str, query: bool, start: &str) -> Result<url::Url, String> {
    if s.is_empty() {
        return Err(start.into());
    }
    if s.chars().any(char::is_whitespace) {
        return Err("An address has no spaces.".into());
    }
    let url = url::Url::parse(s).map_err(|_| start.to_string())?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(start.into());
    }
    if url.host_str().is_none() {
        return Err("The address needs a host name.".into());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("Leave out the user name and password.".into());
    }
    if query {
        if url.fragment().is_some() {
            return Err("Leave out the # and everything after it.".into());
        }
    } else if url.query().is_some() || url.fragment().is_some() {
        return Err("Leave out everything after the path.".into());
    }
    Ok(url)
}

/// An MCP server name as it is stored, or why it can't be one.
pub fn mcp_name(s: &str) -> Result<String, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("Enter a name for the MCP server.".into());
    }
    if s.chars().count() > MCP_NAME_MAX
        || !s
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return Err(format!(
            "A name is 1 to {MCP_NAME_MAX} lowercase letters, digits and hyphens."
        ));
    }
    Ok(s.to_string())
}

/// The owner's prompt as it is stored, or why it can't be one.
pub fn prompt(s: &str) -> Result<String, String> {
    if s.chars().count() > PROMPT_MAX {
        return Err("The prompt is at most 4,000 characters.".into());
    }
    Ok(s.to_string())
}

#[cfg(test)]
mod tests {

    #[test]
    fn speech_services_get_the_primary_language_subtag() {
        assert_eq!(primary_language("en-US"), "en");
        assert_eq!(primary_language("de_DE"), "de");
        assert_eq!(primary_language("PT-br"), "pt");
        assert_eq!(primary_language("haw"), "haw");
    }

    use super::*;
    use crate::presets::{Delegation, HistoryPolicy};

    #[test]
    fn toml_round_trips_including_extra() {
        let text = r#"
language = "de"
follow_ups = false
future_key = 7

[[connections]]
id = "openai-1"
preset = "openai"
base_url = "https://api.openai.com/v1"
revision = 3

[stt]
connection = "openai-1"
model = "whisper-1"
checked = 3

[tts]
connection = "openai-1"
model = "nova"
checked = 3

[chat_override]
delegation = "hand_off"
history = { new_turn_only = {} }
first_text_s = 90

[place]
name = "Munich"
lat = 48.14
lon = 11.58

[ack]
enabled = true
phrase = "On it."

[tools]
weather = false

[[stations]]
name = "Radio"
url = "http://radio.example/stream"

[[mcp_servers]]
id = "m1"
name = "home"
host = "mcp.example"
enabled = true
revision = 1

[future_table]
x = 1
"#;
        let c: AssistConfig = toml::from_str(text).unwrap();
        assert_eq!(c.language.as_deref(), Some("de"));
        assert!(!c.follow_ups);
        assert_eq!(c.connections.len(), 1);
        assert_eq!(c.stt.as_ref().unwrap().model, "whisper-1");
        assert_eq!(c.chat_override.delegation, Some(Delegation::HandOff));
        assert_eq!(c.stations[0].name, "Radio");
        assert_eq!(c.tools.get("weather"), Some(&false));
        assert!(c.extra.contains_key("future_key"));
        assert!(c.extra.contains_key("future_table"));

        let back: AssistConfig = toml::from_str(&toml::to_string(&c).unwrap()).unwrap();
        assert_eq!(back, c);
        assert_eq!(back.extra, c.extra);
    }

    #[test]
    fn defaults_are_what_the_design_says() {
        let c = AssistConfig::default();
        assert!(c.follow_ups);
        assert!(c.ack.enabled);
        assert!(c.stations.is_empty());
        assert!(c.mcp_servers.is_empty());
        let from_empty: AssistConfig = toml::from_str("").unwrap();
        assert_eq!(from_empty, c);
    }

    #[test]
    fn history_policy_round_trips() {
        for policy in [
            HistoryPolicy::Turns(10),
            HistoryPolicy::Bytes(65_536),
            HistoryPolicy::NewTurnOnly,
        ] {
            let o = ChatOverride {
                history: Some(policy),
                ..ChatOverride::default()
            };
            let back: ChatOverride = toml::from_str(&toml::to_string(&o).unwrap()).unwrap();
            assert_eq!(back, o);
        }
        let o = ChatOverride {
            reasoning: Some(crate::presets::Reasoning::None),
            ..ChatOverride::default()
        };
        let text = toml::to_string(&o).unwrap();
        assert_eq!(text.trim(), "reasoning = \"none\"");
        assert_eq!(toml::from_str::<ChatOverride>(&text).unwrap(), o);
    }

    #[test]
    fn languages() {
        for ok in ["de", "en", "en-US", "zh-Hant", "pt-BR", "de_DE"] {
            assert!(language(ok).is_ok(), "{ok}");
        }
        assert_eq!(
            language(" ").unwrap_err(),
            "Enter a language like en or en-US."
        );
        for bad in ["x", "englisch", "e", "en-US-extra-more", "de!", "d1"] {
            assert!(language(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn base_urls() {
        assert_eq!(
            base_url("https://api.openai.com/v1").unwrap(),
            "https://api.openai.com/v1"
        );
        assert!(base_url("http://192.168.1.5:11434/v1").is_ok());
        assert_eq!(
            base_url("").unwrap_err(),
            "Enter the full address, starting with http:// or https://."
        );
        assert_eq!(
            base_url("ftp://example.com").unwrap_err(),
            "Enter the full address, starting with http:// or https://."
        );
        assert_eq!(
            base_url("https://user:pass@example.com/v1").unwrap_err(),
            "Leave out the user name and password."
        );
        assert_eq!(
            base_url("https://example.com/v1?key=1").unwrap_err(),
            "Leave out everything after the path."
        );
        assert_eq!(
            base_url("not a url").unwrap_err(),
            "An address has no spaces."
        );
    }

    #[test]
    fn mcp_names() {
        assert_eq!(mcp_name(" home-1 ").unwrap(), "home-1");
        assert_eq!(
            mcp_name("").unwrap_err(),
            "Enter a name for the MCP server."
        );
        for bad in ["Home", "home_1", "höme", &"a".repeat(25)] {
            assert!(mcp_name(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn prompts() {
        assert_eq!(prompt("Be brief.").unwrap(), "Be brief.");
        assert!(prompt(&"a".repeat(PROMPT_MAX)).is_ok());
        assert_eq!(
            prompt(&"a".repeat(PROMPT_MAX + 1)).unwrap_err(),
            "The prompt is at most 4,000 characters."
        );
    }
}
