//! The entity set of the previous firmware, so Home Assistant keeps its entity IDs.

use crate::types::{
    ButtonConfig, DeviceConfig, EntityCategory, EventConfig, LightConfig, MediaFormat,
    MediaPlayerConfig, MediaPurpose, NumberConfig, NumberMode, SelectConfig, SwitchConfig,
    TextSensorConfig, TimeConfig, UpdateConfig, media_feature,
};

fn strings(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| (*s).to_owned()).collect()
}

fn number(object_id: &str, name: &str, unit: Option<&str>, icon: &str) -> NumberConfig {
    NumberConfig {
        object_id: object_id.into(),
        name: name.into(),
        icon: icon.into(),
        min: 0.0,
        max: 100.0,
        step: 1.0,
        unit: unit.map(str::to_owned),
        entity_category: EntityCategory::Config,
        mode: NumberMode::Slider,
    }
}

fn event(object_id: &str, name: &str, device_class: Option<&str>, types: &[&str]) -> EventConfig {
    EventConfig {
        object_id: object_id.into(),
        name: name.into(),
        icon: String::new(),
        device_class: device_class.map(str::to_owned),
        event_types: strings(types),
    }
}

fn format(
    format: &str,
    sample_rate: u32,
    num_channels: u32,
    purpose: MediaPurpose,
    sample_bytes: u32,
) -> MediaFormat {
    MediaFormat {
        format: format.into(),
        sample_rate,
        num_channels,
        purpose,
        sample_bytes,
    }
}

/// Fill `cfg` with the entities the previous firmware exposed (names are part of the HA unique id),
/// plus Pinna's own controls.
/// The "Voice mode" select's options: the dashboard's names for the modes.
pub const MODE_HOME_ASSISTANT: &str = "Home Assistant";
pub const MODE_STANDALONE: &str = "Standalone";
/// The "Install updates" select's options (ADR 0022).
pub const INSTALL_MANUALLY: &str = "Manually";
pub const INSTALL_AUTOMATICALLY: &str = "Automatically";

