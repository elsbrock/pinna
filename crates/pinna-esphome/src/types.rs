//! Public configuration, event and command types.

use std::net::{IpAddr, Ipv4Addr};

use bytes::Bytes;

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

/// Static description of the device.
#[derive(Debug, Clone, PartialEq, Eq)]
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
    /// Address to listen on.
    pub bind: IpAddr,
    pub wake_words: Vec<WakeWordInfo>,
    pub active_wake_words: Vec<String>,
    /// How many wake words Home Assistant may activate at once.
    pub max_active_wake_words: u32,
    pub light: Option<LightConfig>,
    pub mute_switch: bool,
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
            bind: IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            wake_words: Vec::new(),
            active_wake_words: Vec::new(),
            max_active_wake_words: 1,
            light: None,
            mute_switch: false,
        }
    }

    /// MAC as `AA:BB:CC:DD:EE:FF`.
    pub fn mac_string(&self) -> String {
        let m = self.mac;
        format!(
            "{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
            m[0], m[1], m[2], m[3], m[4], m[5]
        )
    }
}

/// TXT records for the `_esphomelib._tcp` mDNS service (publishing is up to the caller).
pub fn mdns_txt(cfg: &DeviceConfig) -> Vec<(String, String)> {
    [
        ("version", ESPHOME_VERSION.to_owned()),
        ("mac", cfg.mac_string()),
        ("platform", cfg.manufacturer.clone()),
        ("board", cfg.model.clone()),
        ("network", "wifi".to_owned()),
        ("friendly_name", cfg.friendly_name.clone()),
        ("project_name", cfg.project_name.clone()),
        ("project_version", cfg.project_version.clone()),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v))
    .collect()
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn txt_records() {
        let cfg = DeviceConfig::new("pinna-1", [0x02, 0, 0, 0, 0xab, 1]);
        let txt = mdns_txt(&cfg);
        assert!(txt.contains(&("mac".into(), "02:00:00:00:AB:01".into())));
        assert!(txt.contains(&("version".into(), "2026.9.0".into())));
        assert!(!txt.iter().any(|(k, _)| k == "api_encryption"));
    }
}
