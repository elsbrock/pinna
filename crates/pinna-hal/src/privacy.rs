//! Hardware mic privacy switch. The PMIC mute key cuts the mics in hardware;
//! the kernel reports the latch in `privacy_state` ("0" or "1").

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::sync::{mpsc, watch};

pub const PRIVACY_STATE_PATH: &str = "/sys/devices/platform/amz_privacy/privacy_state";
pub const PRIVACY_STATE_FALLBACK_PATH: &str = "/sys/devices/platform/amz-privacy/privacy_state";
pub const PRIVACY_TRIGGER_PATH: &str = "/sys/devices/platform/amz_privacy/privacy_trigger";
pub const PRIVACY_TRIGGER_FALLBACK_PATH: &str = "/sys/devices/platform/amz-privacy/privacy_trigger";
pub const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// First `0`/`1` character of the file, as the previous firmware parses it.
pub fn parse_state(text: &str) -> Option<bool> {
    match text.trim_start().chars().next()? {
        '0' => Some(false),
        '1' => Some(true),
        _ => None,
    }
}

/// Read the latch: `Some(true)` = mics muted. `None` if no path is readable.
pub fn read_privacy_state() -> Option<bool> {
    read_privacy_state_from(&[
        PRIVACY_STATE_PATH.as_ref(),
        PRIVACY_STATE_FALLBACK_PATH.as_ref(),
    ])
}

pub fn read_privacy_state_from(paths: &[&Path]) -> Option<bool> {
    paths.iter().find_map(|p| {
        std::fs::read_to_string(p)
            .ok()
            .and_then(|s| parse_state(&s))
    })
}

/// Why the latch could not be closed from software.
#[derive(Debug, thiserror::Error)]
pub enum PrivacyError {
    #[error("no privacy trigger to write")]
    NoTrigger,
    #[error("write {}: {source}", path.display())]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
}

/// Close the latch from software: write `1` to the first `privacy_trigger`
/// that takes it. The driver (`0012-misc-Amazon-privacy-mute-control.patch`,
/// `privacy_trigger_store`) enters privacy on a `1` and has no way to leave
/// it from userspace; only the physical key does. The change reaches
/// readers as the next `privacy_state`.
pub fn enter_privacy_to(paths: &[&Path]) -> Result<(), PrivacyError> {
    let mut last = PrivacyError::NoTrigger;
    for path in paths {
        match std::fs::write(path, "1") {
            Ok(()) => return Ok(()),
            Err(source) => {
                last = PrivacyError::Write {
                    path: path.to_path_buf(),
                    source,
                };
            }
        }
    }
    Err(last)
}

/// Emits the current state once the hardware is readable, then every change
/// (`true` = muted). Polls every 100 ms.
pub fn privacy_stream() -> mpsc::Receiver<bool> {
    privacy_stream_from(vec![
        PRIVACY_STATE_PATH.into(),
        PRIVACY_STATE_FALLBACK_PATH.into(),
    ])
}

pub fn privacy_stream_from(paths: Vec<PathBuf>) -> mpsc::Receiver<bool> {
    let (tx, rx) = mpsc::channel(8);
    tokio::spawn(async move {
        let refs: Vec<&Path> = paths.iter().map(|p| p.as_path()).collect();
        let mut last = None;
        let mut tick = tokio::time::interval(POLL_INTERVAL);
        loop {
            tick.tick().await;
            let Some(state) = read_privacy_state_from(&refs) else {
                continue;
            };
            if last != Some(state) {
                last = Some(state);
                if tx.send(state).await.is_err() {
                    return;
                }
            }
        }
    });
    rx
}

/// Mock privacy switch toggled by the mock `mute` button.
#[derive(Clone)]
pub struct MockPrivacy {
    muted: Arc<AtomicBool>,
    tx: Arc<watch::Sender<bool>>,
}

impl Default for MockPrivacy {
    fn default() -> Self {
        let (tx, _) = watch::channel(false);
        MockPrivacy {
            muted: Arc::default(),
            tx: Arc::new(tx),
        }
    }
}

impl MockPrivacy {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_muted(&self) -> bool {
        self.muted.load(Ordering::SeqCst)
    }

    /// Flip the state and return the new one.
    pub fn toggle(&self) -> bool {
        let new = !self.muted.fetch_xor(true, Ordering::SeqCst);
        self.tx.send_replace(new);
        new
    }

    /// Current state, then changes.
    pub fn subscribe(&self) -> watch::Receiver<bool> {
        self.tx.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse() {
        assert_eq!(parse_state("1\n"), Some(true));
        assert_eq!(parse_state("  0"), Some(false));
        assert_eq!(parse_state("muted"), None);
        assert_eq!(parse_state(""), None);
    }

    #[test]
    fn fallback_path_used() {
        let dir = std::env::temp_dir().join(format!("pinna-priv-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let b = dir.join("b");
        std::fs::write(&b, "1\n").unwrap();
        let missing = dir.join("a");
        assert_eq!(read_privacy_state_from(&[&missing, &b]), Some(true));
        assert_eq!(read_privacy_state_from(&[&missing]), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn entering_privacy_writes_a_one_to_the_first_trigger_that_takes_it() {
        let dir = std::env::temp_dir().join(format!("pinna-priv3-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let missing = dir.join("a/privacy_trigger");
        let trigger = dir.join("privacy_trigger");
        std::fs::write(&trigger, "").unwrap();

        enter_privacy_to(&[&missing, &trigger]).unwrap();

        assert_eq!(std::fs::read_to_string(&trigger).unwrap(), "1");
        assert!(matches!(
            enter_privacy_to(&[&missing]),
            Err(PrivacyError::Write { .. })
        ));
        assert!(matches!(
            enter_privacy_to(&[]),
            Err(PrivacyError::NoTrigger)
        ));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn stream_emits_initial_and_changes() {
        let dir = std::env::temp_dir().join(format!("pinna-priv2-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("privacy_state");
        std::fs::write(&f, "0\n").unwrap();
        let mut rx = privacy_stream_from(vec![f.clone()]);
        assert_eq!(rx.recv().await, Some(false));
        std::fs::write(&f, "1\n").unwrap();
        assert_eq!(rx.recv().await, Some(true));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn mock_toggles() {
        let m = MockPrivacy::new();
        let rx = m.subscribe();
        assert!(!m.is_muted());
        assert!(m.toggle());
        assert!(m.is_muted());
        assert!(*rx.borrow());
        assert!(!m.toggle());
    }
}
