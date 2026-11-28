//! The events exchanged by one standalone run and the Brain.

use std::{path::PathBuf, sync::Arc};

use futures_core::future::BoxFuture;
use pinna_mcp::ToolSet;
use tokio::time::Instant;

pub mod listen;
pub(crate) mod reply;

pub use crate::conversation::Turn;
use crate::{
    audio::{MicInput, wav},
    error::{ProviderError, Reason, Slot},
    observe::{Guard, Observer, Spend},
    pipeline::Pipeline,
    reply::ReplySink,
    stt::is_invented,
};

#[derive(Clone, Debug, PartialEq)]
pub enum RunKind {
    Request,
    FollowUp,
    Typed(String),
}

#[derive(Clone, Debug)]
pub enum RunEvent {
    SpeechEnded,
    NoSpeech { invented: bool },
    Progress,
    ReplyReady,
    Done { turn: Option<Turn>, spoke: bool },
    Failed(ProviderError),
}

pub trait RunEvents: Send + Sync {
    fn emit(&self, ev: RunEvent) -> BoxFuture<'_, ()>;
}

/// What a spoken run changes once it knows who spoke (ADR 0038).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Personal {
    /// The system prompt for the reply.
    pub system: String,
    /// `None` keeps the request's history; `Some(vec![])` starts a new
    /// conversation.
    pub history: Option<Vec<Turn>>,
}

/// The hook after speech-to-text. Who speaks is known only then, while the
/// system prompt and the history were fixed when the run started.
pub trait Personalize: Send + Sync {
    /// Called once, after speech-to-text returned a transcript that isn't
    /// invented and before the reply, for spoken runs only. It must not wait:
    /// recognition that isn't finished counts as not recognized.
    ///
    /// It returns a future so that a caller may queue what it decided before
    /// the reply's events follow it.
    fn after_transcript<'a>(&'a self, kind: &'a RunKind) -> BoxFuture<'a, Option<Personal>>;
}

pub struct RunRequest {
    pub kind: RunKind,
    pub pipeline: Arc<Pipeline>,
    pub system: String,
    pub history: Vec<Turn>,
    pub language: Option<String>,
    pub user: Option<String>,
    pub mic: Option<MicInput>,
    pub reply: ReplySink,
    pub tools: Arc<dyn ToolSet>,
    pub ack: Option<PathBuf>,
    pub observer: Arc<dyn Observer>,
    /// Takes who spoke into account for spoken runs; typed runs never call it.
    pub personalize: Option<Arc<dyn Personalize>>,
}

/// Execute one request as a single future. Dropping it drops every in-flight
/// provider request; normal completion emits exactly one terminal event.
pub async fn run(req: RunRequest, events: &dyn RunEvents) {
    let RunRequest {
        kind,
        pipeline,
        system,
        history,
        language,
        user,
        mic,
        reply: sink,
        tools,
        ack,
        observer,
        personalize,
    } = req;
    let mut system = system;
    let mut history = history;
    let question = match &kind {
        RunKind::Typed(question) => question.clone(),
        RunKind::Request | RunKind::FollowUp => {
            let Some(mut mic) = mic else {
                return fail(
                    ProviderError::new(Slot::Stt, pipeline.presets.stt, Reason::Internal),
                    sink,
                    observer,
                    events,
                )
                .await;
            };
            let frames = match listen::listen(&mut mic).await {
                Ok(frames) => frames,
                Err(listen::Heard::NoSpeech) => {
                    observer.guard(Guard::NoSpeech);
                    drop(sink);
                    events.emit(RunEvent::NoSpeech { invented: false }).await;
                    return;
                }
            };
            drop(mic);
            events.emit(RunEvent::SpeechEnded).await;

            let seconds = frames.iter().map(|frame| frame.len()).sum::<usize>() as f32 / 32_000.0;
            observer.spent(pipeline.presets.stt, Spend::AudioSeconds(seconds));
            let started = Instant::now();
            let transcript = match tokio::time::timeout(
                pipeline.behaviour.limits.stt,
                pipeline.stt.transcribe(wav(&frames), language.as_deref()),
            )
            .await
            {
                Ok(Ok(transcript)) => transcript,
                Ok(Err(error)) => {
                    return fail(error, sink, observer, events).await;
                }
                Err(_) => {
                    return fail(
                        ProviderError::new(Slot::Stt, pipeline.presets.stt, Reason::Timeout),
                        sink,
                        observer,
                        events,
                    )
                    .await;
                }
            };
            observer.stage(Slot::Stt, started.elapsed().as_secs_f64());
            if is_invented(&transcript) {
                observer.guard(Guard::Invented);
                drop(sink);
                events.emit(RunEvent::NoSpeech { invented: true }).await;
                return;
            }
            if let Some(personalize) = &personalize
                && let Some(personal) = personalize.after_transcript(&kind).await
            {
                system = personal.system;
                if let Some(turns) = personal.history {
                    history = turns;
                }
            }
            transcript.trim().to_owned()
        }
    };

    let outcome = reply::reply(
        reply::ReplyRequest {
            question,
            history: &history,
            system: &system,
            user_id: user.as_deref(),
            chat: pipeline.chat.as_ref(),
            tts: pipeline.tts.as_ref(),
            tools: tools.as_ref(),
            behaviour: &pipeline.behaviour,
            chat_preset: pipeline.presets.chat,
            tts_preset: pipeline.presets.tts,
            sink: &sink,
            ack: ack.as_deref(),
            observer: observer.as_ref(),
        },
        events,
    )
    .await;
    match outcome {
        Ok(outcome) => {
            drop(sink);
            events
                .emit(RunEvent::Done {
                    turn: outcome.turn,
                    spoke: outcome.spoke,
                })
                .await;
        }
        Err(error) => fail(error, sink, observer, events).await,
    }
}

