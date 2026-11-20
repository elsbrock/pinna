//! What the Sendspin player keeps across restarts (ADR 0027 §7): its X25519 identity, its
//! pairing key and pairings, the last server that played, and the stream's volume and mute.
//! They live in `sendspin.toml` beside the settings, mode 0600, written atomically. The file is
//! created the first time Sendspin is switched on; a factory reset (ADR 0010) removes it with
//! the rest of `/var/lib/pinna`, and the next switch-on makes a new identity.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use anyhow::{Context, Result};
use pinna_sendspin::{Identity, PairingStore};
use serde::{Deserialize, Serialize};

/// Only pinnad reads it: it holds private keys.
const MODE: u32 = 0o600;

/// The stream's level until a server or the owner sets one: the satellite's own volume
/// decides how loud it is.
pub const VOLUME_DEFAULT: u8 = 100;

/// The player's keys and pairings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Keys {
    pub identity: Identity,
    pub pairing: PairingStore,
}

impl Keys {
    /// A new identity and pairing key.
    pub fn generate() -> Result<Self> {
        Ok(Self {
            identity: Identity::generate().context("making the player's identity")?,
            pairing: PairingStore::generate()
                .map_err(|_| anyhow::anyhow!("making the player's pairing key"))?,
        })
    }
}

/// Everything in the file. The plain values come first: TOML can't put them after a table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Saved {
    #[serde(default = "volume_default")]
    pub volume: u8,
    #[serde(default)]
    pub muted: bool,
    #[serde(flatten)]
    pub keys: Keys,
}

fn volume_default() -> u8 {
    VOLUME_DEFAULT
}

/// Reads the file. `None` when there is none yet, or when it was unreadable: a damaged file is
/// moved aside to `sendspin.toml.bad`, so that whoever looks into it later finds it, and the
/// player starts afresh. Blocking.
pub fn load(path: &Path) -> Option<Saved> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => {
            tracing::warn!("sendspin: reading {}: {e}; starting afresh", path.display());
            set_aside(path);
            return None;
        }
    };
    match toml::from_str::<Saved>(&text) {
        Ok(mut saved) => {
            saved.volume = saved.volume.min(100);
            Some(saved)
        }
        Err(e) => {
            tracing::warn!(
                "sendspin: {} is damaged ({}); starting afresh with a new identity",
                path.display(),
                e.message()
            );
            set_aside(path);
            None
        }
    }
}

fn bad_path(path: &Path) -> PathBuf {
    let mut bad = path.as_os_str().to_owned();
    bad.push(".bad");
    PathBuf::from(bad)
}

fn set_aside(path: &Path) {
    let bad = bad_path(path);
    if let Err(e) = std::fs::rename(path, &bad) {
        tracing::warn!("sendspin: moving {} aside: {e}", path.display());
    }
}

/// Writes the file. Blocking.
pub fn save(path: &Path, saved: &Saved) -> Result<()> {
    let text = toml::to_string(saved).context("encoding the Sendspin state")?;
    crate::store::write_durable(path, text.as_bytes(), MODE)
        .with_context(|| format!("writing {}", path.display()))
}

/// The least time between two writes from [`Saver::save`]. Any host on the network can send
/// volume and mute commands as fast as it likes (ADR 0027 §4), and each one changes the state:
/// the gap bounds the wear that puts on the flash.
const GAP: Duration = Duration::from_secs(2);

/// Writes the file from the blocking pool, one write at a time, each the latest state handed
/// over: a volume slider dragged in Music Assistant sends many changes, and only the last one
/// needs to reach the flash. One task writes while changes keep coming, at most once per
/// [`GAP`]; a change made meanwhile waits for it rather than starting another.
#[derive(Clone)]
pub struct Saver {
    path: PathBuf,
    pending: Arc<Mutex<Pending>>,
    one_at_a_time: Arc<Mutex<()>>,
    #[cfg(test)]
    writes: Arc<std::sync::atomic::AtomicUsize>,
}

#[derive(Default)]
struct Pending {
    latest: Option<Saved>,
    /// The writer task runs: it takes `latest` when its turn comes.
    writer: bool,
}

