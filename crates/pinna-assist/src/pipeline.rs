//! Build the three provider adapters for one validated standalone snapshot.

use std::{path::Path, sync::Arc};

use pinna_http::{ClientKind, Secret, client};

use crate::{
    chat::{AnthropicChat, Chat, GeminiChat, OpenAiChat},
    config::{AssistConfig, Connection, SlotConfig},
    error::Slot,
    http::Auth,
    limits::RunLimits,
    presets::{Behaviour, ChatProtocol, PresetId, SpeechProtocol, preset},
    secrets::Secrets,
    stt::{ElevenLabsStt, OpenAiStt, Stt, SttSetup},
    tts::{ElevenLabsTts, OpenAiTts, Tts},
};

const OPENAI_TTS_MODEL: &str = "tts-1";
const ELEVENLABS_TTS_MODEL: &str = "eleven_multilingual_v2";

/// The synthesis model the pipeline uses for a protocol. The text-to-speech
/// slot stores a voice, so its save-time check needs the model from here.
pub fn tts_model(protocol: SpeechProtocol) -> &'static str {
    match protocol {
        SpeechProtocol::OpenAi => OPENAI_TTS_MODEL,
        SpeechProtocol::ElevenLabs => ELEVENLABS_TTS_MODEL,
    }
}

pub struct Pipeline {
    pub stt: Arc<dyn Stt>,
    pub chat: Arc<dyn Chat>,
    pub tts: Arc<dyn Tts>,
    pub presets: [PresetId; 3],
    pub model: String,
    pub behaviour: Behaviour,
    pub label: String,
}

struct BuiltChat {
    adapter: Arc<dyn Chat>,
    model: String,
    behaviour: Behaviour,
    label: String,
}

impl Pipeline {
    pub fn build(cfg: &AssistConfig, secrets: &Secrets) -> Result<Pipeline, Vec<(Slot, String)>> {
        let stt = build_stt(cfg, secrets);
        let chat = build_chat(cfg, secrets);
        let tts = build_tts(cfg, secrets);
        let mut errors = Vec::new();
        if let Err(error) = &stt {
            errors.push((Slot::Stt, error.clone()));
        }
        if let Err(error) = &chat {
            errors.push((Slot::Chat, error.clone()));
        }
        if let Err(error) = &tts {
            errors.push((Slot::Tts, error.clone()));
        }
        if !errors.is_empty() {
            return Err(errors);
        }

        let chat = chat.expect("checked above");
        Ok(Pipeline {
            stt: stt.expect("checked above"),
            presets: [
                cfg.stt.as_ref().expect("checked above").preset(cfg),
                cfg.chat.as_ref().expect("checked above").preset(cfg),
                cfg.tts.as_ref().expect("checked above").preset(cfg),
            ],
            chat: chat.adapter,
            tts: tts.expect("checked above"),
            model: chat.model.clone(),
            behaviour: chat.behaviour,
            label: chat.label,
        })
    }
}

trait SlotPreset {
    fn preset(&self, cfg: &AssistConfig) -> PresetId;
}

impl SlotPreset for SlotConfig {
    fn preset(&self, cfg: &AssistConfig) -> PresetId {
        cfg.connections
            .iter()
            .find(|connection| connection.id == self.connection)
            .expect("a successfully built slot has a connection")
            .preset
    }
}

fn build_stt(cfg: &AssistConfig, secrets: &Secrets) -> Result<Arc<dyn Stt>, String> {
    let (slot, connection) = resolve(cfg.stt.as_ref(), cfg, Slot::Stt)?;
    let row = preset(connection.preset);
    if !row.slots.stt {
        return Err(protocol(Slot::Stt, connection.preset));
    }
    let model = required_model(slot, Slot::Stt)?;
    let setup = SttSetup {
        http: client(ClientKind::Provider {
            read: Some(RunLimits::MODEL.stt),
        }),
        base_url: connection.base_url.clone(),
        model,
        auth: speech_auth(row.speech_protocol, secrets, connection),
        preset: connection.preset,
        clock_synced,
    };
    Ok(match row.speech_protocol {
        SpeechProtocol::OpenAi => Arc::new(OpenAiStt::new(setup)),
        SpeechProtocol::ElevenLabs => Arc::new(ElevenLabsStt::new(setup)),
    })
}

fn build_chat(cfg: &AssistConfig, secrets: &Secrets) -> Result<BuiltChat, String> {
    let (slot, connection) = resolve(cfg.chat.as_ref(), cfg, Slot::Chat)?;
    let row = preset(connection.preset);
    if !row.slots.chat {
        return Err(protocol(Slot::Chat, connection.preset));
    }
    let model = row
        .model
        .map(str::to_owned)
        .map_or_else(|| required_model(slot, Slot::Chat), Ok)?;
    let behaviour = row.behaviour.overridden(&cfg.chat_override);
    let read = match behaviour.delegation {
        crate::presets::Delegation::ToolLoop => Some(behaviour.limits.first_text),
        crate::presets::Delegation::HandOff => None,
    };
    let http = client(ClientKind::Provider { read });
    let key = secrets
        .key_for(&connection.id, &connection.base_url)
        .cloned();
    let adapter: Arc<dyn Chat> = match row.chat_protocol {
        ChatProtocol::OpenAi => Arc::new(OpenAiChat::new(
            http,
            &connection.base_url,
            &model,
            key,
            connection.preset,
            clock_synced,
        )),
        ChatProtocol::Anthropic => Arc::new(AnthropicChat::new(
            http,
            &connection.base_url,
            &model,
            required_key(key, Slot::Chat)?,
            clock_synced,
        )),
        ChatProtocol::Gemini => Arc::new(GeminiChat::new(
            http,
            &connection.base_url,
            &model,
            required_key(key, Slot::Chat)?,
            clock_synced,
        )),
    };
    Ok(BuiltChat {
        adapter,
        label: format!("{}: {model}", connection.preset.label()),
        model,
        behaviour,
    })
}

