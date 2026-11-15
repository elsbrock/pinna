//! Virtual Echo Dot hardware for the development VM (ADR 0014). These go
//! through the same paths pinnad reads on the device:
//!
//! - buttons: a uinput device with the Dot's keys. [`crate::buttons`] finds it
//!   in `/dev/input` like the device's gpio-keys, so autorepeat and long-press
//!   detection run unchanged.
//! - the microphone privacy latch: a file holding `0`/`1`, like the PMIC's
//!   `privacy_state` in sysfs, read by [`crate::privacy`];
//! - the LED ring: frames published on a channel instead of written to the
//!   IS31FL3236, so the dashboard draws what pinnad drew.

use std::io;
use std::path::{Path, PathBuf};

use evdev::uinput::VirtualDevice;
use evdev::{AttributeSet, EventType, InputEvent, KeyCode};
use tokio::sync::watch;

use crate::buttons::{Button, KEY_HELP, KEY_POWER, KEY_VOLUMEDOWN, KEY_VOLUMEUP};
use crate::led::{Frame, LedRing, RING_PIXELS, Rgb};

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

/// The LED ring as a channel of frames; subscribers see the latest one.
pub struct VirtualLeds {
    tx: watch::Sender<Frame>,
}

impl VirtualLeds {
    pub fn new(tx: watch::Sender<Frame>) -> Self {
        Self { tx }
    }
}

impl LedRing for VirtualLeds {
    fn set(&mut self, frame: &Frame) -> crate::Result<()> {
        self.tx.send_if_modified(|f| {
            let changed = f != frame;
            *f = *frame;
            changed
        });
        Ok(())
    }
}

/// A frame as 72 hex digits, `rrggbb` per pixel in ring order.
pub fn frame_hex(frame: &Frame) -> String {
    frame
        .iter()
        .map(|p| format!("{:02x}{:02x}{:02x}", p.r, p.g, p.b))
        .collect()
}

/// An unlit ring.
pub const DARK: Frame = [Rgb::OFF; RING_PIXELS];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leds_publish_changed_frames() {
        let (tx, mut rx) = watch::channel(DARK);
        let mut leds = VirtualLeds::new(tx);
        let mut frame = DARK;
        frame[0] = Rgb::new(0, 0x80, 0xff);
        leds.set(&frame).unwrap();
        assert!(rx.has_changed().unwrap());
        assert_eq!(*rx.borrow_and_update(), frame);
        leds.set(&frame).unwrap();
        assert!(!rx.has_changed().unwrap(), "same frame: no change");
        assert_eq!(&frame_hex(&frame)[..12], "0080ff000000");
        assert_eq!(frame_hex(&frame).len(), 72);
    }

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