impl Saver {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            pending: Arc::default(),
            one_at_a_time: Arc::default(),
            #[cfg(test)]
            writes: Arc::default(),
        }
    }

    #[cfg(test)]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// How many times the file was written.
    #[cfg(test)]
    fn writes(&self) -> usize {
        self.writes.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn pending(&self) -> std::sync::MutexGuard<'_, Pending> {
        self.pending.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Saves `saved` later: at once if nothing was written in the last [`GAP`], otherwise
    /// when it has passed. Returns at once.
    pub fn save(&self, saved: Saved) {
        let mut pending = self.pending();
        pending.latest = Some(saved);
        if std::mem::replace(&mut pending.writer, true) {
            return;
        }
        drop(pending);
        tokio::spawn(self.clone().write_while_pending());
    }

    /// The writer task: writes the latest state and waits out the gap, until no change came
    /// in meanwhile.
    async fn write_while_pending(self) {
        loop {
            {
                let mut pending = self.pending();
                if pending.latest.is_none() {
                    pending.writer = false;
                    return;
                }
            }
            let saver = self.clone();
            match tokio::task::spawn_blocking(move || saver.write_latest()).await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => tracing::warn!("sendspin: {e:#}"),
                Err(e) => tracing::warn!("sendspin: saving the state: {e}"),
            }
            tokio::time::sleep(GAP).await;
        }
    }

    /// Saves `saved` and waits until it is on the flash.
    pub async fn save_now(&self, saved: Saved) -> Result<()> {
        self.pending().latest = Some(saved);
        let saver = self.clone();
        tokio::task::spawn_blocking(move || saver.write_latest())
            .await
            .context("saving the Sendspin state")?
    }

    fn write_latest(&self) -> Result<()> {
        let _turn = self
            .one_at_a_time
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let latest = self.pending().latest.take();
        match latest {
            Some(saved) => {
                #[cfg(test)]
                self.writes
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                save(&self.path, &saved)
            }
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pinna_sendspin::PairingRecord;
    use pinna_sendspin::psk::Psk;
    use std::os::unix::fs::PermissionsExt;

    fn saved() -> Saved {
        let mut keys = Keys::generate().unwrap();
        keys.pairing.insert(
            PairingRecord {
                server_id: "server-a".into(),
                long_term_psk: Psk::random().unwrap(),
            },
            &[],
        );
        keys.pairing.set_last_playback_server("server-a");
        Saved {
            volume: 35,
            muted: true,
            keys,
        }
    }

    #[test]
    fn round_trips_with_mode_0600() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sendspin.toml");
        assert_eq!(load(&path), None);
        let s = saved();
        save(&path, &s).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        assert_eq!(load(&path), Some(s.clone()));
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("volume = 35\nmuted = true\n"), "{text}");
        assert!(text.contains("[identity]"), "{text}");
    }

    #[test]
    fn a_damaged_file_is_set_aside() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sendspin.toml");
        std::fs::write(&path, "volume = 20\n[identity]\nprivate = \"short\"\n").unwrap();
        assert_eq!(load(&path), None);
        assert!(!path.exists());
        let bad = std::fs::read_to_string(dir.path().join("sendspin.toml.bad")).unwrap();
        assert!(bad.contains("short"));
    }

    #[test]
    fn levels_out_of_range_or_missing_are_fixed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sendspin.toml");
        let s = saved();
        let mut text = toml::to_string(&s).unwrap();
        text = text.replace("volume = 35\nmuted = true\n", "volume = 250\n");
        std::fs::write(&path, text).unwrap();
        let loaded = load(&path).unwrap();
        assert_eq!((loaded.volume, loaded.muted), (100, false));
        assert_eq!(loaded.keys, s.keys);
    }

    #[tokio::test]
    async fn the_saver_writes_the_latest_state() {
        let dir = tempfile::tempdir().unwrap();
        let saver = Saver::new(dir.path().join("sendspin.toml"));
        let mut s = saved();
        for volume in 0..20 {
            s.volume = volume;
            saver.save(s.clone());
        }
        s.volume = 77;
        saver.save_now(s.clone()).await.unwrap();
        assert_eq!(load(saver.path()).unwrap().volume, 77);
    }

    /// A server that sends volume commands as fast as it likes starts one write, not one per
    /// command, and the file is written at most once per gap: the first change at once, the
    /// last one when the gap has passed.
    #[tokio::test]
    async fn a_stream_of_changes_writes_once_per_gap() {
        let dir = tempfile::tempdir().unwrap();
        let saver = Saver::new(dir.path().join("sendspin.toml"));
        let mut s = saved();
        let start = tokio::time::Instant::now();
        for volume in 0..50 {
            s.volume = volume;
            saver.save(s.clone());
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(start.elapsed() < GAP, "the test ran too slowly to tell");
        assert_eq!(saver.writes(), 1);

        let deadline = start + GAP + Duration::from_secs(3);
        while load(saver.path()).is_none_or(|l| l.volume != 49) {
            assert!(tokio::time::Instant::now() < deadline, "the last change");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(start.elapsed() >= GAP);
        assert_eq!(saver.writes(), 2);
    }
}