fn build_tts(cfg: &AssistConfig, secrets: &Secrets) -> Result<Arc<dyn Tts>, String> {
    let (slot, connection) = resolve(cfg.tts.as_ref(), cfg, Slot::Tts)?;
    let row = preset(connection.preset);
    if !row.slots.tts {
        return Err(protocol(Slot::Tts, connection.preset));
    }
    let voice = required_model(slot, Slot::Tts)?;
    let http = client(ClientKind::Provider {
        read: Some(RunLimits::MODEL.tts_first_audio),
    });
    let auth = speech_auth(row.speech_protocol, secrets, connection);
    Ok(match row.speech_protocol {
        SpeechProtocol::OpenAi => Arc::new(OpenAiTts::new(
            http,
            &connection.base_url,
            OPENAI_TTS_MODEL,
            voice,
            connection.preset,
            auth,
            clock_synced,
        )),
        SpeechProtocol::ElevenLabs => Arc::new(ElevenLabsTts::new(
            http,
            &connection.base_url,
            ELEVENLABS_TTS_MODEL,
            voice,
            cfg.language.clone(),
            connection.preset,
            auth,
            clock_synced,
        )),
    })
}

fn resolve<'a>(
    slot: Option<&'a SlotConfig>,
    cfg: &'a AssistConfig,
    kind: Slot,
) -> Result<(&'a SlotConfig, &'a Connection), String> {
    let slot = slot.ok_or_else(|| missing(kind))?;
    let connection = cfg
        .connections
        .iter()
        .find(|connection| connection.id == slot.connection)
        .ok_or_else(|| format!("Choose the {} connection again.", slot_name(kind)))?;
    Ok((slot, connection))
}

fn required_model(slot: &SlotConfig, kind: Slot) -> Result<String, String> {
    if slot.model.trim().is_empty() {
        let what = if kind == Slot::Tts { "voice" } else { "model" };
        Err(format!("Enter the {} {what}.", slot_name(kind)))
    } else {
        Ok(slot.model.clone())
    }
}

fn required_key(key: Option<Secret>, kind: Slot) -> Result<Secret, String> {
    key.ok_or_else(|| format!("Enter a key for the {} connection.", slot_name(kind)))
}

fn speech_auth(protocol: SpeechProtocol, secrets: &Secrets, connection: &Connection) -> Auth {
    let key = secrets
        .key_for(&connection.id, &connection.base_url)
        .cloned();
    match (protocol, key) {
        (_, None) => Auth::None,
        (SpeechProtocol::OpenAi, Some(key)) => Auth::Bearer(key),
        (SpeechProtocol::ElevenLabs, Some(key)) => Auth::ApiKey("xi-api-key", key),
    }
}

fn missing(kind: Slot) -> String {
    match kind {
        Slot::Stt => "Choose a speech-to-text connection and model.".into(),
        Slot::Chat => "Choose a brain connection and model.".into(),
        Slot::Tts => "Choose a text-to-speech connection and voice.".into(),
        Slot::Tool => unreachable!(),
    }
}

fn protocol(kind: Slot, preset: PresetId) -> String {
    format!(
        "The {} preset cannot provide {}.",
        preset.label(),
        slot_name(kind)
    )
}

fn slot_name(slot: Slot) -> &'static str {
    match slot {
        Slot::Stt => "speech-to-text",
        Slot::Chat => "brain",
        Slot::Tts => "text-to-speech",
        Slot::Tool => "tool",
    }
}

fn clock_synced() -> bool {
    Path::new("/run/systemd/timesync/synchronized").exists()
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
        let pipeline = Pipeline::build(&cfg, &Secrets::default()).unwrap();
        assert_eq!(pipeline.presets, [PresetId::OpenAi; 3]);
        assert_eq!(pipeline.model, "gpt-test");
        assert_eq!(pipeline.label, "openai: gpt-test");
    }

    #[test]
    fn missing_slots_return_owner_sentences_together() {
        let errors = match Pipeline::build(&AssistConfig::default(), &Secrets::default()) {
            Ok(_) => panic!("an empty configuration cannot build"),
            Err(errors) => errors,
        };
        assert_eq!(
            errors,
            [
                (
                    Slot::Stt,
                    "Choose a speech-to-text connection and model.".into()
                ),
                (Slot::Chat, "Choose a brain connection and model.".into()),
                (
                    Slot::Tts,
                    "Choose a text-to-speech connection and voice.".into()
                ),
            ]
        );
    }

    #[test]
    fn a_preset_that_does_not_serve_a_slot_is_a_protocol_mismatch() {
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
        let errors = match Pipeline::build(&cfg, &Secrets::default()) {
            Ok(_) => panic!("OpenRouter has no speech-to-text protocol"),
            Err(errors) => errors,
        };
        assert_eq!(
            errors,
            [(
                Slot::Stt,
                "The openrouter preset cannot provide speech-to-text.".into()
            )]
        );
    }
}
