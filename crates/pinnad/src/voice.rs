//! The device's state machine: wake word / button → Home Assistant Assist run
//! over the ESPHome API → TTS playback, plus timers, announcements, mute, the
//! light entity and volume. Single task, all inputs arrive on one channel.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant, SystemTime};

use pinna_audio::beamview::{self, BeamViewReader};
use pinna_audio::{MediaPhase, MediaState, Speaker, tone};
use pinna_esphome::{
    DeviceEvent, EsphomeHandle, HaCommand, MediaPlayerCommand, MediaPlayerStateKind,
    TimerEventType, VoiceEventType,
};
use pinna_hal::anim::Effect;
use pinna_hal::buttons::{Button, ButtonEvent};
use pinna_hal::led::Rgb;
use tokio::sync::{mpsc, oneshot, watch};

use crate::calibration::{Calibration, Event as CalibrationEvent};
use crate::leds::{self, LedState, LightEffect};
use crate::settings::{Settings, Store};
use crate::wakewords::{Catalogue, KeywordSet};

/// Flags shared with the capture task.
#[derive(Default)]
pub struct Shared {
    /// Forward microphone frames to Home Assistant.
    pub streaming: AtomicBool,
    /// Run the wake word detector on microphone frames.
    pub wake_enabled: AtomicBool,
    /// Microphone muted (hardware latch or Home Assistant switch).
    pub muted: AtomicBool,
    /// Wake word accept threshold (f32 bits).
    pub threshold: AtomicU32,
}

#[derive(Debug)]
pub enum Input {
    Ha(HaCommand),
    Button(ButtonEvent),
    Privacy(bool),
    Wake {
        /// The id of the word that fired.
        id: String,
        score: f32,
    },
    /// An utterance scored at least the detector's support level without a
    /// detection, while detection was armed.
    NearMiss {
        score: f32,
        /// The score reached the threshold; the detector's other rules rejected it.
        reached_threshold: bool,
    },
    /// The playback's first audio is sounding.
    PlaybackStarted {
        token: u64,
    },
    PlaybackDone {
        token: u64,
        ok: bool,
    },
    /// The media lane changed state (playing, paused, ended, failed).
    Media(MediaState),
    Api(ApiCommand),
}

/// Answer to an API command: the state right after it was applied, or why it was refused.
pub type ApiReply = oneshot::Sender<Result<Snapshot, String>>;

#[derive(Debug)]
pub enum ApiCommand {
    SetVolume(u8, ApiReply),
    SetMute(bool, ApiReply),
    StartConversation(ApiReply),
    StopConversation(ApiReply),
    LedTest(ApiReply),
    UpdateSettings(SettingsPatch, ApiReply),
    ClearNotification(ApiReply),
    StartCalibration(ApiReply),
    StopCalibration(ApiReply),
}

/// Settings to change from the API; `None` leaves a setting as it is.
#[derive(Debug, Default)]
pub struct SettingsPatch {
    pub wake_word: Option<bool>,
    pub wake_sensitivity: Option<u8>,
    pub led_brightness: Option<u8>,
    /// Already checked against the catalogue (`Catalogue::validate`).
    pub wake_words: Option<Vec<String>>,
}

/// A wake or a near miss, for the dashboard's recent list.
#[derive(Debug, Clone, PartialEq)]
pub struct WakeRecord {
    pub unix_ms: u64,
    pub woke: bool,
    pub score: f32,
    pub reached_threshold: bool,
}

impl WakeRecord {
    fn now(woke: bool, score: f32, reached_threshold: bool) -> Self {
        let unix_ms = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64);
        Self {
            unix_ms,
            woke,
            score,
            reached_threshold,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TimerSnapshot {
    pub id: String,
    pub name: String,
    pub total: u32,
    /// Seconds left at `at`.
    pub left: u32,
    pub active: bool,
    pub at: Instant,
}

/// What the brain publishes for the API. Holds nothing that changes every
/// second (uptime, countdowns): readers derive those from `at`.
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    pub phase: pinna_api::Phase,
    pub hw_muted: bool,
    pub sw_muted: bool,
    pub volume: u8,
    pub ha_connected: bool,
    pub voice_subscribed: bool,
    /// The wake word detector is armed.
    pub wake_enabled: bool,
    pub timers: Vec<TimerSnapshot>,
    /// A finished timer's alarm is sounding (finished timers leave `timers`).
    pub alarm_ringing: bool,
    pub settings: crate::settings::Settings,
    /// Wakes and near misses, oldest first.
    pub recent_wakes: Vec<WakeRecord>,
    /// The sensitivity tuning from the device's buttons, while it runs.
    pub calibration: Option<CalibrationSnapshot>,
    /// The ids of the words listening now (ADR 0013).
    pub active_wake_words: Vec<String>,
}

/// A running calibration, as the API shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct CalibrationSnapshot {
    /// The sensitivity when calibration started, for undo.
    pub started_from: u8,
    /// What happened so far, oldest first.
    pub events: Vec<CalibrationEvent>,
}

