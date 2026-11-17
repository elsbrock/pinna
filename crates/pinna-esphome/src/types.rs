//! Public configuration, event and command types.

use std::net::{IpAddr, Ipv6Addr};

use bytes::Bytes;

use crate::frame::NOISE_PROTOCOL;

/// ESPHome version reported to Home Assistant.
pub const ESPHOME_VERSION: &str = "2026.9.0";
/// `voice_assistant_feature_flags`: VOICE_ASSISTANT | API_AUDIO | TIMERS | ANNOUNCE | START_CONVERSATION.
pub const VOICE_FEATURE_FLAGS: u32 = 1 | 4 | 8 | 16 | 32;

/// A wake word offered to Home Assistant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WakeWordInfo {
    pub id: String,
    pub wake_word: String,
    pub trained_languages: Vec<String>,
}

/// The LED ring exposed as an RGB light.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LightConfig {
    pub name: String,
    pub object_id: String,
    /// Effect names offered to Home Assistant (without the implicit "None").
    pub effects: Vec<String>,
}

/// `EntityCategory` of an entity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EntityCategory {
    #[default]
    None,
    Config,
    Diagnostic,
}

/// `NumberMode` of a number entity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NumberMode {
    #[default]
    Auto,
    Box,
    Slider,
}

/// A number entity (slider or box).
#[derive(Debug, Clone, PartialEq)]
pub struct NumberConfig {
    pub object_id: String,
    pub name: String,
    pub icon: String,
    pub min: f32,
    pub max: f32,
    pub step: f32,
    pub unit: Option<String>,
    pub entity_category: EntityCategory,
    pub mode: NumberMode,
}

/// A button entity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ButtonConfig {
    pub object_id: String,
    pub name: String,
    pub icon: String,
}

/// An event entity; [`DeviceEvent::Event`] must use one of `event_types`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventConfig {
    pub object_id: String,
    pub name: String,
    pub icon: String,
    pub device_class: Option<String>,
    pub event_types: Vec<String>,
}

/// A text sensor entity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextSensorConfig {
    pub object_id: String,
    pub name: String,
    pub icon: String,
}

/// A switch besides the built-in microphone mute switch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwitchConfig {
    pub object_id: String,
    pub name: String,
    pub icon: String,
    pub entity_category: EntityCategory,
}

/// A select entity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectConfig {
    pub object_id: String,
    pub name: String,
    pub icon: String,
    pub options: Vec<String>,
    pub entity_category: EntityCategory,
}

/// An update entity (Home Assistant's update card).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateConfig {
    pub object_id: String,
    pub name: String,
    pub icon: String,
    /// For example, "firmware".
    pub device_class: Option<String>,
    pub entity_category: EntityCategory,
}

/// What an update entity shows. The default is unknown until the device reports a state.
#[derive(Debug, Clone, PartialEq)]
pub struct UpdateState {
    /// Home Assistant shows the entity as unknown when this is true.
    pub missing: bool,
    pub in_progress: bool,
    /// 0..=100; `None` means no progress is known.
    pub progress: Option<f32>,
    pub current_version: String,
    pub latest_version: String,
    pub title: String,
    pub release_summary: String,
    pub release_url: String,
}

impl Default for UpdateState {
    fn default() -> Self {
        Self {
            missing: true,
            in_progress: false,
            progress: None,
            current_version: String::new(),
            latest_version: String::new(),
            title: String::new(),
            release_summary: String::new(),
            release_url: String::new(),
        }
    }
}

/// Home Assistant's update command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateCommand {
    Install,
    Check,
}

/// Purpose of a media player format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MediaPurpose {
    #[default]
    Default,
    Announcement,
}

/// An audio format the media player accepts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaFormat {
    pub format: String,
    pub sample_rate: u32,
    pub num_channels: u32,
    pub purpose: MediaPurpose,
    pub sample_bytes: u32,
}

/// `MediaPlayerEntityFeature` bits (as in aioesphomeapi / Home Assistant).
pub mod media_feature {
    pub const PAUSE: u32 = 1 << 0;
    pub const VOLUME_SET: u32 = 1 << 2;
    pub const VOLUME_MUTE: u32 = 1 << 3;
    pub const PLAY_MEDIA: u32 = 1 << 9;
    pub const STOP: u32 = 1 << 12;
    pub const PLAY: u32 = 1 << 14;
    pub const MEDIA_ANNOUNCE: u32 = 1 << 20;
}

/// The media player entity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaPlayerConfig {
    pub object_id: String,
    pub name: String,
    pub icon: String,
    pub supports_pause: bool,
    /// `media_feature` bits.
    pub feature_flags: u32,
    pub supported_formats: Vec<MediaFormat>,
}

