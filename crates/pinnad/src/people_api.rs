//! The people the satellite recognizes: ConnectRPC `pinna.v1.PeopleService`
//! (ADR 0038), served with the dashboard beside `AssistantService` and behind
//! the same sign-in.
//!
//! The handlers only translate: [`People`] validates, stores and starts or
//! drops the speaker model, and [`Enroller`] records a voice. No response
//! carries a voiceprint.

use std::sync::Arc;

use buffa::{EnumValue, Inline, MessageField};
use connectrpc::{
    ConnectError, RequestContext, Response, Router, ServiceRequest, ServiceResult, ServiceStream,
};
use futures::StreamExt;
use pinna_api::{
    DeletePersonRequest, DeletePersonResponse, EnrollDone, EnrollEvent, EnrollFailed,
    EnrollFailure, EnrollProgress, EnrollRequest, EnrollStarted, EveryoneElse as EveryoneElseView,
    GetPeopleRequest, GetPeopleResponse, Limits, PeopleService, Person as PersonMessage,
    Recognition, RecognitionState, SetEveryoneElseRequest, SetEveryoneElseResponse,
    SetPersonRequest, SetPersonResponse, VoiceState,
};

use crate::api::{caller, session_cookie};
use crate::assistant_api::refused;
use crate::people::enroll::{Enroller, Event, Failure};
use crate::people::recognizer::State;
use crate::people::voiceprints::Voice;
use crate::people::{
    EveryoneElse, MAX_INSTRUCTIONS, MAX_NAME, MAX_PEOPLE, People, PersonEdit, PersonView,
};

pub struct PeopleApi {
    people: Arc<People>,
    enroller: Option<Arc<Enroller>>,
}

impl PeopleApi {
    pub fn new(people: Arc<People>) -> Self {
        Self {
            people,
            enroller: None,
        }
    }

    /// Answer `Enroll` with `enroller`.
    pub fn with_enroller(mut self, enroller: Arc<Enroller>) -> Self {
        self.enroller = Some(enroller);
        self
    }
}

/// The routes, for the dashboard listener.
pub fn router(api: Arc<PeopleApi>) -> Router {
    Router::new().add_service(api)
}

fn person(p: PersonView) -> PersonMessage {
    PersonMessage {
        id: p.person.id,
        name: p.person.name,
        instructions: p.person.instructions,
        revision: p.person.revision,
        voice: EnumValue::Known(match p.voice {
            Voice::None => VoiceState::VOICE_STATE_NONE,
            Voice::Enrolled => VoiceState::VOICE_STATE_ENROLLED,
            Voice::OtherModel => VoiceState::VOICE_STATE_OTHER_MODEL,
            Voice::OlderCapture => VoiceState::VOICE_STATE_OLDER_CAPTURE,
        }),
        close_to: p.close_to,
        ..Default::default()
    }
}

fn everyone_else(e: EveryoneElse) -> MessageField<EveryoneElseView, Inline<EveryoneElseView>> {
    MessageField::some(EveryoneElseView {
        instructions: e.instructions,
        revision: e.revision,
        ..Default::default()
    })
}

fn recognition(state: State) -> Recognition {
    let (state, reason) = match state {
        State::NotNeeded => (
            RecognitionState::RECOGNITION_STATE_NOT_NEEDED,
            String::new(),
        ),
        State::Loading => (RecognitionState::RECOGNITION_STATE_LOADING, String::new()),
        State::Ready => (RecognitionState::RECOGNITION_STATE_READY, String::new()),
        State::Unavailable(reason) => (RecognitionState::RECOGNITION_STATE_UNAVAILABLE, reason),
    };
    Recognition {
        state: EnumValue::Known(state),
        reason,
        ..Default::default()
    }
}

fn enroll_event(event: Event) -> EnrollEvent {
    use pinna_api::enroll_event::Event as Message;
    let event = match event {
        Event::Started { seconds } => Message::Started(Box::new(EnrollStarted {
            seconds,
            ..Default::default()
        })),
        Event::Progress {
            seconds_heard,
            seconds_of_speech,
            level_dbfs,
        } => Message::Progress(Box::new(EnrollProgress {
            seconds_heard,
            seconds_of_speech,
            level_dbfs,
            ..Default::default()
        })),
        Event::Done {
            person: saved,
            seconds_of_speech,
            close_to,
        } => Message::Done(Box::new(EnrollDone {
            person: MessageField::some(person(saved)),
            seconds_of_speech,
            close_to,
            ..Default::default()
        })),
        Event::Failed { reason, sentence } => Message::Failed(Box::new(EnrollFailed {
            reason: EnumValue::Known(match reason {
                Failure::TooShort => EnrollFailure::ENROLL_FAILURE_TOO_SHORT,
                Failure::TooQuiet => EnrollFailure::ENROLL_FAILURE_TOO_QUIET,
                Failure::Clipped => EnrollFailure::ENROLL_FAILURE_CLIPPED,
                Failure::Inconsistent => EnrollFailure::ENROLL_FAILURE_INCONSISTENT,
                Failure::Ended => EnrollFailure::ENROLL_FAILURE_ENDED,
                Failure::Busy => EnrollFailure::ENROLL_FAILURE_BUSY,
                Failure::Unavailable => EnrollFailure::ENROLL_FAILURE_UNAVAILABLE,
            }),
            message: sentence,
            ..Default::default()
        })),
    };
    EnrollEvent {
        event: Some(event),
        ..Default::default()
    }
}

