//! The device's state machine: wake word / button → Home Assistant Assist run
//! over the ESPHome API → TTS playback, plus timers, announcements, mute, the
//! light entity and volume. Single task, all inputs arrive on one channel.

mod local;
pub(crate) mod messages;
mod mode;
#[cfg(test)]
mod tests;
mod timers;

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime};

use pinna_audio::beamview::{self, BeamViewReader};
use pinna_audio::{MediaPhase, MediaState, STREAM_URL, Speaker, tone};
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
use crate::settings::{FILE_MODE, Mode, Settings};
use crate::store::Store;
use crate::wakewords::{Catalogue, KeywordSet};
#[cfg(test)]
pub(crate) use local::NoopSpawner;
pub(crate) use local::{DeviceFacts, LocalOptions, LocalStart, RunSpawner};
pub(crate) use messages::{Delivery, Taken};
use timers::{Origin, Timer};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RunId(pub u64);

/// The standalone readiness and latest failure shown to the owner.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AssistStatus {
    pub ready: bool,
    pub slots: pinna_assist::presets::Slots,
    pub label: String,
    /// Cleared once a request is answered again.
    pub last_error: Option<LastFailure>,
}

/// The last standalone request that failed, and when.
#[derive(Clone, Debug, PartialEq)]
pub struct LastFailure {
    pub error: pinna_assist::error::ProviderError,
    pub at: SystemTime,
}

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
    /// The standalone run receiving microphone frames.
    pub mic: crate::mic::MicTap,
    /// The wake detector's latest noise energy; zero means unknown.
    pub noise_energy: AtomicU64,
    /// What `/metrics` counts (ADR 0020).
    pub counters: crate::metrics::Counters,
}

/// Someone must press Action (ADR 0029): the dashboard asked for presence at
/// the device. The Brain takes the next Action press for it.
#[derive(Debug)]
pub struct Presence {
    /// When the wait ends.
    pub until: Instant,
    /// Answered by the press.
    pub pressed: oneshot::Sender<()>,
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
    /// Wake words whose classifier didn't load, and why.
    WakeWordsFailed(Vec<(String, String)>),
    Assist {
        run: RunId,
        ev: pinna_assist::run::RunEvent,
    },
    RunTool {
        run: RunId,
        cmd: ApiCommand,
    },
    /// A message from another member of the satellite group (ADR 0026).
    Message(Delivery),
    /// The dashboard waits for a press of Action (ADR 0029).
    Presence(Presence),
}

/// Answer to an API command: the state right after it was applied, or why it was refused.
pub type ApiReply = oneshot::Sender<Result<Snapshot, String>>;

#[derive(Debug)]
pub enum ApiCommand {
    SetVolume(u8, ApiReply),
    /// A bridge's sender volume for live streams (ADR 0011).
    SetStreamVolume(u8, ApiReply),
    /// The track a bridge's stream plays, under its source name (ADR 0011).
    SetStreamMetadata(String, pinna_audio::Track, ApiReply),
    SetMute(bool, ApiReply),
    /// The `mute_microphone` tool: closes the hardware latch when there is
    /// one, so the Microphone off button can open it again.
    VoiceMute(ApiReply),
    /// Switch the mode once the satellite is idle, so a reply that confirms
    /// it still plays; at once when it already is.
    SwitchMode(Mode, ApiReply),
    StartConversation(ApiReply),
    StopConversation(ApiReply),
    /// `cancel_request` from another member of the satellite group
    /// (ADR 0026): silence the alarm, end the request, or stop a message or
    /// `speak` that plays. Music plays on.
    CancelForGroup(ApiReply),
    LedTest(ApiReply),
    UpdateSettings(SettingsPatch, ApiReply),
    ClearNotification(ApiReply),
    StartCalibration(ApiReply),
    StopCalibration(ApiReply),
    StartBluetoothPairing(ApiReply),
    StopBluetoothPairing(ApiReply),
    /// Already checked (`bluetooth::valid_address`).
    ForgetBluetoothDevice(String, ApiReply),
    /// From the Bluetooth bridge.
    ReportBluetooth(crate::bluetooth::Bridge, ApiReply),
    SetRingLight(RingLightPatch, ApiReply),
    /// Play, Pause, Stop, Mute or Unmute, and the media player's volume
    /// (0..=100); at least one of them.
    ControlMedia(Option<MediaPlayerCommand>, Option<u8>, ApiReply),
    /// Already checked: http(s), not too long.
    PlayMedia(String, ApiReply),
    /// Reply once audio is decoding, or with the reason it failed.
    PlayMediaChecked(String, oneshot::Sender<Result<(), String>>),
    SetTimer {
        name: Option<String>,
        seconds: u32,
        reply: oneshot::Sender<Result<String, String>>,
    },
    /// A timer id, or its name when the id is unknown.
    CancelTimer(String, ApiReply),
    /// Say a text with the assistant's voice (Play sample, `/mcp speak`).
    /// Replies once it sounds, or with why it can't.
    Speak(String, oneshot::Sender<Result<(), String>>),
    /// A question typed on the dashboard; standalone mode only. Replies
    /// once the request has started.
    AskText(String, ApiReply),
    /// A custom wake word (ADR 0013). `staged` is the model in the wake words
    /// directory, already tested; the brain checks the rules again, moves it
    /// to its place and replies with the id. The word isn't made active.
    AddWakeWord {
        phrase: String,
        staged: std::path::PathBuf,
        format: pinna_wakeword::ModelFormat,
        sha256: String,
        reply: oneshot::Sender<Result<String, String>>,
    },
    /// Remove a custom word; it leaves the active words if it was one.
    RemoveWakeWord(String, ApiReply),
}

impl ApiCommand {
    fn refuse(self, why: &str) {
        match self {
            Self::SetVolume(_, reply)
            | Self::SetStreamVolume(_, reply)
            | Self::SetStreamMetadata(_, _, reply)
            | Self::SetMute(_, reply)
            | Self::VoiceMute(reply)
            | Self::SwitchMode(_, reply)
            | Self::StartConversation(reply)
            | Self::StopConversation(reply)
            | Self::CancelForGroup(reply)
            | Self::LedTest(reply)
            | Self::UpdateSettings(_, reply)
            | Self::ClearNotification(reply)
            | Self::StartCalibration(reply)
            | Self::StopCalibration(reply)
            | Self::StartBluetoothPairing(reply)
            | Self::StopBluetoothPairing(reply)
            | Self::ForgetBluetoothDevice(_, reply)
            | Self::ReportBluetooth(_, reply)
            | Self::SetRingLight(_, reply)
            | Self::ControlMedia(_, _, reply)
            | Self::PlayMedia(_, reply)
            | Self::CancelTimer(_, reply)
            | Self::RemoveWakeWord(_, reply)
            | Self::AskText(_, reply) => {
                let _ = reply.send(Err(why.into()));
            }
            Self::PlayMediaChecked(_, reply) | Self::Speak(_, reply) => {
                let _ = reply.send(Err(why.into()));
            }
            Self::SetTimer { reply, .. } | Self::AddWakeWord { reply, .. } => {
                let _ = reply.send(Err(why.into()));
            }
        }
    }
}

/// The media player, as the API shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct MediaSnapshot {
    pub state: MediaPlayerStateKind,
    pub url: Option<String>,
    pub stream: bool,
    pub muted: bool,
    pub error: Option<String>,
    /// The URL whose play failed, with `error`; `url` is what still plays.
    pub error_url: Option<String>,
    pub available: bool,
    /// The media player's volume: the stream's while a stream plays.
    pub volume: u8,
    /// What plays, as its source names it.
    pub track: pinna_audio::Track,
    /// The source a stream's header names, while a stream plays.
    pub source: String,
}

impl Default for MediaSnapshot {
    fn default() -> Self {
        Self {
            state: MediaPlayerStateKind::Idle,
            url: None,
            stream: false,
            muted: false,
            error: None,
            error_url: None,
            available: false,
            volume: 0,
            track: pinna_audio::Track::default(),
            source: String::new(),
        }
    }
}

/// The ring light: Home Assistant's "LED ring" light entity.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RingLight {
    pub on: bool,
    /// 0..=1.
    pub brightness: f32,
    /// 0..=1 each.
    pub rgb: (f32, f32, f32),
    pub effect: LightEffect,
}

impl Default for RingLight {
    fn default() -> Self {
        Self {
            on: false,
            brightness: 1.0,
            rgb: (1.0, 1.0, 1.0),
            effect: LightEffect::Solid,
        }
    }
}

/// Changes to the ring light; `None` leaves a field as it is.
#[derive(Debug, Default)]
pub struct RingLightPatch {
    pub on: Option<bool>,
    pub brightness: Option<f32>,
    pub rgb: Option<(f32, f32, f32)>,
    pub effect: Option<LightEffect>,
}

/// Settings to change from the API; `None` leaves a setting as it is.
#[derive(Debug, Default)]
pub struct SettingsPatch {
    pub mode: Option<Mode>,
    pub wake_word: Option<bool>,
    pub wake_sensitivity: Option<u8>,
    pub led_brightness: Option<u8>,
    /// Already checked against the catalogue (`Catalogue::validate`).
    pub wake_words: Option<Vec<String>>,
    /// Already checked (`settings::check_friendly_name`).
    pub friendly_name: Option<String>,
    /// Already checked (`settings::check_hostname`).
    pub hostname: Option<String>,
    /// Already checked against the satellite's zones.
    pub time_zone: Option<String>,
    pub wake_sound: Option<bool>,
    /// Already checked (`settings::check_url`); `Some(None)` clears it.
    pub url: Option<Option<String>>,
    pub airplay: Option<bool>,
    pub bluetooth: Option<bool>,
    pub sendspin: Option<bool>,
    /// Stop following `volume` at this level.
    pub reply_volume: Option<u8>,
    /// `true`: follow `volume` again. `false`: keep the current level.
    /// Never `true` together with `reply_volume` (the API refuses that).
    pub reply_follows_volume: Option<bool>,
    pub listening_direction: Option<bool>,
    /// Never set while pinnad.toml sets `[update] manifest` (the API refuses it).
    pub update_channel: Option<crate::updates::UpdateChannel>,
    pub group_messages: Option<bool>,
    pub install_updates: Option<crate::updates::auto::InstallMode>,
    /// Already checked: a window has a start and an end that differ.
    pub install_window: Option<crate::updates::auto::Window>,
}

/// A wake or a near miss, for the dashboard's recent list.
#[derive(Debug, Clone, PartialEq)]
pub struct WakeRecord {
    pub unix_ms: u64,
    pub woke: bool,
    pub score: f32,
    pub reached_threshold: bool,
    /// The word that woke it; empty for a near miss.
    pub phrase: String,
}

