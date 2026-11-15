//! Physical buttons via evdev, with software autorepeat and long-press
//! detection (a port of LibreEcho's buttond input handling).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

const INPUT_DIR: &str = "/dev/input";
pub const REPEAT_DELAY: Duration = Duration::from_millis(400);
pub const REPEAT_INTERVAL: Duration = Duration::from_millis(150);
pub const LONG_PRESS: Duration = Duration::from_secs(2);
pub const RESCAN_INTERVAL: Duration = Duration::from_secs(5);

// Linux input event codes (linux/input-event-codes.h).
pub const KEY_MUTE: u16 = 113;
pub const KEY_VOLUMEDOWN: u16 = 114;
pub const KEY_VOLUMEUP: u16 = 115;
pub const KEY_POWER: u16 = 116;
pub const KEY_HELP: u16 = 138;
pub const KEY_MICMUTE: u16 = 248;

const WATCHED_KEYS: [u16; 6] = [
    KEY_VOLUMEUP,
    KEY_VOLUMEDOWN,
    KEY_MUTE,
    KEY_MICMUTE,
    KEY_POWER,
    KEY_HELP,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Button {
    VolumeUp,
    VolumeDown,
    Action,
    Mute,
}

impl Button {
    /// Map a key code. On radar_puffin/biscuit KEY_POWER is the microphone
    /// mute button (measured in LibreEcho: ten mute presses gave ten
    /// KEY_POWER events; there is no separate power button), so it maps to
    /// `Mute`, not `Action`.
    pub fn from_code(code: u16) -> Option<Button> {
        match code {
            KEY_VOLUMEUP => Some(Button::VolumeUp),
            KEY_VOLUMEDOWN => Some(Button::VolumeDown),
            KEY_HELP => Some(Button::Action),
            KEY_MUTE | KEY_MICMUTE | KEY_POWER => Some(Button::Mute),
            _ => None,
        }
    }

    fn repeats(self) -> bool {
        matches!(self, Button::VolumeUp | Button::VolumeDown)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonEvent {
    Press(Button),
    Release(Button),
    /// Software autorepeat (volume keys only).
    Repeat(Button),
    /// Action held for at least [`LONG_PRESS`]; emitted once per hold.
    LongPress(Button),
}

struct Held {
    button: Button,
    next_repeat: Option<Instant>,
    long_at: Option<Instant>,
}

/// Pure press/repeat/long-press state machine. Feed it key transitions and
/// timer ticks with explicit timestamps.
#[derive(Default)]
pub struct KeyTracker {
    held: Vec<Held>,
}

impl KeyTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// `value`: 0 release, 1 press, 2 kernel autorepeat. Kernel repeats are
    /// ignored for held keys (the software repeat is authoritative, so they
    /// are never double counted); for an unknown key they count as a press.
    pub fn key(&mut self, button: Button, value: i32, now: Instant) -> Option<ButtonEvent> {
        let idx = self.held.iter().position(|h| h.button == button);
        match (value, idx) {
            (0, Some(i)) => {
                self.held.remove(i);
                Some(ButtonEvent::Release(button))
            }
            (0, None) => None,
            (_, Some(_)) => None,
            (_, None) => {
                self.held.push(Held {
                    button,
                    next_repeat: button.repeats().then(|| now + REPEAT_DELAY),
                    long_at: (button == Button::Action).then(|| now + LONG_PRESS),
                });
                Some(ButtonEvent::Press(button))
            }
        }
    }

    /// Earliest instant at which `tick` has something to emit.
    pub fn deadline(&self) -> Option<Instant> {
        self.held
            .iter()
            .flat_map(|h| [h.next_repeat, h.long_at])
            .flatten()
            .min()
    }

    pub fn tick(&mut self, now: Instant) -> Vec<ButtonEvent> {
        let mut out = Vec::new();
        for h in &mut self.held {
            if let Some(at) = h.next_repeat
                && now >= at
            {
                out.push(ButtonEvent::Repeat(h.button));
                h.next_repeat = Some(now + REPEAT_INTERVAL);
            }
            if let Some(at) = h.long_at
                && now >= at
            {
                out.push(ButtonEvent::LongPress(h.button));
                h.long_at = None;
            }
        }
        out
    }
}

/// Does a device with these supported key codes look like a button device?
pub fn is_button_device(has_key: impl Fn(u16) -> bool) -> bool {
    WATCHED_KEYS.iter().any(|&k| has_key(k))
}

/// Start watching `/dev/input/event*` (rescanned every 5 s for hot-plug).
/// Must be called inside a tokio runtime.
pub fn button_stream() -> mpsc::Receiver<ButtonEvent> {
    button_stream_in(PathBuf::from(INPUT_DIR))
}

pub fn button_stream_in(dir: PathBuf) -> mpsc::Receiver<ButtonEvent> {
    let (tx, rx) = mpsc::channel(64);
    tokio::spawn(run_buttons(dir, tx));
    rx
}

struct RawKey {
    button: Button,
    value: i32,
}

async fn run_buttons(dir: PathBuf, tx: mpsc::Sender<ButtonEvent>) {
    let (raw_tx, mut raw_rx) = mpsc::channel::<RawKey>(64);
    let watched: Arc<Mutex<HashSet<PathBuf>>> = Arc::default();
    let mut tracker = KeyTracker::new();
    let mut rescan = tokio::time::interval(RESCAN_INTERVAL);
    loop {
        let deadline = tracker.deadline();
        let sleep = async {
            match deadline {
                Some(d) => tokio::time::sleep_until(d.into()).await,
                None => std::future::pending().await,
            }
        };
        let mut events = Vec::new();
        tokio::select! {
            _ = rescan.tick() => scan_devices(&dir, &watched, &raw_tx),
            Some(k) = raw_rx.recv() => {
                events.extend(tracker.key(k.button, k.value, Instant::now()));
            }
            _ = sleep => events = tracker.tick(Instant::now()),
        }
        for e in events {
            if tx.send(e).await.is_err() {
                return;
            }
        }
    }
}

fn scan_devices(dir: &Path, watched: &Arc<Mutex<HashSet<PathBuf>>>, raw_tx: &mpsc::Sender<RawKey>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        tracing::debug!("cannot read {}", dir.display());
        return;
    };
    for entry in rd.flatten() {
        let path = entry.path();
        let is_event = path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("event"));
        if !is_event || watched.lock().unwrap().contains(&path) {
            continue;
        }
        let Ok(dev) = evdev::Device::open(&path) else {
            continue;
        };
        let interesting = dev
            .supported_keys()
            .is_some_and(|keys| is_button_device(|c| keys.contains(evdev::KeyCode::new(c))));
        if !interesting {
            continue;
        }
        let Ok(mut stream) = dev.into_event_stream() else {
            continue;
        };
        tracing::info!(path = %path.display(), "watching button device");
        watched.lock().unwrap().insert(path.clone());
        let raw_tx = raw_tx.clone();
        let watched = watched.clone();
        tokio::spawn(async move {
            loop {
                match stream.next_event().await {
                    Ok(ev) => {
                        if ev.event_type() != evdev::EventType::KEY {
                            continue;
                        }
                        if let Some(button) = Button::from_code(ev.code()) {
                            let key = RawKey {
                                button,
                                value: ev.value(),
                            };
                            if raw_tx.send(key).await.is_err() {
                                break;
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!(path = %path.display(), "button device gone: {e}");
                        break;
                    }
                }
            }
            watched.lock().unwrap().remove(&path);
        });
    }
}

// ---------------------------------------------------------------------------
// Mock

/// Parse a mock command line: `up`, `down`, `action`, `mute`.
pub fn parse_mock_line(line: &str) -> Option<Button> {
    match line.trim().to_ascii_lowercase().as_str() {
        "up" => Some(Button::VolumeUp),
        "down" => Some(Button::VolumeDown),
        "action" => Some(Button::Action),
        "mute" => Some(Button::Mute),
        _ => None,
    }
}

/// Mock buttons: each stdin line (`up`, `down`, `action`, `mute`) yields a
/// Press immediately followed by a Release.
pub struct MockButtons;

impl MockButtons {
    pub fn from_stdin() -> mpsc::Receiver<ButtonEvent> {
        use tokio::io::{AsyncBufReadExt, BufReader};
        let (tx, rx) = mpsc::channel(16);
        tokio::spawn(async move {
            let mut lines = BufReader::new(tokio::io::stdin()).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let Some(b) = parse_mock_line(&line) else {
                    continue;
                };
                for e in [ButtonEvent::Press(b), ButtonEvent::Release(b)] {
                    if tx.send(e).await.is_err() {
                        return;
                    }
                }
            }
        });
        rx
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn code_mapping() {
        assert_eq!(Button::from_code(115), Some(Button::VolumeUp));
        assert_eq!(Button::from_code(114), Some(Button::VolumeDown));
        assert_eq!(Button::from_code(138), Some(Button::Action));
        assert_eq!(Button::from_code(113), Some(Button::Mute));
        assert_eq!(Button::from_code(116), Some(Button::Mute));
        assert_eq!(Button::from_code(248), Some(Button::Mute));
        assert_eq!(Button::from_code(30), None);
    }

    #[test]
    fn device_filter() {
        assert!(is_button_device(|c| c == KEY_HELP));
        assert!(is_button_device(|c| c == KEY_POWER));
        assert!(!is_button_device(|c| c == 30));
    }

    #[test]
    fn volume_repeat_after_400_then_every_150() {
        let t0 = Instant::now();
        let mut k = KeyTracker::new();
        assert_eq!(
            k.key(Button::VolumeUp, 1, t0),
            Some(ButtonEvent::Press(Button::VolumeUp))
        );
        assert_eq!(k.deadline(), Some(t0 + ms(400)));
        assert!(k.tick(t0 + ms(399)).is_empty());
        assert_eq!(
            k.tick(t0 + ms(400)),
            vec![ButtonEvent::Repeat(Button::VolumeUp)]
        );
        assert_eq!(k.deadline(), Some(t0 + ms(550)));
        assert_eq!(
            k.tick(t0 + ms(550)),
            vec![ButtonEvent::Repeat(Button::VolumeUp)]
        );
        assert_eq!(
            k.tick(t0 + ms(700)),
            vec![ButtonEvent::Repeat(Button::VolumeUp)]
        );
        assert_eq!(
            k.key(Button::VolumeUp, 0, t0 + ms(710)),
            Some(ButtonEvent::Release(Button::VolumeUp))
        );
        assert_eq!(k.deadline(), None);
        assert!(k.tick(t0 + ms(2000)).is_empty());
    }

    #[test]
    fn kernel_repeats_are_not_double_counted() {
        let t0 = Instant::now();
        let mut k = KeyTracker::new();
        k.key(Button::VolumeDown, 1, t0);
        assert_eq!(k.key(Button::VolumeDown, 2, t0 + ms(250)), None);
        assert_eq!(k.key(Button::VolumeDown, 2, t0 + ms(300)), None);
        assert_eq!(k.deadline(), Some(t0 + ms(400)));
        // A repeat for a key we never saw pressed counts as a press.
        assert_eq!(
            k.key(Button::VolumeUp, 2, t0),
            Some(ButtonEvent::Press(Button::VolumeUp))
        );
    }

    #[test]
    fn duplicate_press_and_stray_release_ignored() {
        let t0 = Instant::now();
        let mut k = KeyTracker::new();
        assert_eq!(k.key(Button::Mute, 0, t0), None);
        assert!(k.key(Button::Mute, 1, t0).is_some());
        assert_eq!(k.key(Button::Mute, 1, t0), None);
    }

    #[test]
    fn mute_and_action_do_not_repeat() {
        let t0 = Instant::now();
        let mut k = KeyTracker::new();
        k.key(Button::Mute, 1, t0);
        assert_eq!(k.deadline(), None);
        k.key(Button::Mute, 0, t0);
        k.key(Button::Action, 1, t0);
        assert_eq!(k.deadline(), Some(t0 + LONG_PRESS));
        assert!(k.tick(t0 + ms(1500)).is_empty());
    }

    #[test]
    fn action_long_press_once() {
        let t0 = Instant::now();
        let mut k = KeyTracker::new();
        k.key(Button::Action, 1, t0);
        assert_eq!(
            k.tick(t0 + ms(2000)),
            vec![ButtonEvent::LongPress(Button::Action)]
        );
        assert!(k.tick(t0 + ms(4000)).is_empty());
        assert_eq!(k.deadline(), None);
        k.key(Button::Action, 0, t0 + ms(4100));
        // A fresh hold arms again.
        k.key(Button::Action, 1, t0 + ms(5000));
        assert_eq!(k.deadline(), Some(t0 + ms(7000)));
    }

    #[test]
    fn short_action_press_has_no_long_press() {
        let t0 = Instant::now();
        let mut k = KeyTracker::new();
        k.key(Button::Action, 1, t0);
        k.key(Button::Action, 0, t0 + ms(300));
        assert!(k.tick(t0 + ms(3000)).is_empty());
    }

    #[test]
    fn mock_lines() {
        assert_eq!(parse_mock_line(" UP\n"), Some(Button::VolumeUp));
        assert_eq!(parse_mock_line("mute"), Some(Button::Mute));
        assert_eq!(parse_mock_line("bogus"), None);
    }
}
