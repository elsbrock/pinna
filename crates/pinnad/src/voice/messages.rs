//! Messages from the other members of the satellite group (ADR 0026). The
//! receiver says each one in its own voice: at once when it is free, or
//! later, when it is free again.
//!
//! Held messages stay in memory only: at most [`MAX_HELD`], each forgotten
//! [`Times::keep`] after it arrived, and all of them when the satellite
//! switches to Home Assistant mode or *Satellite group* goes off. They have
//! no ring of their own, and Home Assistant hears nothing of them. A
//! message's text never reaches a log line: only its sender and its length.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

use pinna_assist::pipeline::Speech;
use pinna_audio::Speaker;
use tokio::sync::{mpsc, oneshot};

use super::local::{SpeakReply, say};
use super::{Brain, Input, Phase, PlaybackKind};
use crate::metrics::StandaloneCounters;
use crate::settings::Mode;

/// Held messages kept at most. Another one drops the oldest of the others.
pub(super) const MAX_HELD: usize = 5;

/// A message from another member, for the Brain to take.
pub struct Delivery {
    /// The sender's name in this satellite's member list.
    pub from: String,
    /// Already checked: trimmed, 1 to 500 characters.
    pub text: String,
    /// What the Brain did with it, once it has taken it.
    pub reply: oneshot::Sender<Taken>,
}

impl std::fmt::Debug for Delivery {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // A message's text never reaches a log line.
        f.debug_struct("Delivery")
            .field("from", &self.from)
            .field("characters", &self.text.chars().count())
            .finish_non_exhaustive()
    }
}

/// What the Brain did with a message, before it plays.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Taken {
    /// It plays now.
    Played,
    /// It plays once the satellite is free.
    Held,
    /// It won't play; one sentence says why.
    Refused(String),
}

/// Why a satellite named `name` takes no messages: it is in Home Assistant
/// mode. The group works in standalone mode only.
pub fn in_home_assistant_mode(name: &str) -> String {
    format!("{name} is in Home Assistant mode and takes no messages.")
}

/// Why a satellite named `name` takes no messages: a switch to Home
/// Assistant mode waits for it to be idle.
pub fn switching_to_home_assistant_mode(name: &str) -> String {
    format!("{name} is switching to Home Assistant mode and takes no messages.")
}

/// Why a satellite named `name` takes no messages: its owner said so.
pub fn switched_off(name: &str) -> String {
    format!("{name} has Group messages switched off.")
}

/// Why a satellite named `name` takes no messages: an unchecked voice isn't
/// enough, unlike for `speak`, since nobody waits at the receiver to hear
/// that it failed.
pub fn no_voice(name: &str) -> String {
    format!("{name} can't say messages: its text-to-speech hasn't passed its check.")
}

/// Why a satellite named `name` takes no messages: it has nothing to say
/// them with.
pub fn no_speaker(name: &str) -> String {
    format!("{name} has no working speaker, so it can't say messages.")
}

/// How long a held message waits, so tests can pass other times.
#[derive(Clone, Copy, Debug)]
pub(super) struct Times {
    /// After its speech failed, a message isn't tried again before this.
    pub retry: Duration,
    /// A held message is forgotten this long after it arrived.
    pub keep: Duration,
}

impl Default for Times {
    fn default() -> Self {
        Self {
            retry: Duration::from_secs(30),
            keep: Duration::from_secs(12 * 60 * 60),
        }
    }
}

/// A message taken, held or playing. Not `Debug`: its text stays out of
/// every log line.
pub(super) struct Message {
    id: u64,
    pub(super) from: String,
    text: String,
    arrived: Instant,
    /// Its speech failed once already.
    failed: bool,
    /// After its speech failed: not tried again before this.
    not_before: Option<Instant>,
}

impl Message {
    fn characters(&self) -> usize {
        self.text.chars().count()
    }
}

/// The Brain's messages: those held, oldest first, and the one playing.
#[derive(Default)]
pub(super) struct Messages {
    pub(super) held: VecDeque<Message>,
    pub(super) playing: Option<Message>,
    next_id: u64,
    pub(super) times: Times,
}

impl Messages {
    /// How many wait to be said.
    pub(super) fn held(&self) -> usize {
        self.held.len()
    }

    /// Hold `message` at the front or at the back. Beyond [`MAX_HELD`], the
    /// oldest of the others goes.
    fn hold(&mut self, message: Message, front: bool) {
        let id = message.id;
        if front {
            self.held.push_front(message);
        } else {
            self.held.push_back(message);
        }
        while self.held.len() > MAX_HELD {
            let Some(oldest) = self
                .held
                .iter()
                .enumerate()
                .filter(|(_, m)| m.id != id)
                .min_by_key(|(_, m)| m.arrived)
                .map(|(i, _)| i)
            else {
                break;
            };
            if let Some(dropped) = self.held.remove(oldest) {
                tracing::info!(
                    "message from {} dropped: {MAX_HELD} newer ones are held",
                    dropped.from
                );
            }
        }
    }

