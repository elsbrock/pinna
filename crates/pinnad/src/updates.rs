//! A/B updates for the dashboard: the slot state, and `pinnactl update` run by
//! pinnad itself, so an install goes on when the browser goes away.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use buffa::MessageField;
use connectrpc::ConnectError;
use pinna_api::{GetUpdatesResponse, InstallResult, Slot as ApiSlot};
use pinna_bootctl::slot::{self, Slot, SlotState};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::broadcast;

/// Lines of install output kept for a late WatchInstall.
const HISTORY: usize = 1000;
const REBOOT_TIMEOUT: Duration = Duration::from_secs(5);

/// Where the updater looks; tests point it at a temp dir.
#[derive(Debug, Clone)]
pub struct UpdaterPaths {
    pub misc: PathBuf,
    pub partdir: PathBuf,
    pub run_dir: PathBuf,
    pub pinnactl: PathBuf,
    pub reboot: Vec<String>,
}

impl Default for UpdaterPaths {
    fn default() -> Self {
        Self {
            misc: "/dev/disk/by-partlabel/misc".into(),
            partdir: "/dev/disk/by-partlabel".into(),
            run_dir: slot::RUN_DIR.into(),
            pinnactl: "pinnactl".into(),
            reboot: vec!["systemctl".into(), "reboot".into()],
        }
    }
}

#[derive(Debug, Clone)]
pub enum Event {
    Line(String),
    Done { ok: bool },
}

/// What a WatchInstall starts from: lines so far, then what follows.
pub struct Progress {
    pub history: Vec<String>,
    pub live: Option<broadcast::Receiver<Event>>,
    /// The last result, when nothing is running.
    pub finished: Option<bool>,
}

struct State {
    running: bool,
    history: VecDeque<String>,
    last: Option<InstallResult>,
    events: broadcast::Sender<Event>,
}

pub struct Updater {
    paths: UpdaterPaths,
    state: Arc<Mutex<State>>,
}

fn lock(state: &Mutex<State>) -> MutexGuard<'_, State> {
    state.lock().unwrap_or_else(|e| e.into_inner())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

impl Updater {
    pub fn new(paths: UpdaterPaths) -> Self {
        Self {
            paths,
            state: Arc::new(Mutex::new(State {
                running: false,
                history: VecDeque::new(),
                last: None,
                events: broadcast::channel(256).0,
            })),
        }
    }

    /// The slots and the running version; `suggested_manifest` is the caller's.
    pub async fn get_updates(&self) -> GetUpdatesResponse {
        let paths = self.paths.clone();
        let mut resp = tokio::task::spawn_blocking(move || read_slots(&paths))
            .await
            .unwrap_or_else(|e| unavailable(format!("reading the slots failed: {e}")));
        let state = lock(&self.state);
        resp.installing = state.running;
        resp.last_install = state
            .last
            .clone()
            .map_or_else(MessageField::none, MessageField::some);
        resp
    }

    /// Start `pinnactl update -- <manifest>`; it runs to the end whoever watches.
    pub fn install(&self, manifest: &str) -> Result<(), ConnectError> {
        let mut state = lock(&self.state);
        if state.running {
            return Err(ConnectError::failed_precondition(
                "an update is already being installed",
            ));
        }
        let mut child = Command::new(&self.paths.pinnactl)
            .args(["update", "--", manifest])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| {
                ConnectError::unavailable(format!(
                    "starting {}: {e}",
                    self.paths.pinnactl.display()
                ))
            })?;
        state.running = true;
        state.history.clear();
        drop(state);

        let shared = self.state.clone();
        let manifest = manifest.to_owned();
        let mut out = BufReader::new(child.stdout.take().expect("stdout is piped")).lines();
        let mut err = BufReader::new(child.stderr.take().expect("stderr is piped")).lines();
        tokio::spawn(async move {
            let (mut out_open, mut err_open) = (true, true);
            let mut last_line = String::new();
            while out_open || err_open {
                let line = tokio::select! {
                    l = out.next_line(), if out_open => (l, true),
                    l = err.next_line(), if err_open => (l, false),
                };
                match line {
                    (Ok(Some(line)), _) => {
                        let mut state = lock(&shared);
                        state.history.push_back(line.clone());
                        if state.history.len() > HISTORY {
                            state.history.pop_front();
                        }
                        let _ = state.events.send(Event::Line(line.clone()));
                        last_line = line;
                    }
                    (_, true) => out_open = false,
                    (_, false) => err_open = false,
                }
            }
            let ok = child.wait().await.is_ok_and(|s| s.success());
            let mut state = lock(&shared);
            state.last = Some(InstallResult {
                ok,
                manifest,
                finished_unix_ms: now_ms(),
                last_line,
                ..Default::default()
            });
            state.running = false;
            let _ = state.events.send(Event::Done { ok });
        });
        Ok(())
    }

    /// History and subscription taken together: no gap, no duplicate.
    pub fn progress(&self) -> Progress {
        let state = lock(&self.state);
        Progress {
            history: state.history.iter().cloned().collect(),
            live: state.running.then(|| state.events.subscribe()),
            finished: if state.running {
                None
            } else {
                state.last.as_ref().map(|r| r.ok)
            },
        }
    }

    pub async fn reboot(&self) -> Result<()> {
        let [program, args @ ..] = self.paths.reboot.as_slice() else {
            bail!("no reboot command");
        };
        let status =
            tokio::time::timeout(REBOOT_TIMEOUT, Command::new(program).args(args).status())
                .await
                .context("the reboot command timed out")?
                .with_context(|| format!("running {program}"))?;
        if !status.success() {
            bail!("{program} exited with {status}");
        }
        Ok(())
    }
}

fn unavailable(reason: impl Into<String>) -> GetUpdatesResponse {
    GetUpdatesResponse {
        available: false,
        unavailable_reason: reason.into(),
        ..Default::default()
    }
}

fn read_slots(paths: &UpdaterPaths) -> GetUpdatesResponse {
    let booted = match slot::booted_slot(&paths.run_dir) {
        Ok(Some(b)) => b,
        Ok(None) => {
            return unavailable(
                "not started through pinna-init: A/B updates work on a satellite image",
            );
        }
        Err(e) => return unavailable(format!("{e:#}")),
    };
    let state = match slot::read_state(&paths.misc) {
        Ok(s) => s.unwrap_or_else(SlotState::implicit),
        Err(e) => return unavailable(format!("{e:#}")),
    };
    let slots = [Slot::A, Slot::B].map(|s| {
        let info = state.get(s);
        ApiSlot {
            name: s.to_string(),
            version: slot_version(paths, s),
            state: if info.successful {
                "good"
            } else if info.tries_left > 0 {
                "trial"
            } else {
                "unbootable"
            }
            .into(),
            tries_left: info.tries_left.into(),
            active: state.active == s,
            booted: booted == s,
            ..Default::default()
        }
    });
    GetUpdatesResponse {
        available: true,
        booted_version: slot::booted_version(&paths.run_dir)
            .ok()
            .flatten()
            .unwrap_or_default(),
        slots: slots.into(),
        ..Default::default()
    }
}

/// The version of the signed image in a slot, empty when there is none. Only
/// the footer is read: hashing the whole slot would take too long.
fn slot_version(paths: &UpdaterPaths, s: Slot) -> String {
    let read = || -> Result<String> {
        let dev = std::fs::File::open(paths.partdir.join(s.partname()))?;
        Ok(pinna_image::read_footer(&dev, &pinna_image::keys::release_key())?.version)
    };
    read().unwrap_or_default()
}
