//! Standalone run state owned by the Brain (ADR 0017).

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Instant;

use futures::future::BoxFuture;
use pinna_assist::audio::MicInput;
use pinna_assist::conversation::{Conversation, Turn};
use pinna_assist::error::{ProviderError, Reason, Slot};
use pinna_assist::limits::RunLimits;
use pinna_assist::observe::Guard;
use pinna_assist::presets::PresetId;
use pinna_assist::reply::{
    AudioBody, AudioKind, ReplySink, ReplySource, SpeechAudio, reply_channel,
};
use pinna_assist::run::{RunEvent, RunKind};
use pinna_audio::{Speaker, VoicePlayback, tone};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

use super::{ApiCommand, AssistStatus, Brain, Input, Phase, PlaybackKind, RunId};
use crate::mic::MIC_QUEUE;
use crate::settings::Mode;
use crate::standalone::AssistLive;

/// Sentences a run may synthesise before the first of them sounds.
const REPLY_AHEAD: usize = 2;

/// Standalone wiring kept separate from `Options` so existing test builders
/// remain exhaustive struct literals.
pub struct LocalOptions {
    pub live: watch::Receiver<Arc<AssistLive>>,
    pub spawner: Arc<dyn RunSpawner>,
    pub timers_file: PathBuf,
}

impl LocalOptions {
    #[cfg(test)]
    fn beside_settings(settings_file: &std::path::Path) -> Self {
        let (_tx, live) = watch::channel(Arc::new(AssistLive::empty()));
        Self {
            live,
            spawner: Arc::new(NoopSpawner),
            timers_file: settings_file.with_file_name("timers.toml"),
        }
    }

    pub(super) fn into_parts(
        self,
    ) -> (
        watch::Receiver<Arc<AssistLive>>,
        Arc<dyn RunSpawner>,
        PathBuf,
    ) {
        (self.live, self.spawner, self.timers_file)
    }
}

pub(super) struct Local {
    pub(super) run: Option<LocalRun>,
    next_run: u64,
    conversation: Conversation,
    /// The brain slot's generation the conversation belongs to.
    chat_gen: u64,
    /// The noise floor before the wake word; follow-ups reuse it so the
    /// reply's tail can't raise it.
    noise_seed: Option<u64>,
    live: watch::Receiver<Arc<AssistLive>>,
    spawner: Arc<dyn RunSpawner>,
    last_error: Option<String>,
    /// The run asked to mute the microphone; applied when the run ends.
    pending_mute: bool,
}

impl Local {
    pub(super) fn new(
        live: watch::Receiver<Arc<AssistLive>>,
        spawner: Arc<dyn RunSpawner>,
    ) -> Self {
        let (history, chat_gen) = {
            let live = live.borrow();
            (live.behaviour().history, live.chat_gen)
        };
        Self {
            run: None,
            next_run: 1,
            conversation: Conversation::new(history),
            chat_gen,
            noise_seed: None,
            live,
            spawner,
            last_error: None,
            pending_mute: false,
        }
    }

    #[cfg(test)]
    pub(super) fn beside_settings(settings_file: &std::path::Path) -> LocalOptions {
        LocalOptions::beside_settings(settings_file)
    }

    pub(super) fn ready(&self) -> bool {
        self.live.borrow().ready
    }

    /// Start the next request without any earlier turns.
    pub(super) fn forget_conversation(&mut self) {
        let live = self.live.borrow();
        self.conversation.reset(live.behaviour().history);
        self.chat_gen = live.chat_gen;
    }

    pub(super) fn status(&self) -> AssistStatus {
        let live = self.live.borrow();
        AssistStatus {
            ready: live.ready,
            slots: live.slots,
            label: live.label.clone(),
            last_error: self.last_error.clone(),
        }
    }
}

pub(super) struct LocalRun {
    id: RunId,
    kind: RunKind,
    task: Option<JoinHandle<()>>,
    deadline: Instant,
    limits: RunLimits,
    source: Option<ReplySource>,
    /// `Done` arrived; its turn is already in the conversation.
    done: bool,
    spoke: bool,
    player_done: bool,
}

impl LocalRun {
    fn expired(&self, now: Instant) -> bool {
        now >= self.deadline
    }

    /// Re-arm the deadline: it only ever moves later.
    fn rearm(&mut self, now: Instant, limit: std::time::Duration) {
        self.deadline = self.deadline.max(now + limit);
    }
}

/// How a run kind is named in logs; a typed question's text is never logged.
fn kind_label(kind: &RunKind) -> &'static str {
    match kind {
        RunKind::Request => "request",
        RunKind::FollowUp => "follow-up",
        RunKind::Typed(_) => "typed question",
    }
}

impl Brain {
    /// Start a standalone run (ADR 0017): a request after the wake word or
    /// Action, a follow-up after a reply, or a typed question. The run task
    /// gets the microphone route and the reply channel; the Brain keeps the
    /// phase, the deadline and the voice lane.
    pub(super) async fn start_local_run(&mut self, kind: RunKind, phrase: Option<String>) {
        let refused = if !self.assist_ready() {
            Some("the assistant isn't set up")
        } else if self.muted() {
            Some("the microphone is muted")
        } else if self.ringing.is_some() {
            Some("a timer alarm is ringing")
        } else {
            None
        };
        if let Some(why) = refused {
            tracing::warn!("no standalone {}: {why}", kind_label(&kind));
            self.error_feedback();
            return;
        }
        debug_assert!(self.local.run.is_none(), "a standalone run is still live");
        // Speak or a media announcement must not talk over the request.
        self.stop_playback();

        let now = Instant::now();
        let live = self.local.live.borrow().clone();
        let limits = live.behaviour().limits;
        let typed = matches!(kind, RunKind::Typed(_));
        if kind != RunKind::FollowUp {
            // A follow-up counts once it hears speech.
            self.shared.counters.runs.inc();
            if self.local.chat_gen != live.chat_gen {
                self.local.conversation.reset(live.behaviour().history);
                self.local.chat_gen = live.chat_gen;
            }
        }
        if kind == RunKind::Request {
            self.local.noise_seed = match self.shared.noise_energy.load(Ordering::Relaxed) {
                0 => None,
                energy => Some(energy),
            };
        }
        let mic = (!typed).then(|| {
            let (tx, frames) = mpsc::channel(MIC_QUEUE);
            self.shared.mic.open(tx);
            MicInput {
                frames,
                noise_seed: self.local.noise_seed,
                follow_up: kind == RunKind::FollowUp,
            }
        });
        let (reply, source) = reply_channel(REPLY_AHEAD);
        let history = self.local.conversation.history(now);
        let id = RunId(self.local.next_run);
        self.local.next_run += 1;

        self.speech_ended = None;
        self.phase = if typed {
            Phase::Thinking(now)
        } else {
            Phase::Listening(now)
        };
        // Open the speaker while the user talks: it takes ~1.4 s on the Dot,
        // and each power-up cracks.
        self.warm_speaker();
        self.talker.clear();
        self.track_talker();
        let device = self.device_facts();
        let task = self.local.spawner.spawn(LocalStart {
            run: id,
            kind: kind.clone(),
            live,
            history,
            mic,
            reply,
            device,
        });
        let deadline = now
            + if typed {
                limits.think_backstop()
            } else {
                limits.listen_backstop()
            };
        self.local.run = Some(LocalRun {
            id,
            kind: kind.clone(),
            task: Some(task),
            deadline,
            limits,
            source: Some(source),
            done: false,
            spoke: false,
            player_done: false,
        });
        match &phrase {
            Some(phrase) => tracing::info!(
                "starting standalone {} {} after {phrase:?}",
                kind_label(&kind),
                id.0
            ),
            None => tracing::info!("starting standalone {} {}", kind_label(&kind), id.0),
        }
        self.event(
            "voice_assistant",
            if typed { "thinking" } else { "listening" },
        )
        .await;
        if kind == RunKind::Request && self.wake_sound() {
            self.play_pcm(tone::wake_chime(), PlaybackKind::Cue);
        }
    }

    fn device_facts(&self) -> DeviceFacts {
        DeviceFacts {
            name: self.friendly_name(),
            time_zone: self.settings.time_zone.clone(),
            device_id: self
                .opts
                .mdns
                .as_ref()
                .map_or_else(|| self.node_name(), |dc| dc.mac_string()),
        }
    }

