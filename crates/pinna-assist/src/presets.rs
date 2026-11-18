//! Presets, wire protocols and per-brain behaviour (ADR 0017 §"The pipeline",
//! §"Presets, not kinds of brain").
//!
//! A preset is a table row: it fills in a provider's base URL, the protocol of
//! each slot it can serve and the default behaviour. Behaviour is data. It says
//! whether pinnad runs the tool loop or hands the request off, how much of the
//! conversation the brain gets, and the run's limits and acknowledgment. The
//! owner's Advanced settings override one row through [`ChatOverride`].

use std::time::Duration;

use pinna_http::Secret;
use serde::{Deserialize, Serialize};

use crate::{error::Slot, http::Auth, limits::RunLimits};

/// A provider, or the owner's own URL (`Custom`). `label()` is bounded, so it
/// can be a metric label.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PresetId {
    OpenAi,
    OpenRouter,
    Anthropic,
    Gemini,
    Groq,
    Mistral,
    ElevenLabs,
    Ollama,
    LmStudio,
    Hermes,
    OpenClaw,
    Custom,
}

impl PresetId {
    /// The preset's name, as shown and as a label.
    pub fn label(&self) -> &'static str {
        match self {
            Self::OpenAi => "openai",
            Self::OpenRouter => "openrouter",
            Self::Anthropic => "anthropic",
            Self::Gemini => "gemini",
            Self::Groq => "groq",
            Self::Mistral => "mistral",
            Self::ElevenLabs => "elevenlabs",
            Self::Ollama => "ollama",
            Self::LmStudio => "lmstudio",
            Self::Hermes => "hermes",
            Self::OpenClaw => "openclaw",
            Self::Custom => "custom",
        }
    }
}

/// The brain's wire protocol.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChatProtocol {
    OpenAi,
    Anthropic,
    Gemini,
}

impl ChatProtocol {
    /// How the key travels.
    pub fn auth(self, key: Option<Secret>) -> Auth {
        match (self, key) {
            (_, None) => Auth::None,
            (Self::OpenAi, Some(key)) => Auth::Bearer(key),
            (Self::Anthropic, Some(key)) => Auth::ApiKey("x-api-key", key),
            (Self::Gemini, Some(key)) => Auth::ApiKey("x-goog-api-key", key),
        }
    }

    /// The model list below the base URL. An OpenAI-compatible base URL
    /// already carries its API version; the other two page their lists, so
    /// ask for the largest page they serve.
    pub fn models_path(self) -> &'static str {
        match self {
            Self::OpenAi => "/models",
            Self::Anthropic => "/v1/models?limit=1000",
            Self::Gemini => "/v1beta/models?pageSize=1000",
        }
    }

    /// A header every request names besides the key: Anthropic refuses a
    /// request without its API version.
    pub fn version_header(self) -> Option<(&'static str, &'static str)> {
        match self {
            Self::Anthropic => Some(("anthropic-version", "2023-06-01")),
            Self::OpenAi | Self::Gemini => None,
        }
    }
}

/// How a connection's key travels for one slot.
pub fn auth(id: PresetId, slot: Slot, key: Option<Secret>) -> Auth {
    let row = preset(id);
    match slot {
        Slot::Chat => row.chat_protocol.auth(key),
        Slot::Stt | Slot::Tts | Slot::Tool => row.speech_protocol.auth(key),
    }
}

/// The speech slots' wire protocol.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpeechProtocol {
    OpenAi,
    ElevenLabs,
}

impl SpeechProtocol {
    /// How the key travels.
    pub fn auth(self, key: Option<Secret>) -> Auth {
        match (self, key) {
            (_, None) => Auth::None,
            (Self::OpenAi, Some(key)) => Auth::Bearer(key),
            (Self::ElevenLabs, Some(key)) => Auth::ApiKey("xi-api-key", key),
        }
    }

    /// The text-to-speech model: the slot names only the voice.
    pub fn tts_model(self) -> &'static str {
        match self {
            Self::OpenAi => "tts-1",
            Self::ElevenLabs => "eleven_multilingual_v2",
        }
    }
}

/// Who runs the tools.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Delegation {
    /// pinnad runs its own tool loop.
    ToolLoop,
    /// The brain is an agent that owns its tools and memory.
    HandOff,
}