#[allow(refining_impl_trait)]
impl PeopleService for PeopleApi {
    async fn get_people(
        &self,
        _ctx: RequestContext,
        _request: ServiceRequest<'_, GetPeopleRequest>,
    ) -> ServiceResult<GetPeopleResponse> {
        let view = self.people.view();
        Response::ok(GetPeopleResponse {
            everyone_else: everyone_else(view.everyone_else),
            people: view.people.into_iter().map(person).collect(),
            recognition: MessageField::some(recognition(view.recognition)),
            limits: MessageField::some(Limits {
                max_people: MAX_PEOPLE as u32,
                max_instructions: MAX_INSTRUCTIONS as u32,
                max_name: MAX_NAME as u32,
                ..Default::default()
            }),
            ..Default::default()
        })
    }

    async fn set_person(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, SetPersonRequest>,
    ) -> ServiceResult<SetPersonResponse> {
        let saved = self
            .people
            .set_person(PersonEdit {
                id: (!request.id.is_empty()).then(|| request.id.to_owned()),
                revision: request.revision,
                name: request.name.to_owned(),
                instructions: request.instructions.to_owned(),
            })
            .map_err(refused)?;
        let view = self
            .people
            .person(&saved.id)
            .expect("the person that was just saved is on the list");
        Response::ok(SetPersonResponse {
            person: MessageField::some(person(view)),
            ..Default::default()
        })
    }

    async fn delete_person(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, DeletePersonRequest>,
    ) -> ServiceResult<DeletePersonResponse> {
        self.people.delete_person(request.id).map_err(refused)?;
        Response::ok(DeletePersonResponse::default())
    }

    async fn set_everyone_else(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, SetEveryoneElseRequest>,
    ) -> ServiceResult<SetEveryoneElseResponse> {
        let saved = self
            .people
            .set_everyone_else(request.instructions.to_owned(), request.revision)
            .map_err(refused)?;
        Response::ok(SetEveryoneElseResponse {
            everyone_else: everyone_else(saved),
            ..Default::default()
        })
    }

    async fn enroll(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, EnrollRequest>,
    ) -> ServiceResult<ServiceStream<EnrollEvent>> {
        let enroller = self
            .enroller
            .as_ref()
            .ok_or_else(|| ConnectError::unavailable("This satellite can't enroll voices."))?;
        let enrollment = enroller
            .start(
                caller(&ctx),
                request.id,
                request.grant,
                session_cookie(&ctx),
            )
            .await?;
        Response::ok(Box::pin(
            enrollment
                .into_stream()
                .map(|event| Ok(enroll_event(event))),
        ))
    }
}

#[cfg(test)]
mod tests {
    use connectrpc::client::{ClientConfig, HttpClient};
    use connectrpc::{ConnectError, ErrorCode, Server};
    use pinna_api::PeopleServiceClient;

    use super::*;
    use crate::people::Paths;
    use crate::people::voiceprints::synthetic;

    const SHA: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    struct Rig {
        client: PeopleServiceClient<HttpClient>,
        people: Arc<People>,
        dir: tempfile::TempDir,
    }

