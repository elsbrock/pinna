//! Virtual Echo Dot hardware for the development VM (ADR 0014). These go
//! through the same paths pinnad reads on the device:
//!
//! - buttons: a uinput device with the Dot's keys. [`crate::buttons`] finds it
//!   in `/dev/input` like the device's gpio-keys, so autorepeat and long-press
//!   detection run unchanged.
//! - the microphone privacy latch: a file holding `0`/`1`, like the PMIC's
//!   `privacy_state` in sysfs, read by [`crate::privacy`].

use std::io;
use std::path::{Path, PathBuf};

use evdev::uinput::VirtualDevice;
use evdev::{AttributeSet, EventType, InputEvent, KeyCode};

use crate::buttons::{Button, KEY_HELP, KEY_POWER, KEY_VOLUMEDOWN, KEY_VOLUMEUP};

/// Name of the uinput device, as `/proc/bus/input/devices` shows it.
pub const BUTTONS_NAME: &str = "Pinna virtual buttons";

/// The key a button sends, as on the Dot (its microphone button sends
/// KEY_POWER); [`Button::from_code`] maps it back.
pub fn key_code(button: Button) -> u16 {
    match button {
        Button::VolumeUp => KEY_VOLUMEUP,
        Button::VolumeDown => KEY_VOLUMEDOWN,
        Button::Action => KEY_HELP,
        Button::Mute => KEY_POWER,
    }
}

/// The Dot's four buttons as a uinput keyboard.
pub struct VirtualButtons {
    dev: VirtualDevice,
}

impl VirtualButtons {
    /// Needs `/dev/uinput` (CONFIG_INPUT_UINPUT).
    pub fn create() -> io::Result<Self> {
        let mut keys = AttributeSet::<KeyCode>::new();
        for b in [
            Button::VolumeUp,
            Button::VolumeDown,
            Button::Action,
            Button::Mute,
        ] {
            keys.insert(KeyCode::new(key_code(b)));
        }
        let dev = VirtualDevice::builder()?
            .name(BUTTONS_NAME)
            .with_keys(&keys)?
            .build()?;
        Ok(Self { dev })
    }

    /// Press (`true`) or release a button.
    pub fn set(&mut self, button: Button, pressed: bool) -> io::Result<()> {
        self.dev.emit(&[InputEvent::new(
            EventType::KEY.0,
            key_code(button),
            i32::from(pressed),
        )])
    }
}

/// The microphone privacy latch as a file (`1` = microphones cut).
#[derive(Debug, Clone)]
pub struct PrivacyLatch {
    path: PathBuf,
}

impl PrivacyLatch {
    /// Create the file, open (`0`): the microphones start live, as the device does.
    pub fn create(path: impl Into<PathBuf>) -> io::Result<Self> {
        let latch = Self { path: path.into() };
        if let Some(dir) = latch.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        latch.set(false)?;
        Ok(latch)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn set(&self, muted: bool) -> io::Result<()> {
        // Write then rename: a poll never reads a half-written file.
        let tmp = self.path.with_extension("tmp");
        std::fs::write(&tmp, if muted { "1\n" } else { "0\n" })?;
        std::fs::rename(&tmp, &self.path)
    }

    /// `true` = muted. A missing or unreadable file reads as muted: the
    /// microphones stay cut when in doubt, as the device's latch does.
    pub fn get(&self) -> bool {
        crate::privacy::read_privacy_state_from(&[self.path.as_path()]).unwrap_or(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_map_back_to_buttons() {
        for b in [
            Button::VolumeUp,
            Button::VolumeDown,
            Button::Action,
            Button::Mute,
        ] {
            assert_eq!(Button::from_code(key_code(b)), Some(b));
        }
    }

    #[test]
    fn latch_round_trips_through_the_privacy_reader() {
        let dir = std::env::temp_dir().join(format!("pinna-virt-{}", std::process::id()));
        let latch = PrivacyLatch::create(dir.join("sub/privacy_state")).unwrap();
        assert!(!latch.get());
        latch.set(true).unwrap();
        assert!(latch.get());
        assert_eq!(
            crate::privacy::read_privacy_state_from(&[latch.path()]),
            Some(true)
        );
        latch.set(false).unwrap();
        assert!(!latch.get());
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(latch.get(), "missing file reads as muted");
    }
}