/// How much of the conversation the brain gets.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HistoryPolicy {
    /// The last `n` turns.
    Turns(usize),
    /// Up to `n` bytes of text, oldest dropped first.
    Bytes(usize),
    /// Only the new turn.
    NewTurnOnly,
}

/// How the tool loop treats a brain: delegation, conversation and limits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Behaviour {
    pub delegation: Delegation,
    pub history: HistoryPolicy,
    pub limits: RunLimits,
    /// Send one stable `user` per device (OpenClaw derives its session from it).
    pub stable_user: bool,
}

impl Behaviour {
    /// Apply the owner's Advanced settings over this behaviour. The limits
    /// follow the delegation in effect: an agent switched to the tool loop
    /// gets a model's tool rounds, a model handed off gets an agent's wait
    /// and acknowledgment.
    pub fn overridden(self, o: &ChatOverride) -> Behaviour {
        let delegation = o.delegation.unwrap_or(self.delegation);
        let mut limits = if delegation == self.delegation {
            self.limits
        } else {
            match delegation {
                Delegation::ToolLoop => RunLimits::MODEL,
                Delegation::HandOff => RunLimits::AGENT,
            }
        };
        if let Some(s) = o.first_text_s {
            limits.first_text = Duration::from_secs(s);
        }
        Behaviour {
            delegation,
            history: o.history.unwrap_or(self.history),
            limits,
            stable_user: self.stable_user,
        }
    }
}

/// The Advanced overrides stored in `standalone.toml`. Every field is optional;
/// an absent field keeps the preset's.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ChatOverride {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delegation: Option<Delegation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub history: Option<HistoryPolicy>,
    /// The first-text limit in seconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_text_s: Option<u64>,
}

/// Which slots a preset can serve.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Slots {
    pub stt: bool,
    pub chat: bool,
    pub tts: bool,
}

/// One preset row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Preset {
    pub id: PresetId,
    /// Base URL; a `<host>` or `<port>` placeholder is the owner's to fill.
    pub base_url: &'static str,
    pub chat_protocol: ChatProtocol,
    pub speech_protocol: SpeechProtocol,
    pub slots: Slots,
    /// A fixed model name, when the client is an agent (OpenClaw).
    pub model: Option<&'static str>,
    pub behaviour: Behaviour,
}

const fn model() -> Behaviour {
    Behaviour {
        delegation: Delegation::ToolLoop,
        history: HistoryPolicy::Turns(10),
        limits: RunLimits::MODEL,
        stable_user: false,
    }
}

const fn agent(history: HistoryPolicy, stable_user: bool) -> Behaviour {
    Behaviour {
        delegation: Delegation::HandOff,
        history,
        limits: RunLimits::AGENT,
        stable_user,
    }
}

const OPENAI_SPEECH: SpeechProtocol = SpeechProtocol::OpenAi;
const ELEVENLABS_SPEECH: SpeechProtocol = SpeechProtocol::ElevenLabs;