    impl Rig {
        async fn start() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let paths = Paths::beside(&dir.path().join("settings.toml"));
            let people = People::open(&paths, "/nonexistent/speaker.onnx".into());
            let router = router(Arc::new(PeopleApi::new(people.clone())));
            let server = Server::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}", server.local_addr().unwrap());
            tokio::spawn(async move { server.serve(router).await });
            Self {
                client: PeopleServiceClient::new(
                    HttpClient::plaintext(),
                    ClientConfig::new(url.parse().unwrap()),
                ),
                people,
                dir,
            }
        }

        async fn get(&self) -> GetPeopleResponse {
            self.client
                .get_people(GetPeopleRequest::default())
                .await
                .unwrap()
                .into_view()
                .to_owned_message()
        }

        async fn add(&self, name: &str) -> Result<PersonMessage, ConnectError> {
            self.set(String::new(), 0, name).await
        }

        async fn set(
            &self,
            id: String,
            revision: u64,
            name: &str,
        ) -> Result<PersonMessage, ConnectError> {
            self.client
                .set_person(SetPersonRequest {
                    id,
                    revision,
                    name: name.into(),
                    ..Default::default()
                })
                .await
                .map(|r| {
                    r.into_view()
                        .to_owned_message()
                        .person
                        .into_option()
                        .unwrap()
                })
        }
    }

    #[tokio::test]
    async fn an_empty_list_shows_the_limits_and_nothing_to_recognize() {
        let rig = Rig::start().await;
        let got = rig.get().await;
        assert!(got.people.is_empty());
        let everyone = got.everyone_else.as_option().unwrap();
        assert_eq!((everyone.instructions.as_str(), everyone.revision), ("", 1));
        let limits = got.limits.as_option().unwrap();
        assert_eq!(
            (limits.max_people, limits.max_instructions, limits.max_name),
            (10, 1_000, 40)
        );
        let state = got.recognition.as_option().unwrap().state;
        assert_eq!(
            state,
            EnumValue::Known(RecognitionState::RECOGNITION_STATE_NOT_NEEDED)
        );
    }

    #[tokio::test]
    async fn people_are_added_renamed_and_refused_when_stale() {
        let rig = Rig::start().await;
        let anna = rig.add("Anna").await.unwrap();
        assert_eq!((anna.id.as_str(), anna.revision), ("p1", 1));
        assert_eq!(anna.voice, EnumValue::Known(VoiceState::VOICE_STATE_NONE));
        let renamed = rig.set("p1".into(), 1, "Anne").await.unwrap();
        assert_eq!((renamed.name.as_str(), renamed.revision), ("Anne", 2));
        let stale = rig.set("p1".into(), 1, "Annie").await.unwrap_err();
        assert_eq!(stale.code, ErrorCode::Aborted);
        assert_eq!(
            stale.message.as_deref(),
            Some("Someone changed Anne since you opened it. Reload to see the change.")
        );
        let refused = rig.add("anne").await.unwrap_err();
        assert_eq!(refused.code, ErrorCode::InvalidArgument);
        assert_eq!(rig.get().await.people[0].name, "Anne");
    }

    #[tokio::test]
    async fn everyone_else_takes_instructions_by_revision() {
        let rig = Rig::start().await;
        let set = |instructions: &str, revision| {
            rig.client.set_everyone_else(SetEveryoneElseRequest {
                instructions: instructions.into(),
                revision,
                ..Default::default()
            })
        };
        let saved = set("Be brief.", 1)
            .await
            .unwrap()
            .into_view()
            .to_owned_message();
        assert_eq!(saved.everyone_else.as_option().unwrap().revision, 2);
        assert_eq!(set("Again.", 1).await.unwrap_err().code, ErrorCode::Aborted);
        assert_eq!(
            rig.get()
                .await
                .everyone_else
                .as_option()
                .unwrap()
                .instructions,
            "Be brief."
        );
    }

    #[tokio::test]
    async fn deleting_a_person_removes_the_voiceprint_file() {
        let rig = Rig::start().await;
        rig.add("Anna").await.unwrap();
        rig.people
            .save_voiceprint("p1", &synthetic(&[1.0, 0.0]), SHA)
            .unwrap();
        let file = rig.dir.path().join("voiceprints/p1.vp");
        assert!(file.exists());
        rig.client
            .delete_person(DeletePersonRequest {
                id: "p1".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(!file.exists());
        assert!(rig.get().await.people.is_empty());
    }

    #[tokio::test]
    async fn enrolling_is_refused_for_an_unknown_person_and_without_an_enroller() {
        let rig = Rig::start().await;
        // The refusal comes as the end of the stream.
        let mut stream = rig
            .client
            .enroll(EnrollRequest {
                id: "p1".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        let Err(e) = stream.message().await else {
            panic!("not refused");
        };
        assert_eq!(e.code, ErrorCode::Unavailable);
        assert_eq!(
            e.message.as_deref(),
            Some("This satellite can't enroll voices.")
        );
    }

    #[tokio::test]
    async fn the_states_follow_the_voiceprints_and_the_model() {
        let rig = Rig::start().await;
        rig.add("Anna").await.unwrap();
        rig.add("Ben").await.unwrap();
        rig.people
            .save_voiceprint("p1", &synthetic(&[1.0, 0.0]), SHA)
            .unwrap();
        rig.people
            .save_voiceprint("p2", &synthetic(&[1.0, 0.1]), SHA)
            .unwrap();
        // The model file is missing: loading fails and says why.
        let mut got = rig.get().await;
        for _ in 0..100 {
            let state = got.recognition.as_option().unwrap().state;
            if state != EnumValue::Known(RecognitionState::RECOGNITION_STATE_LOADING) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            got = rig.get().await;
        }
        let recognition = got.recognition.as_option().unwrap();
        assert_eq!(
            recognition.state,
            EnumValue::Known(RecognitionState::RECOGNITION_STATE_UNAVAILABLE)
        );
        assert!(recognition.reason.contains("/nonexistent/speaker.onnx"));
        assert_eq!(
            got.people[0].voice,
            EnumValue::Known(VoiceState::VOICE_STATE_ENROLLED)
        );
        assert_eq!(got.people[0].close_to, ["p2"]);
        assert_eq!(got.people[1].close_to, ["p1"]);
    }
}
