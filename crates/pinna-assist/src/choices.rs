//! What a service offers for a slot, for the dashboard's suggestions (ADR
//! 0017 §"Errors": the save-time check stays the proof). The brain's models,
//! the speech models of an OpenAI-compatible server, and ElevenLabs' voices
//! and speech models, as the service lists them now: no list is kept in
//! Pinna. A service that lists nothing for the slot offers nothing, and the
//! owner types the name.

use serde::Deserialize;

use crate::{
    check::{CHECK_TIMEOUT, CheckSetup, MODELS_CAP, models, offered_models, refusal},
    error::{ProviderError, Reason, Slot},
    presets::{SpeechProtocol, preset},
};

/// What the field asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wanted {
    /// The slot's model: the brain's, speech-to-text's, or the one that
    /// speaks the text-to-speech voice.
    Models,
    /// Text-to-speech voices.
    Voices,
}

/// One suggestion: what goes into the field, and what to show for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Choice {
    pub id: String,
    pub label: String,
}

/// The dashboard filters by typing; more than this helps nobody.
pub const CHOICES_MAX: usize = 500;

/// The service's choices for `setup.slot`, or the sentence why it couldn't
/// list them. `setup.model` is not used.
pub async fn choices(setup: &CheckSetup, wanted: Wanted) -> Result<Vec<Choice>, String> {
    match tokio::time::timeout(CHECK_TIMEOUT, list(setup, wanted)).await {
        Ok(Ok(mut list)) => {
            list.truncate(CHOICES_MAX);
            Ok(list)
        }
        Ok(Err(error)) => Err(refusal(&error)),
        Err(_) => Err(refusal(&setup.endpoint.error(setup.slot, Reason::Timeout))),
    }
}

async fn list(setup: &CheckSetup, wanted: Wanted) -> Result<Vec<Choice>, ProviderError> {
    let speech = preset(setup.endpoint.preset).speech_protocol;
    match (setup.slot, wanted, speech) {
        (Slot::Chat, Wanted::Models, _) => listed_ids(setup, |_| true).await,
        // An OpenAI-compatible list mixes every kind of model; a server
        // whose names don't say gets them all.
        (Slot::Stt, Wanted::Models, SpeechProtocol::OpenAi) => {
            listed_ids(setup, |id| {
                id.contains("whisper") || id.contains("transcribe")
            })
            .await
        }
        (Slot::Tts, Wanted::Models, SpeechProtocol::OpenAi) => {
            listed_ids(setup, |id| id.contains("tts")).await
        }
        (Slot::Tts, Wanted::Models, SpeechProtocol::ElevenLabs) => elevenlabs_models(setup).await,
        (Slot::Tts, Wanted::Voices, SpeechProtocol::ElevenLabs) => elevenlabs_voices(setup).await,
        // ElevenLabs lists no speech-to-text models, and OpenAI no voices.
        _ => Ok(Vec::new()),
    }
}

/// The model list's ids that `wanted` keeps, sorted; all of them when it
/// keeps none.
async fn listed_ids(
    setup: &CheckSetup,
    wanted: impl Fn(&str) -> bool,
) -> Result<Vec<Choice>, ProviderError> {
    let Some(body) = models(setup, setup.slot).await? else {
        return Ok(Vec::new());
    };
    let mut ids = offered_models(&body).unwrap_or_default();
    ids.sort();
    ids.dedup();
    let kept: Vec<String> = ids.iter().filter(|id| wanted(id)).cloned().collect();
    let ids = if kept.is_empty() { ids } else { kept };
    Ok(ids
        .into_iter()
        .map(|id| Choice {
            label: id.clone(),
            id,
        })
        .collect())
}

async fn elevenlabs_get(setup: &CheckSetup, path: &str) -> Result<Vec<u8>, ProviderError> {
    let endpoint = &setup.endpoint;
    let response = endpoint
        .send(endpoint.http.get(endpoint.url(path)), setup.slot)
        .await?;
    Ok(endpoint
        .read_capped(response, MODELS_CAP, setup.slot)
        .await?
        .to_vec())
}

/// ElevenLabs' models that speak.
async fn elevenlabs_models(setup: &CheckSetup) -> Result<Vec<Choice>, ProviderError> {
    #[derive(Deserialize)]
    struct Model {
        model_id: String,
        #[serde(default)]
        name: String,
        #[serde(default)]
        can_do_text_to_speech: bool,
    }
    let body = elevenlabs_get(setup, "/v1/models").await?;
    let models: Vec<Model> = serde_json::from_slice(&body)
        .map_err(|_| setup.endpoint.error(setup.slot, Reason::Protocol))?;
    Ok(models
        .into_iter()
        .filter(|m| m.can_do_text_to_speech && !m.model_id.is_empty())
        .map(|m| Choice {
            label: if m.name.is_empty() {
                m.model_id.clone()
            } else {
                m.name
            },
            id: m.model_id,
        })
        .collect())
}