async fn fail(
    error: ProviderError,
    sink: ReplySink,
    observer: Arc<dyn Observer>,
    events: &dyn RunEvents,
) {
    observer.failed(&error);
    drop(sink);
    events.emit(RunEvent::Failed(error)).await;
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{
            Mutex,
            atomic::{AtomicBool, Ordering},
        },
        time::Duration,
    };

    use bytes::Bytes;
    use tokio::sync::{Notify, mpsc};

    use super::{reply::test_util::*, *};
    use crate::{
        chat::{Chat, ChatDelta, Message},
        limits::RunLimits,
        presets::{Behaviour, PresetId},
        reply::{AudioKind, ReplySource, reply_channel},
        stt::Stt,
        tts::Tts,
    };

    struct FakeStt {
        answer: &'static str,
        delay: Duration,
        calls: Mutex<usize>,
    }

    impl FakeStt {
        fn new(answer: &'static str) -> Self {
            Self {
                answer,
                delay: Duration::ZERO,
                calls: Mutex::new(0),
            }
        }

        fn delayed(answer: &'static str, delay: Duration) -> Self {
            Self {
                answer,
                delay,
                calls: Mutex::new(0),
            }
        }

        fn calls(&self) -> usize {
            *self.calls.lock().unwrap()
        }
    }

    impl Stt for FakeStt {
        fn transcribe<'a>(
            &'a self,
            _: Bytes,
            _: Option<&'a str>,
        ) -> BoxFuture<'a, Result<String, ProviderError>> {
            *self.calls.lock().unwrap() += 1;
            Box::pin(async move {
                tokio::time::sleep(self.delay).await;
                Ok(self.answer.to_owned())
            })
        }
    }

    fn pipeline(
        stt: Arc<dyn Stt>,
        chat: Arc<dyn Chat>,
        tts: Arc<dyn Tts>,
        limits: RunLimits,
    ) -> Arc<Pipeline> {
        let behaviour = Behaviour {
            limits,
            ..crate::presets::preset(PresetId::OpenAi).behaviour
        };
        Arc::new(Pipeline {
            stt,
            chat,
            tts,
            presets: crate::presets::Slots::all(PresetId::OpenAi),
            behaviour,
            label: "test".into(),
        })
    }

    fn request(
        kind: RunKind,
        mic: Option<MicInput>,
        pipeline: Arc<Pipeline>,
        sink: ReplySink,
        observer: Arc<TestObserver>,
    ) -> RunRequest {
        RunRequest {
            kind,
            pipeline,
            system: "Be brief.".into(),
            history: Vec::new(),
            language: Some("en".into()),
            user: None,
            mic,
            reply: sink,
            tools: Arc::new(NoTools),
            ack: None,
            observer,
            personalize: None,
        }
    }

    fn frame(amplitude: i16) -> Bytes {
        let mut bytes = Vec::with_capacity(320);
        for _ in 0..160 {
            bytes.extend_from_slice(&amplitude.to_le_bytes());
        }
        Bytes::from(bytes)
    }

    fn mic(amplitudes: impl IntoIterator<Item = i16>) -> MicInput {
        let (tx, rx) = mpsc::channel(2_000);
        for amplitude in amplitudes {
            tx.try_send(frame(amplitude)).unwrap();
        }
        MicInput {
            frames: rx,
            noise_seed: Some(256),
            follow_up: false,
        }
    }

    fn speech() -> MicInput {
        mic(std::iter::repeat_n(3_000, 110).chain(std::iter::repeat_n(10, 100)))
    }

    async fn receive(source: &mut ReplySource, count: usize) {
        for _ in 0..count {
            let item = source.recv().await.unwrap();
            assert_eq!(item.kind, AudioKind::Sentence);
            source.started(item.kind);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_voice_run_emits_speech_reply_and_done_in_order() {
        let stt = Arc::new(FakeStt::new("Question"));
        let chat = Arc::new(ScriptedChat::new([vec![
            ChatDelta::Text("Answer.".into()),
            ChatDelta::End,
        ]]));
        let tts = Arc::new(ScriptedTts::default());
        let observer = Arc::new(TestObserver::default());
        let (sink, mut source) = reply_channel(2);
        let events = TestEvents::default();
        let run = run(
            request(
                RunKind::Request,
                Some(speech()),
                pipeline(stt, chat.clone(), tts, RunLimits::MODEL),
                sink,
                observer,
            ),
            &events,
        );
        tokio::join!(run, receive(&mut source, 1));
        let requests = chat.requests();
        assert_eq!(requests[0].max_tokens, RunLimits::MODEL.max_tokens);
        assert_eq!(requests[0].user, None);
        let events = events.events();
        assert_eq!(events.len(), 4, "{events:?}");
        assert!(matches!(events[0], RunEvent::SpeechEnded));
        assert!(matches!(events[1], RunEvent::Progress));
        assert!(matches!(events[2], RunEvent::ReplyReady));
        assert!(matches!(
            &events[3],
            RunEvent::Done {
                turn: Some(Turn { user, assistant }),
                spoke: true,
            } if user == "Question" && assistant == "Answer."
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn no_speech_never_calls_speech_to_text() {
        let stt = Arc::new(FakeStt::new("unused"));
        let chat = Arc::new(ScriptedChat::new([]));
        let tts = Arc::new(ScriptedTts::default());
        let observer = Arc::new(TestObserver::default());
        let (sink, _source) = reply_channel(2);
        let events = TestEvents::default();
        run(
            request(
                RunKind::Request,
                Some(mic(std::iter::repeat_n(10, 600))),
                pipeline(stt.clone(), chat, tts, RunLimits::MODEL),
                sink,
                observer.clone(),
            ),
            &events,
        )
        .await;
        assert_eq!(stt.calls(), 0);
        let events = events.events();
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], RunEvent::NoSpeech { invented: false }));
        assert_eq!(*observer.guards.lock().unwrap(), [Guard::NoSpeech]);
    }

    #[tokio::test(start_paused = true)]
    async fn an_invented_transcript_never_reaches_the_brain() {
        let stt = Arc::new(FakeStt::new("Thank you for watching."));
        let chat = Arc::new(ScriptedChat::new([]));
        let tts = Arc::new(ScriptedTts::default());
        let observer = Arc::new(TestObserver::default());
        let (sink, _source) = reply_channel(2);
        let events = TestEvents::default();
        run(
            request(
                RunKind::Request,
                Some(speech()),
                pipeline(stt, chat.clone(), tts, RunLimits::MODEL),
                sink,
                observer.clone(),
            ),
            &events,
        )
        .await;
        assert!(chat.requests().is_empty());
        let events = events.events();
        assert_eq!(events.len(), 2);
        assert!(matches!(events[0], RunEvent::SpeechEnded));
        assert!(matches!(events[1], RunEvent::NoSpeech { invented: true }));
        assert_eq!(*observer.guards.lock().unwrap(), [Guard::Invented]);
    }

    #[tokio::test(start_paused = true)]
    async fn an_stt_timeout_emits_failed() {
        let stt = Arc::new(FakeStt::delayed("late", Duration::from_secs(1)));
        let chat = Arc::new(ScriptedChat::new([]));
        let tts = Arc::new(ScriptedTts::default());
        let observer = Arc::new(TestObserver::default());
        let (sink, _source) = reply_channel(2);
        let events = TestEvents::default();
        let limits = RunLimits {
            stt: Duration::from_millis(10),
            ..RunLimits::MODEL
        };
        run(
            request(
                RunKind::Request,
                Some(speech()),
                pipeline(stt, chat, tts, limits),
                sink,
                observer.clone(),
            ),
            &events,
        )
        .await;
        let events = events.events();
        assert!(matches!(events[0], RunEvent::SpeechEnded));
        assert!(matches!(
            &events[1],
            RunEvent::Failed(error) if error.slot == Slot::Stt && error.reason == Reason::Timeout
        ));
        assert_eq!(events.len(), 2);
        assert_eq!(observer.failures.lock().unwrap().len(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn a_typed_request_skips_listening_and_transcription() {
        let stt = Arc::new(FakeStt::new("unused"));
        let chat = Arc::new(ScriptedChat::new([vec![
            ChatDelta::Text("Typed answer.".into()),
            ChatDelta::End,
        ]]));
        let tts = Arc::new(ScriptedTts::default());
        let observer = Arc::new(TestObserver::default());
        let (sink, mut source) = reply_channel(2);
        let events = TestEvents::default();
        let run = run(
            request(
                RunKind::Typed("Typed question".into()),
                None,
                pipeline(stt.clone(), chat, tts, RunLimits::MODEL),
                sink,
                observer,
            ),
            &events,
        );
        tokio::join!(run, receive(&mut source, 1));
        assert_eq!(stt.calls(), 0);
        let events = events.events();
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, RunEvent::SpeechEnded))
        );
        assert!(matches!(
            &events.last().unwrap(),
            RunEvent::Done {
                turn: Some(Turn { user, .. }),
                ..
            } if user == "Typed question"
        ));
    }

    /// Answers every call with the same thing and counts the calls.
    struct FixedPersonal {
        personal: Option<Personal>,
        calls: Mutex<Vec<RunKind>>,
    }

    impl FixedPersonal {
        fn new(personal: Option<Personal>) -> Arc<Self> {
            Arc::new(Self {
                personal,
                calls: Mutex::new(Vec::new()),
            })
        }

        fn calls(&self) -> Vec<RunKind> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl Personalize for FixedPersonal {
        fn after_transcript<'a>(&'a self, kind: &'a RunKind) -> BoxFuture<'a, Option<Personal>> {
            self.calls.lock().unwrap().push(kind.clone());
            Box::pin(async move { self.personal.clone() })
        }
    }

    fn turn(user: &str, assistant: &str) -> Turn {
        Turn {
            user: user.into(),
            assistant: assistant.into(),
        }
    }

    /// Run `kind` with `personalize` and the request's own history of one
    /// turn; returns what the brain was asked.
    async fn personalized(
        kind: RunKind,
        answer: &'static str,
        personalize: Arc<FixedPersonal>,
    ) -> Vec<reply::test_util::RecordedRequest> {
        let spoken = !matches!(kind, RunKind::Typed(_));
        let stt = Arc::new(FakeStt::new(answer));
        let chat = Arc::new(ScriptedChat::new([vec![
            ChatDelta::Text("Answer.".into()),
            ChatDelta::End,
        ]]));
        let tts = Arc::new(ScriptedTts::default());
        let (sink, mut source) = reply_channel(2);
        let mut req = request(
            kind,
            spoken.then(speech),
            pipeline(stt, chat.clone(), tts, RunLimits::MODEL),
            sink,
            Arc::new(TestObserver::default()),
        );
        req.history = vec![turn("Before", "Earlier.")];
        req.personalize = Some(personalize);
        let events = TestEvents::default();
        tokio::join!(run(req, &events), receive(&mut source, 1));
        chat.requests()
    }

    #[tokio::test(start_paused = true)]
    async fn the_hook_replaces_the_prompt_and_the_history() {
        let personalize = FixedPersonal::new(Some(Personal {
            system: "For Anna.".into(),
            history: Some(Vec::new()),
        }));
        let requests = personalized(RunKind::Request, "Question", personalize.clone()).await;
        assert_eq!(requests[0].system, "For Anna.");
        assert_eq!(requests[0].messages.len(), 1, "{:?}", requests[0].messages);
        assert_eq!(personalize.calls(), [RunKind::Request]);

        let personalize = FixedPersonal::new(Some(Personal {
            system: "For Ben.".into(),
            history: Some(vec![turn("Mine", "Reply.")]),
        }));
        let requests = personalized(RunKind::FollowUp, "Question", personalize.clone()).await;
        assert_eq!(requests[0].system, "For Ben.");
        assert_eq!(requests[0].messages.len(), 3);
        assert_eq!(personalize.calls(), [RunKind::FollowUp]);
    }

    #[tokio::test(start_paused = true)]
    async fn no_history_in_the_hook_keeps_the_requests() {
        let personalize = FixedPersonal::new(Some(Personal {
            system: "For Anna.".into(),
            history: None,
        }));
        let requests = personalized(RunKind::Request, "Question", personalize).await;
        assert_eq!(requests[0].system, "For Anna.");
        assert_eq!(requests[0].messages.len(), 3);
        assert!(matches!(&requests[0].messages[0], Message::User(text) if text == "Before"));
    }

    #[tokio::test(start_paused = true)]
    async fn no_answer_from_the_hook_keeps_the_requests_prompt() {
        let personalize = FixedPersonal::new(None);
        let requests = personalized(RunKind::Request, "Question", personalize.clone()).await;
        assert_eq!(requests[0].system, "Be brief.");
        assert_eq!(requests[0].messages.len(), 3);
        assert_eq!(personalize.calls().len(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn the_hook_never_hears_of_a_typed_run() {
        let personalize = FixedPersonal::new(Some(Personal {
            system: "For Anna.".into(),
            history: Some(Vec::new()),
        }));
        let requests = personalized(
            RunKind::Typed("Question".into()),
            "unused",
            personalize.clone(),
        )
        .await;
        assert!(personalize.calls().is_empty());
        assert_eq!(requests[0].system, "Be brief.");
        assert_eq!(requests[0].messages.len(), 3);
    }

    #[tokio::test(start_paused = true)]
    async fn the_hook_waits_for_a_transcript_that_is_not_invented() {
        let personalize = FixedPersonal::new(Some(Personal {
            system: "For Anna.".into(),
            history: None,
        }));
        let chat = Arc::new(ScriptedChat::new([]));
        for said in ["Thank you for watching.", "  "] {
            let (sink, _source) = reply_channel(2);
            let mut req = request(
                RunKind::Request,
                Some(speech()),
                pipeline(
                    Arc::new(FakeStt::new(said)),
                    chat.clone(),
                    Arc::new(ScriptedTts::default()),
                    RunLimits::MODEL,
                ),
                sink,
                Arc::new(TestObserver::default()),
            );
            req.personalize = Some(personalize.clone());
            run(req, &TestEvents::default()).await;
        }
        assert!(personalize.calls().is_empty());
        assert!(chat.requests().is_empty());
    }

    #[derive(Default)]
    struct PendingTts {
        started: Notify,
        dropped: AtomicBool,
    }

    struct DropFlag<'a>(&'a AtomicBool);

    impl Drop for DropFlag<'_> {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    impl Tts for PendingTts {
        fn speak<'a>(
            &'a self,
            _: &'a str,
        ) -> BoxFuture<'a, Result<crate::reply::SpeechAudio, ProviderError>> {
            Box::pin(async move {
                let _guard = DropFlag(&self.dropped);
                self.started.notify_one();
                std::future::pending().await
            })
        }
    }

    #[tokio::test(start_paused = true)]
    async fn aborting_a_run_drops_synthesis_and_emits_nothing_more() {
        let stt = Arc::new(FakeStt::new("unused"));
        let chat = Arc::new(ScriptedChat::pending_after(vec![ChatDelta::Text(
            "One. Two".into(),
        )]));
        let tts = Arc::new(PendingTts::default());
        let observer = Arc::new(TestObserver::default());
        let (sink, _source) = reply_channel(2);
        let events = Arc::new(TestEvents::default());
        let request = request(
            RunKind::Typed("Question".into()),
            None,
            pipeline(stt, chat, tts.clone(), RunLimits::MODEL),
            sink,
            observer,
        );
        let notified = tts.started.notified();
        let task_events = events.clone();
        let task = tokio::spawn(async move { run(request, task_events.as_ref()).await });
        notified.await;
        let count = events.events().len();
        task.abort();
        let _ = task.await;
        assert!(tts.dropped.load(Ordering::SeqCst));
        tokio::task::yield_now().await;
        assert_eq!(events.events().len(), count);
        assert!(!events.events().iter().any(|event| matches!(
            event,
            RunEvent::Done { .. } | RunEvent::Failed(_) | RunEvent::NoSpeech { .. }
        )));
    }
}
