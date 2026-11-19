//! Server side of the ESPHome native API (plaintext or Noise), so Home Assistant adopts Pinna as
//! an ESPHome device with a voice assistant, an RGB light, a microphone-mute switch and
//! generic number, button, event, text sensor, media player, select, time and update entities ([`echo_dot_entities`]).
//!
//! # API
//!
//! ```ignore
//! let mut cfg = DeviceConfig::new("pinna-kitchen", mac);
//! cfg.light = Some(LightConfig { name: "LED ring".into(), object_id: "led_ring".into(), effects: vec![] });
//! cfg.mute_switch = true;
//! let (handle, mut commands) = pinna_esphome::serve(cfg).await?;
//! handle.send(DeviceEvent::MuteState(false)).await?;
//! while let Some(cmd) = commands.recv().await { /* HaCommand */ }
//! ```
//!
//! * [`serve`] binds `cfg.bind:cfg.port`, returns an [`EsphomeHandle`] (clone it freely) to push
//!   [`DeviceEvent`]s, and an `mpsc::Receiver<`[`HaCommand`]`>` carrying everything Home
//!   Assistant sends. Dropping the receiver only loses commands (they are logged).
//! * Current light and mute state live inside the server; clients that issue
//!   `SubscribeStates` get them immediately. Commands from HA are *not* applied
//!   automatically: the device acts on them and reports back with `LightState`/`MuteState`.
//! * Voice audio and events go to the single client that subscribed to the voice assistant
//!   (with API audio). `VoiceStart` is ignored without a subscriber; `VoiceAudio` is only
//!   forwarded between `VoiceStart` and `VoiceAudioEnd`/`VoiceStop`.
//! * At most two clients are served; idle ones are pinged after 60 s and dropped after 120 s.
//! * [`mdns_txt`] returns the TXT records for `_esphomelib._tcp`; publishing is the caller's job.
//! * Framing is behind [`frame::FrameCodec`]. With `cfg.encryption_key` set the server speaks
//!   only Noise (`Noise_NNpsk0_25519_ChaChaPoly_SHA256`) and [`mdns_txt`] advertises it.
//!   Without a key it is provisionable: plaintext, or Noise with the all-zero key, and the
//!   first key anyone sends is taken. A key, first or rotated over a connection encrypted
//!   with the current one, is used for new connections at once and reported as
//!   [`HaCommand::EncryptionKeySet`] so the caller can persist it.

pub mod frame;
mod profile;
pub mod proto;
mod server;
mod types;

pub use profile::{
    INSTALL_AUTOMATICALLY, INSTALL_MANUALLY, MODE_HOME_ASSISTANT, MODE_STANDALONE,
    echo_dot_entities,
};
pub use server::{EsphomeHandle, LIGHT_KEY, MUTE_KEY, Stopped, serve};
pub use types::{
    ButtonConfig, DeviceConfig, DeviceEvent, ESPHOME_VERSION, EntityCategory, EventConfig,
    HaCommand, InvalidKey, LightConfig, LightState, MediaFormat, MediaPlayerCommand,
    MediaPlayerConfig, MediaPlayerStateKind, MediaPurpose, NumberConfig, NumberMode, SelectConfig,
    SwitchConfig, TextSensorConfig, TimeConfig, TimerEventType, UpdateCommand, UpdateConfig,
    UpdateState, VOICE_FEATURE_FLAGS, VoiceEventType, WakeWordInfo, mdns_txt, media_feature,
    parse_encryption_key,
};