impl WakeRecord {
    fn now(woke: bool, score: f32, reached_threshold: bool, phrase: String) -> Self {
        let unix_ms = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64);
        Self {
            unix_ms,
            woke,
            score,
            reached_threshold,
            phrase,
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
    pub mode: Mode,
    pub assist: AssistStatus,
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
    /// The words on offer, custom ones and failed ones included.
    pub wake_words: Arc<Catalogue>,
    /// The ESPHome node name, which is also the hostname (ADR 0010).
    pub name: String,
    /// The name Home Assistant shows, and whether it is still the default.
    pub friendly_name: String,
    pub default_name: bool,
    pub wake_sound: bool,
    /// The ESPHome API is encrypted.
    pub encrypted: bool,
    /// The ring runs its test pattern.
    pub led_test: bool,
    /// The AirPlay receiver as it is (ADR 0011).
    pub airplay: crate::airplay::Report,
    /// Bluetooth as it is (ADR 0011).
    pub bluetooth: crate::bluetooth::Report,
    /// The Sendspin player as it is (ADR 0027).
    pub sendspin: crate::sendspin::Report,
    pub ring_light: RingLight,
    pub media: MediaSnapshot,
    /// The "Updates" setting in effect (ADR 0022).
    pub update_channel: crate::updates::UpdateChannel,
    /// Messages from the satellite group waiting to be said (ADR 0026).
    pub held_messages: u32,
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
            mode: Mode::HomeAssistant,
            assist: AssistStatus::default(),
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
            wake_words: Arc::default(),
            name: String::new(),
            friendly_name: String::new(),
            default_name: true,
            wake_sound: false,
            encrypted: false,
            led_test: false,
            airplay: Default::default(),
            bluetooth: Default::default(),
            sendspin: Default::default(),
            ring_light: RingLight::default(),
            media: MediaSnapshot::default(),
            update_channel: crate::updates::UpdateChannel::Off,
            held_messages: 0,
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
    Reply,
    Speak,
    Announce {
        start_conversation: bool,
    },
    /// Media player `play_media` with `announcement` set.
    MediaAnnounce,
    Alarm,
    Cue,
    /// A message from another member of the satellite group (ADR 0026).
    Message,
}

/// Two presses of the same button within this window form a double press.
const DOUBLE_PRESS: Duration = Duration::from_millis(400);
/// Wakes and near misses kept for the dashboard.
const RECENT_WAKES: usize = 20;

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
    /// What the dashboard's settings fall back to (ADR 0007).
    pub defaults: Defaults,
    /// The mDNS record, rewritten when the satellite is renamed; `None`
    /// without mDNS.
    pub mdns: Option<pinna_esphome::DeviceConfig>,
    /// The ESPHome API is encrypted at start.
    pub encrypted: bool,
    pub has_hw_privacy: bool,
    /// Where a voice mute closes the hardware latch: the driver's
    /// `privacy_trigger`, or the virtual Dot's latch file.
    pub privacy_trigger: Vec<std::path::PathBuf>,
    /// Where runtime settings are persisted.
    pub settings_file: std::path::PathBuf,
    /// AirPlay can run: there is a speaker and a stream socket.
    pub airplay: bool,
    /// Bluetooth can run: AirPlay's needs, and a Bluetooth controller.
    pub bluetooth: bool,
    /// The Sendspin player (ADR 0027), started in `run`; `None` in tests
    /// that don't need it. Without a speaker the switch is refused.
    pub sendspin: Option<crate::sendspin::Setup>,
    /// Settings at startup (already loaded from `settings_file`).
    pub settings: Settings,
    /// Update channels and installs (ADR 0022); `None` in tests that don't
    /// need them.
    pub updates: Option<Arc<crate::updates::Updater>>,
}

/// The values `settings.toml` falls back to: pinnad.toml's, or the hardware's.
#[derive(Debug, Clone)]
pub struct Defaults {
    /// The node name without a `hostname` setting.
    pub node_name: String,
    pub friendly_name: String,
    /// The name is the model and MAC suffix: nobody chose one.
    pub default_name: bool,
    pub wake_sound: bool,
    /// "Updates" without a setting (`updates::default_channel`).
    pub update_channel: crate::updates::UpdateChannel,
}

pub struct Brain {
    esp: EsphomeHandle,
    speaker: Option<Arc<Speaker>>,
    shared: Arc<Shared>,
    leds: watch::Sender<LedState>,
    status: watch::Sender<Snapshot>,
    self_tx: mpsc::Sender<Input>,
    opts: Options,
    local: local::Local,
    /// Writes `opts.mdns` after boot, one change after another.
    mdns_writer: Arc<crate::identity::RecordWriter>,

    phase: Phase,
    conversation_id: String,
    continue_conversation: bool,
    run_ended: bool,
    /// A run was stopped before Home Assistant ended it. Home Assistant still
    /// ends a cancelled run with RUN_END, often after the next run started;
    /// the stopped run's events up to then must not act on the new one.
    stale_run: bool,
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
    store: Store<Settings>,
    last_press: HashMap<Button, Instant>,
    media_state: MediaPlayerStateKind,
    media_muted: bool,
    /// Last state reported by the media lane.
    media_phase: MediaPhase,
    /// The media lane is on a live stream (AirPlay, Bluetooth).
    media_stream: bool,
    /// What the media lane plays or last played, and why it last failed.
    media_url: Option<String>,
    media_error: Option<String>,
    /// The URL whose play failed, with `media_error`.
    media_error_url: Option<String>,
    /// What the media lane plays, as its source names it, and a stream's
    /// source.
    media_track: pinna_audio::Track,
    media_source: String,
    /// The bridges' sender volume for live streams, 0..=100 (not saved:
    /// each bridge sets its own before it connects).
    stream_volume: u8,
    ducked: bool,

    timers: HashMap<String, Timer>,
    timer_store: timers::TimerStore,
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
    /// Applies the AirPlay switch, one change at a time (started in `run`).
    airplay: Option<mpsc::UnboundedSender<Option<crate::airplay::AirPlay>>>,
    /// The receiver's state from that worker, and its sender until `run`.
    airplay_report: watch::Receiver<crate::airplay::Report>,
    airplay_report_tx: Option<watch::Sender<crate::airplay::Report>>,
    /// Starts and stops Bluetooth, one change at a time (started in `run`).
    bluetooth: Option<mpsc::UnboundedSender<bool>>,
    /// The bridge's unit, from that worker, and its sender until `run`.
    bt_unit: watch::Receiver<crate::airplay::State>,
    bt_unit_tx: Option<watch::Sender<crate::airplay::State>>,
    /// What the bridge last reported.
    bt_bridge: crate::bluetooth::Bridge,
    /// The pairing window, while it is open: when it closes.
    bt_pairing_until: Option<(Instant, std::time::SystemTime)>,
    /// Why Bluetooth didn't start at boot, if it was skipped.
    bt_skipped: Option<String>,
    /// Since when the bridge has been ready with the Wi-Fi up, until the
    /// start counts and the marker goes (`bluetooth::SETTLE`).
    bt_ready_since: Option<Instant>,
    /// The Sendspin player (ADR 0027), started in `run`.
    sendspin: Option<crate::sendspin::Handle>,
    /// The player as it is: from the service once `run` starts it.
    sendspin_report: watch::Receiver<crate::sendspin::Report>,
    /// Sendspin can run: there is a speaker.
    sendspin_available: bool,
    /// What the Sendspin stream last showed, for its paused group (ADR 0027
    /// §14): the media lane shows a track only while its source plays.
    sendspin_track: pinna_audio::Track,
    /// Wakes and near misses, oldest first.
    recent_wakes: VecDeque<WakeRecord>,
    /// The sensitivity tuning with the device's buttons, while it runs.
    calibration: Option<Calibration>,
    /// A confirmation waiting for the Action button (ADR 0029).
    presence: Option<Presence>,
    /// Action confirmed a change and is still held: the rest of that hold
    /// is the confirmation's too, so no `long_press` follows (ADR 0029).
    confirm_held: bool,
    /// Renamed while Home Assistant was connected: make it reconnect, and read
    /// the new name, once the satellite is idle.
    reconnect_pending: bool,
    /// The mode changed while Home Assistant was connected: make it reconnect,
    /// and read the new voice features, once no run, alarm or voice playback
    /// is left. Media may keep playing.
    reconnect_for_mode: bool,
    /// A switch `switch_assistant` asked for while a run, alarm or playback
    /// was going; applied from `tick` once the satellite is idle.
    pending_mode: Option<Mode>,
    encrypted: bool,
    /// When the restart after an update from Home Assistant was last tried.
    update_restart_tried: Option<Instant>,
    /// Messages from the satellite group: held, and the one playing.
    messages: messages::Messages,
}