static PRESETS: &[Preset] = &[
    Preset {
        id: PresetId::OpenAi,
        base_url: "https://api.openai.com/v1",
        chat_protocol: ChatProtocol::OpenAi,
        speech_protocol: OPENAI_SPEECH,
        slots: Slots {
            stt: true,
            chat: true,
            tts: true,
        },
        model: None,
        behaviour: model(),
    },
    Preset {
        id: PresetId::OpenRouter,
        base_url: "https://openrouter.ai/api/v1",
        chat_protocol: ChatProtocol::OpenAi,
        speech_protocol: OPENAI_SPEECH,
        slots: Slots {
            stt: false,
            chat: true,
            tts: false,
        },
        model: None,
        behaviour: model(),
    },
    Preset {
        id: PresetId::Anthropic,
        base_url: "https://api.anthropic.com",
        chat_protocol: ChatProtocol::Anthropic,
        speech_protocol: OPENAI_SPEECH,
        slots: Slots {
            stt: false,
            chat: true,
            tts: false,
        },
        model: None,
        behaviour: model(),
    },
    Preset {
        id: PresetId::Gemini,
        base_url: "https://generativelanguage.googleapis.com",
        chat_protocol: ChatProtocol::Gemini,
        speech_protocol: OPENAI_SPEECH,
        slots: Slots {
            stt: false,
            chat: true,
            tts: false,
        },
        model: None,
        behaviour: model(),
    },
    Preset {
        id: PresetId::Groq,
        base_url: "https://api.groq.com/openai/v1",
        chat_protocol: ChatProtocol::OpenAi,
        speech_protocol: OPENAI_SPEECH,
        slots: Slots {
            stt: true,
            chat: true,
            tts: false,
        },
        model: None,
        behaviour: model(),
    },
    Preset {
        id: PresetId::Mistral,
        base_url: "https://api.mistral.ai/v1",
        chat_protocol: ChatProtocol::OpenAi,
        speech_protocol: OPENAI_SPEECH,
        slots: Slots {
            stt: false,
            chat: true,
            tts: false,
        },
        model: None,
        behaviour: model(),
    },
    Preset {
        id: PresetId::ElevenLabs,
        base_url: "https://api.elevenlabs.io",
        chat_protocol: ChatProtocol::OpenAi,
        speech_protocol: ELEVENLABS_SPEECH,
        slots: Slots {
            stt: true,
            chat: false,
            tts: true,
        },
        model: None,
        behaviour: model(),
    },
    Preset {
        id: PresetId::Ollama,
        base_url: "http://<host>:11434/v1",
        chat_protocol: ChatProtocol::OpenAi,
        speech_protocol: OPENAI_SPEECH,
        slots: Slots {
            stt: false,
            chat: true,
            tts: false,
        },
        model: None,
        behaviour: model(),
    },
    Preset {
        id: PresetId::LmStudio,
        base_url: "http://<host>:1234/v1",
        chat_protocol: ChatProtocol::OpenAi,
        speech_protocol: OPENAI_SPEECH,
        slots: Slots {
            stt: false,
            chat: true,
            tts: false,
        },
        model: None,
        behaviour: model(),
    },
    Preset {
        id: PresetId::Hermes,
        base_url: "http://<host>:8642/v1",
        chat_protocol: ChatProtocol::OpenAi,
        speech_protocol: OPENAI_SPEECH,
        slots: Slots {
            stt: false,
            chat: true,
            tts: false,
        },
        model: None,
        behaviour: agent(HistoryPolicy::Bytes(64 * 1024), false),
    },
    Preset {
        id: PresetId::OpenClaw,
        base_url: "http://<host>:<port>/v1",
        chat_protocol: ChatProtocol::OpenAi,
        speech_protocol: OPENAI_SPEECH,
        slots: Slots {
            stt: false,
            chat: true,
            tts: false,
        },
        model: Some("openclaw/default"),
        behaviour: agent(HistoryPolicy::NewTurnOnly, true),
    },
    Preset {
        id: PresetId::Custom,
        base_url: "",
        chat_protocol: ChatProtocol::OpenAi,
        speech_protocol: OPENAI_SPEECH,
        slots: Slots {
            stt: true,
            chat: true,
            tts: true,
        },
        model: None,
        behaviour: model(),
    },
];

/// Every preset row, in table order.
pub fn presets() -> &'static [Preset] {
    PRESETS
}