    pub(super) fn cancel_local(&mut self) {
        let Some(mut run) = self.local.run.take() else {
            return;
        };
        if let Some(task) = run.task.take() {
            task.abort();
        }
        self.shared.mic.close();
        if let Some(speaker) = &self.speaker {
            speaker.stop();
        }
    }

    /// An event from the run task. Events from a run that is no longer the
    /// current one are dropped.
    pub(super) async fn assist_event(&mut self, run: RunId, ev: RunEvent) {
        let now = Instant::now();
        let Some(current) = self.local.run.as_mut().filter(|r| r.id == run) else {
            tracing::debug!("dropped an event from standalone run {}", run.0);
            return;
        };
        let step = step(self.phase, &current.kind, &ev, now);
        if current.kind == RunKind::FollowUp
            && matches!(ev, RunEvent::SpeechEnded)
            && matches!(step.phase, Phase::Thinking(_))
        {
            // A follow-up counts as a run once it hears speech.
            self.shared.counters.runs.inc();
            local_guard(Guard::FollowUp);
        }
        if let RunEvent::Done { turn, spoke } = ev {
            current.done = true;
            current.spoke = spoke;
            if let Some(turn) = turn {
                self.local.conversation.push(turn, now);
            }
        }
        if let Some(current) = self.local.run.as_mut() {
            let limits = current.limits;
            match step.deadline {
                DeadlineRule::Keep => {}
                DeadlineRule::Think => current.rearm(now, limits.think_backstop()),
                DeadlineRule::Round => current.rearm(now, limits.round_backstop()),
                DeadlineRule::Reply => current.rearm(now, limits.reply_backstop()),
            }
        }
        self.phase = step.phase;
        for effect in step.effects {
            match effect {
                Effect::CloseMic => self.shared.mic.close(),
                Effect::EndOfSpeech => self.end_of_speech(now),
                Effect::Event(kind) => self.event("voice_assistant", kind).await,
                Effect::ErrorCue => self.error_feedback(),
                Effect::StartPlayer => self.start_reply_player(),
                Effect::Finish => self.finish_local().await,
                Effect::Fail(error) => self.fail_local(error).await,
                Effect::Guard(guard) => local_guard(guard),
            }
        }
        match &self.local.run {
            // A request that heard nothing ends without "finished".
            Some(_) if self.phase == Phase::Idle => {
                self.local.run = None;
                self.apply_deferred_mute().await;
            }
            // A spoken reply ends when both the run and its player are done.
            Some(run) if run.done && run.player_done => self.finish_local().await,
            _ => {}
        }
    }

    /// Fail the run whose deadline passed. The pipeline's own stage limits
    /// come first; this catches a stage that stalls anyway.
    pub(super) async fn tick_local(&mut self, now: Instant) {
        let Some(run) = self.local.run.as_ref().filter(|r| r.expired(now)) else {
            return;
        };
        let slot = match self.phase {
            Phase::Listening(_) => Slot::Stt,
            // The player has the reply channel: speech is late.
            Phase::Thinking(_) if run.source.is_none() => Slot::Tts,
            Phase::Speaking => Slot::Tts,
            _ => Slot::Chat,
        };
        tracing::warn!(
            "standalone {} {} passed its deadline",
            kind_label(&run.kind),
            run.id.0
        );
        let preset = self.slot_preset(slot);
        self.fail_local(ProviderError::new(slot, preset, Reason::Timeout))
            .await;
    }

    /// The preset serving `slot` in the current configuration.
    fn slot_preset(&self, slot: Slot) -> PresetId {
        let index = match slot {
            Slot::Stt => 0,
            Slot::Chat | Slot::Tool => 1,
            Slot::Tts => 2,
        };
        self.local
            .live
            .borrow()
            .pipeline
            .as_ref()
            .map_or(PresetId::Custom, |p| p.presets[index])
    }

    /// End the run with its failure: stop it and everything it plays, then
    /// tell the owner.
    pub(super) async fn fail_local(&mut self, error: ProviderError) {
        self.cancel_local();
        self.stop_playback();
        self.phase = Phase::Idle;
        self.event("voice_assistant", "stopped").await;
        tracing::warn!("standalone request failed: {error}");
        self.local.last_error = Some(error.to_string());
        self.error_feedback();
        self.apply_deferred_mute().await;
    }

    /// A device command from a run's tool. Only the current run may act; a
    /// mute it asks for waits for the run to end, so the reply confirming it
    /// still plays.
    pub(super) async fn run_tool(&mut self, run: RunId, cmd: ApiCommand) {
        if !self.local.run.as_ref().is_some_and(|r| r.id == run) {
            tracing::debug!("refused a device command from standalone run {}", run.0);
            cmd.refuse("The request was cancelled.");
            return;
        }
        match cmd {
            ApiCommand::SetMute(true, reply) => {
                tracing::info!("the microphone mutes when this request ends");
                self.local.pending_mute = true;
                let _ = reply.send(Ok(self.snapshot()));
            }
            cmd => self.api(cmd).await,
        }
    }

    /// The run is gone: mute the microphone if it asked to.
    pub(super) async fn apply_deferred_mute(&mut self) {
        if std::mem::take(&mut self.local.pending_mute) {
            tracing::info!("muting the microphone as the request asked");
            self.sw_muted = true;
            Box::pin(self.apply_mute()).await;
        }
    }

    /// Hand the reply channel to the player, which owns the voice lane until
    /// the reply has sounded. A chime still sounding stops.
    fn start_reply_player(&mut self) {
        let Some(source) = self.local.run.as_mut().and_then(|run| run.source.take()) else {
            return;
        };
        let token = self.begin_playback(PlaybackKind::Reply);
        let task = tokio::spawn(play_reply(
            self.speaker.clone(),
            source,
            token,
            self.self_tx.clone(),
        ));
        self.playback = Some((token, PlaybackKind::Reply, task));
    }

    /// The reply's first sentence sounds: it may speak for the speaking limit.
    pub(super) fn reply_started(&mut self, now: Instant) {
        if let Some(run) = self.local.run.as_mut() {
            let limit = run.limits.speaking;
            run.rearm(now, limit);
        }
    }

    /// The reply player ended: everything queued has sounded, or an item
    /// failed. A failure before the run is done fails the run.
    pub(super) async fn reply_done(&mut self, ok: bool) {
        let Some(run) = self.local.run.as_mut() else {
            return;
        };
        run.player_done = true;
        if run.done {
            self.finish_local().await;
        } else if !ok {
            let preset = self.slot_preset(Slot::Tts);
            self.fail_local(ProviderError::new(Slot::Tts, preset, Reason::Protocol))
                .await;
        }
    }

    /// The run is over and nothing of it is left to play. After a spoken
    /// reply the conversation may go on without the wake word.
    async fn finish_local(&mut self) {
        let Some(run) = self.local.run.take() else {
            return;
        };
        self.phase = Phase::Idle;
        self.event("voice_assistant", "finished").await;
        self.apply_deferred_mute().await;
        if self.follows_up(&run) {
            self.start_local_run(RunKind::FollowUp, None).await;
        }
    }

    /// Listen once more after a spoken reply to a request or follow-up:
    /// the owner left follow-ups on, the microphone is on, no alarm rings
    /// and standalone mode is still set up. There is no count cap; a window
    /// that hears nothing ends the conversation.
    fn follows_up(&self, run: &LocalRun) -> bool {
        run.spoke
            && matches!(run.kind, RunKind::Request | RunKind::FollowUp)
            && self.local.live.borrow().follow_ups
            && !self.muted()
            && self.ringing.is_none()
            && self.mode() == Mode::Standalone
            && self.assist_ready()
    }
}

/// Play a standalone reply: the one task that puts it on the voice lane, kept
/// in the Brain's playback slot. Reports [`Input::PlaybackStarted`] when the
/// first sentence sounds and [`Input::PlaybackDone`] once the run closed the
/// channel and the last item has sounded.
async fn play_reply(
    speaker: Option<Arc<Speaker>>,
    source: ReplySource,
    token: u64,
    tx: mpsc::Sender<Input>,
) {
    let ok = match speaker {
        Some(speaker) => {
            let ok = reply_in_turn(&speaker, source, token, &tx).await;
            if !ok {
                // What is still queued must not sound without its player.
                speaker.stop();
            }
            ok
        }
        None => {
            tracing::warn!("no speaker to play the standalone reply");
            false
        }
    };
    let _ = tx.send(Input::PlaybackDone { token, ok }).await;
}

/// A reply item queued on the voice lane.
struct Queued {
    kind: AudioKind,
    playback: VoicePlayback,
    sounding: bool,
}

