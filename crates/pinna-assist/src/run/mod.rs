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
    pub clock_synced: fn() -> bool,
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
        clock_synced: _,
    } = req;
    let question = match kind {
        RunKind::Typed(question) => question,
        RunKind::Request | RunKind::FollowUp => {
            let Some(mut mic) = mic else {
                return fail(
                    ProviderError::new(Slot::Stt, pipeline.presets[0], Reason::Internal),
                    sink,
                    observer,
                    events,
                )
                .await;
            };
            let frames = match listen::listen(&mut mic, &pipeline.behaviour.limits).await {
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
            observer.spent(pipeline.presets[0], Spend::AudioSeconds(seconds));
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
                        ProviderError::new(Slot::Stt, pipeline.presets[0], Reason::Timeout),
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
            chat_preset: pipeline.presets[1],
            tts_preset: pipeline.presets[2],
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
                    turn: Some(outcome.turn),
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
        chat::{Chat, ChatDelta},
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
            presets: [PresetId::OpenAi; 3],
            model: "test-model".into(),
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
            clock_synced: || true,
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