/// Static description of the device.
#[derive(Debug, Clone, PartialEq)]
pub struct DeviceConfig {
    pub name: String,
    pub friendly_name: String,
    pub mac: [u8; 6],
    pub model: String,
    pub manufacturer: String,
    pub project_name: String,
    pub project_version: String,
    pub suggested_area: Option<String>,
    pub port: u16,
    /// Address to listen on. The default `::` takes IPv4 too (Linux's
    /// `bindv6only` is 0): mDNS announces every address of the satellite.
    pub bind: IpAddr,
    pub voice_feature_flags: u32,
    pub wake_words: Vec<WakeWordInfo>,
    pub active_wake_words: Vec<String>,
    /// How many wake words Home Assistant may activate at once.
    pub max_active_wake_words: u32,
    pub light: Option<LightConfig>,
    pub mute_switch: bool,
    pub numbers: Vec<NumberConfig>,
    pub buttons: Vec<ButtonConfig>,
    pub events: Vec<EventConfig>,
    pub text_sensors: Vec<TextSensorConfig>,
    pub switches: Vec<SwitchConfig>,
    pub selects: Vec<SelectConfig>,
    pub updates: Vec<UpdateConfig>,
    pub media_player: Option<MediaPlayerConfig>,
    /// Noise pre-shared key. `None` serves the plaintext API.
    pub encryption_key: Option<[u8; 32]>,
    /// Port of the device's web page, which Home Assistant links to as
    /// "Visit". 0 = none.
    pub webserver_port: u16,
    /// The dashboard address (ADR 0021), announced for other satellites.
    pub dashboard_url: Option<String>,
}

/// The encryption key is not base64 of exactly 32 bytes.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("encryption key must be base64 of 32 bytes")]
pub struct InvalidKey;

/// Decode an ESPHome `api: encryption: key:` value (base64 of 32 bytes).
pub fn parse_encryption_key(s: &str) -> Result<[u8; 32], InvalidKey> {
    use base64::{Engine, engine::general_purpose::STANDARD};
    let raw = STANDARD.decode(s.trim()).map_err(|_| InvalidKey)?;
    raw.try_into().map_err(|_| InvalidKey)
}

impl DeviceConfig {
    /// Defaults for everything but the identity: port 6053, no light, no switch.
    pub fn new(name: impl Into<String>, mac: [u8; 6]) -> Self {
        let name = name.into();
        Self {
            friendly_name: name.clone(),
            name,
            mac,
            model: "pinna".into(),
            manufacturer: "Pinna".into(),
            project_name: "pinna.satellite".into(),
            project_version: env!("CARGO_PKG_VERSION").into(),
            suggested_area: None,
            port: 6053,
            bind: IpAddr::V6(Ipv6Addr::UNSPECIFIED),
            voice_feature_flags: VOICE_FEATURE_FLAGS,
            wake_words: Vec::new(),
            active_wake_words: Vec::new(),
            max_active_wake_words: 1,
            light: None,
            mute_switch: false,
            numbers: Vec::new(),
            buttons: Vec::new(),
            events: Vec::new(),
            text_sensors: Vec::new(),
            switches: Vec::new(),
            selects: Vec::new(),
            updates: Vec::new(),
            media_player: None,
            encryption_key: None,
            webserver_port: 0,
            dashboard_url: None,
        }
    }

    /// MAC as `aa:bb:cc:dd:ee:ff`. Home Assistant builds entity unique ids from this string
    /// verbatim, so its case must stay stable (the previous firmware, whose entries Pinna takes over,
    /// reported lowercase).
    pub fn mac_string(&self) -> String {
        let m = self.mac;
        format!(
            "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            m[0], m[1], m[2], m[3], m[4], m[5]
        )
    }
}

/// TXT records for the `_esphomelib._tcp` mDNS service (publishing is up to the caller).
pub fn mdns_txt(cfg: &DeviceConfig) -> Vec<(String, String)> {
    let mut txt = vec![
        ("version", ESPHOME_VERSION.to_owned()),
        ("mac", cfg.mac_string()),
        ("platform", cfg.manufacturer.clone()),
        ("board", cfg.model.clone()),
        ("network", "wifi".to_owned()),
        ("friendly_name", cfg.friendly_name.clone()),
        ("project_name", cfg.project_name.clone()),
        ("project_version", cfg.project_version.clone()),
    ];
    if cfg.encryption_key.is_some() {
        txt.push(("api_encryption", NOISE_PROTOCOL.to_owned()));
    }
    // The web listener's port and the dashboard address, for other satellites'
    // switcher (ADR 0021). Home Assistant ignores keys it doesn't know.
    if cfg.webserver_port != 0 {
        txt.push(("web_port", cfg.webserver_port.to_string()));
    }
    if let Some(url) = &cfg.dashboard_url {
        txt.push(("url", url.clone()));
    }
    txt.into_iter().map(|(k, v)| (k.to_owned(), v)).collect()
}