enum Next {
    Received(Option<SpeechAudio>),
    Opened(pinna_audio::Result<VoicePlayback>),
    Started { sounded: bool },
    Ended(pinna_audio::Result<()>),
}

/// Queue each item behind the one before it, opening one at a time so the
/// lane keeps the run's order. An item returns its synthesis credit when it
/// starts to sound. False if an item failed.
async fn reply_in_turn(
    speaker: &Speaker,
    mut source: ReplySource,
    token: u64,
    tx: &mpsc::Sender<Input>,
) -> bool {
    let mut queued: VecDeque<Queued> = VecDeque::new();
    let mut opening: Option<(AudioKind, BoxFuture<'_, pinna_audio::Result<VoicePlayback>>)> = None;
    let mut closed = false;
    let mut announced = false;
    loop {
        if closed && opening.is_none() && queued.is_empty() {
            return true;
        }
        let receive = !closed && opening.is_none();
        let is_opening = opening.is_some();
        let front = queued.front_mut();
        let has_front = front.is_some();
        let next = tokio::select! {
            item = source.recv(), if receive => Next::Received(item),
            opened = async {
                opening.as_mut().expect("an item is opening").1.as_mut().await
            }, if is_opening => Next::Opened(opened),
            next = async {
                let item = front.expect("an item is queued");
                if item.sounding {
                    Next::Ended((&mut item.playback.done).await.unwrap_or_else(|_| {
                        Err(pinna_audio::Error::Other("speaker task ended".into()))
                    }))
                } else {
                    Next::Started {
                        sounded: (&mut item.playback.started).await.is_ok(),
                    }
                }
            }, if has_front => next,
        };
        match next {
            Next::Received(Some(audio)) => {
                opening = Some((audio.kind, open_reply_item(speaker, audio)));
            }
            Next::Received(None) => closed = true,
            Next::Opened(Ok(playback)) => {
                let (kind, _) = opening.take().expect("an item was opening");
                queued.push_back(Queued {
                    kind,
                    playback,
                    sounding: false,
                });
            }
            Next::Opened(Err(e)) => {
                tracing::warn!("standalone reply audio: {e}");
                return false;
            }
            Next::Started { sounded } => {
                let item = queued.front_mut().expect("an item is queued");
                item.sounding = true;
                if sounded && item.kind == AudioKind::Sentence && !announced {
                    announced = true;
                    let _ = tx.send(Input::PlaybackStarted { token }).await;
                }
                source.started(item.kind);
            }
            Next::Ended(Ok(())) => {
                queued.pop_front();
            }
            Next::Ended(Err(e)) => {
                tracing::warn!("standalone reply audio: {e}");
                return false;
            }
        }
    }
}

/// Queue one reply item on the voice lane; resolves at its first decoded audio.
fn open_reply_item(
    speaker: &Speaker,
    audio: SpeechAudio,
) -> BoxFuture<'_, pinna_audio::Result<VoicePlayback>> {
    let SpeechAudio { mime, body, .. } = audio;
    match body {
        AudioBody::Stream(body) => {
            Box::pin(async move { speaker.play_stream(mime.as_deref(), body).await })
        }
        AudioBody::File(path) => {
            Box::pin(async move { speaker.play_url(&path.to_string_lossy()).await })
        }
    }
}

/// A standalone guard fired: no speech, an invented transcript, and so on.
fn local_guard(guard: Guard) {
    tracing::debug!("standalone guard: {guard:?}");
}

