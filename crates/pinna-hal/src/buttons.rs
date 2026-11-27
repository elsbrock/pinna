//! Physical buttons via evdev, with software autorepeat and long-press
//! detection (a port of LibreEcho's buttond input handling).

use std::collections::HashSet;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

const INPUT_DIR: &str = "/dev/input";
pub const REPEAT_DELAY: Duration = Duration::from_millis(400);
pub const REPEAT_INTERVAL: Duration = Duration::from_millis(150);
pub const LONG_PRESS: Duration = Duration::from_secs(2);
pub const RESCAN_INTERVAL: Duration = Duration::from_secs(5);

pub use crate::board::{
    Button, KEY_HELP, KEY_MICMUTE, KEY_MUTE, KEY_POWER, KEY_VOLUMEDOWN, KEY_VOLUMEUP,
};

/// Volume keys repeat while held; the others don't.
fn repeats(button: Button) -> bool {
    matches!(button, Button::VolumeUp | Button::VolumeDown)
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
                    next_repeat: repeats(button).then(|| now + REPEAT_DELAY),
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

/// Does a device with these supported key codes send any of the board's
/// keys?
pub fn is_button_device(keys: &[(u16, Button)], has_key: impl Fn(u16) -> bool) -> bool {
    keys.iter().any(|&(k, _)| has_key(k))
}

/// Start watching `/dev/input/event*` for the board's `keys` (rescanned
/// every 5 s for hot-plug). Must be called inside a tokio runtime.
pub fn button_stream(keys: &'static [(u16, Button)]) -> mpsc::Receiver<ButtonEvent> {
    button_stream_in(PathBuf::from(INPUT_DIR), keys)
}

pub fn button_stream_in(
    dir: PathBuf,
    keys: &'static [(u16, Button)],
) -> mpsc::Receiver<ButtonEvent> {
    let (tx, rx) = mpsc::channel(64);
    tokio::spawn(run_buttons(dir, keys, tx));
    rx
}

struct RawKey {
    button: Button,
    value: i32,
}

/// An input node, by path and inode: a device that takes the `eventN` of
/// one that left gets a new node.
type Node = (PathBuf, u64);

async fn run_buttons(dir: PathBuf, keys: &'static [(u16, Button)], tx: mpsc::Sender<ButtonEvent>) {
    let (raw_tx, mut raw_rx) = mpsc::channel::<RawKey>(64);
    let watched: Arc<Mutex<HashSet<PathBuf>>> = Arc::default();
    let skipped: Arc<Mutex<HashSet<Node>>> = Arc::default();
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
            _ = rescan.tick() => {
                // Opening and closing input nodes can block for tens of
                // milliseconds (`to_open`). On a runtime worker that holds
                // up pinnad's audio tasks.
                let (dir, watched, skipped, raw_tx) =
                    (dir.clone(), watched.clone(), skipped.clone(), raw_tx.clone());
                let scan = tokio::task::spawn_blocking(move || {
                    scan_devices(&dir, keys, &watched, &skipped, &raw_tx)
                });
                if let Err(e) = scan.await {
                    tracing::warn!("looking for button devices: {e}");
                }
            }
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

fn scan_devices(
    dir: &Path,
    keys: &'static [(u16, Button)],
    watched: &Arc<Mutex<HashSet<PathBuf>>>,
    skipped: &Mutex<HashSet<Node>>,
    raw_tx: &mpsc::Sender<RawKey>,
) {
    let nodes = to_open(dir, &watched.lock().unwrap(), &mut skipped.lock().unwrap());
    for (path, ino) in nodes {
        let Ok(dev) = evdev::Device::open(&path) else {
            continue;
        };
        let interesting = dev.supported_keys().is_some_and(|supported| {
            is_button_device(keys, |c| supported.contains(evdev::KeyCode::new(c)))
        });
        if !interesting {
            skipped.lock().unwrap().insert((path, ino));
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
                        if let Some(button) = crate::board::button_in(keys, ev.code()) {
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

/// The `event*` nodes in `dir` to open: not watched, and not already found
/// to send none of the board's keys. Skipped nodes that are gone are
/// forgotten.
///
/// Closing an evdev node waits for an RCU grace period (`evdev_release`).
/// On the Echo Dot (5th gen), `gpio-privacy-state` has no keys; reopening
/// and closing it on every rescan blocked for 64 ms or more each time, and
/// the speaker ran dry every 5 s (measured 2026-11-27, 0.7.0-beta.1).
fn to_open(dir: &Path, watched: &HashSet<PathBuf>, skipped: &mut HashSet<Node>) -> Vec<Node> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        tracing::debug!("cannot read {}", dir.display());
        return Vec::new();
    };
    let mut present = HashSet::new();
    let mut open = Vec::new();
    for entry in rd.flatten() {
        let path = entry.path();
        let is_event = path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("event"));
        if !is_event || watched.contains(&path) {
            continue;
        }
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        let node = (path, meta.ino());
        if !skipped.contains(&node) {
            open.push(node.clone());
        }
        present.insert(node);
    }
    skipped.retain(|n| present.contains(n));
    open
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
    fn a_node_without_keys_is_opened_once() {
        let dir = std::env::temp_dir().join(format!("pinna-buttons-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (event0, event1) = (dir.join("event0"), dir.join("event1"));
        for f in [&event0, &event1, &dir.join("mice")] {
            std::fs::write(f, b"").unwrap();
        }
        let watched = HashSet::from([event1.clone()]);
        let mut skipped = HashSet::new();

        let first = to_open(&dir, &watched, &mut skipped);
        assert_eq!(
            first.iter().map(|n| &n.0).collect::<Vec<_>>(),
            [&event0],
            "only event nodes nobody watches"
        );
        // scan_devices found none of the board's keys on it.
        skipped.extend(first);
        assert!(to_open(&dir, &watched, &mut skipped).is_empty());

        // Another device takes event0: a new node, checked again.
        std::fs::write(dir.join("new"), b"").unwrap();
        std::fs::rename(dir.join("new"), &event0).unwrap();
        let again = to_open(&dir, &watched, &mut skipped);
        assert_eq!(again.iter().map(|n| &n.0).collect::<Vec<_>>(), [&event0]);
        assert!(skipped.is_empty(), "the old node is forgotten");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn device_filter() {
        let keys = crate::board::MT8163_KEYS;
        assert!(is_button_device(keys, |c| c == KEY_HELP));
        assert!(is_button_device(keys, |c| c == KEY_POWER));
        assert!(!is_button_device(keys, |c| c == 30));
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