/// Current state of the light, as last reported by the device.
#[derive(Debug, Clone, PartialEq)]
pub struct LightState {
    pub on: bool,
    pub brightness: f32,
    pub rgb: (f32, f32, f32),
    pub effect: Option<String>,
}

impl Default for LightState {
    fn default() -> Self {
        Self {
            on: false,
            brightness: 1.0,
            rgb: (1.0, 1.0, 1.0),
            effect: None,
        }
    }
}

/// Device to Home Assistant.
#[derive(Debug, Clone, PartialEq)]
pub enum DeviceEvent {
    LightState {
        on: bool,
        brightness: f32,
        rgb: (f32, f32, f32),
        effect: Option<String>,
    },
    MuteState(bool),
    VoiceFeatureFlags(u32),
    /// Begin a voice run; ignored unless a client is subscribed to the voice assistant.
    VoiceStart {
        conversation_id: String,
        wake_word_phrase: Option<String>,
        use_vad: bool,
    },
    /// 16 kHz s16le mono; only forwarded between `VoiceStart` and `VoiceAudioEnd`/`VoiceStop`.
    VoiceAudio(Bytes),
    VoiceAudioEnd,
    /// Abort the current run.
    VoiceStop,
    AnnounceFinished {
        success: bool,
    },
    /// Ask Home Assistant for the time; answered with [`HaCommand::Time`].
    RequestTime,
    /// Current value of a configured number.
    NumberState {
        object_id: String,
        value: f32,
    },
    /// Fire a configured event entity (not cached; unknown types are dropped).
    Event {
        object_id: String,
        event_type: String,
    },
    TextSensorState {
        object_id: String,
        value: String,
    },
    /// State of a configured switch (not the microphone mute switch).
    SwitchState {
        object_id: String,
        on: bool,
    },
    SelectState {
        object_id: String,
        state: String,
    },
    UpdateState {
        object_id: String,
        state: UpdateState,
    },
    MediaPlayerState {
        state: MediaPlayerStateKind,
        /// 0..1
        volume: f32,
        muted: bool,
    },
    /// The device chose its active wake words itself (ids from
    /// `DeviceConfig::wake_words`); Home Assistant reads them when it next asks.
    ActiveWakeWords(Vec<String>),
    /// The device was renamed. Home Assistant reads the name when it connects;
    /// [`DeviceEvent::Disconnect`] makes it reconnect.
    FriendlyName(String),
    /// The node name changed (the hostname, ADR 0010). Home Assistant reads it
    /// when it connects; [`DeviceEvent::Disconnect`] makes it reconnect.
    NodeName(String),
    /// Ask every client to disconnect. Home Assistant reconnects on its own
    /// and reads the device info again.
    Disconnect,
}

/// State of the media player.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaPlayerStateKind {
    Idle,
    Playing,
    Paused,
    Announcing,
}

/// Home Assistant to device.
#[derive(Debug, Clone, PartialEq)]
pub enum HaCommand {
    /// Fields HA did not set are `None`. An effect of "None" means no effect.
    Light {
        on: Option<bool>,
        brightness: Option<f32>,
        rgb: Option<(f32, f32, f32)>,
        effect: Option<String>,
    },
    MuteSwitch(bool),
    Number {
        object_id: String,
        value: f32,
    },
    Button {
        object_id: String,
    },
    /// A configured switch (not the microphone mute switch) was turned on or off.
    Switch {
        object_id: String,
        on: bool,
    },
    Select {
        object_id: String,
        state: String,
    },
    Update {
        object_id: String,
        command: UpdateCommand,
    },
    /// Fields HA did not set are `None`.
    MediaPlayer {
        command: Option<MediaPlayerCommand>,
        volume: Option<f32>,
        media_url: Option<String>,
        announcement: bool,
    },
    /// Answer to [`DeviceEvent::VoiceStart`].
    VoiceStarted {
        port: u32,
        error: bool,
    },
    VoiceEvent {
        event: VoiceEventType,
        data: Vec<(String, String)>,
    },
    Timer {
        event: TimerEventType,
        id: String,
        name: String,
        total_seconds: u32,
        seconds_left: u32,
        is_active: bool,
    },
    Announce {
        media_id: String,
        text: String,
        preannounce_media_id: String,
        start_conversation: bool,
    },
    SetActiveWakeWords(Vec<String>),
    Time {
        epoch_seconds: u32,
    },
    /// A client completed the API hello.
    ClientConnected {
        peer: String,
    },
    ClientDisconnected,
    /// The voice-assistant subscription changed (only the subscribing client counts).
    VoiceSubscribed(bool),
    /// Home Assistant pushed a new encryption key over an encrypted connection. The server
    /// already uses it for new connections; the caller should persist it.
    EncryptionKeySet([u8; 32]),
}