pub fn echo_dot_entities(cfg: &mut DeviceConfig) {
    cfg.light = Some(LightConfig {
        name: "LED ring".into(),
        object_id: "led_ring".into(),
        effects: strings(&["Solid", "Pulse", "Flash"]),
    });
    cfg.mute_switch = true;
    cfg.switches = vec![
        SwitchConfig {
            object_id: "wake_word".into(),
            name: "Wake word".into(),
            icon: "mdi:account-voice".into(),
            entity_category: EntityCategory::Config,
        },
        SwitchConfig {
            object_id: "listening_direction".into(),
            name: "Listening direction".into(),
            icon: "mdi:compass-outline".into(),
            entity_category: EntityCategory::Config,
        },
        // An AirPlay receiver for the media lane (ADR 0011); off by default.
        SwitchConfig {
            object_id: "airplay".into(),
            name: "AirPlay".into(),
            icon: "mdi:cast-audio".into(),
            entity_category: EntityCategory::Config,
        },
        // A Bluetooth A2DP sink for the media lane (ADR 0011); off by default.
        // "audio": not ESPHome's Bluetooth proxy.
        SwitchConfig {
            object_id: "bluetooth".into(),
            name: "Bluetooth audio".into(),
            icon: "mdi:bluetooth-audio".into(),
            entity_category: EntityCategory::Config,
        },
        // The Bluetooth pairing window: on for two minutes, off when it
        // closes (a phone paired, time ran out, stopped) or can't open.
        SwitchConfig {
            object_id: "bluetooth_pairing".into(),
            name: "Bluetooth pairing".into(),
            icon: "mdi:bluetooth-connect".into(),
            entity_category: EntityCategory::None,
        },
        // A Sendspin player for Music Assistant's groups (ADR 0027); off by
        // default.
        SwitchConfig {
            object_id: "sendspin".into(),
            name: "Sendspin".into(),
            icon: "mdi:speaker-multiple".into(),
            entity_category: EntityCategory::Config,
        },
        // Tune the sensitivity with the volume buttons; turns itself off when
        // calibration ends.
        SwitchConfig {
            object_id: "wake_word_calibration".into(),
            name: "Wake word calibration".into(),
            icon: "mdi:tune-vertical".into(),
            entity_category: EntityCategory::Config,
        },
        // A chime when listening starts (ADR 0007).
        SwitchConfig {
            object_id: "wake_sound".into(),
            name: "Wake sound".into(),
            icon: "mdi:bell-outline".into(),
            entity_category: EntityCategory::Config,
        },
        // Pending notification: the ring pulses until the Action button, the
        // dashboard or Home Assistant clears it.
        SwitchConfig {
            object_id: "notification".into(),
            name: "Notification".into(),
            icon: "mdi:bell-ring".into(),
            entity_category: EntityCategory::None,
        },
        // Messages from the other members of the satellite group (ADR 0026).
        // A control, not configuration: an automation turns it off at night.
        // Listed in both modes, like the Voice mode select.
        SwitchConfig {
            object_id: "group_messages".into(),
            name: "Group messages".into(),
            icon: "mdi:message-text-outline".into(),
            entity_category: EntityCategory::None,
        },
    ];
    cfg.numbers = vec![
        number("volume", "Volume", Some("%"), "mdi:volume-high"),
        number(
            "reply_volume",
            "Reply volume",
            Some("%"),
            "mdi:account-voice",
        ),
        number(
            "wake_word_sensitivity",
            "Wake word sensitivity",
            None,
            "mdi:tune-variant",
        ),
        number(
            "led_brightness",
            "LED brightness",
            Some("%"),
            "mdi:brightness-6",
        ),
    ];
    cfg.buttons = vec![ButtonConfig {
        object_id: "cancel_request".into(),
        name: "Cancel request".into(),
        icon: "mdi:close-circle-outline".into(),
    }];
    let press = ["press", "long_press", "double_press"];
    cfg.events = vec![
        event("volume_up", "Volume up button", Some("button"), &press),
        event("volume_down", "Volume down button", Some("button"), &press),
        event(
            "microphone_mute",
            "Microphone mute button",
            Some("button"),
            &press,
        ),
        event("action", "Action button", Some("button"), &press),
        event(
            "voice_assistant",
            "Voice assistant",
            None,
            &[
                "wake_word",
                "listening",
                "thinking",
                "replying",
                "finished",
                "stopped",
            ],
        ),
        event("playback", "Playback", None, &["started", "stopped"]),
        event(
            "timer",
            "Timer",
            None,
            &["started", "updated", "cancelled", "finished"],
        ),
        // Someone cleared the notification at the device or the dashboard
        // (not when Home Assistant turns the switch off).
        event("notification", "Notification", None, &["cleared"]),
    ];
    cfg.text_sensors = vec![TextSensorConfig {
        object_id: "last_wake_word".into(),
        name: "Last wake word".into(),
        icon: "mdi:account-voice".into(),
    }];
    cfg.media_player = Some(MediaPlayerConfig {
        object_id: "media_player".into(),
        name: "Media player".into(),
        icon: String::new(),
        supports_pause: true,
        feature_flags: media_feature::PAUSE
            | media_feature::VOLUME_SET
            | media_feature::VOLUME_MUTE
            | media_feature::PLAY_MEDIA
            | media_feature::STOP
            | media_feature::PLAY
            | media_feature::MEDIA_ANNOUNCE,
        supported_formats: vec![
            format("mp3", 48000, 2, MediaPurpose::Default, 0),
            // Home Assistant renders replies in the first announcement
            // format. Speech as 22.05 kHz mono MP3 is ~4 KB/s (48 kHz WAV
            // would be 96 KB/s), so replies still arrive over a weak link.
            format("mp3", 22050, 1, MediaPurpose::Announcement, 0),
            format("wav", 22050, 1, MediaPurpose::Announcement, 2),
        ],
    });
    cfg.selects = vec![
        // The update channel (ADR 0022). When pinnad.toml sets a manifest URL,
        // pinnad replaces these options with that URL.
        SelectConfig {
            object_id: "update_channel".into(),
            name: "Updates".into(),
            icon: "mdi:update".into(),
            options: strings(&["stable", "beta", "off"]),
            entity_category: EntityCategory::Config,
        },
        // Who answers requests (ADR 0017): Standalone is refused until the
        // satellite's own services are ready, as on the dashboard. Not
        // "Assistant": Home Assistant's pipeline select on the same device
        // page has that name. The object ID is older than the name.
        SelectConfig {
            object_id: "assistant".into(),
            name: "Voice mode".into(),
            icon: "mdi:account-voice".into(),
            options: strings(&[MODE_HOME_ASSISTANT, MODE_STANDALONE]),
            entity_category: EntityCategory::Config,
        },
        // Automatic installs and their window (ADR 0022).
        SelectConfig {
            object_id: "install_updates".into(),
            name: "Install updates".into(),
            icon: "mdi:calendar-clock".into(),
            options: strings(&[INSTALL_MANUALLY, INSTALL_AUTOMATICALLY]),
            entity_category: EntityCategory::Config,
        },
    ];
    cfg.times = vec![
        TimeConfig {
            object_id: "install_window_start".into(),
            name: "Install window start".into(),
            icon: "mdi:clock-start".into(),
            entity_category: EntityCategory::Config,
        },
        TimeConfig {
            object_id: "install_window_end".into(),
            name: "Install window end".into(),
            icon: "mdi:clock-end".into(),
            entity_category: EntityCategory::Config,
        },
    ];
    // The release the satellite runs and the newest on its channel (ADR 0022).
    cfg.updates = vec![UpdateConfig {
        object_id: "firmware".into(),
        name: "Firmware".into(),
        icon: String::new(),
        device_class: Some("firmware".into()),
        entity_category: EntityCategory::Config,
    }];
}