#[derive(Clone, Debug, PartialEq)]
enum Effect {
    CloseMic,
    EndOfSpeech,
    Event(&'static str),
    ErrorCue,
    StartPlayer,
    Finish,
    Fail(ProviderError),
    Guard(Guard),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DeadlineRule {
    Keep,
    Think,
    Round,
    Reply,
}

#[derive(Debug, PartialEq)]
struct Step {
    phase: Phase,
    deadline: DeadlineRule,
    effects: Vec<Effect>,
}

fn unchanged(phase: Phase) -> Step {
    Step {
        phase,
        deadline: DeadlineRule::Keep,
        effects: Vec::new(),
    }
}

fn step(phase: Phase, kind: &RunKind, ev: &RunEvent, now: Instant) -> Step {
    match (phase, kind, ev) {
        (Phase::Listening(_), RunKind::Request | RunKind::FollowUp, RunEvent::SpeechEnded) => {
            Step {
                phase: Phase::Thinking(now),
                deadline: DeadlineRule::Think,
                effects: vec![
                    Effect::CloseMic,
                    Effect::EndOfSpeech,
                    Effect::Event("thinking"),
                ],
            }
        }
        (Phase::Listening(_), RunKind::Request, RunEvent::NoSpeech { invented }) => Step {
            phase: Phase::Idle,
            deadline: DeadlineRule::Keep,
            effects: vec![
                Effect::CloseMic,
                Effect::Guard(if *invented {
                    Guard::Invented
                } else {
                    Guard::NoSpeech
                }),
                Effect::Event("stopped"),
                Effect::ErrorCue,
            ],
        },
        (Phase::Listening(_), RunKind::FollowUp, RunEvent::NoSpeech { invented }) => Step {
            phase: Phase::Idle,
            deadline: DeadlineRule::Keep,
            effects: vec![
                Effect::CloseMic,
                Effect::Guard(if *invented {
                    Guard::Invented
                } else {
                    Guard::NoSpeech
                }),
                Effect::Finish,
            ],
        },
        (Phase::Thinking(_) | Phase::Speaking, _, RunEvent::Progress) => Step {
            phase,
            deadline: DeadlineRule::Round,
            effects: Vec::new(),
        },
        (Phase::Thinking(_), _, RunEvent::ReplyReady) => Step {
            phase,
            deadline: DeadlineRule::Reply,
            effects: vec![Effect::StartPlayer],
        },
        (phase, _, RunEvent::Done { spoke: false, .. }) if phase != Phase::Idle => Step {
            phase: Phase::Idle,
            deadline: DeadlineRule::Keep,
            effects: vec![Effect::Finish],
        },
        (phase, _, RunEvent::Done { spoke: true, .. }) => unchanged(phase),
        (phase, _, RunEvent::Failed(error)) if phase != Phase::Idle => Step {
            phase: Phase::Idle,
            deadline: DeadlineRule::Keep,
            effects: vec![Effect::Fail(error.clone())],
        },
        _ => unchanged(phase),
    }
}

pub(crate) trait RunSpawner: Send + Sync {
    fn spawn(&self, start: LocalStart) -> JoinHandle<()>;
}

pub(crate) struct NoopSpawner;

impl RunSpawner for NoopSpawner {
    fn spawn(&self, _start: LocalStart) -> JoinHandle<()> {
        tokio::spawn(std::future::pending())
    }
}

#[allow(dead_code)]
pub(crate) struct LocalStart {
    pub run: RunId,
    pub kind: RunKind,
    pub live: Arc<AssistLive>,
    pub history: Vec<Turn>,
    pub mic: Option<MicInput>,
    pub reply: ReplySink,
    pub device: DeviceFacts,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct DeviceFacts {
    pub name: String,
    pub time_zone: Option<String>,
    pub device_id: String,
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::AtomicBool;
    use std::time::{Duration, SystemTime};

    use pinna_assist::error::{Reason, Slot};
    use pinna_assist::reply::Credit;
    use pinna_audio::{Backend, PlaybackState, SpeakerConfig};
    use pinna_esphome::{DeviceEvent, EsphomeHandle, HaCommand};
    use pinna_hal::anim::Effect as LedEffect;
    use pinna_hal::buttons::{Button, ButtonEvent};
    use tokio::sync::oneshot;

    use super::*;
    use crate::leds::LedState;
    use crate::settings::{Mode, Settings};
    use crate::voice::{ApiCommand, ApiReply, Defaults, Input, Options, Shared, Snapshot};

    /// A run the fake spawner started: what the Brain handed it, and whether
    /// its task has been dropped.
    struct FakeRun {
        start: LocalStart,
        dropped: Arc<AtomicBool>,
    }

    /// Records each `LocalStart` and runs a task that never ends on its own.
    #[derive(Default)]
    struct FakeSpawner {
        runs: Mutex<Vec<FakeRun>>,
    }

    impl FakeSpawner {
        fn count(&self) -> usize {
            self.runs.lock().unwrap().len()
        }

        fn take(&self) -> FakeRun {
            self.runs.lock().unwrap().pop().expect("a run was started")
        }
    }

    impl RunSpawner for FakeSpawner {
        fn spawn(&self, start: LocalStart) -> JoinHandle<()> {
            let dropped = Arc::new(AtomicBool::new(false));
            // Captured, not created inside: a task aborted before its first
            // poll still drops it.
            let guard = DropFlag(dropped.clone());
            let task = tokio::spawn(async move {
                let _guard = guard;
                std::future::pending::<()>().await;
            });
            self.runs.lock().unwrap().push(FakeRun { start, dropped });
            task
        }
    }

    struct TestDevice {
        brain: Brain,
        shared: Arc<Shared>,
        events: mpsc::Receiver<DeviceEvent>,
        /// What the Brain sends itself (playback reports).
        inputs: mpsc::Receiver<Input>,
        leds: watch::Receiver<LedState>,
        live: watch::Sender<Arc<AssistLive>>,
        spawner: Arc<FakeSpawner>,
        dir: tempfile::TempDir,
    }

    impl TestDevice {
        fn new(mode: Mode, with_speaker: bool) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let (esp, events) = EsphomeHandle::detached(64);
            let shared = Arc::new(Shared::default());
            let speaker = with_speaker.then(|| {
                Arc::new(Speaker::new(SpeakerConfig::new(Backend::WavFile(
                    dir.path().join("output.wav"),
                ))))
            });
            let (leds_tx, leds) = watch::channel(LedState::default());
            let (self_tx, inputs) = mpsc::channel(64);
            let (live_tx, live) = watch::channel(Arc::new(AssistLive::empty()));
            let spawner = Arc::new(FakeSpawner::default());
            let brain = Brain::new_with_local(
                esp,
                speaker,
                shared.clone(),
                leds_tx,
                watch::channel(Snapshot::default()).0,
                self_tx,
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
                    },
                    mdns: None,
                    encrypted: false,
                    has_hw_privacy: false,
                    airplay: false,
                    bluetooth: false,
                    settings: Settings {
                        mode,
                        ..Settings::default()
                    },
                },
                LocalOptions {
                    live,
                    spawner: spawner.clone(),
                    timers_file: dir.path().join("timers.toml"),
                },
            );
            Self {
                brain,
                shared,
                events,
                inputs,
                leds,
                live: live_tx,
                spawner,
                dir,
            }
        }

        /// A standalone device whose three slots are checked.
        fn ready() -> Self {
            Self::ready_with(false)
        }

        fn ready_with(speaker: bool) -> Self {
            let device = Self::new(Mode::Standalone, speaker);
            device.set_live(AssistLive {
                ready: true,
                slots: [true; 3],
                label: "test brain".into(),
                ..AssistLive::empty()
            });
            device
        }

        fn set_live(&self, live: AssistLive) {
            self.live.send(Arc::new(live)).unwrap();
        }

        fn events(&mut self) -> Vec<DeviceEvent> {
            let mut events = Vec::new();
            while let Ok(event) = self.events.try_recv() {
                events.push(event);
            }
            events
        }

        fn overlay(&self) -> Option<LedEffect> {
            self.leds.borrow().overlay.map(|(effect, _)| effect)
        }

        async fn wake(&mut self) {
            self.brain
                .handle(Input::Wake {
                    id: "alexa_v0.1".into(),
                    score: 0.9,
                })
                .await;
        }

        async fn api(
            &mut self,
            cmd: impl FnOnce(ApiReply) -> ApiCommand,
        ) -> Result<Snapshot, String> {
            let (reply, result) = oneshot::channel();
            self.brain.handle(Input::Api(cmd(reply))).await;
            result.await.unwrap()
        }

        async fn assist(&mut self, run: RunId, ev: RunEvent) {
            self.brain.handle(Input::Assist { run, ev }).await;
        }

        /// The next report the Brain sent itself; it is not handled.
        async fn next_input(&mut self) -> Input {
            tokio::time::timeout(Duration::from_secs(5), self.inputs.recv())
                .await
                .expect("the playback reported in time")
                .expect("the Brain's sender is alive")
        }

        /// Hand the Brain its own reports until the playback is done.
        /// Returns how many times it said its first audio sounded.
        async fn play_out(&mut self) -> usize {
            let mut started = 0;
            loop {
                let input = self.next_input().await;
                let done = matches!(input, Input::PlaybackDone { .. });
                started += usize::from(matches!(input, Input::PlaybackStarted { .. }));
                self.brain.handle(input).await;
                if done {
                    return started;
                }
            }
        }

        /// A device command from a run's tool call.
        async fn run_tool(
            &mut self,
            run: RunId,
            cmd: impl FnOnce(ApiReply) -> ApiCommand,
        ) -> Result<Snapshot, String> {
            let (reply, result) = oneshot::channel();
            self.brain
                .handle(Input::RunTool {
                    run,
                    cmd: cmd(reply),
                })
                .await;
            result.await.unwrap()
        }

        fn run_id(&self) -> RunId {
            self.brain.local.run.as_ref().expect("a run is live").id
        }

        /// Start a request with the Action button and take what the run got.
        async fn request(&mut self) -> FakeRun {
            self.brain
                .handle(Input::Button(ButtonEvent::Press(Button::Action)))
                .await;
            assert!(matches!(self.brain.phase, Phase::Listening(_)));
            self.events();
            self.spawner.take()
        }
    }

    struct DropFlag(Arc<AtomicBool>);

    impl Drop for DropFlag {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    /// Wait for an aborted task to be dropped.
    async fn dropped(flag: &AtomicBool) -> bool {
        for _ in 0..100 {
            if flag.load(Ordering::SeqCst) {
                return true;
            }
            tokio::task::yield_now().await;
        }
        flag.load(Ordering::SeqCst)
    }

    fn mic_closed(run: &mut FakeRun) -> bool {
        let mic = run
            .start
            .mic
            .as_mut()
            .expect("a voice run has a microphone");
        mic.frames.try_recv() == Err(mpsc::error::TryRecvError::Disconnected)
    }

    fn insert_local_run(device: &mut TestDevice) -> Arc<AtomicBool> {
        let now = Instant::now();
        let (_sink, source) = reply_channel(2);
        let dropped = Arc::new(AtomicBool::new(false));
        let task = tokio::spawn({
            let dropped = dropped.clone();
            async move {
                let _guard = DropFlag(dropped);
                std::future::pending::<()>().await;
            }
        });
        device.brain.local.run = Some(LocalRun {
            id: RunId(1),
            kind: RunKind::Request,
            task: Some(task),
            deadline: now + Duration::from_secs(60),
            limits: RunLimits::MODEL,
            source: Some(source),
            done: false,
            spoke: false,
            player_done: false,
        });
        device.brain.phase = Phase::Thinking(now);
        dropped
    }

    fn voice_event(kind: &str) -> DeviceEvent {
        DeviceEvent::Event {
            object_id: "voice_assistant".into(),
            event_type: kind.into(),
        }
    }

    fn stopped() -> DeviceEvent {
        voice_event("stopped")
    }

    fn kinds() -> [RunKind; 3] {
        [
            RunKind::Request,
            RunKind::FollowUp,
            RunKind::Typed("question".into()),
        ]
    }

    fn failure() -> ProviderError {
        ProviderError::new(Slot::Chat, PresetId::OpenAi, Reason::Timeout)
    }

    #[test]
    fn speech_ended_closes_the_microphone_and_starts_thinking() {
        let now = Instant::now();
        for kind in &kinds()[..2] {
            assert_eq!(
                step(Phase::Listening(now), kind, &RunEvent::SpeechEnded, now),
                Step {
                    phase: Phase::Thinking(now),
                    deadline: DeadlineRule::Think,
                    effects: vec![
                        Effect::CloseMic,
                        Effect::EndOfSpeech,
                        Effect::Event("thinking"),
                    ],
                }
            );
        }
    }

    #[test]
    fn no_speech_after_a_request_has_the_home_assistant_error_feedback() {
        let now = Instant::now();
        for (invented, guard) in [(false, Guard::NoSpeech), (true, Guard::Invented)] {
            assert_eq!(
                step(
                    Phase::Listening(now),
                    &RunKind::Request,
                    &RunEvent::NoSpeech { invented },
                    now,
                ),
                Step {
                    phase: Phase::Idle,
                    deadline: DeadlineRule::Keep,
                    effects: vec![
                        Effect::CloseMic,
                        Effect::Guard(guard),
                        Effect::Event("stopped"),
                        Effect::ErrorCue,
                    ],
                }
            );
        }
    }