    /// The first held message that may play at `now`: one whose speech
    /// failed waits out its retry.
    fn next(&mut self, now: Instant) -> Option<Message> {
        let i = self
            .held
            .iter()
            .position(|m| m.not_before.is_none_or(|t| t <= now))?;
        self.held.remove(i)
    }

    /// Forget the held messages that arrived [`Times::keep`] ago or more.
    fn expire(&mut self, now: Instant) {
        let keep = self.times.keep;
        self.held.retain(|m| {
            let kept = now.saturating_duration_since(m.arrived) < keep;
            if !kept {
                tracing::info!(
                    "message from {} forgotten: it was held for {} hours",
                    m.from,
                    keep.as_secs() / 3600
                );
            }
            kept
        });
    }

    /// Forget every message. The one playing plays to its end, but isn't
    /// held again.
    fn forget_all(&mut self, why: &str) {
        let count = self.held.len() + usize::from(self.playing.is_some());
        if count > 0 {
            tracing::info!("{count} group messages dropped: {why}");
        }
        self.held.clear();
        self.playing = None;
    }
}

impl Brain {
    /// A message from another member. It is refused, or held, and the
    /// answer says whether it plays now; the sender hears it before the
    /// message plays.
    pub(super) fn take_message(&mut self, delivery: Delivery, now: Instant) {
        let Delivery { from, text, reply } = delivery;
        let characters = text.chars().count();
        if reply.is_closed() {
            // The PeerService stopped waiting (its limit, or the caller went
            // away) and told the sender it wasn't taken. Saying it now would
            // contradict that, and the retry the sender was invited to make
            // would say it twice.
            tracing::info!(
                "message from {from}, {characters} characters: dropped, the sender stopped waiting"
            );
            return;
        }
        if let Some(why) = self.refuse_message() {
            tracing::info!("message from {from}, {characters} characters: refused: {why}");
            let _ = reply.send(Taken::Refused(why));
            return;
        }
        self.messages.expire(now);
        let id = self.messages.next_id;
        self.messages.next_id += 1;
        let message = Message {
            id,
            from: from.clone(),
            text,
            arrived: now,
            failed: false,
            not_before: None,
        };
        self.messages.hold(message, false);
        self.play_held(now);
        let played = self.messages.playing.as_ref().is_some_and(|m| m.id == id);
        tracing::info!(
            "message from {from}, {characters} characters: {}",
            if played { "plays now" } else { "held" }
        );
        let _ = reply.send(if played { Taken::Played } else { Taken::Held });
    }

    /// Why this satellite can't take a message; `None` when it can. The
    /// PeerService checks only the mode, Group messages and the
    /// text-to-speech slot before it asks, from a snapshot that may be a
    /// moment old: only the Brain knows of a switch waiting for the
    /// satellite to be idle, and of the speaker.
    fn refuse_message(&self) -> Option<String> {
        let name = self.friendly_name();
        let live = self.local.live();
        if self.mode() != Mode::Standalone {
            Some(in_home_assistant_mode(&name))
        } else if self.pending_mode == Some(Mode::HomeAssistant) {
            // The switch drops every held message, so one taken now would
            // never play.
            Some(switching_to_home_assistant_mode(&name))
        } else if !self.settings.group_messages() {
            Some(switched_off(&name))
        } else if !live.slots.tts || live.speech.is_none() {
            Some(no_voice(&name))
        } else if self.speaker.is_none() {
            Some(no_speaker(&name))
        } else {
            None
        }
    }

    /// A message may play: no request runs (its follow-up included), no
    /// alarm rings, nothing plays on the voice lane, no calibration runs,
    /// and the microphones are on. Muted is the owner saying "not now"
    /// (ADR 0017). Music may play: the voice lane ducks it. A mode switch
    /// waiting for the satellite to be idle goes first, or a row of held
    /// messages would keep it waiting.
    fn free_for_a_message(&self) -> bool {
        self.phase == Phase::Idle
            && self.local.run.is_none()
            && self.ringing.is_none()
            && self.calibration.is_none()
            && self.playback.is_none()
            && !self.muted()
            && self.pending_mode.is_none()
    }

    /// Say the oldest held message that may play, when the satellite is
    /// free.
    fn play_held(&mut self, now: Instant) {
        if !self.free_for_a_message() {
            return;
        }
        if let Some(message) = self.messages.next(now) {
            self.play_message(message, now);
        }
    }