/// The row for `id`.
pub fn preset(id: PresetId) -> &'static Preset {
    PRESETS
        .iter()
        .find(|p| p.id == id)
        .expect("every preset id has a row")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique_and_every_id_has_a_row() {
        let mut seen = Vec::new();
        for p in presets() {
            assert!(!seen.contains(&p.id), "{:?} appears twice", p.id);
            seen.push(p.id);
        }
        for id in [
            PresetId::OpenAi,
            PresetId::OpenRouter,
            PresetId::Anthropic,
            PresetId::Gemini,
            PresetId::Groq,
            PresetId::Mistral,
            PresetId::ElevenLabs,
            PresetId::Ollama,
            PresetId::LmStudio,
            PresetId::Hermes,
            PresetId::OpenClaw,
            PresetId::Custom,
        ] {
            assert_eq!(preset(id).id, id);
        }
    }

    #[test]
    fn base_urls_parse() {
        for p in presets() {
            // The owner fills `<host>` and `<port>`; Custom has no URL at all.
            if p.base_url.is_empty() || p.base_url.contains('<') {
                continue;
            }
            let url =
                url::Url::parse(p.base_url).unwrap_or_else(|e| panic!("{}: {e}", p.id.label()));
            assert!(matches!(url.scheme(), "http" | "https"), "{}", p.id.label());
            assert!(url.host_str().is_some(), "{}", p.id.label());
        }
    }

    #[test]
    fn local_presets_use_the_owner_host() {
        assert_eq!(preset(PresetId::Ollama).base_url, "http://<host>:11434/v1");
        assert_eq!(preset(PresetId::LmStudio).base_url, "http://<host>:1234/v1");
        assert_eq!(preset(PresetId::Hermes).base_url, "http://<host>:8642/v1");
        assert_eq!(
            preset(PresetId::OpenClaw).base_url,
            "http://<host>:<port>/v1"
        );
    }

    #[test]
    fn agents_hand_off() {
        let hermes = preset(PresetId::Hermes).behaviour;
        assert_eq!(hermes.delegation, Delegation::HandOff);
        assert_eq!(hermes.history, HistoryPolicy::Bytes(64 * 1024));
        assert!(!hermes.stable_user);
        assert_eq!(hermes.limits, RunLimits::AGENT);

        let openclaw = preset(PresetId::OpenClaw).behaviour;
        assert_eq!(openclaw.delegation, Delegation::HandOff);
        assert_eq!(openclaw.history, HistoryPolicy::NewTurnOnly);
        assert!(openclaw.stable_user);
        assert_eq!(preset(PresetId::OpenClaw).model, Some("openclaw/default"));
    }

    #[test]
    fn slots_follow_the_table() {
        let openai = preset(PresetId::OpenAi).slots;
        assert!(openai.stt && openai.chat && openai.tts);
        assert_eq!(
            preset(PresetId::ElevenLabs).speech_protocol,
            SpeechProtocol::ElevenLabs
        );
        assert_eq!(
            preset(PresetId::Anthropic).chat_protocol,
            ChatProtocol::Anthropic
        );
        assert_eq!(preset(PresetId::Gemini).chat_protocol, ChatProtocol::Gemini);
        assert!(!preset(PresetId::OpenRouter).slots.stt);
        assert!(!preset(PresetId::ElevenLabs).slots.chat);
    }

    #[test]
    fn an_override_switches_the_delegation() {
        let base = preset(PresetId::OpenAi).behaviour;
        assert_eq!(base.delegation, Delegation::ToolLoop);

        let o = ChatOverride {
            delegation: Some(Delegation::HandOff),
            history: Some(HistoryPolicy::NewTurnOnly),
            first_text_s: Some(120),
        };
        let over = base.overridden(&o);
        assert_eq!(over.delegation, Delegation::HandOff);
        assert_eq!(over.history, HistoryPolicy::NewTurnOnly);
        assert_eq!(over.limits.first_text, Duration::from_secs(120));
        // Untouched fields stay.
        assert_eq!(over.limits.stt, base.limits.stt);
        // A model handed off waits and acknowledges like an agent.
        assert_eq!(over.limits.ack_after, RunLimits::AGENT.ack_after);
        assert_eq!(over.limits.rounds, RunLimits::AGENT.rounds);

        // An agent switched to the tool loop gets a model's tool rounds.
        let agent = preset(PresetId::Hermes).behaviour;
        let looped = agent.overridden(&ChatOverride {
            delegation: Some(Delegation::ToolLoop),
            ..ChatOverride::default()
        });
        assert_eq!(looped.limits, RunLimits::MODEL);

        // An empty override changes nothing.
        assert_eq!(base.overridden(&ChatOverride::default()), base);
    }

    #[test]
    fn labels_round_trip_through_serde() {
        #[derive(Serialize, Deserialize)]
        struct Holder {
            preset: PresetId,
        }
        for p in presets() {
            let text = toml::to_string(&Holder { preset: p.id }).unwrap();
            assert!(text.contains(p.id.label()), "{text}");
            let back: Holder = toml::from_str(&text).unwrap();
            assert_eq!(back.preset, p.id);
        }
    }
}