    #[test]
    fn no_speech_after_a_follow_up_finishes_quietly() {
        let now = Instant::now();
        for (invented, guard) in [(false, Guard::NoSpeech), (true, Guard::Invented)] {
            assert_eq!(
                step(
                    Phase::Listening(now),
                    &RunKind::FollowUp,
                    &RunEvent::NoSpeech { invented },
                    now,
                ),
                Step {
                    phase: Phase::Idle,
                    deadline: DeadlineRule::Keep,
                    effects: vec![Effect::CloseMic, Effect::Guard(guard), Effect::Finish],
                }
            );
        }
    }

    #[test]
    fn progress_rearms_the_current_round_while_thinking_or_speaking() {
        let now = Instant::now();
        for kind in &kinds() {
            for phase in [Phase::Thinking(now), Phase::Speaking] {
                assert_eq!(
                    step(phase, kind, &RunEvent::Progress, now),
                    Step {
                        phase,
                        deadline: DeadlineRule::Round,
                        effects: Vec::new(),
                    }
                );
            }
        }
    }

    #[test]
    fn reply_ready_starts_the_player_without_announcing_speech() {
        let now = Instant::now();
        for kind in &kinds() {
            assert_eq!(
                step(Phase::Thinking(now), kind, &RunEvent::ReplyReady, now),
                Step {
                    phase: Phase::Thinking(now),
                    deadline: DeadlineRule::Reply,
                    effects: vec![Effect::StartPlayer],
                }
            );
        }
    }

    #[test]
    fn done_finishes_at_once_only_when_nothing_was_spoken() {
        let now = Instant::now();
        for kind in &kinds() {
            assert_eq!(
                step(
                    Phase::Thinking(now),
                    kind,
                    &RunEvent::Done {
                        turn: None,
                        spoke: false,
                    },
                    now,
                ),
                Step {
                    phase: Phase::Idle,
                    deadline: DeadlineRule::Keep,
                    effects: vec![Effect::Finish],
                }
            );
            assert_eq!(
                step(
                    Phase::Speaking,
                    kind,
                    &RunEvent::Done {
                        turn: None,
                        spoke: true,
                    },
                    now,
                ),
                unchanged(Phase::Speaking)
            );
        }
    }

    #[test]
    fn failure_ends_every_active_run_with_its_provider_error() {
        let now = Instant::now();
        for kind in &kinds() {
            for phase in [
                Phase::Starting(now),
                Phase::Listening(now),
                Phase::Thinking(now),
                Phase::Speaking,
            ] {
                assert_eq!(
                    step(phase, kind, &RunEvent::Failed(failure()), now),
                    Step {
                        phase: Phase::Idle,
                        deadline: DeadlineRule::Keep,
                        effects: vec![Effect::Fail(failure())],
                    }
                );
            }
        }
    }

    #[test]
    fn events_from_the_wrong_phase_or_a_typed_listen_are_ignored() {
        let now = Instant::now();
        let listening = Phase::Listening(now);
        assert_eq!(
            step(
                listening,
                &RunKind::Typed("question".into()),
                &RunEvent::SpeechEnded,
                now
            ),
            unchanged(listening)
        );
        assert_eq!(
            step(
                listening,
                &RunKind::Typed("question".into()),
                &RunEvent::NoSpeech { invented: false },
                now,
            ),
            unchanged(listening)
        );
        for kind in &kinds() {
            assert_eq!(
                step(Phase::Thinking(now), kind, &RunEvent::SpeechEnded, now),
                unchanged(Phase::Thinking(now))
            );
            assert_eq!(
                step(listening, kind, &RunEvent::ReplyReady, now),
                unchanged(listening)
            );
            assert_eq!(
                step(listening, kind, &RunEvent::Progress, now),
                unchanged(listening)
            );
            for event in [
                RunEvent::SpeechEnded,
                RunEvent::NoSpeech { invented: false },
                RunEvent::Progress,
                RunEvent::ReplyReady,
                RunEvent::Done {
                    turn: None,
                    spoke: false,
                },
                RunEvent::Failed(failure()),
            ] {
                assert_eq!(step(Phase::Idle, kind, &event, now), unchanged(Phase::Idle));
            }
        }
    }

    #[test]
    fn transition_tests_use_real_instants_not_tokio_time() {
        let start = Instant::now();
        let later = start + Duration::from_secs(2);
        let Step { phase, .. } = step(
            Phase::Listening(start),
            &RunKind::Request,
            &RunEvent::SpeechEnded,
            later,
        );
        assert_eq!(phase, Phase::Thinking(later));
    }

    #[tokio::test]
    async fn aborting_a_local_run_stops_its_task_microphone_and_speaker_without_voice_stop() {
        let mut device = TestDevice::new(Mode::Standalone, true);
        let dropped = insert_local_run(&mut device);
        let (tx, mut mic) = mpsc::channel(1);
        device.shared.mic.open(tx);
        let speaker = device.brain.speaker.clone().unwrap();
        let playing = tokio::spawn({
            let speaker = speaker.clone();
            async move { speaker.play_pcm(tone::beep(440.0, 5_000, 0.5)).await }
        });
        let mut state = speaker.state();
        tokio::time::timeout(Duration::from_secs(2), async {
            while *state.borrow() != PlaybackState::Playing {
                state.changed().await.unwrap();
            }
        })
        .await
        .unwrap();

        let started = Instant::now();
        device.brain.abort_run(true).await;

        tokio::time::timeout(Duration::from_secs(1), playing)
            .await
            .expect("speaker stop did not cancel the clip")
            .unwrap()
            .unwrap();
        assert!(started.elapsed() < Duration::from_secs(1));
        tokio::task::yield_now().await;
        assert!(dropped.load(Ordering::SeqCst));
        assert_eq!(mic.try_recv(), Err(mpsc::error::TryRecvError::Disconnected));
        assert_eq!(device.events(), [stopped()]);
        assert!(device.brain.local.run.is_none());
        assert_eq!(device.brain.phase, Phase::Idle);
    }

    #[tokio::test]
    async fn aborting_a_home_assistant_run_still_sends_voice_stop() {
        let mut device = TestDevice::new(Mode::HomeAssistant, false);
        device.brain.phase = Phase::Listening(Instant::now());

        device.brain.abort_run(true).await;

        assert_eq!(device.events(), [stopped(), DeviceEvent::VoiceStop]);
        assert!(device.brain.stale_run);
        assert_eq!(device.brain.phase, Phase::Idle);
    }

    #[tokio::test]
    async fn the_last_home_assistant_disconnect_leaves_a_local_run_alone() {
        let mut device = TestDevice::new(Mode::Standalone, false);
        let dropped = insert_local_run(&mut device);
        device.brain.clients = 1;

        device.brain.ha(HaCommand::ClientDisconnected).await;

        assert_eq!(device.brain.clients, 0);
        assert!(device.brain.local.run.is_some());
        assert!(matches!(device.brain.phase, Phase::Thinking(_)));
        assert!(!dropped.load(Ordering::SeqCst));
        assert!(device.events().is_empty());
    }

    #[tokio::test]
    async fn home_assistant_time_limits_do_not_expire_a_local_run() {
        let mut device = TestDevice::new(Mode::Standalone, false);
        insert_local_run(&mut device);
        device.brain.phase =
            Phase::Thinking(Instant::now() - crate::voice::THINK_TIMEOUT - Duration::from_secs(1));

        device.brain.tick().await;

        assert!(device.brain.local.run.is_some());
        assert!(matches!(device.brain.phase, Phase::Thinking(_)));
        assert!(device.events().is_empty());
    }

    #[tokio::test]
    async fn a_standalone_wake_listens_locally_without_home_assistant() {
        let mut device = TestDevice::ready();
        device.shared.noise_energy.store(4_096, Ordering::Relaxed);

        device.wake().await;

        assert!(matches!(device.brain.phase, Phase::Listening(_)));
        let events = device.events();
        assert!(
            !events
                .iter()
                .any(|ev| matches!(ev, DeviceEvent::VoiceStart { .. }))
        );
        assert_eq!(events.last(), Some(&voice_event("listening")));
        let (mut seen, mut route) = (0, None);
        assert!(device.shared.mic.current(&mut seen, &mut route));
        assert!(route.is_some(), "the microphone goes to the run");

        let run = device.spawner.take();
        assert_eq!(run.start.kind, RunKind::Request);
        assert_eq!(run.start.run, device.brain.local.run.as_ref().unwrap().id);
        assert!(run.start.history.is_empty());
        assert_eq!(run.start.device.name, "Test");
        let mic = run.start.mic.as_ref().unwrap();
        assert!(!mic.follow_up);
        assert_eq!(mic.noise_seed, Some(4_096));
        assert_eq!(
            device.brain.snapshot().phase,
            pinna_api::Phase::PHASE_LISTENING
        );
    }