    fn play_message(&mut self, message: Message, now: Instant) {
        let live = self.local.live();
        let (speech, language) = (live.speech.clone(), live.language.clone());
        let (Some(speaker), Some(speech)) = (self.speaker.clone(), speech) else {
            // Taken with a voice that is gone since: as if its speech failed.
            self.messages.playing = Some(message);
            self.message_failed(now);
            return;
        };
        tracing::info!(
            "saying a message from {}, {} characters",
            message.from,
            message.characters()
        );
        let said =
            crate::standalone::ack::message(language.as_deref(), &message.from, &message.text);
        let token = self.begin_playback(PlaybackKind::Message);
        let task = tokio::spawn(say_message(
            speaker,
            speech,
            said,
            token,
            self.self_tx.clone(),
            self.shared.counters.standalone.clone(),
        ));
        self.messages.playing = Some(message);
        self.playback = Some((token, PlaybackKind::Message, task));
    }

    /// The message's playback ended: said, or its speech failed.
    pub(super) fn message_done(&mut self, ok: bool, now: Instant) {
        if !ok {
            self.message_failed(now);
        } else if let Some(message) = self.messages.playing.take() {
            tracing::info!("said the message from {}", message.from);
        }
    }

    /// Its speech failed: held again once, at the front, and tried no
    /// sooner than [`Times::retry`] later. A second failure drops it.
    fn message_failed(&mut self, now: Instant) {
        let Some(mut message) = self.messages.playing.take() else {
            return;
        };
        if message.failed {
            tracing::info!(
                "message from {} dropped: its speech failed twice",
                message.from
            );
            return;
        }
        tracing::info!(
            "message from {}: its speech failed; held to try again in {} s",
            message.from,
            self.messages.times.retry.as_secs()
        );
        message.failed = true;
        message.not_before = Some(now + self.messages.times.retry);
        self.messages.hold(message, true);
    }

    /// Something else took the voice lane from the message playing: the
    /// wake word, an alarm, an announcement or the calibration. It is held
    /// again, at the front, and plays from its start once the satellite is
    /// free. No failure counts.
    pub(super) fn message_cut(&mut self) {
        if let Some(message) = self.messages.playing.take() {
            tracing::info!("message from {} held again", message.from);
            self.messages.hold(message, true);
        }
    }

    /// A person stopped the message playing, with `by`: it is dropped, as
    /// a reply is.
    pub(super) fn message_stopped(&mut self, by: &str) {
        if let Some(message) = self.messages.playing.take() {
            tracing::info!("message from {} stopped by {by}", message.from);
        }
    }

    /// After every input and tick: forget what may not wait any more, then
    /// say what can be said.
    pub(super) fn tend_messages(&mut self, now: Instant) {
        if self.mode() != Mode::Standalone {
            self.messages
                .forget_all("the satellite is in Home Assistant mode");
        } else if !self.local.group_on() {
            self.messages.forget_all("Satellite group is off");
        }
        self.messages.expire(now);
        self.play_held(now);
    }
}

