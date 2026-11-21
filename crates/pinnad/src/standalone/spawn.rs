//! The production run spawner (ADR 0017 §"The pipeline": a standalone
//! request is its own task).
//!
//! The Brain hands over what a run needs; the spawner adds the system prompt,
//! the device tools and the metrics, and runs the request on its own task.
//! Every event goes back to the Brain as [`Input::Assist`] under the run's
//! id. A run that panics fails like any other, so the Brain never waits on a
//! task that is gone.

use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::SystemTime;

use futures::FutureExt;
use futures::future::BoxFuture;
use pinna_assist::config::primary_language;
use pinna_assist::error::{ProviderError, Reason, Slot};
use pinna_assist::observe::Observer;
use pinna_assist::presets::PresetId;
use pinna_assist::prompt::{self, Context, GroupMember, MemberMode};
use pinna_assist::run::{RunEvent, RunEvents, RunRequest};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

use super::AssistLive;
use super::runlog::RunLog;
use crate::group::Group;
use crate::settings::Mode;
use crate::tools::{Caller, DeviceTools, Surface, ToolKit};
use crate::voice::{DeviceFacts, Input, LocalStart, RunId, RunSpawner, Snapshot};

/// Builds each standalone run from the live configuration and spawns it.
pub struct ProdSpawner {
    /// The Brain's input queue: run events and the run's device commands.
    inputs: mpsc::Sender<Input>,
    status: watch::Receiver<Snapshot>,
    observer: Arc<dyn Observer>,
    kit: ToolKit,
}

impl ProdSpawner {
    pub fn new(
        inputs: mpsc::Sender<Input>,
        status: watch::Receiver<Snapshot>,
        observer: Arc<dyn Observer>,
        kit: ToolKit,
    ) -> Self {
        Self {
            inputs,
            status,
            observer,
            kit,
        }
    }

    fn request(
        &self,
        start: LocalStart,
        observer: Arc<dyn Observer>,
    ) -> Result<RunRequest, ProviderError> {
        let LocalStart {
            run,
            kind,
            live,
            history,
            mic,
            reply,
            device,
        } = start;
        let Some(pipeline) = live.pipeline.clone() else {
            // The configuration changed between the Brain's readiness check
            // and the start.
            tracing::warn!("standalone run {} has no pipeline", run.0);
            return Err(ProviderError::new(
                Slot::Chat,
                PresetId::Custom,
                Reason::Internal,
            ));
        };
        let tools = DeviceTools::new(
            &self.kit,
            self.inputs.clone(),
            self.status.clone(),
            live.clone(),
            Surface::Run,
            Caller::Run(run),
        );
        Ok(RunRequest {
            kind,
            pipeline,
            system: system_prompt(
                &live,
                &device,
                group_context(self.kit.group().map(|g| g.as_ref())),
                SystemTime::now(),
            ),
            history,
            // Speech services take the primary subtag only (`en`, not
            // `en-US`); the prompt keeps the whole tag.
            language: live.language.as_deref().map(primary_language),
            // One per satellite; only a brain that asks for it (OpenClaw)
            // gets it.
            user: Some(stable_user(&device)),
            mic,
            reply,
            tools: Arc::new(tools),
            ack: live.ack.clone(),
            observer,
        })
    }
}

impl RunSpawner for ProdSpawner {
    fn spawn(&self, start: LocalStart) -> JoinHandle<()> {
        let events = BrainEvents {
            run: start.run,
            inputs: self.inputs.clone(),
        };
        // The run's own observer: the metrics, and its line in the log.
        let log = RunLog::new(start.run, self.observer.clone());
        let request = self.request(start, log.clone());
        tokio::spawn(async move {
            let failure = match request {
                Ok(request) => {
                    let preset = request.pipeline.presets.chat;
                    let outcome = AssertUnwindSafe(pinna_assist::run::run(request, &events))
                        .catch_unwind()
                        .await;
                    if outcome.is_ok() {
                        log.finished();
                        return;
                    }
                    tracing::error!("standalone run {} panicked", events.run.0);
                    // Where it panicked is unknown: the brain stands for the request.
                    ProviderError::new(Slot::Chat, preset, Reason::Internal)
                }
                Err(failure) => failure,
            };
            // The run never got to report it.
            log.failed(&failure);
            log.finished();
            events.emit(RunEvent::Failed(failure)).await;
        })
    }
}