    #[tokio::test]
    async fn a_standalone_run_is_refused_when_not_ready_muted_or_ringing() {
        let mut device = TestDevice::new(Mode::Standalone, false);
        device.wake().await;
        assert_eq!(device.brain.phase, Phase::Idle);
        assert_eq!(device.spawner.count(), 0);
        assert_eq!(device.overlay(), Some(LedEffect::Error));
        assert!(device.api(ApiCommand::StartConversation).await.is_err());

        let mut device = TestDevice::ready();
        device.brain.sw_muted = true;
        device.brain.start_local_run(RunKind::Request, None).await;
        assert_eq!(device.brain.phase, Phase::Idle);
        assert_eq!(device.overlay(), Some(LedEffect::Error));

        let mut device = TestDevice::ready();
        device.brain.ringing = Some(("tea".into(), Instant::now()));
        device.brain.start_local_run(RunKind::Request, None).await;
        assert_eq!(device.brain.phase, Phase::Idle);
        assert_eq!(device.spawner.count(), 0);
        let (mut seen, mut route) = (0, None);
        device.shared.mic.current(&mut seen, &mut route);
        assert!(route.is_none(), "a refused run never opens the microphone");
    }

    #[derive(Clone, Copy, Debug)]
    enum StopPath {
        Wake,
        Action,
        Mute,
        StopConversation,
        CancelRequest,
        TimerFinish,
        MediaAnnounce,
    }

    async fn stop(device: &mut TestDevice, path: StopPath) {
        match path {
            StopPath::Wake => {
                // The detector runs once the run stops listening.
                device.brain.phase = Phase::Thinking(Instant::now());
                device.wake().await;
            }
            StopPath::Action => {
                device
                    .brain
                    .handle(Input::Button(ButtonEvent::Press(Button::Action)))
                    .await;
            }
            StopPath::Mute => device.brain.handle(Input::Privacy(true)).await,
            StopPath::StopConversation => {
                device.api(ApiCommand::StopConversation).await.unwrap();
            }
            StopPath::CancelRequest => {
                device
                    .brain
                    .handle(Input::Ha(HaCommand::Button {
                        object_id: "cancel_request".into(),
                    }))
                    .await;
            }
            StopPath::TimerFinish => {
                let (reply, id) = oneshot::channel();
                device
                    .brain
                    .handle(Input::Api(ApiCommand::SetTimer {
                        name: Some("Tea".into()),
                        seconds: 1,
                        reply,
                    }))
                    .await;
                id.await.unwrap().unwrap();
                device
                    .brain
                    .tick_timers(
                        Instant::now() + Duration::from_secs(2),
                        SystemTime::now(),
                        false,
                    )
                    .await;
                assert!(device.brain.ringing.is_some());
            }
            StopPath::MediaAnnounce => {
                device
                    .brain
                    .handle(Input::Ha(HaCommand::MediaPlayer {
                        command: None,
                        volume: None,
                        media_url: Some("http://ha.test/announce.mp3".into()),
                        announcement: true,
                    }))
                    .await;
            }
        }
    }

    #[tokio::test]
    async fn every_stop_path_drops_the_run_task_and_closes_the_microphone() {
        for path in [
            StopPath::Action,
            StopPath::Mute,
            StopPath::StopConversation,
            StopPath::CancelRequest,
            StopPath::TimerFinish,
            StopPath::MediaAnnounce,
            StopPath::Wake,
        ] {
            let mut device = TestDevice::ready();
            let mut run = device.request().await;

            stop(&mut device, path).await;

            assert!(
                dropped(&run.dropped).await,
                "{path:?} left the run task alive"
            );
            assert!(mic_closed(&mut run), "{path:?} left the microphone open");
            let events = device.events();
            assert!(events.contains(&stopped()), "{path:?}: {events:?}");
            assert!(!events.contains(&DeviceEvent::VoiceStop), "{path:?}");
            if let StopPath::Wake = path {
                // The wake word starts the next request at once.
                assert_eq!(device.spawner.count(), 1);
                assert!(matches!(device.brain.phase, Phase::Listening(_)));
            } else {
                assert_eq!(device.spawner.count(), 0, "{path:?}");
                assert!(device.brain.local.run.is_none(), "{path:?}");
                assert!(
                    !matches!(device.brain.phase, Phase::Listening(_) | Phase::Thinking(_)),
                    "{path:?}"
                );
            }
        }
    }

    fn turn(user: &str, assistant: &str) -> Turn {
        Turn {
            user: user.into(),
            assistant: assistant.into(),
        }
    }

    #[tokio::test]
    async fn speech_ended_starts_thinking_with_the_microphone_closed() {
        let mut device = TestDevice::ready();
        let mut run = device.request().await;

        device.assist(run.start.run, RunEvent::SpeechEnded).await;

        assert!(matches!(device.brain.phase, Phase::Thinking(_)));
        assert!(device.brain.speech_ended.is_some());
        assert!(mic_closed(&mut run));
        assert_eq!(device.events(), [voice_event("thinking")]);
        assert_eq!(
            device.brain.snapshot().phase,
            pinna_api::Phase::PHASE_THINKING
        );
    }

    #[tokio::test]
    async fn done_without_speech_finishes_and_keeps_the_turn() {
        let mut device = TestDevice::ready();
        let run = device.request().await;
        device.assist(run.start.run, RunEvent::SpeechEnded).await;
        device.events();

        device
            .assist(
                run.start.run,
                RunEvent::Done {
                    turn: Some(turn("Hello?", "")),
                    spoke: false,
                },
            )
            .await;

        assert_eq!(device.brain.phase, Phase::Idle);
        assert!(device.brain.local.run.is_none());
        assert_eq!(device.events(), [voice_event("finished")]);
        assert_eq!(device.overlay(), None);
        let next = device.request().await;
        assert_eq!(next.start.history, [turn("Hello?", "")]);
    }

    #[tokio::test]
    async fn a_request_that_hears_nothing_stops_with_the_error_cue() {
        let mut device = TestDevice::ready();
        let mut run = device.request().await;

        device
            .assist(run.start.run, RunEvent::NoSpeech { invented: false })
            .await;

        assert_eq!(device.brain.phase, Phase::Idle);
        assert!(device.brain.local.run.is_none());
        assert!(mic_closed(&mut run));
        assert_eq!(device.events(), [stopped()]);
        assert_eq!(device.overlay(), Some(LedEffect::Error));
    }

    #[tokio::test]
    async fn a_failure_stops_the_run_and_shows_its_error() {
        let mut device = TestDevice::ready();
        let run = device.request().await;
        device.assist(run.start.run, RunEvent::SpeechEnded).await;
        device.events();

        device
            .assist(run.start.run, RunEvent::Failed(failure()))
            .await;

        assert_eq!(device.brain.phase, Phase::Idle);
        assert!(device.brain.local.run.is_none());
        assert!(dropped(&run.dropped).await);
        assert_eq!(device.events(), [stopped()]);
        assert_eq!(device.overlay(), Some(LedEffect::Error));
        let error = "brain via openai: request timed out";
        assert_eq!(
            device.brain.snapshot().assist.last_error.as_deref(),
            Some(error)
        );
    }

    #[tokio::test]
    async fn events_from_a_stale_run_are_ignored() {
        let mut device = TestDevice::ready();
        let first = device.request().await;
        device.api(ApiCommand::StopConversation).await.unwrap();
        let second = device.request().await;
        assert_ne!(first.start.run, second.start.run);

        for ev in [
            RunEvent::SpeechEnded,
            RunEvent::Failed(failure()),
            RunEvent::Done {
                turn: Some(turn("Old question", "Old answer")),
                spoke: false,
            },
        ] {
            device.assist(first.start.run, ev).await;
        }

        assert!(matches!(device.brain.phase, Phase::Listening(_)));
        assert_eq!(device.run_id(), second.start.run);
        assert!(device.events().is_empty());
        assert_eq!(device.brain.snapshot().assist.last_error, None);
        // The stale run's turn never reached the conversation.
        assert!(
            device
                .brain
                .local
                .conversation
                .history(Instant::now())
                .is_empty()
        );
    }