/// A restart after an update that didn't happen is tried again after this.
const UPDATE_RESTART_RETRY: Duration = Duration::from_secs(60);

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
    #[cfg(test)]
    pub fn new(
        esp: EsphomeHandle,
        speaker: Option<Arc<Speaker>>,
        shared: Arc<Shared>,
        leds: watch::Sender<LedState>,
        status: watch::Sender<Snapshot>,
        self_tx: mpsc::Sender<Input>,
        opts: Options,
    ) -> Self {
        let local = local::Local::beside_settings(&opts.settings_file);
        Self::new_with_local(esp, speaker, shared, leds, status, self_tx, opts, local)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new_with_local(
        esp: EsphomeHandle,
        speaker: Option<Arc<Speaker>>,
        shared: Arc<Shared>,
        leds: watch::Sender<LedState>,
        status: watch::Sender<Snapshot>,
        self_tx: mpsc::Sender<Input>,
        opts: Options,
        local: LocalOptions,
    ) -> Self {
        let store = Store::new(&opts.settings_file, FILE_MODE);
        let (live, spawner, timers_file) = local.into_parts();
        let timer_store =
            timers::TimerStore::load(&timers_file, opts.settings.mode == Mode::Standalone);
        let local = local::Local::new(live, spawner);
        let active_wake = opts.active_wake_words.clone();
        let settings = opts.settings.clone();
        shared
            .threshold
            .store(settings.wake_threshold().to_bits(), Ordering::SeqCst);
        let (bt_unit_tx, bt_unit) = watch::channel(crate::airplay::State::Off);
        let (airplay_report_tx, airplay_report) = watch::channel(crate::airplay::Report {
            state: if opts.airplay {
                crate::airplay::State::Off
            } else {
                crate::airplay::State::Unavailable
            },
            name: String::new(),
        });
        let sendspin_available = opts
            .sendspin
            .as_ref()
            .is_some_and(crate::sendspin::Setup::has_speaker);
        let (_, sendspin_report) =
            watch::channel(crate::sendspin::Report::idle(sendspin_available));
        Self {
            esp,
            speaker,
            shared,
            leds,
            status,
            self_tx,
            local,
            mdns_writer: crate::identity::RecordWriter::new(),
            phase: Phase::Idle,
            conversation_id: String::new(),
            continue_conversation: false,
            run_ended: false,
            stale_run: false,
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
            media_stream: false,
            media_url: None,
            media_error: None,
            media_error_url: None,
            media_track: pinna_audio::Track::default(),
            media_source: String::new(),
            stream_volume: 100,
            ducked: false,
            timers: HashMap::new(),
            timer_store,
            ringing: None,
            playback: None,
            next_token: 1,
            booted_at: Instant::now(),
            boot_done: false,
            led_test: None,
            beams: BeamViewReader::new(beamview::PATH),
            talker: leds::Talker::default(),
            speaker_quiet_since: Instant::now(),
            airplay: None,
            airplay_report,
            airplay_report_tx: Some(airplay_report_tx),
            bluetooth: None,
            bt_unit,
            bt_unit_tx: Some(bt_unit_tx),
            bt_bridge: Default::default(),
            bt_pairing_until: None,
            bt_skipped: None,
            bt_ready_since: None,
            sendspin: None,
            sendspin_report,
            sendspin_available,
            sendspin_track: pinna_audio::Track::default(),
            recent_wakes: VecDeque::with_capacity(RECENT_WAKES),
            calibration: None,
            presence: None,
            confirm_held: false,
            reconnect_pending: false,
            update_restart_tried: None,
            reconnect_for_mode: false,
            pending_mode: None,
            encrypted: opts.encrypted,
            messages: messages::Messages::default(),
            opts,
        }
    }

    pub async fn run(mut self, mut inputs: mpsc::Receiver<Input>) {
        let mut tick = tokio::time::interval(Duration::from_millis(250));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        self.shared.muted.store(self.muted(), Ordering::SeqCst);
        self.apply_reply_volume();
        if self.opts.airplay {
            if let Some(report) = self.airplay_report_tx.take() {
                self.airplay = Some(crate::airplay::worker(report));
            }
            // Also off: a bridge left over from before a restart stops.
            self.apply_airplay();
        } else if self.settings.airplay {
            // Saved on, but no speaker or stream socket this time: a bridge
            // and record from before a restart go. The setting stays saved.
            tokio::task::spawn_blocking(crate::airplay::off);
        }
        if self.opts.bluetooth {
            if let Some(unit) = self.bt_unit_tx.take() {
                self.bluetooth = Some(crate::bluetooth::worker(unit));
            }
            if self.settings.bluetooth && crate::bluetooth::left_from_last_boot() {
                // The last start never got to running with the Wi-Fi up.
                tracing::warn!("bluetooth: the last start didn't finish; staying off this time");
                self.bt_skipped = Some(
                    "Bluetooth didn't come up last time, so it didn't start this time.".into(),
                );
            } else if let Some(tx) = &self.bluetooth {
                // Also off: a target left over from before a restart stops.
                let _ = tx.send(self.settings.bluetooth);
            }
        }
        if let Some(setup) = self.opts.sendspin.take() {
            let (handle, report) = crate::sendspin::worker(setup);
            self.sendspin = Some(handle);
            self.sendspin_report = report;
            // Also off: a record left over from before a restart goes. Saved
            // on without a speaker, the setting stays saved, as AirPlay's.
            self.apply_sendspin();
        }
        let mut sendspin_changes = self
            .sendspin
            .is_some()
            .then(|| self.sendspin_report.clone());
        self.restore_timers(Instant::now(), SystemTime::now(), crate::clock::synced)
            .await;
        self.refresh();
        let mut sent_media = (self.media_state, self.media_volume(), self.media_muted());
        // Home Assistant's "Bluetooth pairing" switch follows the window,
        // however it opened or closed.
        let mut sent_pairing = false;
        let mut updates = self.opts.updates.as_ref().map(|u| u.subscribe());
        loop {
            tokio::select! {
                input = inputs.recv() => match input {
                    Some(input) => self.handle(input).await,
                    None => return,
                },
                _ = tick.tick() => self.tick().await,
                Some(()) = async {
                    match &mut updates {
                        Some(rx) => rx.changed().await.ok(),
                        None => std::future::pending().await,
                    }
                } => self.send_update_state().await,
                Some(()) = async {
                    match &mut sendspin_changes {
                        Some(rx) => rx.changed().await.ok(),
                        None => std::future::pending().await,
                    }
                } => self.update_media_state(),
            }
            let media = (self.media_state, self.media_volume(), self.media_muted());
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
            let pairing = self.bt_pairing_until.is_some();
            if pairing != sent_pairing {
                sent_pairing = pairing;
                self.send_pairing_switch(pairing).await;
            }
            self.set_group_record();
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
                if self.wake_armed() && !self.muted() {
                    self.record_wake(WakeRecord::now(true, score, false, phrase.clone()));
                    self.shared
                        .counters
                        .wakes
                        .get_or_create(&crate::metrics::Word { word: id.clone() })
                        .inc();
                    if self.phase != Phase::Idle {
                        let what = if self.phase == Phase::Speaking {
                            "reply"
                        } else {
                            "request"
                        };
                        tracing::info!("{what} interrupted by the wake word");
                        if self.local.run.is_some() {
                            self.count_guard(pinna_assist::observe::Guard::StoppedByWake);
                        }
                        self.interrupt().await;
                    }
                    if self.ringing.is_some() {
                        // The wake word only stops the alarm; no request starts.
                        tracing::info!("timer alarm dismissed by wake word");
                        self.ringing = None;
                        self.stop_playback();
                    } else if let Some(c) = &mut self.calibration {
                        // Calibration acknowledges the wake itself: no request.
                        c.woke(score, self.settings.wake_sensitivity, Instant::now());
                        self.overlay(Effect::CalibrationHeard);
                        if self.wake_sound() {
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
                self.record_wake(WakeRecord::now(
                    false,
                    score,
                    reached_threshold,
                    String::new(),
                ));
                self.shared.counters.near_misses.inc();
                if let Some(c) = &mut self.calibration {
                    c.near_miss(score, Instant::now());
                }
            }
            Input::Button(ev) => self.button(ev).await,
            Input::Privacy(muted) => {
                tracing::info!("hardware mute: {muted}");
                // Opening the latch is the owner's say: the button turns the
                // microphones on, whoever muted them. Only a change counts;
                // the poll's first reading at boot must keep a saved mute.
                if self.hw_muted && !muted {
                    self.sw_muted = false;
                    self.local.pending_mute = false;
                }
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
                self.media_track = state.track;
                self.media_source = state.source;
                self.media_stream = state.url.as_deref() == Some(STREAM_URL);
                if self.media_stream && self.media_source == crate::sendspin::SOURCE {
                    self.sendspin_track.clone_from(&self.media_track);
                } else if self.media_phase == MediaPhase::Playing
                    && self.sendspin_report.borrow().group_paused
                    && let Some(s) = &self.sendspin
                {
                    // Another source took the lane while the group waits
                    // paused from here: the satellite leaves it, so the
                    // group's next play doesn't cut this one off (ADR 0027
                    // §14). A URL takes the lane only once it plays: one
                    // that fails to open changes nothing (ADR 0011 §6).
                    s.another_source();
                }
                self.media_url = state.url;
                self.media_error = state.error;
                self.media_error_url = state.error_url;
                self.update_media_state();
            }
            Input::Ha(cmd) => self.ha(cmd).await,
            Input::Api(cmd) => self.api(cmd).await,
            Input::WakeWordsFailed(failed) => self.wake_words_failed(failed).await,
            Input::Assist { run, ev } => self.assist_event(run, ev).await,
            Input::RunTool { run, cmd } => self.run_tool(run, cmd).await,
            Input::Message(delivery) => self.take_message(delivery, Instant::now()),
            Input::Presence(presence) => {
                tracing::info!("waiting for a press of Action to confirm a change");
                self.presence = Some(presence);
            }
        }
        self.tend_messages(Instant::now());
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
            ApiCommand::SetStreamVolume(percent, reply) => {
                self.set_stream_volume(percent);
                (Ok(()), reply)
            }
            ApiCommand::SetStreamMetadata(source, track, reply) => {
                if let Some(s) = &self.speaker {
                    s.set_stream_track(&source, track);
                }
                (Ok(()), reply)
            }
            ApiCommand::SetMute(on, reply) => {
                self.sw_muted = on;
                self.apply_mute().await;
                (Ok(()), reply)
            }
            ApiCommand::VoiceMute(reply) => {
                self.voice_mute().await;
                (Ok(()), reply)
            }
            ApiCommand::SwitchMode(mode, reply) => (self.switch_mode(mode).await, reply),
            ApiCommand::StartConversation(reply) => {
                let result = if self.ringing.is_some() || self.phase != Phase::Idle {
                    Err("a conversation or timer alarm is already active".to_string())
                } else if self.muted() {
                    Err("the microphone is muted".to_string())
                } else if !self.assist_ready() {
                    let why = match self.mode() {
                        Mode::HomeAssistant => {
                            "Home Assistant has not subscribed to voice assistant runs"
                        }
                        Mode::Standalone => mode::NOT_READY,
                    };
                    Err(why.to_string())
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
            ApiCommand::CancelForGroup(reply) => {
                if self.ringing.is_some() {
                    tracing::info!("timer alarm dismissed by the satellite group");
                    self.ringing = None;
                    self.stop_playback();
                } else if self.phase != Phase::Idle {
                    tracing::info!("run cancelled by the satellite group");
                    self.abort_run(true).await;
                } else if matches!(
                    self.playback,
                    Some((_, PlaybackKind::Speak | PlaybackKind::Message, _))
                ) {
                    // A person asked for it to stop, as with the Action
                    // button: a message is dropped, not held again.
                    self.message_stopped("the satellite group");
                    self.stop_playback();
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
            ApiCommand::UpdateSettings(patch, reply) => match self.refuse_settings(&patch) {
                Some(why) => (Err(why.into()), reply),
                None => (self.update_settings(patch).await, reply),
            },
            ApiCommand::StartCalibration(reply) => (self.set_calibration(true).await, reply),
            ApiCommand::StopCalibration(reply) => {
                let _ = self.set_calibration(false).await;
                (Ok(()), reply)
            }
            ApiCommand::StartBluetoothPairing(reply) => (self.start_pairing(), reply),
            ApiCommand::StopBluetoothPairing(reply) => {
                self.stop_pairing();
                (Ok(()), reply)
            }
            ApiCommand::ForgetBluetoothDevice(address, reply) => {
                if self.bt_active() {
                    tracing::info!("bluetooth: forgetting {address}");
                    let gone = address.clone();
                    let res =
                        tokio::task::spawn_blocking(move || crate::bluetooth::forget(&address))
                            .await
                            .unwrap_or_else(|e| Err(e.to_string()));
                    if res.is_ok() {
                        // Gone from Status now, not at the bridge's next report.
                        self.bt_bridge.devices.retain(|d| d.address != gone);
                    }
                    (res, reply)
                } else {
                    (Err("Switch Bluetooth on to forget a phone.".into()), reply)
                }
            }
            ApiCommand::ReportBluetooth(bridge, reply) => {
                self.bluetooth_report(bridge);
                (Ok(()), reply)
            }
            ApiCommand::SetRingLight(patch, reply) => {
                self.set_light(patch).await;
                (Ok(()), reply)
            }
            ApiCommand::ControlMedia(_, _, reply) | ApiCommand::PlayMedia(_, reply)
                if self.speaker.is_none() =>
            {
                let why = "This satellite has no working speaker, so it can't play media. \
                           Logs, under System, says why.";
                (Err(why.into()), reply)
            }
            ApiCommand::ControlMedia(command, volume, reply) => {
                // The same path as Home Assistant's media player.
                let volume = volume.map(|v| f32::from(v) / 100.0);
                self.media(command, volume, None, false).await;
                (Ok(()), reply)
            }
            ApiCommand::PlayMedia(url, reply) => {
                self.media(None, None, Some(url), false).await;
                (Ok(()), reply)
            }
            ApiCommand::PlayMediaChecked(url, reply) => {
                self.play_media_checked(url, reply);
                self.refresh();
                return;
            }
            ApiCommand::CancelTimer(id_or_name, reply) => {
                (self.cancel_timer(&id_or_name).await, reply)
            }
            ApiCommand::SetTimer {
                name,
                seconds,
                reply,
            } => {
                let result = self.set_timer(name, seconds).await;
                self.refresh();
                let _ = reply.send(result);
                return;
            }
            ApiCommand::Speak(text, reply) => {
                self.speak(text, reply);
                self.refresh();
                return;
            }
            ApiCommand::AskText(question, reply) => (self.ask_text(question).await, reply),
            ApiCommand::AddWakeWord {
                phrase,
                staged,
                format,
                sha256,
                reply,
            } => {
                let result = self.add_wake_word(&phrase, &staged, format, sha256).await;
                self.refresh();
                let _ = reply.send(result);
                return;
            }
            ApiCommand::RemoveWakeWord(id, reply) => (self.remove_wake_word(&id).await, reply),
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
            mode: self.settings.mode,
            assist: self.local.status(),
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
            settings: Settings {
                airplay: self.airplay_on(),
                bluetooth: self.bluetooth_on(),
                sendspin: self.sendspin_on(),
                ..self.settings.clone()
            },
            recent_wakes: self.recent_wakes.iter().cloned().collect(),
            calibration: self.calibration.as_ref().map(|c| CalibrationSnapshot {
                started_from: c.started_from,
                events: c.events.iter().copied().collect(),
            }),
            active_wake_words: self.active_wake.clone(),
            wake_words: self.opts.wake_words.clone(),
            name: self.node_name(),
            friendly_name: self.friendly_name(),
            default_name: self.settings.friendly_name.is_none() && self.opts.defaults.default_name,
            wake_sound: self.wake_sound(),
            encrypted: self.encrypted,
            led_test: self.led_test.is_some(),
            media: MediaSnapshot {
                state: self.media_state,
                url: self.media_url.clone().filter(|u| u != STREAM_URL),
                stream: self.media_stream,
                muted: self.media_muted(),
                error: self.media_error.clone(),
                error_url: self.media_error_url.clone(),
                available: self.speaker.is_some(),
                volume: self.media_volume(),
                // A paused group streams nothing, yet keeps what it played.
                track: if self.sendspin_paused() {
                    self.sendspin_track.clone()
                } else {
                    self.media_track.clone()
                },
                source: if self.sendspin_paused() {
                    crate::sendspin::SOURCE.into()
                } else {
                    self.media_source.clone()
                },
            },
            ring_light: RingLight {
                on: self.light_on,
                brightness: self.light_brightness,
                rgb: self.light_rgb,
                effect: self.light_effect,
            },
            update_channel: self.update_channel(),
            held_messages: self.messages.held() as u32,
            airplay: self.airplay_report.borrow().clone(),
            sendspin: self.sendspin_report.borrow().clone(),
            bluetooth: crate::bluetooth::report(
                self.opts.bluetooth,
                self.settings.bluetooth,
                self.bt_skipped.as_deref(),
                &self.bt_unit.borrow(),
                &self.bt_bridge,
                &self.friendly_name(),
                (
                    self.bt_pairing_until
                        .and_then(|(_, at)| at.duration_since(std::time::UNIX_EPOCH).ok())
                        .map_or(0, |d| d.as_millis() as u64),
                    self.bt_pairing_until.map(|(until, _)| until),
                ),
            ),
        }
    }

    fn friendly_name(&self) -> String {
        self.settings
            .friendly_name
            .clone()
            .unwrap_or_else(|| self.opts.defaults.friendly_name.clone())
    }

    fn node_name(&self) -> String {
        self.settings
            .hostname
            .clone()
            .unwrap_or_else(|| self.opts.defaults.node_name.clone())
    }

    fn wake_sound(&self) -> bool {
        self.settings
            .wake_sound
            .unwrap_or(self.opts.defaults.wake_sound)
    }

    async fn set_wake_sound(&mut self, on: bool) {
        tracing::info!("wake sound: {on}");
        self.settings.wake_sound = Some(on);
        self.send(DeviceEvent::SwitchState {
            object_id: "wake_sound".into(),
            on,
        })
        .await;
    }

    /// The *Group messages* switch (ADR 0026): whether `Deliver` takes
    /// messages from the other members. Messages already held still play.
    async fn set_group_messages(&mut self, on: bool) {
        tracing::info!("group messages: {on}");
        self.settings.group_messages = Some(on);
        self.send(DeviceEvent::SwitchState {
            object_id: "group_messages".into(),
            on,
        })
        .await;
    }

    /// Rename (ADR 0007): the device info and the mDNS record at once; Home
    /// Assistant reads the name when it reconnects, once the satellite is idle.
    async fn set_friendly_name(&mut self, name: String) {
        if self.settings.friendly_name.as_ref() == Some(&name) {
            return;
        }
        tracing::info!("renamed to {name:?}");
        self.settings.friendly_name = Some(name.clone());
        self.send(DeviceEvent::FriendlyName(name.clone())).await;
        if let Some(dc) = &mut self.opts.mdns {
            dc.friendly_name = name;
            self.mdns_writer.publish(dc.clone());
        }
        self.reconnect_pending = self.clients > 0;
        // Senders list it under the new name too.
        if self.airplay_on() {
            self.apply_airplay();
        }
        if self.sendspin_on()
            && let Some(s) = &self.sendspin
        {
            s.rename(&self.friendly_name());
        }
    }

    /// Rename the satellite (ADR 0010): hostname, mDNS records and the node
    /// name at once; Home Assistant reads the name when it reconnects, once
    /// the satellite is idle.
    async fn set_hostname(&mut self, name: String) {
        if self.node_name() == name {
            return;
        }
        tracing::info!("hostname: {name}");
        self.settings.hostname = Some(name.clone());
        self.send(DeviceEvent::NodeName(name.clone())).await;
        if let Some(dc) = &mut self.opts.mdns {
            dc.name.clone_from(&name);
        }
        // resolved moves the ESPHome record with the hostname (`Name=%H`,
        // ADR 0024): rewriting it here would leave the old one published.
        tokio::task::spawn_blocking(move || {
            if let Err(e) = crate::identity::set_hostname_live(&name) {
                tracing::warn!("hostname: {e:#}");
            }
        });
        self.reconnect_pending = self.clients > 0;
    }

    /// Set the dashboard address (ADR 0021): the Host allowlist follows at
    /// once, and the mDNS record tells other satellites.
    fn set_dashboard_url(&mut self, url: Option<String>) {
        if self.settings.url == url {
            return;
        }
        tracing::info!("dashboard address: {url:?}");
        self.settings.url.clone_from(&url);
        if let Some(dc) = &mut self.opts.mdns {
            dc.dashboard_url = url;
            self.mdns_writer.publish(dc.clone());
        }
    }

    /// Follow the satellite group in the mDNS record (ADR 0026): the switch,
    /// joining and leaving change it. The standalone service saves the
    /// group; the Brain stays the only writer of the record.
    fn set_group_record(&mut self) {
        let record = self.local.group_record();
        if let Some(dc) = group_record_changed(&mut self.opts.mdns, record) {
            self.mdns_writer.publish(dc);
        }
    }

    /// Set the time zone (ADR 0007): the satellite's own clock and logs.
    fn set_time_zone(&mut self, zone: String) {
        if self.settings.time_zone.as_deref().unwrap_or("UTC") == zone {
            return;
        }
        tracing::info!("time zone: {zone}");
        self.settings.time_zone = Some(zone.clone());
        // The install window is local time.
        self.push_install_policy();
        tokio::task::spawn_blocking(move || {
            use crate::timezone::*;
            let paths = (ETC_LOCALTIME.as_ref(), LINK.as_ref(), ZONEINFO.as_ref());
            if let Err(e) = apply(&zone, paths.0, paths.1, paths.2) {
                tracing::warn!("time zone: {e:#}");
            }
        });
    }

    // --- Updates (ADR 0022) ---------------------------------------------------

    fn update_channel(&self) -> crate::updates::UpdateChannel {
        self.settings
            .update_channel
            .unwrap_or(self.opts.defaults.update_channel)
    }

    /// pinnad.toml's `[update] manifest` replaces the channel.
    fn update_override(&self) -> bool {
        self.opts
            .updates
            .as_ref()
            .is_some_and(|u| matches!(u.source(), crate::updates::Source::Manifest(_)))
    }

    async fn set_update_channel(&mut self, c: crate::updates::UpdateChannel) {
        if self.update_override() {
            return;
        }
        tracing::info!("updates: {}", c.as_str());
        self.settings.update_channel = Some(c);
        if let Some(u) = &self.opts.updates {
            u.set_source(crate::updates::Source::Channel(c));
        }
        self.send_update_channel().await;
    }

    /// What the updater needs to know about automatic installs.
    fn install_policy(&self) -> crate::updates::Policy {
        crate::updates::Policy {
            mode: self.settings.install_updates(),
            window: self.settings.install_window(),
            zone: self.settings.time_zone.clone(),
        }
    }

    fn push_install_policy(&self) {
        if let Some(u) = &self.opts.updates {
            u.set_automatic(self.install_policy());
        }
    }

    async fn set_install_updates(&mut self, mode: crate::updates::auto::InstallMode) {
        tracing::info!("install updates: {mode:?}");
        self.settings.install_updates = Some(mode);
        self.push_install_policy();
        self.send_install_updates().await;
    }

    async fn set_install_window(&mut self, window: crate::updates::auto::Window) {
        use crate::updates::auto::format_time;
        tracing::info!(
            "install window: {} to {}",
            format_time(window.start()),
            format_time(window.end())
        );
        self.settings.install_window_start = Some(format_time(window.start()));
        self.settings.install_window_end = Some(format_time(window.end()));
        self.push_install_policy();
        self.send_install_window().await;
    }

    /// The "Install updates" select.
    async fn send_install_updates(&self) {
        self.send(DeviceEvent::SelectState {
            object_id: "install_updates".into(),
            state: self.settings.install_updates().label().to_owned(),
        })
        .await;
    }

    /// The "Install window start" and "Install window end" times.
    async fn send_install_window(&self) {
        let window = self.settings.install_window();
        for (object_id, minute) in [
            ("install_window_start", window.start()),
            ("install_window_end", window.end()),
        ] {
            let (hour, minute) = crate::updates::auto::split_time(minute);
            self.send(DeviceEvent::TimeState {
                object_id: object_id.into(),
                hour,
                minute,
            })
            .await;
        }
    }

    /// Home Assistant set one end of the window. Both ends are kept: a
    /// window without length is refused, and the entity shows the old time again.
    async fn install_window_from_ha(&mut self, object_id: &str, hour: u32, minute: u32) {
        use crate::updates::auto::Window;
        let current = self.settings.install_window();
        let time = (hour * 60 + minute) as u16;
        let (start, end) = match object_id {
            "install_window_start" => (time, current.end()),
            _ => (current.start(), time),
        };
        match Window::new(start, end) {
            Ok(window) => {
                self.set_install_window(window).await;
                if let Err(e) = self.store.try_save(&self.settings) {
                    tracing::warn!("saving the settings: {e}");
                }
            }
            Err(e) => {
                tracing::info!("install window: {e}");
                self.send_install_window().await;
            }
        }
    }

    /// The "Updates" select: the channel, or the URL that replaces it.
    async fn send_update_channel(&self) {
        let state = match self.opts.updates.as_ref().map(|u| u.source()) {
            Some(crate::updates::Source::Manifest(url)) => url,
            _ => self.update_channel().as_str().to_owned(),
        };
        self.send(DeviceEvent::SelectState {
            object_id: "update_channel".into(),
            state,
        })
        .await;
    }

    /// The "Firmware" update entity, from what GetUpdates shows.
    async fn send_update_state(&self) {
        let Some(u) = &self.opts.updates else {
            return;
        };
        if self.clients == 0 {
            return;
        }
        let resp = u.get_updates().await;
        let state = crate::updates::home_assistant_state(&resp, u.install_progress());
        self.send(DeviceEvent::UpdateState {
            object_id: "firmware".into(),
            state,
        })
        .await;
    }

    /// Home Assistant's Install and Check (ADR 0022). Install takes the
    /// release the entity shows; its command has no answer, so a refusal
    /// only reaches the log (the entity's summary said why beforehand).
    async fn update_command(&mut self, command: pinna_esphome::UpdateCommand) {
        let Some(u) = self.opts.updates.clone() else {
            return;
        };
        match command {
            pinna_esphome::UpdateCommand::Check => {
                tokio::spawn(async move {
                    if let Err(e) = u.check().await {
                        tracing::info!("update check: {}", e.message.unwrap_or_default());
                    }
                });
            }
            pinna_esphome::UpdateCommand::Install => {
                let resp = u.get_updates().await;
                let Some(version) = resp.release.as_option().map(|r| r.version.clone()) else {
                    tracing::info!(
                        "updates: Home Assistant asked to install, but no release is known"
                    );
                    return;
                };
                if let Err(e) = u
                    .install_release(&version, false, crate::updates::Origin::HomeAssistant)
                    .await
                {
                    tracing::warn!(
                        "updates: Home Assistant's install refused: {}",
                        e.message.unwrap_or_default()
                    );
                }
            }
        }
    }

    /// After an update from Home Assistant or a tool: restart once nothing would be
    /// cut off, no timer runs and nothing plays (ADR 0022).
    async fn restart_for_update(&mut self) {
        let Some(u) = self.opts.updates.clone() else {
            return;
        };
        let now = Instant::now();
        if !self.quiet()
            || !self.timers.is_empty()
            || self
                .update_restart_tried
                .is_some_and(|t| now - t < UPDATE_RESTART_RETRY)
            || !u.restart_due()
        {
            return;
        }
        self.update_restart_tried = Some(now);
        tracing::info!("updates: restarting into the installed update");
        if let Err(e) = u.reboot("update").await {
            tracing::warn!("updates: the restart failed: {e:#}");
        }
    }

    /// Nothing a reconnect would cut off: no request, alarm, reply,
    /// announcement or media playing.
    fn quiet(&self) -> bool {
        self.phase == Phase::Idle
            && self.ringing.is_none()
            && self.playback.is_none()
            && !matches!(
                self.media_state,
                MediaPlayerStateKind::Playing | MediaPlayerStateKind::Announcing
            )
    }

    fn record_wake(&mut self, record: WakeRecord) {
        if self.recent_wakes.len() == RECENT_WAKES {
            self.recent_wakes.pop_front();
        }
        self.recent_wakes.push_back(record);
    }

    // --- Home Assistant -----------------------------------------------------

    async fn ha(&mut self, cmd: HaCommand) {
        if !self.ha_allowed(&cmd) {
            tracing::debug!("Home Assistant command ignored in standalone mode: {cmd:?}");
            if matches!(&cmd, HaCommand::Announce { .. }) {
                self.send(DeviceEvent::AnnounceFinished { success: false })
                    .await;
            }
            return;
        }
        match cmd {
            HaCommand::ClientConnected { peer } => {
                self.clients += 1;
                tracing::info!("Home Assistant connected from {peer}");
                self.send(DeviceEvent::MuteState(self.muted())).await;
                self.send_update_channel().await;
                self.send_install_updates().await;
                self.send_install_window().await;
                self.send_mode().await;
                self.send_update_state().await;
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
                    object_id: "airplay".into(),
                    on: self.airplay_on(),
                })
                .await;
                self.send(DeviceEvent::SwitchState {
                    object_id: "bluetooth".into(),
                    on: self.bluetooth_on(),
                })
                .await;
                self.send(DeviceEvent::SwitchState {
                    object_id: "sendspin".into(),
                    on: self.sendspin_on(),
                })
                .await;
                self.send_pairing_switch(self.bt_pairing_until.is_some())
                    .await;
                self.send(DeviceEvent::SwitchState {
                    object_id: "notification".into(),
                    on: self.settings.notification,
                })
                .await;
                self.send(DeviceEvent::SwitchState {
                    object_id: "wake_sound".into(),
                    on: self.wake_sound(),
                })
                .await;
                self.send(DeviceEvent::SwitchState {
                    object_id: "group_messages".into(),
                    on: self.settings.group_messages(),
                })
                .await;
                self.send_calibration_switch(self.calibration.is_some())
                    .await;
                self.send_media_state().await;
            }
            HaCommand::ClientDisconnected => {
                self.clients = self.clients.saturating_sub(1);
                tracing::info!("Home Assistant disconnected");
                if self.clients == 0 && self.phase != Phase::Idle && self.local.run.is_none() {
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
                let effect = effect.and_then(|e| {
                    let known = LightEffect::from_name(&e);
                    if known.is_none() {
                        tracing::warn!("unknown light effect {e:?}");
                    }
                    known
                });
                self.set_light(RingLightPatch {
                    on,
                    brightness,
                    rgb,
                    effect,
                })
                .await;
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
                self.set_listening_direction(on).await;
                self.store.save(&self.settings);
            }
            HaCommand::Switch { object_id, on } if object_id == "wake_sound" => {
                self.set_wake_sound(on).await;
                self.store.save(&self.settings);
            }
            HaCommand::Switch { object_id, on } if object_id == "group_messages" => {
                self.set_group_messages(on).await;
                self.store.save(&self.settings);
            }
            HaCommand::Switch { object_id, on } if object_id == "airplay" => {
                self.set_airplay(on).await;
            }
            HaCommand::Switch { object_id, on } if object_id == "bluetooth" => {
                self.set_bluetooth(on).await;
            }
            HaCommand::Switch { object_id, on } if object_id == "sendspin" => {
                self.set_sendspin(on).await;
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
            HaCommand::Select { object_id, state } if object_id == "update_channel" => {
                match crate::updates::UpdateChannel::parse(&state) {
                    Some(c) if !self.update_override() => {
                        self.set_update_channel(c).await;
                        if let Err(e) = self.store.try_save(&self.settings) {
                            tracing::warn!("saving the settings: {e}");
                        }
                    }
                    // The URL from pinnad.toml, its only option: nothing to change.
                    _ => self.send_update_channel().await,
                }
            }
            HaCommand::Select { object_id, state } if object_id == "install_updates" => {
                match crate::updates::auto::InstallMode::from_label(&state) {
                    Some(mode) => {
                        self.set_install_updates(mode).await;
                        if let Err(e) = self.store.try_save(&self.settings) {
                            tracing::warn!("saving the settings: {e}");
                        }
                    }
                    None => self.send_install_updates().await,
                }
            }
            HaCommand::TimeOfDay {
                object_id,
                hour,
                minute,
                ..
            } if object_id == "install_window_start" || object_id == "install_window_end" => {
                self.install_window_from_ha(&object_id, hour, minute).await;
            }
            HaCommand::Select { object_id, state } if object_id == "assistant" => {
                self.mode_from_ha(&state).await;
            }
            HaCommand::Update { object_id, command } if object_id == "firmware" => {
                self.update_command(command).await;
            }
            HaCommand::Switch { object_id, on } if object_id == "bluetooth_pairing" => {
                if !on {
                    self.stop_pairing();
                } else if let Err(why) = self.start_pairing() {
                    tracing::info!("bluetooth: no pairing window: {why}");
                    // It didn't open: the switch goes back off.
                    self.send_pairing_switch(false).await;
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
                self.encrypted = true;
                match crate::identity::store_encryption_key(&self.opts.key_file, &key) {
                    Ok(()) => tracing::info!(
                        "Home Assistant provisioned an encryption key; stored in {}",
                        self.opts.key_file.display()
                    ),
                    Err(e) => tracing::error!("could not persist the encryption key: {e:#}"),
                }
                // Announce it, so Home Assistant asks for Noise (ADR 0018).
                if let Some(dc) = &mut self.opts.mdns {
                    dc.encryption_key = Some(key);
                    self.mdns_writer.publish(dc.clone());
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
        if self.stale_run {
            match event {
                // The stopped run's last event.
                VoiceEventType::RunEnd => {
                    self.stale_run = false;
                    return;
                }
                // Home Assistant ends a run before it starts the next one: a
                // stopped run that hadn't started there sends nothing more.
                VoiceEventType::RunStart => self.stale_run = false,
                _ => return,
            }
        }
        match event {
            VoiceEventType::RunStart => {
                if let Some(url) = get("url") {
                    self.tts_url = Some(url);
                }
            }
            VoiceEventType::SttVadEnd | VoiceEventType::SttEnd => {
                self.stop_streaming();
                if matches!(self.phase, Phase::Listening(_) | Phase::Starting(_)) {
                    // Both times count audio sent so far; Home Assistant's
                    // includes the network round trip of its decision.
                    let shadow = self.shared.mic.shadow_report();
                    match shadow.end {
                        Some((end, at)) => tracing::info!(
                            "end of speech: Home Assistant after {} ms of audio, \
                             shadow endpointer {end:?} after {at} ms",
                            shadow.streamed_ms
                        ),
                        None => tracing::info!(
                            "end of speech: Home Assistant after {} ms of audio, \
                             shadow endpointer still listening",
                            shadow.streamed_ms
                        ),
                    }
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
                self.shared.counters.error(&get("code").unwrap_or_default());
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
                    Timer {
                        name,
                        total: total.max(1),
                        left,
                        active,
                        at: Instant::now(),
                        origin: Origin::Ha,
                    },
                );
            }
            TimerEventType::Cancelled => {
                self.timers.remove(&id);
            }
            TimerEventType::Finished => self.timer_finished(id).await,
            TimerEventType::Unknown(_) => {}
        }
    }

    async fn set_timer(&mut self, name: Option<String>, seconds: u32) -> Result<String, String> {
        if self.settings.mode != Mode::Standalone {
            return Err("Device timers are available in standalone mode only.".into());
        }
        let seconds = crate::checks::timer_seconds(seconds)?;
        if self.timers.len() >= crate::checks::TIMERS_MAX {
            return Err(format!(
                "At most {} timers can run at once.",
                crate::checks::TIMERS_MAX
            ));
        }
        let id = loop {
            let id = format!("local-{:016x}", fastrand::u64(..));
            if !self.timers.contains_key(&id) {
                break id;
            }
        };
        let now = Instant::now();
        tracing::info!("device timer {id} {name:?} {seconds}s");
        self.event("timer", "started").await;
        self.timers.insert(
            id.clone(),
            Timer {
                name: name.unwrap_or_default(),
                total: seconds,
                left: seconds,
                active: true,
                at: now,
                origin: Origin::Local,
            },
        );
        self.save_timers();
        Ok(id)
    }

    async fn cancel_timer(&mut self, id_or_name: &str) -> Result<(), String> {
        if self.settings.mode != Mode::Standalone {
            return Err("Device timers are available in standalone mode only.".into());
        }
        let id = if self.timers.contains_key(id_or_name) {
            Some(id_or_name.to_owned())
        } else {
            let mut matches: Vec<_> = self
                .timers
                .iter()
                .filter(|(_, timer)| timer.name == id_or_name)
                .map(|(id, _)| id.clone())
                .collect();
            matches.sort();
            matches.into_iter().next()
        };
        let Some(id) = id else {
            return Err("No device timer has that id or name.".into());
        };
        self.event("timer", "cancelled").await;
        self.timers.remove(&id);
        self.save_timers();
        Ok(())
    }

    /// Why a settings change can't be made at all, judged under the mode it
    /// asks for. A refused change changes nothing.
    fn refuse_settings(&self, patch: &SettingsPatch) -> Option<&'static str> {
        let mode = patch.mode.unwrap_or(self.mode());
        if patch.wake_words.is_some() && self.clients > 0 && mode == Mode::HomeAssistant {
            // Its dropdowns can't show a word chosen here (ADR 0013).
            return Some(
                "Home Assistant chooses the wake words while it is connected. \
                 Choose them on this satellite's device page there.",
            );
        }
        if patch.airplay == Some(true) && !self.opts.airplay {
            return Some(
                "This satellite has no working speaker, so it can't receive AirPlay. \
                 Logs, under System, says why.",
            );
        }
        if patch.bluetooth == Some(true) && !self.opts.bluetooth {
            return Some(
                "This satellite has no Bluetooth or no working speaker. \
                 Logs, under System, says why.",
            );
        }
        if patch.sendspin == Some(true) && !self.sendspin_available {
            return Some(
                "This satellite has no working speaker, so it can't play from Music Assistant. \
                 Logs, under System, says why.",
            );
        }
        None
    }

    /// Apply a settings change and save it once. The mode goes first, so the
    /// other settings apply under it; its only refusal comes before it
    /// changes anything.
    async fn update_settings(&mut self, patch: SettingsPatch) -> Result<(), String> {
        if let Some(mode) = patch.mode {
            self.set_mode(mode).await?;
        }
        // The same paths as Home Assistant's entities, so it sees the change.
        if let Some(name) = patch.friendly_name {
            self.set_friendly_name(name).await;
        }
        if let Some(h) = patch.hostname {
            self.set_hostname(h).await;
        }
        if let Some(zone) = patch.time_zone {
            self.set_time_zone(zone);
        }
        if let Some(on) = patch.wake_sound {
            self.set_wake_sound(on).await;
        }
        if let Some(url) = patch.url {
            self.set_dashboard_url(url);
        }
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
        if let Some(on) = patch.airplay {
            self.set_airplay(on).await;
        }
        if let Some(on) = patch.bluetooth {
            self.set_bluetooth(on).await;
        }
        if let Some(on) = patch.sendspin {
            self.set_sendspin(on).await;
        }
        match (patch.reply_follows_volume, patch.reply_volume) {
            (Some(true), _) => self.set_reply_volume(None).await,
            (_, Some(v)) => self.set_reply_volume(Some(v)).await,
            (Some(false), None) if self.settings.reply_volume.is_none() => {
                self.set_reply_volume(Some(self.settings.volume)).await;
            }
            _ => {}
        }
        if let Some(on) = patch.listening_direction {
            self.set_listening_direction(on).await;
        }
        if let Some(c) = patch.update_channel {
            self.set_update_channel(c).await;
        }
        if let Some(on) = patch.group_messages {
            self.set_group_messages(on).await;
        }
        if let Some(mode) = patch.install_updates {
            self.set_install_updates(mode).await;
        }
        if let Some(window) = patch.install_window {
            self.set_install_window(window).await;
        }
        // Applied, but a restart would undo it: say so (ADR 0007).
        self.store.try_save(&self.settings).map_err(|e| {
            tracing::warn!("saving the settings: {e}");
            format!(
                "The change applies now, but the satellite couldn't save it ({e}). \
                 It is lost when the satellite restarts."
            )
        })
    }

    async fn timer_finished(&mut self, id: String) {
        let local = self
            .timers
            .get(&id)
            .is_some_and(|timer| timer.origin == Origin::Local);
        self.timers.remove(&id);
        if local {
            self.save_timers();
        }
        if self.phase != Phase::Idle {
            self.abort_run(true).await;
        }
        self.ringing = Some((id, Instant::now()));
        self.play_pcm(tone::timer_alarm(), PlaybackKind::Alarm);
    }

    fn save_timers(&mut self) {
        self.save_timers_at(Instant::now(), SystemTime::now(), crate::clock::synced());
    }

    fn save_timers_at(&mut self, now: Instant, wall: SystemTime, synced: bool) {
        self.timer_store.save(&self.timers, now, wall, synced);
    }

    /// Take the saved device timers at boot, and correct them from their
    /// saved deadlines once the clock has synced.
    async fn restore_timers(&mut self, now: Instant, wall: SystemTime, synced: fn() -> bool) {
        let (ring, dropped) = match self.timer_store.restore(now, wall, synced) {
            None => return,
            Some(timers::Restore::Ready {
                timers: restored,
                ring,
                dropped,
            }) => {
                self.timers.extend(restored);
                (ring, dropped)
            }
            Some(timers::Restore::Correct(deadlines)) => {
                timers::correct(&mut self.timers, &deadlines, now, wall)
            }
        };
        let changed = !ring.is_empty() || !dropped.is_empty();
        for id in ring {
            self.event("timer", "finished").await;
            self.timer_finished(id).await;
        }
        if changed {
            self.save_timers_at(now, wall, synced());
        }
    }

    /// The ring light, from Home Assistant or the dashboard; Home Assistant
    /// sees the result.
    async fn set_light(&mut self, patch: RingLightPatch) {
        if let Some(on) = patch.on {
            self.light_on = on;
        }
        if let Some(b) = patch.brightness {
            self.light_brightness = b.clamp(0.0, 1.0);
        }
        if let Some(c) = patch.rgb {
            self.light_rgb = c;
        }
        if let Some(effect) = patch.effect {
            self.light_effect = effect;
        }
        self.send_light().await;
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
        // A confirmation takes the press for itself, whatever the ring shows
        // and with the microphone muted: it stops no alarm, starts no
        // request and reaches Home Assistant as no event (ADR 0029).
        if matches!(ev, ButtonEvent::Press(Button::Action))
            && let Some(presence) = self.presence.take()
            && presence.pressed.send(()).is_ok()
        {
            tracing::info!("Action pressed to confirm a change");
            self.confirm_held = true;
            return;
        }
        if self.confirm_held {
            match ev {
                ButtonEvent::LongPress(Button::Action) => return,
                ButtonEvent::Release(Button::Action) => {
                    self.confirm_held = false;
                    return;
                }
                // A release went missing: a new press starts a new hold.
                ButtonEvent::Press(Button::Action) => self.confirm_held = false,
                _ => {}
            }
        }
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
                    if self.local.run.is_some() {
                        self.count_guard(pinna_assist::observe::Guard::StoppedByButton);
                    }
                    self.abort_run(true).await;
                } else if matches!(
                    self.playback,
                    Some((
                        _,
                        PlaybackKind::MediaAnnounce | PlaybackKind::Speak | PlaybackKind::Message,
                        _
                    ))
                ) {
                    // The ring shows it speaking: stop it, as for a reply.
                    tracing::info!("announcement stopped by button");
                    self.message_stopped("the Action button");
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
            volume: f32::from(self.media_volume()) / 100.0,
            muted: self.media_muted(),
        })
        .await;
    }

    /// What the Home Assistant media player shows as its volume: Sendspin's
    /// while its stream plays (ADR 0027 §15), the stream volume while
    /// another stream plays, the speaker volume otherwise, also while the
    /// Sendspin group waits paused.
    fn media_volume(&self) -> u8 {
        if self.sendspin_streams() {
            self.sendspin_report.borrow().volume
        } else if self.media_stream {
            self.stream_volume
        } else {
            self.settings.volume
        }
    }

    /// The media player's mute: the lane's, or Sendspin's own while its
    /// stream plays.
    fn media_muted(&self) -> bool {
        self.media_muted || (self.sendspin_streams() && self.sendspin_report.borrow().muted)
    }

    /// The media player's volume from Home Assistant: Sendspin's own while its
    /// stream plays, which the server hears (ADR 0027 §15); the stream's own
    /// volume while another stream plays (the speaker volume stays where it
    /// is); the speaker volume otherwise. A volume set before an announcement
    /// while the group waits paused sets the speaker's, as on any idle
    /// satellite.
    async fn set_media_volume(&mut self, v: u8) {
        if self.sendspin_streams() {
            if let Some(s) = &self.sendspin {
                s.set_volume(v);
            }
        } else if self.media_stream {
            self.set_stream_volume(v);
        } else {
            self.set_volume(v).await;
        }
    }

    /// Stream volume from a bridge or from Home Assistant during a stream.
    /// The run loop reports the change to Home Assistant.
    fn set_stream_volume(&mut self, v: u8) {
        self.stream_volume = v.min(100);
        if let Some(s) = &self.speaker {
            s.set_stream_volume(self.stream_volume);
        }
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
    /// timer arc, media player announcement or Speak above it. Muted and a
    /// ringing alarm are checked by the caller.
    fn notification_shown(&self) -> bool {
        self.settings.notification
            && self.phase == Phase::Idle
            && !self.timers.values().any(|t| t.active)
            && !matches!(
                self.playback,
                Some((_, PlaybackKind::MediaAnnounce | PlaybackKind::Speak, _))
            )
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
        let inputs = self.self_tx.clone();
        tokio::task::spawn_blocking(move || {
            let loaded = catalogue.load(&ids);
            let _ = tx.send(KeywordSet {
                generation,
                keywords: loaded.keywords,
            });
            if !loaded.failed.is_empty() {
                let _ = inputs.blocking_send(Input::WakeWordsFailed(loaded.failed));
            }
        });
    }

    /// Tell Home Assistant the words on offer changed, and ask it to
    /// reconnect once the satellite is quiet: it reads the list only when it
    /// connects (ADR 0013).
    async fn wake_words_changed(&mut self) {
        self.send(DeviceEvent::WakeWords(
            self.opts.wake_words.for_home_assistant(),
        ))
        .await;
        self.reconnect_pending = self.clients > 0;
    }

    /// Words whose classifier didn't load can't listen: say so in the status
    /// and to Home Assistant, and stop listening for them.
    async fn wake_words_failed(&mut self, failed: Vec<(String, String)>) {
        self.opts.wake_words = Arc::new(self.opts.wake_words.with_failed(&failed));
        self.wake_words_changed().await;
        self.set_wake_words(self.active_wake.clone(), false).await;
    }

    /// Add a custom word (ADR 0013). The API tested the model; the rules are
    /// checked again here, where adding and removing are one at a time.
    async fn add_wake_word(
        &mut self,
        phrase: &str,
        staged: &std::path::Path,
        format: pinna_wakeword::ModelFormat,
        sha256: String,
    ) -> Result<String, String> {
        let (phrase, id) = self
            .opts
            .wake_words
            .plan_custom(phrase)
            .map_err(crate::wakewords::Refusal::into_message)?;
        let ext = match format {
            pinna_wakeword::ModelFormat::Onnx => "onnx",
            pinna_wakeword::ModelFormat::Tflite => "tflite",
        };
        let file = format!("{id}.{ext}");
        let next = self.opts.wake_words.with_added(crate::wakewords::Entry {
            id: id.clone(),
            phrase,
            file: file.clone(),
            sha256,
            failed: None,
        });
        let saved =
            std::fs::rename(staged, next.dir().join(&file)).and_then(|()| next.save_index());
        if let Err(e) = saved {
            tracing::error!("saving custom wake word {id}: {e}");
            let _ = std::fs::remove_file(next.dir().join(&file));
            return Err(
                "The satellite couldn't save the wake word. Logs, under System, says why.".into(),
            );
        }
        tracing::info!("custom wake word {id} added");
        self.opts.wake_words = Arc::new(next);
        self.wake_words_changed().await;
        Ok(id)
    }

    /// Remove a custom word, and stop listening for it if it was chosen.
    async fn remove_wake_word(&mut self, id: &str) -> Result<(), String> {
        let word = self
            .opts
            .wake_words
            .find(id)
            .filter(|w| w.custom)
            .ok_or_else(|| format!("This satellite has no custom wake word \"{id}\"."))?;
        let file = word.file.clone();
        let next = self.opts.wake_words.without(id);
        next.save_index().map_err(|e| {
            tracing::error!("removing custom wake word {id}: {e}");
            "The satellite couldn't save the change. Logs, under System, says why.".to_string()
        })?;
        if let Err(e) = std::fs::remove_file(&file) {
            tracing::warn!("removing {}: {e}", file.display());
        }
        tracing::info!("custom wake word {id} removed");
        self.opts.wake_words = Arc::new(next);
        // Home Assistant's server learns the new list first, so that the
        // change of the active words that follows is known to it.
        self.wake_words_changed().await;
        if self.active_wake.iter().any(|a| a == id) {
            let rest = self
                .active_wake
                .iter()
                .filter(|a| *a != id)
                .cloned()
                .collect();
            self.set_wake_words(rest, false).await;
        }
        Ok(())
    }

    /// AirPlay as it is: saved on, and the device can play streams.
    fn airplay_on(&self) -> bool {
        self.settings.airplay && self.opts.airplay
    }

    /// Hand the receiver as it should be to the worker: on under the current
    /// name, or off.
    fn apply_airplay(&self) {
        if let Some(tx) = &self.airplay {
            let want = self
                .airplay_on()
                .then(|| crate::airplay::AirPlay::new(&self.friendly_name()));
            let _ = tx.send(want);
        }
    }

    /// AirPlay on or off, from the dashboard or Home Assistant. Stays off on
    /// a device that can't play streams.
    async fn set_airplay(&mut self, on: bool) {
        let on = on && self.opts.airplay;
        if on != self.settings.airplay {
            self.settings.airplay = on;
            self.store.save(&self.settings);
            self.apply_airplay();
        }
        self.send(DeviceEvent::SwitchState {
            object_id: "airplay".into(),
            on,
        })
        .await;
    }

    /// Sendspin as it is: saved on, and the device has a speaker.
    fn sendspin_on(&self) -> bool {
        self.settings.sendspin && self.sendspin_available
    }

    /// Hand the player as it should be to the service: on under the current
    /// name, or off.
    fn apply_sendspin(&self) {
        if let Some(s) = &self.sendspin {
            s.enable(self.sendspin_on().then(|| crate::sendspin::Params {
                name: self.friendly_name(),
            }));
        }
    }

    /// Sendspin on or off, from the dashboard or Home Assistant. Stays off on
    /// a device without a speaker.
    async fn set_sendspin(&mut self, on: bool) {
        let on = on && self.sendspin_available;
        if on != self.settings.sendspin {
            self.settings.sendspin = on;
            self.store.save(&self.settings);
            self.apply_sendspin();
        }
        self.send(DeviceEvent::SwitchState {
            object_id: "sendspin".into(),
            on,
        })
        .await;
    }

    /// The media lane plays nothing, and the Sendspin group this satellite
    /// paused waits: the media player shows it Paused, from Sendspin, so that
    /// Play resumes the group (ADR 0027 §14). After Stop, or a queue that ran
    /// out, it shows Idle.
    fn sendspin_paused(&self) -> bool {
        self.media_phase == MediaPhase::Idle && self.sendspin_report.borrow().group_paused
    }

    /// The lane plays the Sendspin stream: the media player's volume and mute
    /// are the stream's own (ADR 0027 §15).
    fn sendspin_streams(&self) -> bool {
        self.media_stream && self.media_source == crate::sendspin::SOURCE
    }

    /// Sendspin owns the media player's transport: its stream is what the
    /// lane shows, or its group waits paused from here. Pause, play and stop
    /// then act on the group (ADR 0027 §14).
    fn sendspin_media(&self) -> bool {
        self.sendspin_streams() || self.sendspin_paused()
    }

    /// Pause, play or stop the Sendspin group, while Sendspin owns the media.
    /// False when it doesn't, or when the server can't take the command now:
    /// the lane acts instead.
    async fn sendspin_control(&self, control: crate::sendspin::Control) -> bool {
        let Some(s) = self.sendspin.as_ref().filter(|_| self.sendspin_media()) else {
            return false;
        };
        let sent = s.control(control).await;
        if !sent {
            tracing::debug!("sendspin: the server takes no {control:?} now; the lane acts");
        }
        sent
    }

    /// Bluetooth as it is: saved on, and the device can run it.
    fn bluetooth_on(&self) -> bool {
        self.settings.bluetooth && self.opts.bluetooth
    }

    /// Bluetooth on or off, from the dashboard or Home Assistant. Stays off on
    /// a device that can't run it. Switching on tries again after a start
    /// that was skipped or failed.
    async fn set_bluetooth(&mut self, on: bool) {
        let on = on && self.opts.bluetooth;
        let failed = matches!(*self.bt_unit.borrow(), crate::airplay::State::Failed(_));
        if on != self.settings.bluetooth || (on && (self.bt_skipped.is_some() || failed)) {
            self.settings.bluetooth = on;
            self.store.save(&self.settings);
            self.bt_skipped = None;
            self.bt_ready_since = None;
            if !on {
                self.bt_pairing_until = None;
                self.bt_bridge = Default::default();
            }
            if let Some(tx) = &self.bluetooth {
                let _ = tx.send(on);
            }
        }
        self.send(DeviceEvent::SwitchState {
            object_id: "bluetooth".into(),
            on,
        })
        .await;
    }

    /// Open the pairing window for `bluetooth::PAIRING`.
    /// Bluetooth on, and not skipped at boot.
    fn bt_active(&self) -> bool {
        self.bluetooth_on() && self.bt_skipped.is_none()
    }

    fn start_pairing(&mut self) -> Result<(), String> {
        if !self.bt_active() {
            return Err("Switch Bluetooth on first.".into());
        }
        // Only while phones can find it: not while starting or failed.
        let running = self.bt_bridge.ready
            && self.bt_bridge.error.is_empty()
            && *self.bt_unit.borrow() == crate::airplay::State::Ready;
        if !running {
            return Err("Bluetooth isn't running yet. Pair once it shows as ready.".into());
        }
        tracing::info!("bluetooth: pairing window open");
        self.bt_pairing_until = Some((
            Instant::now() + crate::bluetooth::PAIRING,
            std::time::SystemTime::now() + crate::bluetooth::PAIRING,
        ));
        Ok(())
    }

    fn stop_pairing(&mut self) {
        if self.bt_pairing_until.take().is_some() {
            tracing::info!("bluetooth: pairing window closed");
        }
    }

    /// The bridge's report: a newly paired phone closes the window, and a
    /// ready bridge with the Wi-Fi up starts the settling time after which
    /// the boot marker goes (`tick`).
    fn bluetooth_report(&mut self, bridge: crate::bluetooth::Bridge) {
        if !self.bt_active() {
            return;
        }
        let paired_new = bridge.devices.iter().any(|d| {
            !self
                .bt_bridge
                .devices
                .iter()
                .any(|o| o.address == d.address)
        });
        if paired_new && self.bt_bridge.ready && self.bt_pairing_until.is_some() {
            tracing::info!("bluetooth: a phone paired; closing the pairing window");
            self.bt_pairing_until = None;
        }
        if bridge.ready && bridge.error.is_empty() {
            self.bt_ready_since.get_or_insert_with(Instant::now);
        } else {
            self.bt_ready_since = None;
        }
        self.bt_bridge = bridge;
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

    async fn send_pairing_switch(&self, on: bool) {
        self.send(DeviceEvent::SwitchState {
            object_id: "bluetooth_pairing".into(),
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
        // The owner says the wake word to it now: a group message waits.
        if matches!(self.playback, Some((_, PlaybackKind::Message, _))) {
            self.stop_playback();
        }
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
                // The dashboard's path, which reports the level back.
                self.set_reply_volume(Some(v)).await;
                self.store.save(&self.settings);
                return;
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

    /// The reply volume, or `None` to follow `volume`; Home Assistant's
    /// number shows the level either way. The caller saves.
    async fn set_reply_volume(&mut self, v: Option<u8>) {
        self.settings.reply_volume = v.map(|v| v.min(100));
        tracing::info!("reply volume {:?}", self.settings.reply_volume);
        self.apply_reply_volume();
        self.send(DeviceEvent::NumberState {
            object_id: "reply_volume".into(),
            value: f32::from(self.settings.reply_volume()),
        })
        .await;
    }

    /// The "Listening direction" switch. The caller saves.
    async fn set_listening_direction(&mut self, on: bool) {
        tracing::info!("listening direction on the ring: {on}");
        self.settings.listening_direction = on;
        self.send(DeviceEvent::SwitchState {
            object_id: "listening_direction".into(),
            on,
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

    /// The Home Assistant media player state from the media lane, the voice
    /// lane and the Sendspin group.
    fn update_media_state(&mut self) {
        self.media_state = if matches!(self.playback, Some((_, PlaybackKind::MediaAnnounce, _))) {
            MediaPlayerStateKind::Announcing
        } else {
            match self.media_phase {
                MediaPhase::Idle if self.sendspin_paused() => MediaPlayerStateKind::Paused,
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
            self.message_stopped("Cancel request");
            self.stop_playback();
            if !self.sendspin_control(crate::sendspin::Control::Stop).await
                && let Some(s) = &self.speaker
            {
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
            self.set_media_volume((v * 100.0).round().clamp(0.0, 100.0) as u8)
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
        // While Sendspin owns the media, pause, play and stop act on its
        // group (ADR 0027 §14); the lane acts only when the server can't.
        let to_group = match command {
            Some(MediaPlayerCommand::Pause) => Some(crate::sendspin::Control::Pause),
            Some(MediaPlayerCommand::Play) => Some(crate::sendspin::Control::Play),
            Some(MediaPlayerCommand::Stop) => Some(crate::sendspin::Control::Stop),
            _ => None,
        };
        let sent = match to_group {
            Some(control) => self.sendspin_control(control).await,
            None => false,
        };
        let media = media.filter(|_| !sent);
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
                let muted = command == Some(MediaPlayerCommand::Mute);
                if self.sendspin_streams()
                    && let Some(s) = &self.sendspin
                {
                    // The stream's own mute, which the server hears (ADR
                    // 0027 §15). Unmuting also lifts the lane's, which would
                    // otherwise keep it silent.
                    s.set_muted(muted);
                    if let Some(m) = media.as_ref().filter(|_| !muted) {
                        m.set_muted(false);
                    }
                } else if let Some(m) = &media {
                    m.set_muted(muted);
                }
            }
            Some(MediaPlayerCommand::VolumeUp) => {
                let v = self.media_volume().saturating_add(5).min(100);
                self.set_media_volume(v).await;
            }
            Some(MediaPlayerCommand::VolumeDown) => {
                let v = self.media_volume().saturating_sub(5);
                self.set_media_volume(v).await;
            }
            _ => {}
        }
    }

    fn play_media_checked(&self, url: String, reply: oneshot::Sender<Result<(), String>>) {
        let Some(speaker) = &self.speaker else {
            let _ = reply.send(Err(
                "This satellite has no working speaker, so it can't play media. \
                 Logs, under System, says why."
                    .into(),
            ));
            return;
        };
        let media = speaker.media();
        tokio::spawn(async move {
            let host = url::Url::parse(&url)
                .ok()
                .and_then(|url| url.host_str().map(str::to_owned))
                .unwrap_or_else(|| "local media".into());
            let result = media.play_url(&url).await;
            match &result {
                Ok(()) => tracing::info!("media host {host}: audio is decoding"),
                Err(_) => tracing::warn!("media host {host}: playback failed"),
            }
            let _ = reply.send(result.map_err(|e| e.to_string()));
        });
    }

    // --- runs -----------------------------------------------------------------

    async fn start_run(&mut self, wake_phrase: Option<String>) {
        if self.mode() == Mode::Standalone {
            return self
                .start_local_run(pinna_assist::run::RunKind::Request, wake_phrase)
                .await;
        }
        if !self.voice_subscribed {
            tracing::warn!("no Home Assistant voice subscription; cannot start a run");
            self.error_feedback();
            return;
        }
        self.shared.counters.runs.inc();
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
        if self.wake_sound() {
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
        let was_local = self.local.run.is_some();
        self.cancel_local();
        self.stop_streaming();
        if self.phase != Phase::Idle {
            self.event("voice_assistant", "stopped").await;
        }
        if tell_ha && !was_local && self.phase != Phase::Idle {
            self.send(DeviceEvent::VoiceStop).await;
            self.stale_run = !self.run_ended;
        }
        self.stop_playback();
        self.phase = Phase::Idle;
        self.continue_conversation = false;
        if was_local {
            self.apply_deferred_mute().await;
        }
    }

    /// The wake word is acted on in every phase but listening, where the
    /// microphone already goes to Home Assistant and the detector doesn't
    /// run. Outside idle it stops the request, reply or announcement, and
    /// starts a new request. Calibration counts wakes only while idle.
    fn wake_armed(&self) -> bool {
        match self.phase {
            Phase::Idle => true,
            Phase::Listening(_) => false,
            Phase::Starting(_) | Phase::Thinking(_) | Phase::Speaking => self.calibration.is_none(),
        }
    }

    /// Stop the request, and the reply or announcement that is loading or
    /// playing, as an ESPHome satellite does: Home Assistant hears the reply
    /// finished, so it doesn't wait for it.
    async fn interrupt(&mut self) {
        if matches!(
            self.playback,
            Some((_, PlaybackKind::Tts | PlaybackKind::Announce { .. }, _))
        ) {
            self.send(DeviceEvent::AnnounceFinished { success: true })
                .await;
        }
        self.abort_run(true).await;
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
        let local = match &self.playback {
            Some((t, PlaybackKind::Tts, _)) if *t == token => false,
            Some((t, PlaybackKind::Reply, _)) if *t == token => true,
            _ => return,
        };
        if let Phase::Thinking(requested) = self.phase {
            let now = Instant::now();
            let sent = (now - requested).as_secs_f32();
            match self.speech_ended {
                Some(end) => {
                    let after = now - end;
                    self.shared
                        .counters
                        .first_audio
                        .observe(after.as_secs_f64());
                    if local {
                        tracing::info!(
                            "reply: first audio {:.2} s after the end of speech",
                            after.as_secs_f32()
                        )
                    } else {
                        tracing::info!(
                            "reply: first audio {:.2} s after the end of speech ({sent:.2} s after Home Assistant started it)",
                            after.as_secs_f32()
                        )
                    }
                }
                None if local => {
                    tracing::info!("reply: first audio {sent:.2} s after the request")
                }
                None => {
                    tracing::info!("reply: first audio {sent:.2} s after Home Assistant started it")
                }
            }
        }
        self.phase = Phase::Speaking;
        self.event("voice_assistant", "replying").await;
        if local {
            self.reply_started(Instant::now());
        }
    }

    fn stop_playback(&mut self) {
        if let Some((_, kind, task)) = self.playback.take() {
            debug_assert!(
                kind != PlaybackKind::Reply || self.local.run.is_none(),
                "a live run's reply stops only with the run"
            );
            self.speaker_quiet_since = Instant::now();
            task.abort();
            if kind == PlaybackKind::MediaAnnounce {
                self.update_media_state();
            }
            if kind == PlaybackKind::Message {
                self.message_cut();
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
            PlaybackKind::Reply => self.reply_done(ok).await,
            PlaybackKind::Message => self.message_done(ok, Instant::now()),
            PlaybackKind::Speak | PlaybackKind::Cue => {}
        }
    }

    // --- housekeeping -------------------------------------------------------------

    async fn tick(&mut self) {
        self.restart_for_update().await;
        let now = Instant::now();
        if self.bt_pairing_until.is_some_and(|(until, _)| now >= until) {
            tracing::info!("bluetooth: pairing window closed");
            self.bt_pairing_until = None;
        }
        if let Some(since) = self.bt_ready_since {
            if !crate::bluetooth::wifi_ok() {
                self.bt_ready_since = None;
            } else if now - since >= crate::bluetooth::SETTLE {
                // The start counts: it runs, and the network with it.
                self.bt_ready_since = None;
                let _ = std::fs::remove_file(crate::bluetooth::MARKER);
                tracing::info!("bluetooth: running with the Wi-Fi up");
            }
        }
        if self.local.run.is_none() {
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
        }
        self.tick_local(now).await;
        self.tick_timers(now, SystemTime::now(), crate::clock::synced)
            .await;
        if self.calibration.as_ref().is_some_and(|c| c.expired(now)) {
            self.end_calibration().await;
        }
        if self
            .presence
            .as_ref()
            .is_some_and(|p| now >= p.until || p.pressed.is_closed())
        {
            self.presence = None;
        }
        if let Some(mode) = self.pending_mode
            && self.switch_idle()
        {
            self.pending_mode = None;
            let patch = SettingsPatch {
                mode: Some(mode),
                ..SettingsPatch::default()
            };
            if let Err(why) = self.update_settings(patch).await {
                tracing::info!("a waiting switch to {} was refused: {why}", mode.label());
                // The select asked for it may still show the request.
                self.send_mode().await;
            }
        }
        if self.reconnect_for_mode
            && self.phase == Phase::Idle
            && self.ringing.is_none()
            && self.playback.is_none()
        {
            self.reconnect_for_mode = false;
            if self.clients > 0 {
                tracing::info!("asking Home Assistant to reconnect for the new mode");
                // The reconnect reads a new name too.
                self.reconnect_pending = false;
                self.send(DeviceEvent::Disconnect).await;
            }
        }
        if self.reconnect_pending && self.quiet() {
            self.reconnect_pending = false;
            if self.clients > 0 {
                tracing::info!("asking Home Assistant to reconnect to read what changed");
                self.send(DeviceEvent::Disconnect).await;
            }
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
        self.tend_messages(now);
        self.track_talker();
    }

    /// `synced` is asked only when a save or a restore needs it: this runs
    /// four times a second.
    async fn tick_timers(&mut self, now: Instant, wall: SystemTime, synced: fn() -> bool) {
        self.restore_timers(now, wall, synced).await;
        for id in timers::due(&self.timers, now) {
            self.event("timer", "finished").await;
            self.timer_finished(id).await;
        }
        let running = self
            .timers
            .values()
            .any(|timer| timer.origin == Origin::Local && timer.active);
        if running && self.timer_store.save_needed(now) {
            self.save_timers_at(now, wall, synced());
        }
    }

    /// While listening, point the ring at the talker's beam (LibreEcho's
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
        } else if self.presence.is_some() {
            // Above Muted: someone who has to press Action needs to see it.
            Effect::Confirm
        } else if muted {
            Effect::Muted
        } else if self.ringing.is_some() {
            Effect::TimerRinging
        } else if self.calibration.is_some() && self.phase == Phase::Idle {
            // Calibration shows where the sensitivity stands.
            Effect::Calibration(self.settings.wake_sensitivity)
        } else if self.bt_pairing_until.is_some() && self.phase == Phase::Idle {
            Effect::Pairing
        } else {
            match self.phase {
                // A media player announcement, a Speak or a group message
                // speaks without a run.
                Phase::Idle
                    if matches!(
                        self.playback,
                        Some((
                            _,
                            PlaybackKind::MediaAnnounce
                                | PlaybackKind::Speak
                                | PlaybackKind::Message,
                            _
                        ))
                    ) =>
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
        // Calibration wakes and wakes that stop an alarm start no request, so
        // they don't need a pipeline.
        let wake = self.wake_armed()
            && !muted
            && self.settings.wake_word
            && (self.assist_ready() || self.calibration.is_some() || self.ringing.is_some())
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

/// The record to publish when its `group` value differs from `group`, with
/// the Brain's copy updated to it; `None` when nothing changed, or without
/// mDNS. Every change restarts systemd-resolved (ADR 0024), so an unchanged
/// record is never written again.
fn group_record_changed(
    mdns: &mut Option<pinna_esphome::DeviceConfig>,
    group: Option<String>,
) -> Option<pinna_esphome::DeviceConfig> {
    let dc = mdns.as_mut().filter(|dc| dc.group != group)?;
    match group.as_deref() {
        None => tracing::info!("mDNS record: Satellite group off"),
        Some("") => tracing::info!("mDNS record: Satellite group on, in no group"),
        Some(id) => tracing::info!("mDNS record: in the satellite group {id}"),
    }
    dc.group = group;
    Some(dc.clone())
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

#[cfg(test)]
mod checked_media_tests {
    use std::path::Path;

    use pinna_audio::{Backend, SpeakerConfig};

    use super::*;

    fn brain_at(dir: &Path, with_speaker: bool) -> Brain {
        let (esp, _events) = EsphomeHandle::detached(8);
        let speaker = with_speaker.then(|| {
            Arc::new(Speaker::new(SpeakerConfig::new(Backend::WavFile(
                dir.join("output.wav"),
            ))))
        });
        Brain::new(
            esp,
            speaker,
            Arc::new(Shared::default()),
            watch::channel(LedState::default()).0,
            watch::channel(Snapshot::default()).0,
            mpsc::channel(8).0,
            Options {
                key_file: dir.join("key"),
                settings_file: dir.join("settings.toml"),
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
                airplay: false,
                bluetooth: false,
                sendspin: None,
                settings: Settings::default(),
            },
        )
    }

    async fn play(brain: &mut Brain, url: String) -> Result<(), String> {
        let (reply, result) = oneshot::channel();
        brain
            .handle(Input::Api(ApiCommand::PlayMediaChecked(url, reply)))
            .await;
        tokio::time::timeout(Duration::from_secs(5), result)
            .await
            .unwrap()
            .unwrap()
    }

    fn wav() -> Vec<u8> {
        let samples = vec![0i16; 1_600];
        let data_len = (samples.len() * 2) as u32;
        let mut bytes = Vec::new();
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
        for sample in samples {
            bytes.extend_from_slice(&sample.to_le_bytes());
        }
        bytes
    }

    #[tokio::test]
    async fn checked_media_keeps_the_no_speaker_refusal() {
        let dir = tempfile::tempdir().unwrap();
        let mut brain = brain_at(dir.path(), false);

        let error = play(&mut brain, "http://radio.test/stream".into())
            .await
            .unwrap_err();

        assert_eq!(
            error,
            "This satellite has no working speaker, so it can't play media. \
             Logs, under System, says why."
        );
    }

    #[tokio::test]
    async fn checked_media_replies_when_a_local_wav_decodes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sample.wav");
        std::fs::write(&path, wav()).unwrap();
        let mut brain = brain_at(dir.path(), true);

        play(&mut brain, path.to_string_lossy().into_owned())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn checked_media_returns_an_unreachable_url_error() {
        let dir = tempfile::tempdir().unwrap();
        let mut brain = brain_at(dir.path(), true);
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/stream", listener.local_addr().unwrap());
        drop(listener);

        let error = play(&mut brain, url).await.unwrap_err();

        assert!(!error.is_empty());
    }
}