impl Default for Snapshot {
    fn default() -> Self {
        Self {
            phase: pinna_api::Phase::PHASE_IDLE,
            hw_muted: false,
            sw_muted: false,
            volume: 0,
            ha_connected: false,
            voice_subscribed: false,
            wake_enabled: false,
            timers: Vec::new(),
            alarm_ringing: false,
            settings: crate::settings::Settings::default(),
            recent_wakes: Vec::new(),
            calibration: None,
            active_wake_words: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Phase {
    Idle,
    /// VoiceStart sent, waiting for HA's VoiceAssistantResponse.
    Starting(Instant),
    /// Streaming the microphone to HA.
    Listening(Instant),
    /// HA is processing (STT/intent/TTS), or its reply is on its way: this
    /// lasts until the reply's first audio sounds.
    Thinking(Instant),
    /// Playing a reply (from its first audio) or an announcement.
    Speaking,
}

#[derive(Debug, Clone, PartialEq)]
enum PlaybackKind {
    Tts,
    Announce {
        start_conversation: bool,
    },
    /// Media player `play_media` with `announcement` set.
    MediaAnnounce,
    Alarm,
    Cue,
}

/// Two presses of the same button within this window form a double press.
const DOUBLE_PRESS: Duration = Duration::from_millis(400);
/// Wakes and near misses kept for the dashboard.
const RECENT_WAKES: usize = 20;

struct HaTimer {
    name: String,
    total: u32,
    left: u32,
    active: bool,
    at: Instant,
}

impl HaTimer {
    fn remaining(&self, now: Instant) -> u32 {
        if self.active {
            self.left
                .saturating_sub(now.saturating_duration_since(self.at).as_secs() as u32)
        } else {
            self.left
        }
    }
}

pub struct Options {
    /// Where a Noise key pushed by Home Assistant is persisted.
    pub key_file: std::path::PathBuf,
    /// The wake words on offer (ADR 0013).
    pub wake_words: Arc<Catalogue>,
    /// The words listening at start: the persisted choice, or `[wake] id`.
    pub active_wake_words: Vec<String>,
    /// Where newly chosen words' classifiers go: the capture loop. `None`
    /// without a detector.
    pub keywords: Option<mpsc::UnboundedSender<KeywordSet>>,
    pub wake_sound: bool,
    pub has_hw_privacy: bool,
    /// Where runtime settings are persisted.
    pub settings_file: std::path::PathBuf,
    /// Settings at startup (already loaded from `settings_file`).
    pub settings: Settings,
}

pub struct Brain {
    esp: EsphomeHandle,
    speaker: Option<Arc<Speaker>>,
    shared: Arc<Shared>,
    leds: watch::Sender<LedState>,
    status: watch::Sender<Snapshot>,
    self_tx: mpsc::Sender<Input>,
    opts: Options,

    phase: Phase,
    conversation_id: String,
    continue_conversation: bool,
    run_ended: bool,
    tts_started: bool,
    tts_url: Option<String>,
    /// When the microphone stopped for the current run (end of speech).
    speech_ended: Option<Instant>,

    hw_muted: bool,
    sw_muted: bool,
    voice_subscribed: bool,
    clients: u32,

    active_wake: Vec<String>,
    /// Counts classifier loads, so a slow one can't undo a newer choice.
    keyword_generation: u64,

    light_on: bool,
    light_brightness: f32,
    light_rgb: (f32, f32, f32),

    light_effect: LightEffect,

    settings: Settings,
    store: Store,
    last_press: HashMap<Button, Instant>,
    media_state: MediaPlayerStateKind,
    media_muted: bool,
    /// Last state reported by the media lane.
    media_phase: MediaPhase,
    ducked: bool,

    timers: HashMap<String, HaTimer>,
    ringing: Option<(String, Instant)>,

    playback: Option<(u64, PlaybackKind, tokio::task::JoinHandle<()>)>,
    next_token: u64,
    booted_at: Instant,
    boot_done: bool,
    /// Running LED test animation (started by the API).
    led_test: Option<Instant>,

    /// The beamformer's state, published by the PipeWire graph on the Dot.
    beams: BeamViewReader,
    /// Where the listening ring points, once the talker's direction is known.
    talker: leds::Talker,
    /// When the speaker last stopped playing a chime, reply or announcement.
    speaker_quiet_since: Instant,
    /// Wakes and near misses, oldest first.
    recent_wakes: VecDeque<WakeRecord>,
    /// The sensitivity tuning with the device's buttons, while it runs.
    calibration: Option<Calibration>,
}

const START_TIMEOUT: Duration = Duration::from_secs(5);
const LISTEN_TIMEOUT: Duration = Duration::from_secs(15);
const THINK_TIMEOUT: Duration = Duration::from_secs(30);
const RING_TIMEOUT: Duration = Duration::from_secs(300);
const BOOT_ANIMATION: Duration = Duration::from_millis(1200);
/// LED test: one effect per step.
const LED_TEST_STEP: Duration = Duration::from_millis(1000);
const LED_TEST_EFFECTS: [Effect; 4] = [
    Effect::Listening,
    Effect::Thinking,
    Effect::Speaking,
    Effect::Muted,
];
/// The beam selector hears the speaker (echo cancellation comes after it) and
/// smooths over ~200 ms, so ignore it while pinnad plays and for this long after.
const SELF_SOUND_HOLD: Duration = Duration::from_millis(300);

impl Brain {
    pub fn new(
        esp: EsphomeHandle,
        speaker: Option<Arc<Speaker>>,
        shared: Arc<Shared>,
        leds: watch::Sender<LedState>,
        status: watch::Sender<Snapshot>,
        self_tx: mpsc::Sender<Input>,
        opts: Options,
    ) -> Self {
        let (store, _) = Store::load(&opts.settings_file, opts.settings.clone());
        let active_wake = opts.active_wake_words.clone();
        let settings = opts.settings.clone();
        shared
            .threshold
            .store(settings.wake_threshold().to_bits(), Ordering::SeqCst);
        Self {
            esp,
            speaker,
            shared,
            leds,
            status,
            self_tx,
            opts,
            phase: Phase::Idle,
            conversation_id: String::new(),
            continue_conversation: false,
            run_ended: false,
            tts_started: false,
            tts_url: None,
            speech_ended: None,
            hw_muted: false,
            voice_subscribed: false,
            clients: 0,
            active_wake,
            keyword_generation: 0,
            light_on: false,
            light_brightness: 1.0,
            light_rgb: (1.0, 1.0, 1.0),
            light_effect: LightEffect::Solid,
            sw_muted: settings.muted,
            settings,
            store,
            last_press: HashMap::new(),
            media_state: MediaPlayerStateKind::Idle,
            media_muted: false,
            media_phase: MediaPhase::Idle,
            ducked: false,
            timers: HashMap::new(),
            ringing: None,
            playback: None,
            next_token: 1,
            booted_at: Instant::now(),
            boot_done: false,
            led_test: None,
            beams: BeamViewReader::new(beamview::PATH),
            talker: leds::Talker::default(),
            speaker_quiet_since: Instant::now(),
            recent_wakes: VecDeque::with_capacity(RECENT_WAKES),
            calibration: None,
        }
    }

    pub async fn run(mut self, mut inputs: mpsc::Receiver<Input>) {
        let mut tick = tokio::time::interval(Duration::from_millis(250));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        self.shared.muted.store(self.muted(), Ordering::SeqCst);
        self.apply_reply_volume();
        self.refresh();
        let mut sent_media = (self.media_state, self.settings.volume, self.media_muted);
        loop {
            tokio::select! {
                input = inputs.recv() => match input {
                    Some(input) => self.handle(input).await,
                    None => return,
                },
                _ = tick.tick() => self.tick().await,
            }
            let media = (self.media_state, self.settings.volume, self.media_muted);
            if media != sent_media {
                if media.0 != sent_media.0 {
                    match (sent_media.0, media.0) {
                        (_, MediaPlayerStateKind::Playing) => {
                            self.event("playback", "started").await
                        }
                        (MediaPlayerStateKind::Playing | MediaPlayerStateKind::Paused, _) => {
                            self.event("playback", "stopped").await
                        }
                        _ => {}
                    }
                }
                sent_media = media;
                self.send_media_state().await;
            }
            self.refresh();
        }
    }

    fn muted(&self) -> bool {
        self.hw_muted || self.sw_muted
    }

    async fn send(&self, ev: DeviceEvent) {
        if self.esp.send(ev).await.is_err() {
            tracing::error!("ESPHome server stopped");
        }
    }

    async fn handle(&mut self, input: Input) {
        match input {
            Input::Wake { id, score } => {
                let phrase = self.opts.wake_words.phrase(&id).to_owned();
                tracing::info!("wake word {phrase:?} detected (score {score:.2})");
                if self.phase == Phase::Idle && self.ringing.is_none() && !self.muted() {
                    self.record_wake(WakeRecord::now(true, score, false));
                    if let Some(c) = &mut self.calibration {
                        // Calibration acknowledges the wake itself: no request.
                        c.woke(score, self.settings.wake_sensitivity, Instant::now());
                        self.overlay(Effect::CalibrationHeard);
                        if self.opts.wake_sound {
                            self.play_pcm(tone::wake_chime(), PlaybackKind::Cue);
                        }
                    } else {
                        self.send(DeviceEvent::TextSensorState {
                            object_id: "last_wake_word".into(),
                            value: phrase.clone(),
                        })
                        .await;
                        self.event("voice_assistant", "wake_word").await;
                        self.start_run(Some(phrase)).await;
                    }
                }
            }
            Input::NearMiss {
                score,
                reached_threshold,
            } => {
                self.record_wake(WakeRecord::now(false, score, reached_threshold));
                if let Some(c) = &mut self.calibration {
                    c.near_miss(score, Instant::now());
                }
            }
            Input::Button(ev) => self.button(ev).await,
            Input::Privacy(muted) => {
                tracing::info!("hardware mute: {muted}");
                self.hw_muted = muted;
                self.apply_mute().await;
            }
            Input::PlaybackStarted { token } => self.playback_started(token).await,
            Input::PlaybackDone { token, ok } => self.playback_done(token, ok).await,
            Input::Media(state) => {
                if let Some(e) = &state.error {
                    tracing::warn!("media: {e}");
                }
                self.media_phase = state.phase;
                self.media_muted = state.muted;
                self.update_media_state();
            }
            Input::Ha(cmd) => self.ha(cmd).await,
            Input::Api(cmd) => self.api(cmd).await,
        }
    }

    // --- API ------------------------------------------------------------------

    async fn api(&mut self, cmd: ApiCommand) {
        let (result, reply) = match cmd {
            ApiCommand::SetVolume(percent, reply) => {
                let v = percent.min(100);
                self.set_volume(v).await;
                self.overlay(Effect::Volume(v));
                (Ok(()), reply)
            }
            ApiCommand::SetMute(on, reply) => {
                self.sw_muted = on;
                self.apply_mute().await;
                (Ok(()), reply)
            }
            ApiCommand::StartConversation(reply) => {
                let result = if self.ringing.is_some() || self.phase != Phase::Idle {
                    Err("a conversation or timer alarm is already active".to_string())
                } else if self.muted() {
                    Err("the microphone is muted".to_string())
                } else if !self.voice_subscribed {
                    Err("Home Assistant has not subscribed to voice assistant runs".to_string())
                } else {
                    self.start_run(None).await;
                    Ok(())
                };
                (result, reply)
            }
            ApiCommand::StopConversation(reply) => {
                if self.ringing.is_some() {
                    self.ringing = None;
                    self.stop_playback();
                } else if self.phase != Phase::Idle {
                    self.abort_run(true).await;
                }
                (Ok(()), reply)
            }
            ApiCommand::LedTest(reply) => {
                self.led_test = Some(Instant::now());
                (Ok(()), reply)
            }
            ApiCommand::ClearNotification(reply) => {
                self.clear_notification().await;
                (Ok(()), reply)
            }
            ApiCommand::UpdateSettings(patch, reply)
                if patch.wake_words.is_some() && self.clients > 0 =>
            {
                // Its dropdowns can't show a word chosen here (ADR 0013).
                let why = "Home Assistant chooses the wake words while it is connected. \
                           Choose them on this satellite's device page there.";
                (Err(why.into()), reply)
            }
            ApiCommand::UpdateSettings(patch, reply) => {
                // The same paths as Home Assistant's entities, so it sees the change.
                if let Some(ids) = patch.wake_words {
                    self.set_wake_words(ids, false).await;
                }
                if let Some(v) = patch.wake_sensitivity {
                    self.number("wake_word_sensitivity".into(), f32::from(v))
                        .await;
                }
                if let Some(v) = patch.led_brightness {
                    self.number("led_brightness".into(), f32::from(v)).await;
                }
                if let Some(on) = patch.wake_word {
                    self.set_wake_word(on).await;
                }
                (Ok(()), reply)
            }
            ApiCommand::StartCalibration(reply) => (self.set_calibration(true).await, reply),
            ApiCommand::StopCalibration(reply) => {
                let _ = self.set_calibration(false).await;
                (Ok(()), reply)
            }
        };
        self.refresh();
        let _ = reply.send(result.map(|()| self.snapshot()));
    }

    fn snapshot(&self) -> Snapshot {
        let mut timers: Vec<TimerSnapshot> = self
            .timers
            .iter()
            .map(|(id, t)| TimerSnapshot {
                id: id.clone(),
                name: t.name.clone(),
                total: t.total,
                left: t.left,
                active: t.active,
                at: t.at,
            })
            .collect();
        timers.sort_by(|a, b| a.id.cmp(&b.id));
        Snapshot {
            phase: match self.phase {
                Phase::Idle => pinna_api::Phase::PHASE_IDLE,
                Phase::Starting(_) | Phase::Listening(_) => pinna_api::Phase::PHASE_LISTENING,
                Phase::Thinking(_) => pinna_api::Phase::PHASE_THINKING,
                Phase::Speaking => pinna_api::Phase::PHASE_SPEAKING,
            },
            hw_muted: self.hw_muted,
            sw_muted: self.sw_muted,
            volume: self.settings.volume,
            ha_connected: self.clients > 0,
            voice_subscribed: self.voice_subscribed,
            wake_enabled: self.shared.wake_enabled.load(Ordering::Relaxed),
            timers,
            alarm_ringing: self.ringing.is_some(),
            settings: self.settings.clone(),
            recent_wakes: self.recent_wakes.iter().cloned().collect(),
            calibration: self.calibration.as_ref().map(|c| CalibrationSnapshot {
                started_from: c.started_from,
                events: c.events.iter().copied().collect(),
            }),
            active_wake_words: self.active_wake.clone(),
        }
    }

    fn record_wake(&mut self, record: WakeRecord) {
        if self.recent_wakes.len() == RECENT_WAKES {
            self.recent_wakes.pop_front();
        }
        self.recent_wakes.push_back(record);
    }

    // --- Home Assistant -----------------------------------------------------

    async fn ha(&mut self, cmd: HaCommand) {
        match cmd {
            HaCommand::ClientConnected { peer } => {
                self.clients += 1;
                tracing::info!("Home Assistant connected from {peer}");
                self.send(DeviceEvent::MuteState(self.muted())).await;
                self.send_light().await;
                self.send_numbers().await;
                self.send(DeviceEvent::SwitchState {
                    object_id: "wake_word".into(),
                    on: self.settings.wake_word,
                })
                .await;
                self.send(DeviceEvent::SwitchState {
                    object_id: "listening_direction".into(),
                    on: self.settings.listening_direction,
                })
                .await;
                self.send(DeviceEvent::SwitchState {
                    object_id: "notification".into(),
                    on: self.settings.notification,
                })
                .await;
                self.send_calibration_switch(self.calibration.is_some())
                    .await;
                self.send_media_state().await;
            }
            HaCommand::ClientDisconnected => {
                self.clients = self.clients.saturating_sub(1);
                tracing::info!("Home Assistant disconnected");
                if self.clients == 0 && self.phase != Phase::Idle {
                    self.abort_run(false).await;
                }
            }
            HaCommand::VoiceSubscribed(on) => {
                tracing::info!("voice assistant subscription: {on}");
                self.voice_subscribed = on;
            }
            HaCommand::VoiceStarted { port, error } => {
                if let Phase::Starting(_) = self.phase {
                    if error || port != 0 {
                        tracing::warn!(
                            "Home Assistant refused the run (port {port}, error {error})"
                        );
                        self.fail().await;
                    } else {
                        self.phase = Phase::Listening(Instant::now());
                        self.shared.streaming.store(true, Ordering::SeqCst);
                        self.event("voice_assistant", "listening").await;
                    }
                }
            }
            HaCommand::VoiceEvent { event, data } => self.voice_event(event, data).await,
            HaCommand::Timer {
                event,
                id,
                name,
                total_seconds,
                seconds_left,
                is_active,
            } => {
                self.timer(event, id, name, total_seconds, seconds_left, is_active)
                    .await
            }
            HaCommand::Announce {
                media_id,
                preannounce_media_id,
                start_conversation,
                ..
            } => {
                tracing::info!("announcement: {media_id}");
                if self.phase != Phase::Idle {
                    self.abort_run(true).await;
                }
                let urls: Vec<String> = [preannounce_media_id, media_id]
                    .into_iter()
                    .filter(|u| !u.is_empty())
                    .collect();
                self.phase = Phase::Speaking;
                self.play_urls(urls, PlaybackKind::Announce { start_conversation });
            }
            HaCommand::Light {
                on,
                brightness,
                rgb,
                effect,
            } => {
                if let Some(on) = on {
                    self.light_on = on;
                }
                if let Some(b) = brightness {
                    self.light_brightness = b.clamp(0.0, 1.0);
                }
                if let Some(c) = rgb {
                    self.light_rgb = c;
                }
                if let Some(e) = effect {
                    match LightEffect::from_name(&e) {
                        Some(effect) => self.light_effect = effect,
                        None => tracing::warn!("unknown light effect {e:?}"),
                    }
                }
                self.send_light().await;
            }
            HaCommand::MuteSwitch(on) => {
                self.sw_muted = on;
                self.apply_mute().await;
            }
            HaCommand::Number { object_id, value } => self.number(object_id, value).await,
            HaCommand::Switch { object_id, on } if object_id == "wake_word" => {
                self.set_wake_word(on).await;
            }
            HaCommand::Switch { object_id, on } if object_id == "listening_direction" => {
                tracing::info!("listening direction on the ring: {on}");
                self.settings.listening_direction = on;
                self.store.save(&self.settings);
                self.send(DeviceEvent::SwitchState { object_id, on }).await;
            }
            HaCommand::Switch { object_id, on } if object_id == "notification" => {
                self.set_notification(on).await;
            }
            HaCommand::Switch { object_id, on } if object_id == "wake_word_calibration" => {
                if let Err(why) = self.set_calibration(on).await {
                    tracing::info!("calibration: not started: {why}");
                    self.send_calibration_switch(false).await;
                }
            }
            HaCommand::Button { object_id } if object_id == "cancel_request" => {
                tracing::info!("cancel requested by Home Assistant");
                self.cancel().await;
            }
            HaCommand::MediaPlayer {
                command,
                volume,
                media_url,
                announcement,
            } => self.media(command, volume, media_url, announcement).await,
            HaCommand::SetActiveWakeWords(ids) => self.set_wake_words(ids, true).await,
            HaCommand::EncryptionKeySet(key) => {
                match crate::identity::store_encryption_key(&self.opts.key_file, &key) {
                    Ok(()) => tracing::info!(
                        "Home Assistant provisioned an encryption key; stored in {}",
                        self.opts.key_file.display()
                    ),
                    Err(e) => tracing::error!("could not persist the encryption key: {e:#}"),
                }
            }
            other => tracing::debug!("ignored: {other:?}"),
        }
    }

    async fn voice_event(&mut self, event: VoiceEventType, data: Vec<(String, String)>) {
        let get = |k: &str| {
            data.iter()
                .find(|(key, _)| key == k)
                .map(|(_, v)| v.clone())
        };
        tracing::debug!("voice event {event:?} {data:?}");
        match event {
            VoiceEventType::RunStart => {
                if let Some(url) = get("url") {
                    self.tts_url = Some(url);
                }
            }
            VoiceEventType::SttVadEnd | VoiceEventType::SttEnd => {
                self.stop_streaming();
                if matches!(self.phase, Phase::Listening(_) | Phase::Starting(_)) {
                    self.end_of_speech(Instant::now());
                    self.event("voice_assistant", "thinking").await;
                }
            }
            VoiceEventType::IntentProgress
                if !self.tts_started && get("tts_start_streaming").as_deref() == Some("1") =>
            {
                self.stop_streaming();
                if let Some(url) = self.tts_url.clone().or_else(|| get("url")) {
                    self.start_tts(url);
                }
            }
            VoiceEventType::IntentEnd => {
                self.continue_conversation = get("continue_conversation").as_deref() == Some("1");
                if let Some(cid) = get("conversation_id").filter(|c| !c.is_empty()) {
                    self.conversation_id = cid;
                }
            }
            VoiceEventType::TtsEnd => {
                self.stop_streaming();
                if !self.tts_started {
                    match get("url").or_else(|| self.tts_url.clone()) {
                        Some(url) => self.start_tts(url),
                        None => tracing::warn!("TTS_END without url"),
                    }
                }
            }
            VoiceEventType::RunEnd => {
                self.run_ended = true;
                self.stop_streaming();
                // A reply still on its way finishes the run when it is done.
                if self.phase != Phase::Speaking && !self.reply_pending() {
                    self.finish_run().await;
                }
            }
            VoiceEventType::Error => {
                tracing::warn!(
                    "Assist error: {} {}",
                    get("code").unwrap_or_default(),
                    get("message").unwrap_or_default()
                );
                self.stop_streaming();
                self.continue_conversation = false;
                if self.phase != Phase::Idle {
                    self.event("voice_assistant", "stopped").await;
                }
                self.phase = Phase::Idle;
                self.stop_playback();
                self.error_feedback();
            }
            _ => {}
        }
    }

    async fn timer(
        &mut self,
        event: TimerEventType,
        id: String,
        name: String,
        total: u32,
        left: u32,
        active: bool,
    ) {
        tracing::info!("timer {event:?} {id} {name:?} {left}/{total}s");
        let kind = match event {
            TimerEventType::Started => Some("started"),
            TimerEventType::Updated => Some("updated"),
            TimerEventType::Cancelled => Some("cancelled"),
            TimerEventType::Finished => Some("finished"),
            TimerEventType::Unknown(_) => None,
        };
        if let Some(kind) = kind {
            self.event("timer", kind).await;
        }
        match event {
            TimerEventType::Started | TimerEventType::Updated => {
                self.timers.insert(
                    id,
                    HaTimer {
                        name,
                        total: total.max(1),
                        left,
                        active,
                        at: Instant::now(),
                    },
                );
            }
            TimerEventType::Cancelled => {
                self.timers.remove(&id);
            }
            TimerEventType::Finished => {
                self.timers.remove(&id);
                if self.phase != Phase::Idle {
                    self.abort_run(true).await;
                }
                self.ringing = Some((id, Instant::now()));
                self.play_pcm(tone::timer_alarm(), PlaybackKind::Alarm);
            }
            TimerEventType::Unknown(_) => {}
        }
    }

    async fn send_light(&self) {
        self.send(DeviceEvent::LightState {
            on: self.light_on,
            brightness: self.light_brightness,
            rgb: self.light_rgb,
            effect: Some(self.light_effect.name().into()),
        })
        .await;
    }

    // --- buttons --------------------------------------------------------------

    async fn button(&mut self, ev: ButtonEvent) {
        // Home Assistant event entities: press / double_press / long_press.
        match ev {
            ButtonEvent::Press(b) => {
                let now = Instant::now();
                let double = self
                    .last_press
                    .get(&b)
                    .is_some_and(|t| now.duration_since(*t) < DOUBLE_PRESS);
                if double {
                    self.last_press.remove(&b);
                } else {
                    self.last_press.insert(b, now);
                }
                let kind = if double { "double_press" } else { "press" };
                if !self.calibration_feedback(b) {
                    self.event(button_entity(b), kind).await;
                }
            }
            ButtonEvent::LongPress(b) if !self.calibration_feedback(b) => {
                self.event(button_entity(b), "long_press").await;
            }
            _ => {}
        }
        match ev {
            ButtonEvent::Press(b @ (Button::VolumeUp | Button::VolumeDown))
            | ButtonEvent::Repeat(b @ (Button::VolumeUp | Button::VolumeDown)) => {
                if self.calibration.is_some() {
                    // One press, one change: holding must not fire after press.
                    if matches!(ev, ButtonEvent::Press(_)) {
                        self.calibrate(b == Button::VolumeUp).await;
                    }
                    return;
                }
                if self.speaker.is_none() {
                    return;
                }
                let v = self.settings.volume as i16;
                let v = if b == Button::VolumeUp { v + 5 } else { v - 5 }.clamp(0, 100) as u8;
                self.set_volume(v).await;
                self.overlay(Effect::Volume(v));
                if matches!(ev, ButtonEvent::Press(_)) && self.playback.is_none() {
                    self.play_pcm(tone::beep(880.0, 40, 0.3), PlaybackKind::Cue);
                }
            }
            // Acts on what the ring shows.
            ButtonEvent::Press(Button::Action) => {
                if self.ringing.is_some() {
                    tracing::info!("timer alarm dismissed");
                    self.ringing = None;
                    self.stop_playback();
                } else if self.calibration.is_some() {
                    self.end_calibration().await;
                } else if self.phase != Phase::Idle {
                    tracing::info!("run cancelled by button");
                    self.abort_run(true).await;
                } else if matches!(self.playback, Some((_, PlaybackKind::MediaAnnounce, _))) {
                    // The ring shows it speaking: stop it, as for a reply.
                    tracing::info!("announcement stopped by button");
                    self.stop_playback();
                } else if self.muted() {
                    self.error_feedback();
                } else if self.notification_shown() {
                    self.clear_notification().await;
                } else {
                    self.start_run(None).await;
                }
            }
            ButtonEvent::Press(Button::Mute) if !self.opts.has_hw_privacy => {
                // No hardware latch (dev machines): treat the button as a toggle.
                self.sw_muted = !self.sw_muted;
                self.apply_mute().await;
            }
            ButtonEvent::LongPress(b) => tracing::info!("long press: {b:?}"),
            _ => {}
        }
    }

    // --- Home Assistant entities ------------------------------------------------

    async fn event(&self, object_id: &str, event_type: &str) {
        self.send(DeviceEvent::Event {
            object_id: object_id.into(),
            event_type: event_type.into(),
        })
        .await;
    }

    async fn send_numbers(&self) {
        let s = &self.settings;
        for (id, v) in [
            ("volume", s.volume),
            ("reply_volume", s.reply_volume()),
            ("wake_word_sensitivity", s.wake_sensitivity),
            ("led_brightness", s.led_brightness),
        ] {
            self.send(DeviceEvent::NumberState {
                object_id: id.into(),
                value: f32::from(v),
            })
            .await;
        }
    }

    async fn send_media_state(&self) {
        self.send(DeviceEvent::MediaPlayerState {
            state: self.media_state,
            volume: f32::from(self.settings.volume) / 100.0,
            muted: self.media_muted,
        })
        .await;
    }

    /// Speaker volume from any source (number, media player, buttons, API).
    async fn set_volume(&mut self, v: u8) {
        let v = v.min(100);
        if let Some(s) = &self.speaker {
            s.set_volume(v);
        }
        self.settings.volume = v;
        self.apply_reply_volume();
        self.store.save(&self.settings);
        self.send(DeviceEvent::NumberState {
            object_id: "volume".into(),
            value: f32::from(v),
        })
        .await;
        if self.settings.reply_volume.is_none() {
            self.send(DeviceEvent::NumberState {
                object_id: "reply_volume".into(),
                value: f32::from(v),
            })
            .await;
        }
    }

    /// The ring shows the pending notification (see `leds.rs`): idle, and no
    /// timer arc or media player announcement above it. Muted and a ringing
    /// alarm are checked by the caller.
    fn notification_shown(&self) -> bool {
        self.settings.notification
            && self.phase == Phase::Idle
            && !self.timers.values().any(|t| t.active)
            && !matches!(self.playback, Some((_, PlaybackKind::MediaAnnounce, _)))
    }

    /// The "Notification" switch, from Home Assistant: no `cleared` event.
    async fn set_notification(&mut self, on: bool) {
        if self.settings.notification != on {
            tracing::info!("notification: {on}");
            self.settings.notification = on;
            self.store.save(&self.settings);
        }
        self.send(DeviceEvent::SwitchState {
            object_id: "notification".into(),
            on,
        })
        .await;
    }

    /// Someone cleared the notification at the device (Action button,
    /// dashboard): Home Assistant sees the switch turn off and a `cleared`
    /// event, so it can tell this from turning the switch off itself.
    async fn clear_notification(&mut self) {
        if !self.settings.notification {
            return;
        }
        self.set_notification(false).await;
        self.event("notification", "cleared").await;
        self.overlay(Effect::NotificationCleared);
    }

    /// Listen for `ids` from now on and remember them. Home Assistant's choice
    /// is already known to the ESPHome server; one made here is told to it.
    async fn set_wake_words(&mut self, ids: Vec<String>, from_ha: bool) {
        let ids = self.opts.wake_words.sanitize(&ids);
        if !from_ha {
            self.send(DeviceEvent::ActiveWakeWords(ids.clone())).await;
        }
        if ids == self.active_wake && self.settings.wake_words.as_ref() == Some(&ids) {
            return;
        }
        tracing::info!("wake words: {:?}", self.opts.wake_words.phrases(&ids));
        self.active_wake = ids.clone();
        self.settings.wake_words = Some(ids.clone());
        self.store.save(&self.settings);
        let Some(tx) = self.opts.keywords.clone() else {
            return;
        };
        self.keyword_generation += 1;
        let generation = self.keyword_generation;
        let catalogue = self.opts.wake_words.clone();
        tokio::task::spawn_blocking(move || {
            let keywords = catalogue.load(&ids);
            let _ = tx.send(KeywordSet {
                generation,
                keywords,
            });
        });
    }

    async fn set_wake_word(&mut self, on: bool) {
        tracing::info!("wake word detection: {on}");
        self.settings.wake_word = on;
        self.store.save(&self.settings);
        if !on && self.calibration.is_some() {
            self.end_calibration().await;
        }
        self.send(DeviceEvent::SwitchState {
            object_id: "wake_word".into(),
            on,
        })
        .await;
    }

    async fn send_calibration_switch(&self, on: bool) {
        self.send(DeviceEvent::SwitchState {
            object_id: "wake_word_calibration".into(),
            on,
        })
        .await;
    }

    /// Calibration on the device, from the dashboard or Home Assistant.
    async fn set_calibration(&mut self, on: bool) -> Result<(), String> {
        if !on {
            self.end_calibration().await;
            return Ok(());
        }
        if self.calibration.is_some() {
            return Ok(());
        }
        if self.muted() {
            return Err("the microphones are muted".to_string());
        }
        if !self.settings.wake_word {
            return Err("the wake word is turned off".to_string());
        }
        if self.ringing.is_some() || self.phase != Phase::Idle {
            return Err("a conversation or timer alarm is already active".to_string());
        }
        tracing::info!(
            "calibration: started at sensitivity {}",
            self.settings.wake_sensitivity
        );
        self.calibration = Some(Calibration::start(
            self.settings.wake_sensitivity,
            Instant::now(),
        ));
        self.send_calibration_switch(true).await;
        Ok(())
    }

    /// End calibration; the sensitivity keeps its value.
    async fn end_calibration(&mut self) {
        if self.calibration.take().is_some() {
            tracing::info!("calibration: ended");
        }
        self.send_calibration_switch(false).await;
    }

    /// While calibrating, the volume buttons give feedback instead of firing
    /// their Home Assistant events.
    fn calibration_feedback(&self, b: Button) -> bool {
        self.calibration.is_some() && matches!(b, Button::VolumeUp | Button::VolumeDown)
    }

    /// A + or − press on the device while calibrating: move the sensitivity by
    /// what the press refers to, or say why nothing changed.
    async fn calibrate(&mut self, raise: bool) {
        let current = self.settings.wake_sensitivity;
        let now = Instant::now();
        let result = match &mut self.calibration {
            Some(c) if raise => c.raise(current, now),
            Some(c) => c.lower(current, now),
            None => return,
        };
        match result {
            Ok(to) => {
                tracing::info!("calibration: sensitivity {current} -> {to}");
                self.number("wake_word_sensitivity".into(), f32::from(to))
                    .await;
            }
            Err(why) => {
                tracing::info!("calibration: {why:?}, no change");
                self.error_feedback();
            }
        }
    }

    async fn number(&mut self, object_id: String, value: f32) {
        let v = value.round().clamp(0.0, 100.0) as u8;
        match object_id.as_str() {
            "volume" => {
                self.set_volume(v).await;
                self.overlay(Effect::Volume(v));
                return;
            }
            "reply_volume" => {
                self.settings.reply_volume = Some(v);
                self.apply_reply_volume();
            }
            "wake_word_sensitivity" => {
                self.settings.wake_sensitivity = v;
                let t = self.settings.wake_threshold();
                self.shared.threshold.store(t.to_bits(), Ordering::SeqCst);
                tracing::info!("wake word sensitivity {v} (threshold {t:.3})");
            }
            "led_brightness" => self.settings.led_brightness = v,
            other => {
                tracing::warn!("unknown number {other}");
                return;
            }
        }
        self.store.save(&self.settings);
        self.send(DeviceEvent::NumberState {
            object_id,
            value: f32::from(v),
        })
        .await;
    }

    /// Replies and announcements play on the voice lane under the master volume,
    /// so the reply volume is applied as a voice-lane gain relative to it (a
    /// reply cannot be louder than the main volume).
    fn apply_reply_volume(&self) {
        let Some(s) = &self.speaker else { return };
        let master = pinna_audio::volume::volume_gain(self.settings.volume);
        let reply = pinna_audio::volume::volume_gain(self.settings.reply_volume());
        let pct = if master > 0.0 {
            (reply / master * 100.0).round().clamp(0.0, 100.0) as u8
        } else {
            0
        };
        s.set_voice_gain(pct);
    }

    /// The Home Assistant media player state from the media lane and the voice lane.
    fn update_media_state(&mut self) {
        self.media_state = if matches!(self.playback, Some((_, PlaybackKind::MediaAnnounce, _))) {
            MediaPlayerStateKind::Announcing
        } else {
            match self.media_phase {
                MediaPhase::Idle => MediaPlayerStateKind::Idle,
                MediaPhase::Buffering | MediaPhase::Playing => MediaPlayerStateKind::Playing,
                MediaPhase::Paused => MediaPlayerStateKind::Paused,
            }
        };
    }

    /// "Cancel request": dismiss an alarm, abort a run, or stop what is playing.
    async fn cancel(&mut self) {
        if self.ringing.is_some() {
            self.ringing = None;
            self.stop_playback();
        } else if self.phase != Phase::Idle {
            self.abort_run(true).await;
        } else {
            self.stop_playback();
            if let Some(s) = &self.speaker {
                s.media().stop();
            }
        }
    }

    async fn media(
        &mut self,
        command: Option<MediaPlayerCommand>,
        volume: Option<f32>,
        media_url: Option<String>,
        announcement: bool,
    ) {
        tracing::info!(
            "media player: command {command:?} volume {volume:?} url {media_url:?} announce {announcement}"
        );
        if let Some(v) = volume {
            self.set_volume((v * 100.0).round().clamp(0.0, 100.0) as u8)
                .await;
        }
        let media = self.speaker.as_ref().map(|s| s.media());
        if let Some(url) = media_url.filter(|u| !u.is_empty()) {
            if announcement {
                // Announcements play on the voice lane, which ducks the music.
                if self.phase != Phase::Idle {
                    self.abort_run(true).await;
                }
                self.play_urls(vec![url], PlaybackKind::MediaAnnounce);
                self.update_media_state();
            } else if let Some(media) = media.clone() {
                tokio::spawn(async move {
                    if let Err(e) = media.play_url(&url).await {
                        tracing::warn!("media {url}: {e:#}");
                    }
                });
            }
        }
        match command {
            Some(MediaPlayerCommand::Stop) => {
                if let Some(m) = &media {
                    m.stop();
                }
                if matches!(self.playback, Some((_, PlaybackKind::MediaAnnounce, _))) {
                    self.stop_playback();
                }
            }
            Some(MediaPlayerCommand::Pause) => {
                if let Some(m) = &media {
                    m.pause();
                }
            }
            Some(MediaPlayerCommand::Play) => {
                if let Some(m) = &media {
                    m.resume();
                }
            }
            Some(MediaPlayerCommand::Mute) | Some(MediaPlayerCommand::Unmute) => {
                if let Some(m) = &media {
                    m.set_muted(command == Some(MediaPlayerCommand::Mute));
                }
            }
            Some(MediaPlayerCommand::VolumeUp) => {
                let v = self.settings.volume.saturating_add(5).min(100);
                self.set_volume(v).await;
            }
            Some(MediaPlayerCommand::VolumeDown) => {
                let v = self.settings.volume.saturating_sub(5);
                self.set_volume(v).await;
            }
            _ => {}
        }
    }

    // --- runs -----------------------------------------------------------------

    async fn start_run(&mut self, wake_phrase: Option<String>) {
        if !self.voice_subscribed {
            tracing::warn!("no Home Assistant voice subscription; cannot start a run");
            self.error_feedback();
            return;
        }
        if self.conversation_id.is_empty() || !self.continue_conversation {
            self.conversation_id = format!("{:016x}", fastrand::u64(..));
        }
        self.continue_conversation = false;
        self.run_ended = false;
        self.tts_started = false;
        self.tts_url = None;
        self.speech_ended = None;
        self.phase = Phase::Starting(Instant::now());
        // Open the speaker while the user talks: it takes ~1.4 s on the Dot.
        self.warm_speaker();
        // The wake word was just spoken: read its direction before the chime.
        self.talker.clear();
        self.track_talker();
        tracing::info!("starting Assist run {}", self.conversation_id);
        self.send(DeviceEvent::VoiceStart {
            conversation_id: self.conversation_id.clone(),
            wake_word_phrase: wake_phrase,
            use_vad: true,
        })
        .await;
        if self.opts.wake_sound {
            self.play_pcm(tone::wake_chime(), PlaybackKind::Cue);
        }
    }

    fn stop_streaming(&self) {
        self.shared.streaming.store(false, Ordering::SeqCst);
    }

    /// The microphone stopped: wait for the reply. The speaker is warmed
    /// again so a long utterance can't have outlasted its idle timeout.
    fn end_of_speech(&mut self, now: Instant) {
        self.phase = Phase::Thinking(now);
        self.speech_ended = Some(now);
        self.warm_speaker();
    }

    fn warm_speaker(&self) {
        if let Some(s) = &self.speaker {
            s.warm();
        }
    }

    /// A reply is fetching or playing.
    fn reply_pending(&self) -> bool {
        matches!(self.playback, Some((_, PlaybackKind::Tts, _)))
    }

    async fn abort_run(&mut self, tell_ha: bool) {
        self.stop_streaming();
        if self.phase != Phase::Idle {
            self.event("voice_assistant", "stopped").await;
        }
        if tell_ha && self.phase != Phase::Idle {
            self.send(DeviceEvent::VoiceStop).await;
        }
        self.stop_playback();
        self.phase = Phase::Idle;
        self.continue_conversation = false;
    }

    async fn finish_run(&mut self) {
        self.stop_streaming();
        self.phase = Phase::Idle;
        self.event("voice_assistant", "finished").await;
        if self.continue_conversation && !self.muted() {
            tracing::info!("continuing conversation {}", self.conversation_id);
            self.continue_conversation = true;
            self.start_run(None).await;
        } else {
            self.continue_conversation = false;
        }
    }

    async fn fail(&mut self) {
        self.stop_streaming();
        self.phase = Phase::Idle;
        self.continue_conversation = false;
        self.error_feedback();
    }

    fn error_feedback(&mut self) {
        self.overlay(Effect::Error);
        if self.playback.is_none() {
            self.play_pcm(tone::error_tone(), PlaybackKind::Cue);
        }
    }

    async fn apply_mute(&mut self) {
        if self.settings.muted != self.sw_muted {
            self.settings.muted = self.sw_muted;
            self.store.save(&self.settings);
        }
        let m = self.muted();
        self.shared.muted.store(m, Ordering::SeqCst);
        if m && self.calibration.is_some() {
            self.end_calibration().await;
        }
        if m && self.phase != Phase::Idle {
            self.abort_run(true).await;
        }
        if let Err(e) = pinna_hal::led::set_mute_lamp(m) {
            tracing::debug!("mute lamp: {e}");
        }
        self.send(DeviceEvent::MuteState(m)).await;
    }

    // --- playback ---------------------------------------------------------------

    fn start_tts(&mut self, url: String) {
        self.tts_started = true;
        // Still thinking until the reply sounds (`playback_started`), and
        // THINK_TIMEOUT from now for it to get there.
        self.phase = Phase::Thinking(Instant::now());
        self.play_urls(vec![url], PlaybackKind::Tts);
    }

    /// The reply's first audio is sounding.
    async fn playback_started(&mut self, token: u64) {
        if !matches!(&self.playback, Some((t, PlaybackKind::Tts, _)) if *t == token) {
            return;
        }
        if let Phase::Thinking(requested) = self.phase {
            let now = Instant::now();
            let sent = (now - requested).as_secs_f32();
            match self.speech_ended {
                Some(end) => tracing::info!(
                    "reply: first audio {:.2} s after the end of speech ({sent:.2} s after Home Assistant started it)",
                    (now - end).as_secs_f32()
                ),
                None => {
                    tracing::info!("reply: first audio {sent:.2} s after Home Assistant started it")
                }
            }
        }
        self.phase = Phase::Speaking;
        self.event("voice_assistant", "replying").await;
    }

    fn stop_playback(&mut self) {
        if let Some((_, kind, task)) = self.playback.take() {
            self.speaker_quiet_since = Instant::now();
            task.abort();
            if kind == PlaybackKind::MediaAnnounce {
                self.update_media_state();
            }
            if let Some(s) = &self.speaker {
                s.stop();
            }
        }
    }

    fn begin_playback(&mut self, kind: PlaybackKind) -> u64 {
        self.stop_playback();
        let token = self.next_token;
        self.next_token += 1;
        let _ = kind;
        token
    }

    fn play_pcm(&mut self, pcm: pinna_audio::Pcm, kind: PlaybackKind) {
        let token = self.begin_playback(kind.clone());
        let speaker = self.speaker.clone();
        let tx = self.self_tx.clone();
        let task = tokio::spawn(async move {
            let ok = match speaker {
                Some(s) => s.play_pcm(pcm).await.is_ok(),
                None => false,
            };
            let _ = tx.send(Input::PlaybackDone { token, ok }).await;
        });
        self.playback = Some((token, kind, task));
    }

    fn play_urls(&mut self, urls: Vec<String>, kind: PlaybackKind) {
        let token = self.begin_playback(kind.clone());
        let speaker = self.speaker.clone();
        let tx = self.self_tx.clone();
        let task = tokio::spawn(async move {
            let ok = match speaker {
                Some(s) => play_in_turn(&s, &urls, token, &tx).await,
                None => {
                    tracing::warn!("no speaker to play {urls:?}");
                    false
                }
            };
            let _ = tx.send(Input::PlaybackDone { token, ok }).await;
        });
        self.playback = Some((token, kind, task));
    }

    async fn playback_done(&mut self, token: u64, ok: bool) {
        let Some((current, kind, _)) = &self.playback else {
            return;
        };
        if *current != token {
            return;
        }
        let kind = kind.clone();
        self.playback = None;
        self.speaker_quiet_since = Instant::now();
        match kind {
            PlaybackKind::Tts => {
                // ESPHome devices report the end of the reply this way too;
                // Home Assistant keeps the satellite "responding" until then.
                self.send(DeviceEvent::AnnounceFinished { success: ok })
                    .await;
                if self.run_ended {
                    self.finish_run().await;
                } else {
                    // Audio finished before RUN_END; wait for it.
                    self.phase = Phase::Thinking(Instant::now());
                }
            }
            PlaybackKind::Announce { start_conversation } => {
                self.send(DeviceEvent::AnnounceFinished { success: ok })
                    .await;
                self.phase = Phase::Idle;
                if start_conversation && ok {
                    self.start_run(None).await;
                }
            }
            PlaybackKind::Alarm => {
                if let Some((_, since)) = &self.ringing {
                    if since.elapsed() < RING_TIMEOUT && ok {
                        self.play_pcm(tone::timer_alarm(), PlaybackKind::Alarm);
                    } else {
                        self.ringing = None;
                    }
                }
            }
            PlaybackKind::MediaAnnounce => self.update_media_state(),
            PlaybackKind::Cue => {}
        }
    }

    // --- housekeeping -------------------------------------------------------------

    async fn tick(&mut self) {
        let now = Instant::now();
        match self.phase {
            Phase::Starting(t) if now - t > START_TIMEOUT => {
                tracing::warn!("Home Assistant did not answer the run start");
                self.send(DeviceEvent::VoiceStop).await;
                self.fail().await;
            }
            Phase::Listening(t) if now - t > LISTEN_TIMEOUT => {
                tracing::warn!("no end of speech from Home Assistant; stopping the microphone");
                self.stop_streaming();
                self.end_of_speech(now);
            }
            Phase::Thinking(t) if now - t > THINK_TIMEOUT => {
                tracing::warn!("Assist run timed out");
                self.abort_run(true).await;
                self.error_feedback();
            }
            _ => {}
        }
        if self.calibration.as_ref().is_some_and(|c| c.expired(now)) {
            self.end_calibration().await;
        }
        if !self.boot_done && now - self.booted_at > BOOT_ANIMATION {
            self.boot_done = true;
        }
        if self
            .led_test
            .is_some_and(|t| now - t >= LED_TEST_STEP * LED_TEST_EFFECTS.len() as u32)
        {
            self.led_test = None;
        }
        self.track_talker();
    }

    /// While listening, point the ring at the talker's beam (the previous firmware's
    /// directional listening ring). Called at run start and every tick.
    fn track_talker(&mut self) {
        let listening = matches!(self.phase, Phase::Starting(_) | Phase::Listening(_));
        if !listening || !self.settings.listening_direction {
            self.talker.clear();
            return;
        }
        // Our own chime or reply would point the ring at the Dot or a wall.
        if self.playback.is_some() || self.speaker_quiet_since.elapsed() < SELF_SOUND_HOLD {
            return;
        }
        let snapshot = self.beams.read();
        if let Some(s) = &snapshot {
            tracing::debug!(
                selected_deg = s.selected_deg,
                contrast_db = format!("{:.1}", s.contrast_db()),
                "talker beam"
            );
        }
        self.talker.update(snapshot.as_ref(), Instant::now());
    }

    fn overlay(&mut self, effect: Effect) {
        self.leds
            .send_modify(|s| s.overlay = Some((effect, Instant::now())));
    }

    /// Recompute derived outputs (LED state, wake word gate) from the state.
    fn refresh(&mut self) {
        let muted = self.muted();
        let led_test = self.led_test.map(|t| {
            let step = (t.elapsed().as_millis() / LED_TEST_STEP.as_millis()) as usize;
            LED_TEST_EFFECTS[step.min(LED_TEST_EFFECTS.len() - 1)]
        });
        let base = if let Some(effect) = led_test {
            effect
        } else if !self.boot_done {
            Effect::Boot
        } else if muted {
            Effect::Muted
        } else if self.ringing.is_some() {
            Effect::TimerRinging
        } else if self.calibration.is_some() && self.phase == Phase::Idle {
            // Calibration shows where the sensitivity stands.
            Effect::Calibration(self.settings.wake_sensitivity)
        } else {
            match self.phase {
                // A media player announcement speaks without a run.
                Phase::Idle
                    if matches!(self.playback, Some((_, PlaybackKind::MediaAnnounce, _))) =>
                {
                    Effect::Speaking
                }
                Phase::Idle => Effect::Idle,
                Phase::Starting(_) | Phase::Listening(_) => self
                    .talker
                    .direction()
                    .map_or(Effect::Listening, Effect::ListeningAt),
                Phase::Thinking(_) => Effect::Thinking,
                Phase::Speaking => Effect::Speaking,
            }
        };
        let light = self.light_on.then(|| {
            let (r, g, b) = self.light_rgb;
            let k = 255.0 * self.light_brightness;
            Rgb {
                r: (r * k).round().clamp(0.0, 255.0) as u8,
                g: (g * k).round().clamp(0.0, 255.0) as u8,
                b: (b * k).round().clamp(0.0, 255.0) as u8,
            }
        });
        let now = Instant::now();
        let timer = self
            .timers
            .values()
            .filter(|t| t.active)
            .map(|t| t.remaining(now) as f32 / t.total as f32)
            .reduce(f32::min);
        let (effect, brightness) = (self.light_effect, self.settings.led_brightness);
        let notification = self.settings.notification;
        self.leds.send_if_modified(|s| {
            let changed = s.base != base
                || s.light != light
                || s.timer != timer
                || s.notification != notification
                || s.light_effect != effect
                || s.brightness != brightness;
            s.base = base;
            s.light = light;
            s.timer = timer;
            s.notification = notification;
            s.light_effect = effect;
            s.brightness = brightness;
            changed
        });
        let duck = matches!(
            self.phase,
            Phase::Starting(_) | Phase::Listening(_) | Phase::Thinking(_)
        ) || self.ringing.is_some();
        if duck != self.ducked {
            self.ducked = duck;
            if let Some(s) = &self.speaker {
                s.duck(duck);
            }
        }
        // Calibration wakes start no request, so they don't need a pipeline.
        let wake = self.phase == Phase::Idle
            && !muted
            && self.settings.wake_word
            && (self.voice_subscribed || self.calibration.is_some())
            && self.ringing.is_none()
            && self.boot_done;
        self.shared.wake_enabled.store(wake, Ordering::SeqCst);
        let snapshot = self.snapshot();
        self.status.send_if_modified(|s| {
            let changed = *s != snapshot;
            *s = snapshot;
            changed
        });
    }
}

/// Object id of a button's Home Assistant event entity.
fn button_entity(b: Button) -> &'static str {
    match b {
        Button::VolumeUp => "volume_up",
        Button::VolumeDown => "volume_down",
        Button::Mute => "microphone_mute",
        Button::Action => "action",
    }
}

/// Play `urls` one after another on the voice lane, each one opened while
/// the one before it plays. The first audio is reported as
/// [`Input::PlaybackStarted`]; false if any of them failed.
async fn play_in_turn(
    speaker: &Speaker,
    urls: &[String],
    token: u64,
    tx: &mpsc::Sender<Input>,
) -> bool {
    let mut ok = true;
    let mut queued = Vec::new();
    for url in urls {
        match speaker.play_url(url).await {
            Ok(p) => queued.push((url, p)),
            Err(e) => {
                tracing::warn!("playing {url}: {e}");
                ok = false;
                break;
            }
        }
    }
    let mut first = true;
    for (url, p) in queued {
        if p.started.await.is_ok() && std::mem::take(&mut first) {
            let _ = tx.send(Input::PlaybackStarted { token }).await;
        }
        match p.done.await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                tracing::warn!("playing {url}: {e}");
                ok = false;
            }
            Err(_) => ok = false,
        }
    }
    ok
}