/// The voices in the account's ElevenLabs library, by name: their IDs are
/// what the slot needs, and nobody can read them.
async fn elevenlabs_voices(setup: &CheckSetup) -> Result<Vec<Choice>, ProviderError> {
    #[derive(Deserialize)]
    struct Voices {
        #[serde(default)]
        voices: Vec<Voice>,
    }
    #[derive(Deserialize)]
    struct Voice {
        voice_id: String,
        #[serde(default)]
        name: String,
    }
    let body = elevenlabs_get(setup, "/v2/voices?page_size=100").await?;
    let list: Voices = serde_json::from_slice(&body)
        .map_err(|_| setup.endpoint.error(setup.slot, Reason::Protocol))?;
    Ok(list
        .voices
        .into_iter()
        .filter(|v| !v.voice_id.is_empty())
        .map(|v| Choice {
            label: if v.name.is_empty() {
                v.voice_id.clone()
            } else {
                v.name
            },
            id: v.voice_id,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use pinna_http::{
        ClientKind, Secret, client,
        reqwest::{Method, StatusCode},
    };

    use super::*;
    use crate::{
        http::Endpoint,
        presets::PresetId,
        testing::{FakeResponse, FakeServer},
    };

    fn setup(server: &FakeServer, slot: Slot, preset: PresetId) -> CheckSetup {
        CheckSetup {
            slot,
            endpoint: Endpoint::new(
                client(ClientKind::Provider {
                    read: Some(Duration::from_secs(5)),
                }),
                &server.url(""),
                crate::presets::auth(preset, slot, Some(Secret::new("sk-test"))),
                preset,
                || true,
            ),
            model: String::new(),
            speech_model: None,
            hand_off: false,
        }
    }

    const LIST: &str = r#"{"data": [{"id": "gpt-4.1-mini"}, {"id": "whisper-1"},
        {"id": "gpt-4o-mini-transcribe"}, {"id": "tts-1"}, {"id": "gpt-4o-mini-tts"}]}"#;

    #[tokio::test]
    async fn an_openai_list_is_split_by_what_each_slot_takes() {
        let server = FakeServer::start().await;
        for _ in 0..3 {
            server.script(
                Method::GET,
                "/models",
                FakeResponse::new(StatusCode::OK, LIST),
            );
        }
        let ids = |list: Vec<Choice>| list.into_iter().map(|c| c.id).collect::<Vec<_>>();
        assert_eq!(
            ids(choices(
                &setup(&server, Slot::Chat, PresetId::OpenAi),
                Wanted::Models
            )
            .await
            .unwrap()),
            [
                "gpt-4.1-mini",
                "gpt-4o-mini-transcribe",
                "gpt-4o-mini-tts",
                "tts-1",
                "whisper-1"
            ]
        );
        assert_eq!(
            ids(
                choices(&setup(&server, Slot::Stt, PresetId::OpenAi), Wanted::Models)
                    .await
                    .unwrap()
            ),
            ["gpt-4o-mini-transcribe", "whisper-1"]
        );
        assert_eq!(
            ids(
                choices(&setup(&server, Slot::Tts, PresetId::OpenAi), Wanted::Models)
                    .await
                    .unwrap()
            ),
            ["gpt-4o-mini-tts", "tts-1"]
        );
        // OpenAI lists no voices: nothing is asked.
        assert_eq!(
            choices(&setup(&server, Slot::Tts, PresetId::OpenAi), Wanted::Voices).await,
            Ok(vec![])
        );
        assert_eq!(server.requests().len(), 3);
    }

    #[tokio::test]
    async fn elevenlabs_voices_come_by_name_and_its_models_that_speak() {
        let server = FakeServer::start().await;
        server.script(
            Method::GET,
            "/v2/voices?page_size=100",
            FakeResponse::new(
                StatusCode::OK,
                r#"{"voices": [{"voice_id": "JBFqnCBsd6RMkjVDRZzb", "name": "George"},
                    {"voice_id": "", "name": "Broken"}], "has_more": false}"#,
            ),
        );
        server.script(
            Method::GET,
            "/v1/models",
            FakeResponse::new(
                StatusCode::OK,
                r#"[{"model_id": "eleven_flash_v2_5", "name": "Eleven Flash v2.5", "can_do_text_to_speech": true},
                    {"model_id": "eleven_english_sts_v2", "name": "STS", "can_do_text_to_speech": false}]"#,
            ),
        );
        let tts = setup(&server, Slot::Tts, PresetId::ElevenLabs);
        assert_eq!(
            choices(&tts, Wanted::Voices).await,
            Ok(vec![Choice {
                id: "JBFqnCBsd6RMkjVDRZzb".into(),
                label: "George".into()
            }])
        );
        assert_eq!(
            choices(&tts, Wanted::Models).await,
            Ok(vec![Choice {
                id: "eleven_flash_v2_5".into(),
                label: "Eleven Flash v2.5".into()
            }])
        );
        assert_eq!(
            server.requests()[0].header("xi-api-key"),
            Some("sk-test"),
            "the key goes along"
        );
        // ElevenLabs lists no speech-to-text models.
        assert_eq!(
            choices(
                &setup(&server, Slot::Stt, PresetId::ElevenLabs),
                Wanted::Models
            )
            .await,
            Ok(vec![])
        );
    }

    #[tokio::test]
    async fn a_list_that_fails_says_why_in_the_checks_words() {
        let server = FakeServer::start().await;
        server.script_status(Method::GET, "/models", StatusCode::UNAUTHORIZED);
        assert_eq!(
            choices(
                &setup(&server, Slot::Chat, PresetId::OpenAi),
                Wanted::Models
            )
            .await,
            Err("The key is wrong. Enter it again.".into())
        );
    }
}