    #[tokio::test]
    async fn the_deadline_is_rearmed_by_events_and_fails_a_stalled_run() {
        let mut device = TestDevice::ready();
        let run = device.request().await;
        let started = Instant::now();
        // Listening is bounded by the recording limit and a margin.
        device
            .brain
            .tick_local(started + Duration::from_secs(10))
            .await;
        assert!(device.brain.local.run.is_some());

        device.assist(run.start.run, RunEvent::SpeechEnded).await;
        // Thinking may take speech-to-text plus the first text.
        device
            .brain
            .tick_local(started + Duration::from_secs(40))
            .await;
        assert!(device.brain.local.run.is_some());
        device.events();

        device
            .brain
            .tick_local(started + Duration::from_secs(60))
            .await;

        assert_eq!(device.brain.phase, Phase::Idle);
        assert!(device.brain.local.run.is_none());
        assert!(dropped(&run.dropped).await);
        assert_eq!(device.events(), [stopped()]);
        assert_eq!(device.overlay(), Some(LedEffect::Error));
        let error = "brain via custom: request timed out";
        assert_eq!(
            device.brain.snapshot().assist.last_error.as_deref(),
            Some(error)
        );
    }

    /// 16-bit mono 16 kHz WAV: the header for `samples` samples.
    fn wav_header(samples: usize) -> Vec<u8> {
        let data_len = (samples * 2) as u32;
        let mut bytes = Vec::with_capacity(44);
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&16_000u32.to_le_bytes());
        bytes.extend_from_slice(&32_000u32.to_le_bytes());
        bytes.extend_from_slice(&2u16.to_le_bytes());
        bytes.extend_from_slice(&16u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&data_len.to_le_bytes());
        bytes
    }

    fn samples(ms: usize) -> Vec<u8> {
        (0..ms * 16).flat_map(|_| 1_000i16.to_le_bytes()).collect()
    }

    fn wav(ms: usize) -> Vec<u8> {
        let mut bytes = wav_header(ms * 16);
        bytes.extend(samples(ms));
        bytes
    }

    fn sentence(
        body: futures::stream::BoxStream<'static, std::io::Result<bytes::Bytes>>,
    ) -> SpeechAudio {
        SpeechAudio {
            kind: AudioKind::Sentence,
            mime: Some("audio/wav".into()),
            body: AudioBody::Stream(body),
        }
    }

    /// A sentence synthesised in one piece.
    fn short_sentence(ms: usize) -> SpeechAudio {
        use futures::StreamExt;
        sentence(futures::stream::iter([Ok(bytes::Bytes::from(wav(ms)))]).boxed())
    }

    /// A sentence whose first `ms` arrive at once and whose rest waits for
    /// `gate`: it sounds, then can't finish until the gate opens.
    fn gated_sentence(ms: usize, gate: oneshot::Receiver<()>) -> SpeechAudio {
        use futures::StreamExt;
        let mut first = wav_header(ms * 16 * 2);
        first.extend(samples(ms));
        let first = futures::stream::iter([Ok(bytes::Bytes::from(first))]);
        let rest = futures::stream::once(async move {
            let _ = gate.await;
            Ok(bytes::Bytes::from(samples(ms)))
        });
        sentence(first.chain(rest).boxed())
    }

    /// A ready device with a speaker, thinking about its request. Follow-ups
    /// are off, so a reply's end is the end of the conversation.
    async fn thinking_with_speaker() -> (TestDevice, RunId, ReplySink) {
        let mut device = TestDevice::ready_with(true);
        device.set_live(AssistLive {
            ready: true,
            slots: [true; 3],
            follow_ups: false,
            ..AssistLive::empty()
        });
        let FakeRun { start, .. } = device.request().await;
        let LocalStart { run, reply, .. } = start;
        device.assist(run, RunEvent::SpeechEnded).await;
        device.events();
        (device, run, reply)
    }

    async fn pending_credit(sink: &ReplySink) -> tokio::task::JoinHandle<Credit> {
        let sink = sink.clone();
        let credit = tokio::spawn(async move { sink.credit().await.unwrap() });
        tokio::task::yield_now().await;
        credit
    }

    #[tokio::test]
    async fn a_reply_plays_in_order_with_two_sentences_ahead_at_most() {
        let (mut device, run, sink) = thinking_with_speaker().await;
        let (open_gate, gate) = oneshot::channel();
        for audio in [gated_sentence(300, gate), short_sentence(100)] {
            let credit = sink.credit().await.unwrap();
            sink.send(audio, Some(credit)).await.unwrap();
        }
        let third = pending_credit(&sink).await;
        assert!(
            !third.is_finished(),
            "a third sentence waits for the first to sound"
        );

        device.assist(run, RunEvent::ReplyReady).await;
        assert!(matches!(
            device.brain.playback,
            Some((_, PlaybackKind::Reply, _))
        ));
        let input = device.next_input().await;
        assert!(matches!(input, Input::PlaybackStarted { .. }), "{input:?}");
        device.brain.handle(input).await;
        assert_eq!(device.brain.phase, Phase::Speaking);
        assert_eq!(device.events(), [voice_event("replying")]);

        // The first sentence sounds, so the third may be synthesised.
        let third = tokio::time::timeout(Duration::from_secs(5), third)
            .await
            .unwrap()
            .unwrap();
        sink.send(short_sentence(100), Some(third)).await.unwrap();
        // The second can't sound before the first, which waits for its gate.
        let fourth = pending_credit(&sink).await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(
            !fourth.is_finished(),
            "a fourth sentence waits for the second to sound"
        );

        open_gate.send(()).unwrap();
        let fourth = tokio::time::timeout(Duration::from_secs(5), fourth)
            .await
            .unwrap()
            .unwrap();
        drop(fourth);
        drop(sink);
        device
            .assist(
                run,
                RunEvent::Done {
                    turn: Some(turn("Question", "One. Two. Three.")),
                    spoke: true,
                },
            )
            .await;
        assert_eq!(
            device.brain.phase,
            Phase::Speaking,
            "the reply is still sounding"
        );

        let before = Instant::now();
        assert_eq!(
            device.play_out().await,
            0,
            "the first audio is reported once"
        );
        assert_eq!(device.brain.phase, Phase::Idle);
        assert!(device.brain.local.run.is_none());
        assert!(device.brain.playback.is_none());
        assert!(device.brain.speaker_quiet_since >= before);
        assert_eq!(device.events(), [voice_event("finished")]);
    }

    #[tokio::test]
    async fn an_acknowledgment_is_never_the_first_audio() {
        let (mut device, run, sink) = thinking_with_speaker().await;
        let ack = device.dir.path().join("ack.wav");
        std::fs::write(&ack, wav(100)).unwrap();
        let ack = || SpeechAudio {
            kind: AudioKind::Ack,
            mime: None,
            body: AudioBody::File(ack.clone()),
        };
        sink.send(ack(), None).await.unwrap();
        device.assist(run, RunEvent::ReplyReady).await;
        drop(sink);

        assert_eq!(device.play_out().await, 0);
        assert!(matches!(device.brain.phase, Phase::Thinking(_)));
        assert!(device.events().is_empty());

        // An acknowledgment and then a sentence: the sentence is the first audio.
        let (mut device, run, sink) = thinking_with_speaker().await;
        sink.send(ack(), None).await.unwrap();
        let credit = sink.credit().await.unwrap();
        sink.send(short_sentence(100), Some(credit)).await.unwrap();
        device.assist(run, RunEvent::ReplyReady).await;
        drop(sink);
        device
            .assist(
                run,
                RunEvent::Done {
                    turn: Some(turn("Question", "Answer.")),
                    spoke: true,
                },
            )
            .await;

        assert_eq!(device.play_out().await, 1);
        assert_eq!(
            device.events(),
            [voice_event("replying"), voice_event("finished")]
        );
        assert_eq!(device.brain.phase, Phase::Idle);
    }

    #[tokio::test]
    async fn aborting_the_run_flushes_its_reply() {
        let (mut device, run, sink) = thinking_with_speaker().await;
        let (_gate_open, gate) = oneshot::channel();
        let credit = sink.credit().await.unwrap();
        sink.send(gated_sentence(300, gate), Some(credit))
            .await
            .unwrap();
        device.assist(run, RunEvent::ReplyReady).await;
        let input = device.next_input().await;
        device.brain.handle(input).await;
        assert_eq!(device.brain.phase, Phase::Speaking);
        let speaker = device.brain.speaker.clone().unwrap();
        assert!(speaker.is_playing());

        device.api(ApiCommand::StopConversation).await.unwrap();

        assert_eq!(device.brain.phase, Phase::Idle);
        assert!(device.brain.playback.is_none());
        assert!(device.brain.local.run.is_none());
        let mut state = speaker.state();
        tokio::time::timeout(Duration::from_secs(2), async {
            while *state.borrow() != PlaybackState::Idle {
                state.changed().await.unwrap();
            }
        })
        .await
        .expect("the voice lane was flushed");
        // The player is gone: its sink sees the reply channel closed.
        assert!(sink.credit().await.is_err());
    }