macro_rules! wire_enum {
    ($(#[$m:meta])* $name:ident { $($var:ident = $val:expr),+ $(,)? }) => {
        $(#[$m])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum $name { $($var,)+ Unknown(i32) }
        impl $name {
            /// Decode a wire value.
            pub fn from_i32(v: i32) -> Self {
                match v { $($val => Self::$var,)+ other => Self::Unknown(other) }
            }
            /// Encode to the wire value.
            pub fn to_i32(self) -> i32 {
                match self { $(Self::$var => $val,)+ Self::Unknown(v) => v }
            }
        }
    };
}

wire_enum! {
    /// `VoiceAssistantEvent` values.
    VoiceEventType {
        Error = 0, RunStart = 1, RunEnd = 2, SttStart = 3, SttEnd = 4, IntentStart = 5,
        IntentEnd = 6, TtsStart = 7, TtsEnd = 8, WakeWordStart = 9, WakeWordEnd = 10,
        SttVadStart = 11, SttVadEnd = 12, WakeWordRejected = 13, TtsStreamStart = 98,
        TtsStreamEnd = 99, IntentProgress = 100,
    }
}

wire_enum! {
    /// `VoiceAssistantTimerEvent` values.
    TimerEventType { Started = 0, Updated = 1, Cancelled = 2, Finished = 3 }
}

wire_enum! {
    /// `MediaPlayerCommand` values.
    MediaPlayerCommand {
        Play = 0, Pause = 1, Stop = 2, Mute = 3, Unmute = 4, Toggle = 5, VolumeUp = 6,
        VolumeDown = 7, Enqueue = 8, RepeatOne = 9, RepeatOff = 10, ClearPlaylist = 11,
        TurnOn = 12, TurnOff = 13,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_state_starts_missing() {
        assert!(UpdateState::default().missing);
    }

    #[test]
    fn enum_roundtrip() {
        for v in [
            0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 98, 99, 100, 55, -1,
        ] {
            assert_eq!(VoiceEventType::from_i32(v).to_i32(), v);
        }
        assert_eq!(VoiceEventType::from_i32(8), VoiceEventType::TtsEnd);
        assert_eq!(VoiceEventType::from_i32(55), VoiceEventType::Unknown(55));
        assert_eq!(TimerEventType::from_i32(3), TimerEventType::Finished);
        assert_eq!(TimerEventType::from_i32(9), TimerEventType::Unknown(9));
    }

    #[test]
    fn media_command_roundtrip() {
        for v in 0..=13 {
            assert!(!matches!(
                MediaPlayerCommand::from_i32(v),
                MediaPlayerCommand::Unknown(_)
            ));
            assert_eq!(MediaPlayerCommand::from_i32(v).to_i32(), v);
        }
        assert_eq!(
            MediaPlayerCommand::from_i32(77),
            MediaPlayerCommand::Unknown(77)
        );
    }

    #[test]
    fn txt_records() {
        let cfg = DeviceConfig::new("pinna-1", [0x02, 0, 0, 0, 0xab, 1]);
        let txt = mdns_txt(&cfg);
        assert!(txt.contains(&("mac".into(), "02:00:00:00:ab:01".into())));
        assert!(txt.contains(&("version".into(), "2026.9.0".into())));
        assert!(!txt.iter().any(|(k, _)| k == "api_encryption"));
        assert!(!txt.iter().any(|(k, _)| k == "web_port" || k == "url"));
        let mut cfg = cfg;
        cfg.encryption_key = Some([1; 32]);
        assert!(mdns_txt(&cfg).contains(&(
            "api_encryption".into(),
            "Noise_NNpsk0_25519_ChaChaPoly_SHA256".into()
        )));
        cfg.webserver_port = 80;
        cfg.dashboard_url = Some("https://dot.example.com/".into());
        let txt = mdns_txt(&cfg);
        assert!(txt.contains(&("web_port".into(), "80".into())));
        assert!(txt.contains(&("url".into(), "https://dot.example.com/".into())));
        cfg.webserver_port = 0;
        assert!(!mdns_txt(&cfg).iter().any(|(k, _)| k == "web_port"));
    }

    #[test]
    fn key_parsing() {
        let k = parse_encryption_key("AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE=").unwrap();
        assert_eq!(k, [1; 32]);
        assert_eq!(parse_encryption_key("AQE="), Err(InvalidKey));
        assert_eq!(parse_encryption_key("not base64!"), Err(InvalidKey));
    }
}