/// The system prompt: the owner's text or the voice rules, then the context.
fn system_prompt(
    live: &AssistLive,
    device: &DeviceFacts,
    group: Vec<GroupMember>,
    now: SystemTime,
) -> String {
    let (time, zone) = crate::clock::local_now(device.time_zone.as_deref(), now);
    prompt::system(
        live.prompt.as_deref(),
        &Context {
            time,
            zone,
            place: live.place.as_ref().map(|place| place.name.clone()),
            language: live.language.clone(),
            satellite: device.name.clone(),
            group,
        },
    )
}

/// The other members of the satellite group, for the context (ADR 0026):
/// so the brain can name them to `send_message`, and say why one in Home
/// Assistant mode is out of reach. Empty in no group.
pub fn group_context(group: Option<&Group>) -> Vec<GroupMember> {
    group.map_or_else(Vec::new, |group| {
        group
            .other_members()
            .into_iter()
            .map(|(member, mode)| GroupMember {
                name: member.name,
                mode: mode.map(|mode| match mode {
                    Mode::Standalone => MemberMode::Standalone,
                    Mode::HomeAssistant => MemberMode::HomeAssistant,
                }),
            })
            .collect()
    })
}

/// The `user` a hand-off agent keys its session on: the same for every
/// request from this satellite.
fn stable_user(device: &DeviceFacts) -> String {
    format!("pinna-{}", device.device_id)
}

/// A run's events, on their way to the Brain.
struct BrainEvents {
    run: RunId,
    inputs: mpsc::Sender<Input>,
}

impl RunEvents for BrainEvents {
    fn emit(&self, ev: RunEvent) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            // The queue closes only when pinnad stops.
            let _ = self.inputs.send(Input::Assist { run: self.run, ev }).await;
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    use bytes::Bytes;
    use futures::StreamExt;
    use futures::stream::BoxStream;
    use pinna_assist::chat::{Chat, ChatDelta, ChatRequest, Message};
    use pinna_assist::limits::RunLimits;
    use pinna_assist::observe::Guard;
    use pinna_assist::pipeline::Pipeline;
    use pinna_assist::presets::{Slots, preset};
    use pinna_assist::reply::{AudioBody, AudioKind, SpeechAudio};
    use pinna_assist::stt::Stt;
    use pinna_assist::tts::Tts;
    use pinna_audio::{Backend, Speaker, SpeakerConfig};
    use pinna_esphome::{DeviceEvent, EsphomeHandle};
    use tokio::sync::oneshot;

    use super::*;
    use crate::leds::LedState;
    use crate::settings::{Mode, Settings};
    use crate::voice::{ApiCommand, ApiReply, Brain, Defaults, LocalOptions, Options, Shared};

    /// The wake word detector's noise floor in a quiet room.
    const QUIET_ENERGY: u64 = 256;

    /// What the speech-to-text fake does with an utterance.
    #[derive(Clone, Copy)]
    enum Heard {
        Text(&'static str),
        Fail(Reason),
        Panic,
    }

    struct FakeStt(Heard);