/// Synthesise and say a message: the one task that puts it on the voice
/// lane, kept in the Brain's playback slot. Reports [`Input::PlaybackDone`].
async fn say_message(
    speaker: Arc<Speaker>,
    speech: Speech,
    text: String,
    token: u64,
    tx: mpsc::Sender<Input>,
    counters: StandaloneCounters,
) {
    let ok = match say(&speaker, &speech, &text, &mut SpeakReply(None), &counters).await {
        Ok(()) => true,
        Err(why) => {
            tracing::info!("a group message's speech failed: {why}");
            false
        }
    };
    let _ = tx.send(Input::PlaybackDone { token, ok }).await;
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use futures::future::BoxFuture;
    use pinna_assist::error::{ProviderError, Reason, Slot};
    use pinna_assist::presets::{PresetId, Slots};
    use pinna_assist::reply::{AudioBody, AudioKind, SpeechAudio};
    use pinna_audio::{Backend, SpeakerConfig};
    use pinna_esphome::{EsphomeHandle, HaCommand};
    use pinna_hal::buttons::{Button, ButtonEvent};
    use tokio::sync::watch;

    use super::*;
    use crate::group::Persisted;
    use crate::leds::LedState;
    use crate::settings::Settings;
    use crate::standalone::AssistLive;
    use crate::voice::{
        ApiCommand, ApiReply, Defaults, LocalOptions, NoopSpawner, Options, SettingsPatch, Shared,
        Snapshot,
    };

    /// What the text-to-speech fake does.
    #[derive(Clone, Copy, Debug)]
    enum Says {
        /// This many milliseconds of WAV.
        Wav(usize),
        /// Fails at once.
        Fails,
    }

    /// Text-to-speech that records what it was asked to say.
    #[derive(Clone)]
    struct Voice {
        says: Arc<Mutex<Says>>,
        asked: Arc<Mutex<Vec<String>>>,
    }

    impl pinna_assist::tts::Tts for Voice {
        fn speak<'a>(&'a self, text: &'a str) -> BoxFuture<'a, Result<SpeechAudio, ProviderError>> {
            self.asked.lock().unwrap().push(text.to_owned());
            let says = *self.says.lock().unwrap();
            Box::pin(async move {
                match says {
                    Says::Wav(ms) => Ok(wav(ms)),
                    Says::Fails => Err(ProviderError::new(
                        Slot::Tts,
                        PresetId::OpenAi,
                        Reason::Protocol,
                    )),
                }
            })
        }
    }

    /// 16-bit mono 16 kHz WAV of `ms` milliseconds.
    fn wav(ms: usize) -> SpeechAudio {
        use futures::StreamExt;
        let samples = ms * 16;
        let data_len = (samples * 2) as u32;
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        for v in [1u16, 1] {
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        for v in [16_000u32, 32_000] {
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        for v in [2u16, 16] {
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&data_len.to_le_bytes());
        bytes.extend((0..samples).flat_map(|_| 1_000i16.to_le_bytes()));
        SpeechAudio {
            kind: AudioKind::Sentence,
            mime: Some("audio/wav".into()),
            body: AudioBody::Stream(futures::stream::iter([Ok(bytes::Bytes::from(bytes))]).boxed()),
        }
    }

    /// A ready standalone satellite named Hall, with a speaker, a checked
    /// voice and Satellite group on.
    struct Device {
        brain: Brain,
        /// What the Brain sends itself: playback reports.
        inputs: mpsc::Receiver<Input>,
        live: watch::Sender<Arc<AssistLive>>,
        voice: Voice,
        _dir: tempfile::TempDir,
    }

    impl Device {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let (esp, _events) = EsphomeHandle::detached(256);
            let speaker = Arc::new(Speaker::new(SpeakerConfig::new(Backend::WavFile(
                dir.path().join("output.wav"),
            ))));
            let (self_tx, inputs) = mpsc::channel(64);
            let (live_tx, live) = watch::channel(Arc::new(AssistLive::empty()));
            let brain = Brain::new_with_local(
                esp,
                Some(speaker),
                Arc::new(Shared::default()),
                watch::channel(LedState::default()).0,
                watch::channel(Snapshot::default()).0,
                self_tx,
                Options {
                    key_file: dir.path().join("key"),
                    settings_file: dir.path().join("settings.toml"),
                    wake_words: crate::wakewords::test_catalogue(),
                    active_wake_words: vec!["alexa_v0.1".into()],
                    keywords: None,
                    defaults: Defaults {
                        node_name: "pinna-hall".into(),
                        friendly_name: "Hall".into(),
                        default_name: false,
                        wake_sound: false,
                        update_channel: crate::updates::UpdateChannel::Off,
                    },
                    updates: None,
                    mdns: None,
                    encrypted: false,
                    has_hw_privacy: false,
                    privacy_trigger: Vec::new(),
                    airplay: false,
                    bluetooth: false,
                    settings: Settings {
                        mode: Mode::Standalone,
                        ..Settings::default()
                    },
                },
                LocalOptions {
                    live,
                    spawner: Arc::new(NoopSpawner),
                    timers_file: dir.path().join("timers.toml"),
                },
            );
            let voice = Voice {
                says: Arc::new(Mutex::new(Says::Wav(50))),
                asked: Arc::default(),
            };
            let device = Self {
                brain,
                inputs,
                live: live_tx,
                voice,
                _dir: dir,
            };
            device.publish(true, true);
            device
        }

        /// The configuration, with the voice checked or not, and Satellite
        /// group on or off.
        fn publish(&self, checked: bool, group: bool) {
            self.publish_in(checked, group, None);
        }

        fn publish_in(&self, checked: bool, group: bool, language: Option<&str>) {
            self.live.send_replace(Arc::new(AssistLive {
                language: language.map(str::to_owned),
                ready: checked,
                slots: Slots::all(checked),
                speech: Some(Speech {
                    tts: Arc::new(self.voice.clone()),
                    preset: PresetId::OpenAi,
                }),
                group: Persisted {
                    on: group,
                    ..Persisted::default()
                },
                ..AssistLive::empty()
            }));
        }

        fn says(&self, says: Says) {
            *self.voice.says.lock().unwrap() = says;
        }

        /// What the voice was asked to say.
        fn asked(&self) -> Vec<String> {
            self.voice.asked.lock().unwrap().clone()
        }

        async fn deliver(&mut self, from: &str, text: &str) -> Taken {
            let (reply, taken) = oneshot::channel();
            self.brain
                .handle(Input::Message(Delivery {
                    from: from.into(),
                    text: text.into(),
                    reply,
                }))
                .await;
            taken.await.unwrap()
        }

        async fn api(&mut self, cmd: impl FnOnce(ApiReply) -> ApiCommand) -> Snapshot {
            let (reply, result) = oneshot::channel();
            self.brain.handle(Input::Api(cmd(reply))).await;
            result.await.unwrap().unwrap()
        }

        async fn mute(&mut self, on: bool) {
            self.api(|r| ApiCommand::SetMute(on, r)).await;
        }

        async fn press(&mut self, button: Button) {
            self.brain
                .handle(Input::Button(ButtonEvent::Press(button)))
                .await;
        }

        /// The sender of the message playing now.
        fn playing(&self) -> Option<&str> {
            assert_eq!(
                self.brain.messages.playing.is_some(),
                matches!(self.brain.playback, Some((_, PlaybackKind::Message, _))),
                "a message plays exactly when it holds the voice lane"
            );
            self.brain
                .messages
                .playing
                .as_ref()
                .map(|m| m.from.as_str())
        }

        /// The senders of the held messages, front first.
        fn held(&self) -> Vec<&str> {
            self.brain
                .messages
                .held
                .iter()
                .map(|m| m.from.as_str())
                .collect()
        }

        /// Hand the Brain its own reports until the playback is done.
        async fn play_out(&mut self) {
            loop {
                let input = tokio::time::timeout(Duration::from_secs(5), self.inputs.recv())
                    .await
                    .expect("the playback reported in time")
                    .expect("the Brain's sender is alive");
                let done = matches!(input, Input::PlaybackDone { .. });
                self.brain.handle(input).await;
                if done {
                    return;
                }
            }
        }
    }

    const HALL: &str = "Hall";

    #[tokio::test]
    async fn an_idle_satellite_says_a_message_at_once_in_its_own_voice() {
        let mut d = Device::new();
        assert_eq!(
            d.deliver("Kitchen", "dinner is ready.").await,
            Taken::Played
        );
        assert_eq!(d.playing(), Some("Kitchen"));
        // The ring shows it speaking, as for a Speak.
        d.brain.boot_done = true;
        d.brain.refresh();
        assert_eq!(
            d.brain.leds.borrow().base,
            pinna_hal::anim::Effect::Speaking
        );
        d.play_out().await;
        assert_eq!(d.asked(), ["Message from Kitchen: dinner is ready."]);
        assert_eq!(d.playing(), None);
        assert!(d.held().is_empty());
        assert!(d.brain.playback.is_none());
    }

    #[tokio::test]
    async fn the_opening_follows_the_receivers_language() {
        let mut d = Device::new();
        d.publish_in(true, true, Some("de-DE"));
        assert_eq!(d.deliver("Küche", "Essen ist fertig.").await, Taken::Played);
        d.play_out().await;
        assert_eq!(d.asked(), ["Nachricht von Küche: Essen ist fertig."]);
    }

    #[tokio::test]
    async fn a_message_whose_sender_stopped_waiting_is_dropped() {
        // The PeerService gave up on the answer and told the sender it was
        // refused, before the Brain got to the message.
        let mut d = Device::new();
        let (reply, taken) = oneshot::channel();
        drop(taken);
        d.brain
            .handle(Input::Message(Delivery {
                from: "Kitchen".into(),
                text: "dinner is ready.".into(),
                reply,
            }))
            .await;
        assert_eq!(d.playing(), None);
        assert!(d.held().is_empty());
        assert!(d.asked().is_empty());
        // A busy satellite doesn't keep it for later either.
        d.mute(true).await;
        let (reply, taken) = oneshot::channel();
        drop(taken);
        d.brain
            .handle(Input::Message(Delivery {
                from: "Kitchen".into(),
                text: "dinner is ready.".into(),
                reply,
            }))
            .await;
        assert!(d.held().is_empty());
    }

    #[tokio::test]
    async fn music_alone_doesnt_hold_a_message() {
        let mut d = Device::new();
        d.brain
            .handle(Input::Media(pinna_audio::MediaState {
                phase: pinna_audio::MediaPhase::Playing,
                url: Some("http://radio.test/stream".into()),
                ..Default::default()
            }))
            .await;
        assert_eq!(
            d.brain.snapshot().media.state,
            pinna_esphome::MediaPlayerStateKind::Playing
        );
        assert_eq!(
            d.deliver("Kitchen", "dinner is ready.").await,
            Taken::Played
        );
    }

    #[tokio::test]
    async fn a_running_request_holds_it_until_the_satellite_is_idle_again() {
        let mut d = Device::new();
        d.press(Button::Action).await;
        assert!(d.brain.local.run.is_some());
        assert_eq!(d.deliver("Kitchen", "dinner is ready.").await, Taken::Held);
        assert_eq!(d.brain.snapshot().held_messages, 1);
        assert_eq!(d.held(), ["Kitchen"]);

        // The request ends: the message plays by itself.
        d.press(Button::Action).await;
        assert!(d.brain.local.run.is_none());
        assert_eq!(d.playing(), Some("Kitchen"));
        assert_eq!(d.brain.snapshot().held_messages, 0);
    }

    #[tokio::test]
    async fn the_follow_up_window_holds_it_too() {
        let mut d = Device::new();
        d.brain
            .start_local_run(pinna_assist::run::RunKind::FollowUp, None)
            .await;
        assert_eq!(d.deliver("Kitchen", "dinner is ready.").await, Taken::Held);
        d.brain.abort_run(true).await;
        d.brain.tend_messages(Instant::now());
        assert_eq!(d.playing(), Some("Kitchen"));
    }

    #[tokio::test]
    async fn anything_on_the_voice_lane_holds_it_and_it_plays_after() {
        let mut d = Device::new();
        let (reply, spoken) = oneshot::channel();
        d.brain
            .handle(Input::Api(ApiCommand::Speak("A story.".into(), reply)))
            .await;
        assert_eq!(spoken.await.unwrap(), Ok(()));
        assert_eq!(d.deliver("Kitchen", "dinner is ready.").await, Taken::Held);
        // Another message waits behind the first.
        assert_eq!(d.deliver("Office", "call me.").await, Taken::Held);
        assert_eq!(d.held(), ["Kitchen", "Office"]);

        d.play_out().await;
        assert_eq!(d.playing(), Some("Kitchen"));
        assert_eq!(d.held(), ["Office"]);
        d.play_out().await;
        assert_eq!(d.playing(), Some("Office"));
        d.play_out().await;
        assert_eq!(d.playing(), None);
        assert_eq!(
            d.asked(),
            [
                "A story.",
                "Message from Kitchen: dinner is ready.",
                "Message from Office: call me."
            ]
        );
    }

    #[tokio::test]
    async fn an_alarm_holds_it_and_cuts_one_that_plays() {
        let mut d = Device::new();
        d.brain.ringing = Some(("tea".into(), Instant::now()));
        assert_eq!(d.deliver("Kitchen", "dinner is ready.").await, Taken::Held);
        d.brain.ringing = None;
        d.brain.tend_messages(Instant::now());
        assert_eq!(d.playing(), Some("Kitchen"));
        d.says(Says::Wav(3_000));
        assert_eq!(d.deliver("Office", "call me.").await, Taken::Held);

        // A timer rings over it: held again, at the front, no failure.
        d.brain.timer_finished("tea".into()).await;
        assert!(d.brain.ringing.is_some());
        assert_eq!(d.playing(), None);
        assert_eq!(d.held(), ["Kitchen", "Office"]);
        assert!(d.brain.messages.held.iter().all(|m| !m.failed));

        d.press(Button::Action).await;
        assert!(d.brain.ringing.is_none());
        assert_eq!(d.playing(), Some("Kitchen"));
    }

    #[tokio::test]
    async fn a_home_assistant_announcement_cuts_it_and_it_is_held_again() {
        let mut d = Device::new();
        d.says(Says::Wav(3_000));
        assert_eq!(
            d.deliver("Kitchen", "dinner is ready.").await,
            Taken::Played
        );
        d.brain
            .handle(Input::Ha(HaCommand::MediaPlayer {
                command: None,
                volume: None,
                media_url: Some("http://ha.test/announce.mp3".into()),
                announcement: true,
            }))
            .await;
        assert!(matches!(
            d.brain.playback,
            Some((_, PlaybackKind::MediaAnnounce, _))
        ));
        assert_eq!(d.held(), ["Kitchen"]);
        assert!(!d.brain.messages.held[0].failed);
    }

    #[tokio::test]
    async fn the_calibration_holds_it_and_cuts_one_that_plays() {
        let mut d = Device::new();
        d.says(Says::Wav(3_000));
        assert_eq!(
            d.deliver("Kitchen", "dinner is ready.").await,
            Taken::Played
        );
        d.api(ApiCommand::StartCalibration).await;
        assert!(d.brain.calibration.is_some());
        assert_eq!(d.playing(), None);
        assert_eq!(d.held(), ["Kitchen"]);
        assert_eq!(d.deliver("Office", "call me.").await, Taken::Held);

        d.api(ApiCommand::StopCalibration).await;
        assert_eq!(d.playing(), Some("Kitchen"));
        assert_eq!(d.held(), ["Office"]);
    }

    #[tokio::test]
    async fn muted_is_not_now() {
        let mut d = Device::new();
        d.mute(true).await;
        assert_eq!(d.deliver("Kitchen", "dinner is ready.").await, Taken::Held);
        d.brain.tick().await;
        assert_eq!(d.playing(), None);
        d.mute(false).await;
        assert_eq!(d.playing(), Some("Kitchen"));
    }

    #[tokio::test]
    async fn the_wake_word_puts_it_back_at_the_front() {
        let mut d = Device::new();
        d.says(Says::Wav(3_000));
        d.mute(true).await;
        d.deliver("Kitchen", "dinner is ready.").await;
        d.deliver("Office", "call me.").await;
        d.mute(false).await;
        assert_eq!(d.playing(), Some("Kitchen"));

        d.brain
            .handle(Input::Wake {
                id: "alexa_v0.1".into(),
                score: 0.9,
            })
            .await;
        assert!(d.brain.local.run.is_some(), "the person wanted to talk");
        assert_eq!(d.playing(), None);
        assert_eq!(d.held(), ["Kitchen", "Office"]);
        assert!(!d.brain.messages.held[0].failed);
    }

    #[tokio::test]
    async fn the_action_button_drops_it() {
        let mut d = Device::new();
        d.says(Says::Wav(3_000));
        assert_eq!(
            d.deliver("Kitchen", "dinner is ready.").await,
            Taken::Played
        );
        d.press(Button::Action).await;
        assert_eq!(d.playing(), None);
        assert!(d.held().is_empty());
        assert!(d.brain.local.run.is_none(), "no request starts");
        assert_eq!(d.brain.phase, Phase::Idle);
    }

    #[tokio::test]
    async fn cancel_request_drops_it_too() {
        let mut d = Device::new();
        d.says(Says::Wav(3_000));
        assert_eq!(
            d.deliver("Kitchen", "dinner is ready.").await,
            Taken::Played
        );
        d.brain
            .handle(Input::Ha(HaCommand::Button {
                object_id: "cancel_request".into(),
            }))
            .await;
        assert_eq!(d.playing(), None);
        assert!(d.held().is_empty());
    }

    #[tokio::test]
    async fn a_message_whose_speech_fails_is_tried_once_more_after_the_wait() {
        let mut d = Device::new();
        d.says(Says::Fails);
        assert_eq!(
            d.deliver("Kitchen", "dinner is ready.").await,
            Taken::Played
        );
        d.play_out().await;
        assert_eq!(d.playing(), None);
        assert_eq!(d.held(), ["Kitchen"]);
        assert!(d.brain.messages.held[0].failed);

        // Not before the wait is over, even with the satellite free.
        let failed_at = Instant::now();
        d.brain.tend_messages(failed_at + Duration::from_secs(29));
        assert_eq!(d.playing(), None);
        // A message that arrives meanwhile isn't kept waiting by it.
        d.says(Says::Wav(50));
        assert_eq!(d.deliver("Office", "call me.").await, Taken::Played);
        d.play_out().await;

        // Then it is tried again, and dropped when that fails too.
        d.says(Says::Fails);
        d.brain.tend_messages(failed_at + Duration::from_secs(31));
        assert_eq!(d.playing(), Some("Kitchen"));
        d.play_out().await;
        assert_eq!(d.playing(), None);
        assert!(d.held().is_empty());
        assert_eq!(d.asked().len(), 3);
    }

    #[tokio::test]
    async fn a_message_said_after_one_failure_is_done() {
        let mut d = Device::new();
        d.says(Says::Fails);
        d.deliver("Kitchen", "dinner is ready.").await;
        d.play_out().await;
        d.says(Says::Wav(50));
        d.brain
            .tend_messages(Instant::now() + d.brain.messages.times.retry);
        assert_eq!(d.playing(), Some("Kitchen"));
        d.play_out().await;
        assert!(d.held().is_empty() && d.playing().is_none());
    }

    #[tokio::test]
    async fn at_most_five_are_held_and_a_sixth_drops_the_oldest() {
        let mut d = Device::new();
        d.mute(true).await;
        for from in ["A", "B", "C", "D", "E"] {
            assert_eq!(d.deliver(from, "hello.").await, Taken::Held);
        }
        assert_eq!(d.deliver("F", "hello.").await, Taken::Held);
        assert_eq!(d.held(), ["B", "C", "D", "E", "F"]);
        assert_eq!(d.brain.snapshot().held_messages, 5);
    }

    #[tokio::test]
    async fn a_message_held_again_counts_among_the_five() {
        let mut d = Device::new();
        d.says(Says::Wav(3_000));
        d.deliver("A", "hello.").await;
        assert_eq!(d.playing(), Some("A"));
        d.mute(true).await;
        for from in ["B", "C", "D", "E", "F"] {
            d.deliver(from, "hello.").await;
        }
        // The calibration cuts A: it goes back to the front, and the oldest
        // of the others goes.
        d.mute(false).await;
        assert_eq!(d.playing(), Some("A"));
        d.api(ApiCommand::StartCalibration).await;
        assert_eq!(d.held(), ["A", "C", "D", "E", "F"]);
    }

    #[tokio::test]
    async fn a_held_message_is_forgotten_after_twelve_hours() {
        let mut d = Device::new();
        d.mute(true).await;
        d.deliver("Kitchen", "dinner is ready.").await;
        let arrived = Instant::now();
        let keep = Duration::from_secs(12 * 60 * 60);
        d.brain
            .tend_messages(arrived + keep - Duration::from_secs(60));
        assert_eq!(d.held(), ["Kitchen"]);
        d.brain
            .tend_messages(arrived + keep + Duration::from_secs(1));
        assert!(d.held().is_empty());
    }

    #[tokio::test]
    async fn home_assistant_mode_drops_them_and_refuses_more() {
        let mut d = Device::new();
        d.mute(true).await;
        d.deliver("Kitchen", "dinner is ready.").await;
        d.api(|r| {
            ApiCommand::UpdateSettings(
                SettingsPatch {
                    mode: Some(Mode::HomeAssistant),
                    ..SettingsPatch::default()
                },
                r,
            )
        })
        .await;
        assert!(d.held().is_empty());
        assert_eq!(
            d.deliver("Kitchen", "dinner is ready.").await,
            Taken::Refused("Hall is in Home Assistant mode and takes no messages.".into())
        );
    }

    #[tokio::test]
    async fn a_waiting_switch_to_home_assistant_mode_goes_before_held_messages() {
        let mut d = Device::new();
        d.says(Says::Wav(3_000));
        d.deliver("Kitchen", "dinner is ready.").await;
        d.deliver("Office", "call me.").await;
        assert_eq!(d.playing(), Some("Kitchen"));
        let snapshot = d
            .api(|r| ApiCommand::SwitchMode(Mode::HomeAssistant, r))
            .await;
        assert_eq!(snapshot.mode, Mode::Standalone, "it waits for Kitchen's");
        // The switch would drop a new one before it plays.
        assert_eq!(
            d.deliver("Garden", "the gate is open.").await,
            Taken::Refused(switching_to_home_assistant_mode(HALL))
        );
        assert_eq!(d.held(), ["Office"]);
        d.says(Says::Wav(50));
        d.press(Button::Action).await;
        assert_eq!(d.playing(), None, "Office's waits for the switch");
        d.brain.tick().await;
        assert_eq!(d.brain.mode(), Mode::HomeAssistant);
        assert!(d.held().is_empty());
    }

    #[tokio::test]
    async fn switching_satellite_group_off_drops_them() {
        let mut d = Device::new();
        d.says(Says::Wav(3_000));
        d.deliver("Kitchen", "dinner is ready.").await;
        d.mute(true).await;
        d.deliver("Office", "call me.").await;
        d.publish(true, false);
        d.brain.tick().await;
        assert!(d.held().is_empty());
        // The one playing plays to its end, but isn't held again.
        assert!(matches!(
            d.brain.playback,
            Some((_, PlaybackKind::Message, _))
        ));
        assert!(d.brain.messages.playing.is_none());
        d.brain.timer_finished("tea".into()).await;
        assert!(d.held().is_empty());
    }

    #[tokio::test]
    async fn group_messages_off_refuses_new_ones_but_held_ones_still_play() {
        let mut d = Device::new();
        d.mute(true).await;
        d.deliver("Kitchen", "dinner is ready.").await;
        d.api(|r| {
            ApiCommand::UpdateSettings(
                SettingsPatch {
                    group_messages: Some(false),
                    ..SettingsPatch::default()
                },
                r,
            )
        })
        .await;
        assert_eq!(
            d.deliver("Office", "call me.").await,
            Taken::Refused(switched_off(HALL))
        );
        d.mute(false).await;
        assert_eq!(d.playing(), Some("Kitchen"));
    }

    #[tokio::test]
    async fn a_message_needs_a_checked_voice_and_a_speaker() {
        let mut d = Device::new();
        d.publish(false, true);
        assert_eq!(
            d.deliver("Kitchen", "dinner is ready.").await,
            Taken::Refused(no_voice(HALL))
        );
        d.publish(true, true);
        d.brain.speaker = None;
        assert_eq!(
            d.deliver("Kitchen", "dinner is ready.").await,
            Taken::Refused(no_speaker(HALL))
        );
        assert!(d.asked().is_empty());
    }

    #[tokio::test]
    async fn a_message_never_reaches_a_log_line() {
        const TEXT: &str = "the spare key is under the third flowerpot";
        let captured = crate::group::tests::capture_logs();
        let mut d = Device::new();
        d.says(Says::Fails);
        d.deliver("Kitchen", TEXT).await;
        d.play_out().await;
        d.mute(true).await;
        d.deliver("Office", TEXT).await;
        d.says(Says::Wav(50));
        d.mute(false).await;
        d.play_out().await;
        let delivery = Delivery {
            from: "Kitchen".into(),
            text: TEXT.into(),
            reply: oneshot::channel().0,
        };
        let debug = format!("{delivery:?}");

        let logs = String::from_utf8_lossy(&captured.lock().unwrap()).into_owned();
        assert!(
            logs.contains("message from Kitchen, 42 characters"),
            "{logs}"
        );
        for text in [&logs, &debug] {
            assert!(!text.contains("flowerpot"), "{text}");
        }
    }
}
