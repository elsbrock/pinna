//! Build the three provider adapters for one standalone snapshot (ADR 0017
//! §"The pipeline").
//!
//! A slot that can't be built (missing, its connection gone, a preset that
//! can't serve it, no model) leaves the pipeline out; the setup service says
//! which, in its own sentences, when the owner edits it.

use std::sync::Arc;

use pinna_http::{ClientKind, client};

use crate::{
    chat::{self, Chat},
    config::{self, AssistConfig, Connection, SlotConfig},
    http::{Auth, Endpoint},
    limits::RunLimits,
    presets::{Behaviour, Delegation, PresetId, preset},
    secrets::Secrets,
    stt::{self, Stt},
    tts::{self, Tts},
};

pub struct Pipeline {
    pub stt: Arc<dyn Stt>,
    pub chat: Arc<dyn Chat>,
    pub tts: Arc<dyn Tts>,
    pub presets: [PresetId; 3],
    pub behaviour: Behaviour,
    pub label: String,
}

/// The text-to-speech slot on its own: a Speak needs only this.
#[derive(Clone)]
pub struct Speech {
    pub tts: Arc<dyn Tts>,
    pub preset: PresetId,
}

impl Pipeline {
    pub fn build(
        cfg: &AssistConfig,
        secrets: &Secrets,
        clock_synced: fn() -> bool,
    ) -> Option<Pipeline> {
        let (stt, stt_preset) = build_stt(cfg, secrets, clock_synced)?;
        let chat = build_chat(cfg, secrets, clock_synced)?;
        let speech = Speech::build(cfg, secrets, clock_synced)?;
        Some(Pipeline {
            stt,
            chat: chat.adapter,
            tts: speech.tts,
            presets: [stt_preset, chat.preset, speech.preset],
            behaviour: chat.behaviour,
            label: chat.label,
        })
    }
}

impl Speech {
    pub fn build(
        cfg: &AssistConfig,
        secrets: &Secrets,
        clock_synced: fn() -> bool,
    ) -> Option<Speech> {
        let (voice, connection) = resolve(cfg.tts.as_ref(), cfg)?;
        let row = preset(connection.preset);
        if !row.slots.tts {
            return None;
        }
        let auth = row.speech_protocol.auth(key(secrets, connection));
        let read = Some(RunLimits::MODEL.tts_first_audio);
        let language = cfg.language.as_deref().map(config::primary_language);
        Some(Speech {
            tts: tts::adapter(
                row.speech_protocol,
                endpoint(connection, auth, read, clock_synced),
                voice.to_owned(),
                language,
            ),
            preset: connection.preset,
        })
    }
}

struct BuiltChat {
    adapter: Arc<dyn Chat>,
    preset: PresetId,
    behaviour: Behaviour,
    label: String,
}

fn build_stt(
    cfg: &AssistConfig,
    secrets: &Secrets,
    clock_synced: fn() -> bool,
) -> Option<(Arc<dyn Stt>, PresetId)> {
    let (model, connection) = resolve(cfg.stt.as_ref(), cfg)?;
    let row = preset(connection.preset);
    if !row.slots.stt {
        return None;
    }
    let auth = row.speech_protocol.auth(key(secrets, connection));
    let read = Some(RunLimits::MODEL.stt);
    let endpoint = endpoint(connection, auth, read, clock_synced);
    let adapter = stt::adapter(row.speech_protocol, endpoint, model.to_owned());
    Some((adapter, connection.preset))
}

fn build_chat(
    cfg: &AssistConfig,
    secrets: &Secrets,
    clock_synced: fn() -> bool,
) -> Option<BuiltChat> {
    let (model, connection) = resolve(cfg.chat.as_ref(), cfg)?;
    let row = preset(connection.preset);
    if !row.slots.chat {
        return None;
    }
    // An agent preset names its own model.
    let model = row.model.unwrap_or(model);
    let behaviour = row.behaviour.overridden(&cfg.chat_override);
    // An agent may think for minutes: only its run limits end it.
    let read = match behaviour.delegation {
        Delegation::ToolLoop => Some(behaviour.limits.first_text),
        Delegation::HandOff => None,
    };
    let auth = row.chat_protocol.auth(key(secrets, connection));
    let endpoint = endpoint(connection, auth, read, clock_synced);
    Some(BuiltChat {
        adapter: chat::adapter(row.chat_protocol, endpoint, model.to_owned()),
        preset: connection.preset,
        behaviour,
        label: format!("{}: {model}", connection.preset.label()),
    })
}

/// A slot's model (or voice) and its connection, when both are there.
fn resolve<'a>(
    slot: Option<&'a SlotConfig>,
    cfg: &'a AssistConfig,
) -> Option<(&'a str, &'a Connection)> {
    let slot = slot?;
    let connection = cfg
        .connections
        .iter()
        .find(|connection| connection.id == slot.connection)?;
    let model = slot.model.trim();
    (!model.is_empty()).then_some((model, connection))
}

fn key(secrets: &Secrets, connection: &Connection) -> Option<pinna_http::Secret> {
    secrets
        .key_for(&connection.id, &connection.base_url)
        .cloned()
}

fn endpoint(
    connection: &Connection,
    auth: Auth,
    read: Option<std::time::Duration>,
    clock_synced: fn() -> bool,
) -> Endpoint {
    Endpoint::new(
        client(ClientKind::Provider { read }),
        &connection.base_url,
        auth,
        connection.preset,
        clock_synced,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn connection(id: &str, preset: PresetId) -> Connection {
        Connection {
            id: id.into(),
            preset,
            base_url: format!("http://{id}.test/v1"),
            revision: 1,
        }
    }

    fn slot(connection: &str, model: &str) -> SlotConfig {
        SlotConfig {
            connection: connection.into(),
            model: model.into(),
            checked: 1,
        }
    }

    #[test]
    fn a_complete_configuration_builds_all_three_slots() {
        let cfg = AssistConfig {
            connections: vec![connection("openai", PresetId::OpenAi)],
            stt: Some(slot("openai", "whisper-1")),
            chat: Some(slot("openai", "gpt-test")),
            tts: Some(slot("openai", "nova")),
            ..AssistConfig::default()
        };
        let pipeline = Pipeline::build(&cfg, &Secrets::default(), || true).unwrap();
        assert_eq!(pipeline.presets, [PresetId::OpenAi; 3]);
        assert_eq!(pipeline.label, "openai: gpt-test");
    }

    #[test]
    fn an_empty_configuration_builds_nothing() {
        let empty = AssistConfig::default();
        assert!(Pipeline::build(&empty, &Secrets::default(), || true).is_none());
        assert!(Speech::build(&empty, &Secrets::default(), || true).is_none());
    }

    #[test]
    fn a_preset_that_does_not_serve_a_slot_builds_no_pipeline() {
        let cfg = AssistConfig {
            connections: vec![
                connection("openrouter", PresetId::OpenRouter),
                connection("openai", PresetId::OpenAi),
            ],
            stt: Some(slot("openrouter", "whisper-1")),
            chat: Some(slot("openrouter", "gpt-test")),
            tts: Some(slot("openai", "nova")),
            ..AssistConfig::default()
        };
        assert!(Pipeline::build(&cfg, &Secrets::default(), || true).is_none());
        // The voice alone still builds, for a Speak.
        let speech = Speech::build(&cfg, &Secrets::default(), || true).unwrap();
        assert_eq!(speech.preset, PresetId::OpenAi);
    }
}