    #[tokio::test]
    async fn a_reply_that_fails_to_play_before_done_fails_the_run() {
        use futures::StreamExt;
        let (mut device, run, sink) = thinking_with_speaker().await;
        let garbage = futures::stream::iter([Ok(bytes::Bytes::from_static(b"not audio at all"))]);
        let credit = sink.credit().await.unwrap();
        sink.send(sentence(garbage.boxed()), Some(credit))
            .await
            .unwrap();
        device.assist(run, RunEvent::ReplyReady).await;

        assert_eq!(device.play_out().await, 0);

        assert_eq!(device.brain.phase, Phase::Idle);
        assert!(device.brain.local.run.is_none());
        assert_eq!(device.events(), [stopped()]);
        assert_eq!(device.overlay(), Some(LedEffect::Error));
        let error = "tts via custom: invalid provider response";
        assert_eq!(
            device.brain.snapshot().assist.last_error.as_deref(),
            Some(error)
        );
    }

    /// Play a reply without a speaker: the Brain sees its player start and end.
    async fn speak_reply(device: &mut TestDevice, run: RunId, spoke: bool) {
        device.assist(run, RunEvent::ReplyReady).await;
        let token = match &device.brain.playback {
            Some((token, PlaybackKind::Reply, _)) => *token,
            other => panic!("no reply player: {other:?}"),
        };
        device.brain.handle(Input::PlaybackStarted { token }).await;
        device
            .brain
            .handle(Input::PlaybackDone { token, ok: true })
            .await;
        device
            .assist(
                run,
                RunEvent::Done {
                    turn: Some(turn("Question", "Answer?")),
                    spoke,
                },
            )
            .await;
    }

    /// A request that is speaking its reply, without a speaker.
    async fn replying(device: &mut TestDevice) -> RunId {
        let run = device.request().await.start.run;
        device.assist(run, RunEvent::SpeechEnded).await;
        run
    }

    #[tokio::test]
    async fn a_spoken_reply_opens_a_follow_up_with_the_same_noise_seed() {
        let mut device = TestDevice::ready();
        device.brain.settings.wake_sound = Some(true);
        device.shared.noise_energy.store(4_096, Ordering::Relaxed);
        let run = device.request().await.start.run;
        assert!(matches!(
            device.brain.playback,
            Some((_, PlaybackKind::Cue, _))
        ));
        device.assist(run, RunEvent::SpeechEnded).await;
        // The reply's tail must not become the follow-up's noise floor.
        device.shared.noise_energy.store(60_000, Ordering::Relaxed);
        device.events();

        speak_reply(&mut device, run, true).await;

        assert_eq!(
            device.events(),
            [
                voice_event("replying"),
                voice_event("finished"),
                voice_event("listening"),
            ]
        );
        assert!(matches!(device.brain.phase, Phase::Listening(_)));
        assert!(
            device.brain.playback.is_none(),
            "a follow-up plays no chime"
        );
        assert_eq!(device.shared.counters.runs.get(), 1);
        let follow_up = device.spawner.take();
        assert_eq!(follow_up.start.kind, RunKind::FollowUp);
        assert_eq!(follow_up.start.history, [turn("Question", "Answer?")]);
        let mic = follow_up.start.mic.as_ref().unwrap();
        assert!(mic.follow_up);
        assert_eq!(mic.noise_seed, Some(4_096));

        // Speech in the window turns it into a counted request.
        device
            .assist(follow_up.start.run, RunEvent::SpeechEnded)
            .await;
        assert!(matches!(device.brain.phase, Phase::Thinking(_)));
        assert_eq!(device.shared.counters.runs.get(), 2);
    }

    #[tokio::test]
    async fn a_follow_up_that_hears_nothing_ends_quietly() {
        let mut device = TestDevice::ready();
        let run = replying(&mut device).await;
        speak_reply(&mut device, run, true).await;
        let mut follow_up = device.spawner.take();
        device.events();

        device
            .assist(follow_up.start.run, RunEvent::NoSpeech { invented: true })
            .await;

        assert_eq!(device.brain.phase, Phase::Idle);
        assert!(device.brain.local.run.is_none());
        assert!(mic_closed(&mut follow_up));
        assert_eq!(device.events(), [voice_event("finished")]);
        assert_eq!(device.overlay(), None);
        assert_eq!(device.spawner.count(), 0, "silence ends the conversation");
    }

    #[tokio::test]
    async fn no_follow_up_when_muted_ringing_switched_off_or_silent() {
        for case in ["muted", "ringing", "off", "silent"] {
            let mut device = TestDevice::ready();
            let run = replying(&mut device).await;
            match case {
                "muted" => device.brain.hw_muted = true,
                "ringing" => device.brain.ringing = Some(("tea".into(), Instant::now())),
                "off" => device.set_live(AssistLive {
                    ready: true,
                    slots: [true; 3],
                    follow_ups: false,
                    ..AssistLive::empty()
                }),
                _ => {}
            }

            speak_reply(&mut device, run, case != "silent").await;

            assert_eq!(device.spawner.count(), 0, "{case}");
            assert!(device.brain.local.run.is_none(), "{case}");
            assert_eq!(device.brain.phase, Phase::Idle, "{case}");
        }
    }

    fn volume_event(value: f32) -> DeviceEvent {
        DeviceEvent::NumberState {
            object_id: "volume".into(),
            value,
        }
    }

    #[tokio::test]
    async fn only_the_current_run_changes_the_device() {
        let mut device = TestDevice::ready();
        let stale = device.request().await.start.run;
        device.api(ApiCommand::StopConversation).await.unwrap();
        let current = device.request().await.start.run;
        let volume = device.brain.settings.volume;

        let refused = device
            .run_tool(stale, |reply| ApiCommand::SetVolume(80, reply))
            .await;

        assert_eq!(refused.unwrap_err(), "The request was cancelled.");
        assert_eq!(device.brain.settings.volume, volume);
        assert!(device.events().is_empty());

        let applied = device
            .run_tool(current, |reply| ApiCommand::SetVolume(80, reply))
            .await
            .unwrap();

        assert_eq!(applied.volume, 80);
        assert_eq!(device.events().first(), Some(&volume_event(80.0)));
    }

    #[tokio::test]
    async fn a_mute_the_run_asks_for_waits_until_it_is_done() {
        let mut device = TestDevice::ready();
        let run = replying(&mut device).await;
        device.events();

        let snapshot = device
            .run_tool(run, |reply| ApiCommand::SetMute(true, reply))
            .await
            .unwrap();

        assert!(!snapshot.sw_muted, "the confirming reply still plays");
        assert!(device.brain.local.run.is_some());
        assert!(device.events().is_empty());

        speak_reply(&mut device, run, true).await;

        assert!(device.brain.muted());
        assert!(device.brain.settings.muted);
        assert!(device.shared.muted.load(Ordering::SeqCst));
        assert_eq!(
            device.events(),
            [
                voice_event("replying"),
                voice_event("finished"),
                DeviceEvent::MuteState(true),
            ]
        );
        assert_eq!(
            device.spawner.count(),
            0,
            "a muted microphone hears no follow-up"
        );
        assert!(!device.brain.local.pending_mute);
    }

    #[tokio::test]
    async fn a_mute_the_run_asked_for_applies_when_it_is_cancelled() {
        let mut device = TestDevice::ready();
        let run = replying(&mut device).await;
        device
            .run_tool(run, |reply| ApiCommand::SetMute(true, reply))
            .await
            .unwrap();
        device.events();

        device.api(ApiCommand::StopConversation).await.unwrap();

        assert!(device.brain.muted());
        assert_eq!(device.events(), [stopped(), DeviceEvent::MuteState(true)]);
        assert!(!device.brain.local.pending_mute);
        // The next request is refused: the microphone is muted.
        device.brain.start_local_run(RunKind::Request, None).await;
        assert_eq!(device.spawner.count(), 0);
    }
}
