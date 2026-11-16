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
use pinna_api::{GetUpdatesResponse, InstallResult, InstallStage, Slot as ApiSlot};
use pinna_bootctl::slot::{self, Slot, SlotState};
use pinna_image::keys::VerifyingKey;
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
    /// Asks the next boot for a factory reset (ADR 0010).
    pub reset_mark: PathBuf,
    /// Slot images must be signed with this key.
    pub key: VerifyingKey,
}

impl Default for UpdaterPaths {
    fn default() -> Self {
        Self {
            misc: "/dev/disk/by-partlabel/misc".into(),
            partdir: "/dev/disk/by-partlabel".into(),
            run_dir: slot::RUN_DIR.into(),
            pinnactl: "pinnactl".into(),
            reboot: vec!["systemctl".into(), "reboot".into()],
            reset_mark: "/var/lib/pinna/factory-reset".into(),
            key: pinna_image::keys::release_key(),
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

    /// Boot `target` next (see SetActiveSlot in the proto). Switching to the
    /// other slot restarts the satellite and puts the slot state back if the
    /// restart fails, so a switch never waits for the next power cut.
    pub async fn set_active(&self, target: Slot) -> Result<(), ConnectError> {
        let paths = self.paths.clone();
        let shared = self.state.clone();
        tokio::task::spawn_blocking(move || {
            // Held throughout: an install would write the slot state too.
            let state = lock(&shared);
            if state.running {
                return Err(ConnectError::failed_precondition(
                    "an update is being installed",
                ));
            }
            let (old, booted) = switch_slot(&paths, target)?;
            if target == booted {
                return Ok(());
            }
            if let Err(e) = reboot_blocking(&paths, "switch-slot") {
                // No restart, so no reason for whatever restarts next.
                let _ =
                    std::fs::remove_file(paths.run_dir.join(pinna_bootctl::reboot::REASON_FILE));
                // No record before: the implicit state boots the same way.
                let old = old.unwrap_or_else(SlotState::implicit);
                if let Err(r) = slot::write_state(&paths.misc, &old) {
                    tracing::error!("putting the slot state back failed: {r:#}");
                }
                return Err(ConnectError::internal(format!(
                    "the restart failed, so the switch was undone: {e:#}"
                )));
            }
            Ok(())
        })
        .await
        .map_err(|e| ConnectError::internal(format!("switching slots failed: {e}")))?
    }

    /// Mark the next boot for a factory reset, then reboot (ADR 0010):
    /// `pinna-factory-reset.service` clears /var before anything uses it.
    pub async fn factory_reset(&self) -> Result<()> {
        let mark = &self.paths.reset_mark;
        let write = || -> std::io::Result<()> {
            let f = std::fs::File::create(mark)?;
            f.sync_all()?;
            if let Some(dir) = mark.parent() {
                std::fs::File::open(dir)?.sync_all()?;
            }
            Ok(())
        };
        write().with_context(|| format!("writing {}", mark.display()))?;
        tracing::warn!("factory reset requested: restarting");
        self.reboot("factory-reset").await
    }

    /// Reboot, recording `reason` for the next boot's report (ADR 0012).
    pub async fn reboot(&self, reason: &str) -> Result<()> {
        let [program, args @ ..] = self.paths.reboot.as_slice() else {
            bail!("no reboot command");
        };
        // Remember why, for the next boot's report (ADR 0012). Best effort.
        let _ = pinna_bootctl::reboot::record_at(&self.paths.run_dir, reason);
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

/// Write the slot state that boots `target` next; returns the state before
/// and the booted slot.
fn switch_slot(
    paths: &UpdaterPaths,
    target: Slot,
) -> Result<(Option<SlotState>, Slot), ConnectError> {
    let internal = |e: anyhow::Error| ConnectError::internal(format!("{e:#}"));
    let booted = slot::booted_slot(&paths.run_dir)
        .map_err(internal)?
        .ok_or_else(|| {
            ConnectError::failed_precondition(
                "not started through pinna-init: there are no slots to switch",
            )
        })?;
    let old = slot::read_state(&paths.misc).map_err(internal)?;
    let state = old.unwrap_or_else(SlotState::implicit);
    if !state.get(booted).successful {
        return Err(ConnectError::failed_precondition(format!(
            "the running slot {booted} is still on trial: slot {} is its only fallback",
            booted.other()
        )));
    }
    let new = if target == booted {
        slot::keep_booted(old, booted)
    } else {
        let info = state.get(target);
        if !info.successful && info.tries_left == 0 {
            return Err(ConnectError::failed_precondition(format!(
                "slot {target} can't boot: install an update into it instead"
            )));
        }
        let image = slot::image(&paths.partdir, target, &paths.key).map_err(|e| {
            ConnectError::failed_precondition(format!("slot {target} has no bootable image: {e:#}"))
        })?;
        tracing::info!(
            "switching to slot {target} (version {}) as a trial",
            image.version
        );
        slot::set_active(old, target, slot::DEFAULT_TRIES)
    };
    if old != Some(new) {
        slot::write_state(&paths.misc, &new).map_err(internal)?;
    }
    Ok((old, booted))
}

/// [`Updater::reboot`] for a blocking thread, with the same timeout.
fn reboot_blocking(paths: &UpdaterPaths, reason: &str) -> Result<()> {
    let [program, args @ ..] = paths.reboot.as_slice() else {
        bail!("no reboot command");
    };
    let _ = pinna_bootctl::reboot::record_at(&paths.run_dir, reason);
    let mut child = std::process::Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .spawn()
        .with_context(|| format!("running {program}"))?;
    let deadline = std::time::Instant::now() + REBOOT_TIMEOUT;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            bail!("the reboot command timed out");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    if !status.success() {
        bail!("{program} exited with {status}");
    }
    Ok(())
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
        Ok(pinna_image::read_footer(&dev, &paths.key)?.version)
    };
    read().unwrap_or_default()
}

/// The install stage a `pinna-update` output line reports; keep in step with
/// the `say(...)` calls there.
pub fn stage_of(line: &str) -> InstallStage {
    if line.starts_with("booted slot ") {
        InstallStage::INSTALL_STAGE_CHECK
    } else if line.starts_with("downloading ") || is_progress(line) {
        InstallStage::INSTALL_STAGE_DOWNLOAD
    } else if line.starts_with("written and flushed ") {
        InstallStage::INSTALL_STAGE_WRITE
    } else if line.starts_with("slot ") && line.contains(" verified") {
        InstallStage::INSTALL_STAGE_VERIFY
    } else if line.starts_with("installed version ") {
        InstallStage::INSTALL_STAGE_INSTALLED
    } else {
        InstallStage::INSTALL_STAGE_UNSPECIFIED
    }
}

/// `  12.0 MiB of 212.4 MiB`: two spaces, then a size.
fn is_progress(line: &str) -> bool {
    line.strip_prefix("  ")
        .is_some_and(|r| r.starts_with(|c: char| c.is_ascii_digit()))
}

#[cfg(test)]
mod stage_tests {
    use super::*;

    #[test]
    fn lines_map_to_stages() {
        let cases = [
            (
                "booted slot a (version 0.3.0); installing version 0.4.0 into slot b",
                InstallStage::INSTALL_STAGE_CHECK,
            ),
            (
                "downloading https://example.org/pinna.root.img (212.4 MiB) into /dev/disk/by-partlabel/root_b",
                InstallStage::INSTALL_STAGE_DOWNLOAD,
            ),
            (
                "  12.0 MiB of 212.4 MiB",
                InstallStage::INSTALL_STAGE_DOWNLOAD,
            ),
            (
                "written and flushed 212.4 MiB; sha256 OK",
                InstallStage::INSTALL_STAGE_WRITE,
            ),
            (
                "slot b verified: signature OK, dm-verity root hash 00ff matches the data",
                InstallStage::INSTALL_STAGE_VERIFY,
            ),
            (
                "installed version 0.4.0 into slot b: the next boot tries it (3 tries), falling back to slot a (version 0.3.0)",
                InstallStage::INSTALL_STAGE_INSTALLED,
            ),
            (
                "sha256 mismatch: got 00, manifest says 11",
                InstallStage::INSTALL_STAGE_UNSPECIFIED,
            ),
            (
                "  indented but no size",
                InstallStage::INSTALL_STAGE_UNSPECIFIED,
            ),
            ("", InstallStage::INSTALL_STAGE_UNSPECIFIED),
        ];
        for (line, stage) in cases {
            assert_eq!(stage_of(line), stage, "{line:?}");
        }
    }
}