    impl Stt for FakeStt {
        fn transcribe<'a>(
            &'a self,
            _wav: Bytes,
            _language: Option<&'a str>,
        ) -> BoxFuture<'a, Result<String, ProviderError>> {
            let heard = self.0;
            Box::pin(async move {
                match heard {
                    Heard::Text(text) => Ok(text.to_owned()),
                    Heard::Fail(reason) => {
                        Err(ProviderError::new(Slot::Stt, PresetId::OpenAi, reason))
                    }
                    Heard::Panic => panic!("the speech-to-text fake panics"),
                }
            })
        }
    }

    /// A brain request as the fake chat saw it.
    #[derive(Clone, Debug)]
    struct Asked {
        system: String,
        messages: Vec<Message>,
        tools: Vec<String>,
        max_tokens: u32,
        user: Option<String>,
    }

    /// Answers every request with one sentence and records what it was asked.
    struct FakeChat {
        answer: &'static str,
        asked: Mutex<Vec<Asked>>,
    }

    impl FakeChat {
        fn answering(answer: &'static str) -> Arc<Self> {
            Arc::new(Self {
                answer,
                asked: Mutex::new(Vec::new()),
            })
        }

        fn asked(&self) -> Vec<Asked> {
            self.asked.lock().unwrap().clone()
        }
    }

    impl Chat for FakeChat {
        fn stream<'a>(
            &'a self,
            req: ChatRequest<'a>,
        ) -> BoxFuture<'a, Result<BoxStream<'a, Result<ChatDelta, ProviderError>>, ProviderError>>
        {
            self.asked.lock().unwrap().push(Asked {
                system: req.system.to_owned(),
                messages: req.messages.to_vec(),
                tools: req.tools.iter().map(|t| t.name.clone()).collect(),
                max_tokens: req.max_tokens,
                user: req.user.map(str::to_owned),
            });
            let deltas = [ChatDelta::Text(self.answer.into()), ChatDelta::End];
            Box::pin(async move { Ok(futures::stream::iter(deltas.map(Ok)).boxed()) })
        }
    }

    /// Speaks every sentence as 100 ms of WAV.
    struct FakeTts;

    impl Tts for FakeTts {
        fn speak<'a>(
            &'a self,
            _text: &'a str,
        ) -> BoxFuture<'a, Result<SpeechAudio, ProviderError>> {
            let samples: Vec<u8> = (0..1_600).flat_map(|_| 1_000i16.to_le_bytes()).collect();
            let wav = pinna_assist::audio::wav(&[Bytes::from(samples)]);
            Box::pin(async move {
                Ok(SpeechAudio {
                    kind: AudioKind::Sentence,
                    mime: Some("audio/wav".into()),
                    body: AudioBody::Stream(futures::stream::iter([Ok(wav)]).boxed()),
                })
            })
        }
    }

    fn pipeline(heard: Heard, chat: Arc<FakeChat>, brain: PresetId) -> Pipeline {
        Pipeline {
            stt: Arc::new(FakeStt(heard)),
            chat,
            tts: Arc::new(FakeTts),
            presets: Slots {
                chat: brain,
                ..Slots::all(PresetId::OpenAi)
            },
            behaviour: preset(brain).behaviour,
            label: format!("{}: test-model", brain.label()),
        }
    }

    /// One 10 ms microphone frame at `amplitude`.
    fn frame(amplitude: i16) -> Bytes {
        Bytes::from(
            (0..160)
                .flat_map(|_| amplitude.to_le_bytes())
                .collect::<Vec<u8>>(),
        )
    }

    /// 1.1 s of speech, then a second of quiet.
    fn speech() -> impl Iterator<Item = Bytes> {
        std::iter::repeat_n(frame(3_000), 110).chain(std::iter::repeat_n(frame(10), 100))
    }

    fn quiet(frames: usize) -> impl Iterator<Item = Bytes> {
        std::iter::repeat_n(frame(10), frames)
    }

    /// A standalone satellite: a real Brain on its own task with a speaker
    /// that writes a file, and the production spawner over fake providers.
    struct Satellite {
        inputs: mpsc::Sender<Input>,
        shared: Arc<Shared>,
        status: watch::Receiver<Snapshot>,
        events: mpsc::Receiver<DeviceEvent>,
        _live: watch::Sender<Arc<AssistLive>>,
        _dir: tempfile::TempDir,
    }

    impl Satellite {
        async fn start(pipeline: Pipeline) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let (esp, events) = EsphomeHandle::detached(256);
            let (status_tx, mut status) = watch::channel(Snapshot::default());
            let (inputs, rx) = mpsc::channel(64);
            let shared = Arc::new(Shared::default());
            shared.noise_energy.store(QUIET_ENERGY, Ordering::Relaxed);
            let speaker = Arc::new(Speaker::new(SpeakerConfig::new(Backend::WavFile(
                dir.path().join("speaker.wav"),
            ))));
            let (live_tx, live) = watch::channel(Arc::new(AssistLive {
                chat_gen: 1,
                ready: true,
                slots: Slots::all(true),
                label: pipeline.label.clone(),
                // A reply ends the conversation.
                follow_ups: false,
                pipeline: Some(Arc::new(pipeline)),
                ..AssistLive::empty()
            }));
            let spawner = ProdSpawner::new(
                inputs.clone(),
                status.clone(),
                Arc::new(shared.counters.standalone.clone()),
                ToolKit::new(shared.counters.clone()),
            );
            let brain = Brain::new_with_local(
                esp,
                Some(speaker),
                shared.clone(),
                watch::channel(LedState::default()).0,
                status_tx,
                inputs.clone(),
                Options {
                    key_file: dir.path().join("key"),
                    settings_file: dir.path().join("settings.toml"),
                    wake_words: crate::wakewords::test_catalogue(),
                    active_wake_words: vec!["alexa_v0.1".into()],
                    keywords: None,
                    defaults: Defaults {
                        node_name: "pinna-test".into(),
                        friendly_name: "Test".into(),
                        default_name: true,
                        wake_sound: false,
                        update_channel: crate::updates::UpdateChannel::Off,
                    },
                    updates: None,
                    mdns: None,
                    encrypted: false,
                    has_hw_privacy: false,
                    privacy_trigger: Vec::new(),
                    mute_lamp: None,
                    airplay: false,
                    bluetooth: false,
                    sendspin: None,
                    settings: Settings {
                        mode: Mode::Standalone,
                        ..Settings::default()
                    },
                },
                LocalOptions {
                    live,
                    spawner: Arc::new(spawner),
                    timers_file: dir.path().join("timers.toml"),
                },
            );
            tokio::spawn(brain.run(rx));
            status
                .wait_for(|s| s.name == "pinna-test")
                .await
                .expect("the Brain published no snapshot");
            Self {
                inputs,
                shared,
                status,
                events,
                _live: live_tx,
                _dir: dir,
            }
        }

        async fn api(&self, cmd: impl FnOnce(ApiReply) -> ApiCommand) -> Result<Snapshot, String> {
            let (reply, answer) = oneshot::channel();
            self.inputs.send(Input::Api(cmd(reply))).await.unwrap();
            answer.await.unwrap()
        }

        /// Talk into the microphone until the run stops listening.
        async fn say(&self, frames: impl IntoIterator<Item = Bytes>) {
            let (mut seen, mut route) = (0, None);
            self.shared.mic.current(&mut seen, &mut route);
            let route = route.expect("the request listens");
            for frame in frames {
                if route.send(frame).await.is_err() {
                    return;
                }
            }
        }

        /// Type a question and wait for its reply to end.
        async fn ask(&mut self, question: &str) -> Vec<String> {
            let question = question.to_owned();
            self.api(|reply| ApiCommand::AskText(question, reply))
                .await
                .unwrap();
            self.voice_events_until("finished").await
        }

        /// The voice assistant events up to and including `last`.
        async fn voice_events_until(&mut self, last: &str) -> Vec<String> {
            let mut seen = Vec::new();
            loop {
                let event = tokio::time::timeout(Duration::from_secs(10), self.events.recv())
                    .await
                    .unwrap_or_else(|_| panic!("no {last:?} after {seen:?}"))
                    .expect("the Brain stopped");
                if let DeviceEvent::Event {
                    object_id,
                    event_type,
                } = event
                    && object_id == "voice_assistant"
                {
                    let done = event_type == last;
                    seen.push(event_type);
                    if done {
                        return seen;
                    }
                }
            }
        }

        async fn idle(&mut self) -> Snapshot {
            self.status
                .wait_for(|s| s.phase == pinna_api::Phase::PHASE_IDLE)
                .await
                .unwrap()
                .clone()
        }
    }

    #[tokio::test]
    async fn a_spoken_request_runs_through_the_pipeline_to_idle() {
        let chat = FakeChat::answering("It is noon.");
        let mut sat = Satellite::start(pipeline(
            Heard::Text("What time is it?"),
            chat.clone(),
            PresetId::OpenAi,
        ))
        .await;

        sat.api(ApiCommand::StartConversation).await.unwrap();
        sat.say(speech()).await;

        assert_eq!(
            sat.voice_events_until("finished").await,
            ["listening", "thinking", "replying", "finished"]
        );
        assert_eq!(sat.idle().await.assist.last_error, None);
        let asked = chat.asked();
        assert_eq!(asked.len(), 1);
        assert_eq!(
            asked[0].messages,
            [Message::User("What time is it?".into())]
        );
        assert!(
            asked[0].system.ends_with("Satellite: Test."),
            "{}",
            asked[0].system
        );
        // A model gets the brain's device tools and no user.
        assert!(asked[0].tools.iter().any(|t| t == "set_volume"));
        assert!(!asked[0].tools.iter().any(|t| t == "status"));
        assert_eq!(asked[0].max_tokens, RunLimits::MODEL.max_tokens);
        assert_eq!(asked[0].user, None);
        assert_eq!(sat.shared.counters.runs.get(), 1);
    }

    #[tokio::test]
    async fn a_run_that_panics_fails_with_an_internal_error() {
        let mut sat = Satellite::start(pipeline(
            Heard::Panic,
            FakeChat::answering("Never."),
            PresetId::OpenAi,
        ))
        .await;

        sat.api(ApiCommand::StartConversation).await.unwrap();
        sat.say(speech()).await;

        assert_eq!(
            sat.voice_events_until("stopped").await,
            ["listening", "thinking", "stopped"]
        );
        assert_eq!(
            sat.idle()
                .await
                .assist
                .last_error
                .map(|f| f.error.to_string())
                .as_deref(),
            Some("brain via openai: internal error")
        );
        let counted = &sat.shared.counters.standalone;
        assert_eq!(counted.failure_total(Slot::Chat, Reason::Internal), 1);
    }

    #[tokio::test]
    async fn a_guard_and_a_failure_the_run_reports_count_once() {
        let mut sat = Satellite::start(pipeline(
            Heard::Fail(Reason::Http(401)),
            FakeChat::answering("Never."),
            PresetId::OpenAi,
        ))
        .await;

        sat.api(ApiCommand::StartConversation).await.unwrap();
        sat.say(quiet(600)).await;
        assert_eq!(
            sat.voice_events_until("stopped").await,
            ["listening", "stopped"]
        );

        sat.api(ApiCommand::StartConversation).await.unwrap();
        sat.say(speech()).await;
        assert_eq!(
            sat.voice_events_until("stopped").await,
            ["listening", "thinking", "stopped"]
        );

        let counted = &sat.shared.counters.standalone;
        assert_eq!(counted.guard_total(Guard::NoSpeech), 1);
        assert_eq!(counted.failure_total(Slot::Stt, Reason::Http(401)), 1);
        assert_eq!(
            sat.idle()
                .await
                .assist
                .last_error
                .map(|f| f.error.to_string())
                .as_deref(),
            Some("stt via openai: HTTP 401")
        );
    }

    #[tokio::test]
    async fn a_hermes_agent_gets_the_conversation_by_size_and_no_tools() {
        let chat = FakeChat::answering("Done.");
        let mut sat = Satellite::start(pipeline(
            Heard::Text("unused"),
            chat.clone(),
            PresetId::Hermes,
        ))
        .await;

        // More turns than a model keeps: an agent's history is bounded by size.
        for n in 1..=12 {
            assert_eq!(
                sat.ask(&format!("Question {n}?")).await,
                ["thinking", "replying", "finished"]
            );
        }

        let asked = chat.asked();
        let last = &asked[11].messages;
        assert_eq!(last.len(), 23);
        assert_eq!(last[0], Message::User("Question 1?".into()));
        assert_eq!(
            last[1],
            Message::Assistant {
                text: "Done.".into(),
                calls: Vec::new(),
                signature: None,
            }
        );
        assert_eq!(last[22], Message::User("Question 12?".into()));
        for request in &asked {
            assert!(request.tools.is_empty(), "an agent runs its own tools");
            assert_eq!(request.max_tokens, RunLimits::AGENT.max_tokens);
            assert_eq!(request.user, None);
        }
    }

    #[tokio::test]
    async fn an_openclaw_agent_gets_only_the_new_turn_under_a_stable_user() {
        let chat = FakeChat::answering("Done.");
        let mut sat = Satellite::start(pipeline(
            Heard::Text("unused"),
            chat.clone(),
            PresetId::OpenClaw,
        ))
        .await;

        for question in ["First?", "Second?"] {
            assert_eq!(
                sat.ask(question).await,
                ["thinking", "replying", "finished"]
            );
        }

        let asked = chat.asked();
        assert_eq!(asked[1].messages, [Message::User("Second?".into())]);
        for request in &asked {
            // Without mDNS the node name stands in for the MAC address.
            assert_eq!(request.user.as_deref(), Some("pinna-pinna-test"));
            assert!(request.tools.is_empty(), "an agent runs its own tools");
            assert_eq!(request.max_tokens, RunLimits::AGENT.max_tokens);
        }
    }
}
