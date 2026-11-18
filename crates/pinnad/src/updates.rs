//! A/B updates for the dashboard and Home Assistant: the slot state, the
//! update channel's checks (ADR 0022), and `pinnactl update` run by pinnad
//! itself, so an install goes on when the browser goes away.
//!
//! A check runs `pinnactl update --check` on the channel's address once after
//! start (once the clock is set), daily at a minute picked at random and
//! kept, and on request. What it found is kept in memory; what Install does
//! with it follows from the slots each time it is asked (`release_view`).

use std::cmp::Ordering;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use buffa::MessageField;
use connectrpc::ConnectError;
use pinna_api::{
    GetUpdatesResponse, InstallResult, InstallStage, Release, ReleaseAction, Slot as ApiSlot,
    UpdateCheck,
};
use pinna_bootctl::bcb::{self, Record};
use pinna_bootctl::slot::{self, Slot};
use pinna_image::keys::VerifyingKey;
use pinna_image::version;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::{broadcast, watch};

/// The project's release service (ADR 0022).
pub const RELEASE_SERVICE: &str = "https://updates.getpinna.dev";
/// A check that takes longer is given up.
const CHECK_TIMEOUT: Duration = Duration::from_secs(180);

/// The "Updates" setting (ADR 0022), as stored in settings.toml.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UpdateChannel {
    Stable,
    Beta,
    Off,
}

impl UpdateChannel {
    pub const ALL: [UpdateChannel; 3] = [Self::Stable, Self::Beta, Self::Off];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stable => "stable",
            Self::Beta => "beta",
            Self::Off => "off",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.as_str() == s)
    }
}

/// Without a setting: development builds start on `off`, a beta release on
/// `beta` (so it stays on the track it was installed from), other releases on
/// `stable` (ADR 0022).
pub fn default_channel(development_build: bool, booted_version: &str) -> UpdateChannel {
    if development_build {
        UpdateChannel::Off
    } else if booted_version.contains("-beta.") {
        UpdateChannel::Beta
    } else {
        UpdateChannel::Stable
    }
}

/// Where checks look.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    Channel(UpdateChannel),
    /// pinnad.toml's `[update] manifest`.
    Manifest(String),
}

impl Default for Source {
    fn default() -> Self {
        Source::Channel(UpdateChannel::Off)
    }
}

impl Source {
    fn url(&self) -> Option<String> {
        match self {
            Source::Channel(UpdateChannel::Off) => None,
            Source::Channel(c) => Some(format!("{RELEASE_SERVICE}/v1/channels/{}", c.as_str())),
            Source::Manifest(u) => Some(u.clone()),
        }
    }

    fn channel(&self) -> Option<UpdateChannel> {
        match self {
            Source::Channel(UpdateChannel::Off) | Source::Manifest(_) => None,
            Source::Channel(c) => Some(*c),
        }
    }
}

/// Who started an install: one from Home Assistant restarts the satellite
/// once it is quiet, since Home Assistant has no restart button (ADR 0022).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    Dashboard,
    HomeAssistant,
}

/// Whether the system clock is set; TLS and the daily schedule need it.
#[derive(Debug, Clone, Copy)]
pub enum Clock {
    /// The kernel's view: synchronized unless `adjtimex` says otherwise.
    Kernel,
    #[cfg(test)]
    Fixed(bool),
}

impl Clock {
    fn synchronized(self) -> bool {
        match self {
            #[cfg(test)]
            Clock::Fixed(b) => b,
            Clock::Kernel => {
                // SAFETY: adjtimex with modes 0 only reads.
                let mut tx: libc::timex = unsafe { std::mem::zeroed() };
                let state = unsafe { libc::adjtimex(&mut tx) };
                state != libc::TIME_ERROR && tx.status & libc::STA_UNSYNC == 0
            }
        }
    }
}

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
    /// The `just push` overlay (ADR 0025); installs are refused while it exists.
    pub overlay: PathBuf,
    /// The minute of the day the daily check runs, picked once.
    pub check_minute: PathBuf,
    /// An install from Home Assistant waits for a restart (in /run, so a
    /// restart of pinnad keeps it and a restart of the satellite ends it).
    pub restart_mark: PathBuf,
    pub clock: Clock,
    /// This build compiles in a developer's key.
    pub development_build: bool,
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
            overlay: "/var/lib/extensions/pinna-dev".into(),
            check_minute: "/var/lib/pinna/update-check".into(),
            restart_mark: "/run/pinna/update-restart".into(),
            clock: Clock::Kernel,
            development_build: pinna_image::keys::DEVELOPMENT_BUILD,
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
    /// How far the running install got, for Home Assistant's progress.
    stage: InstallStage,
    fraction: Option<f32>,
}

/// A release a check found, as `pinnactl update --check` printed it.
#[derive(Debug, Clone, PartialEq)]
pub struct Found {
    /// The manifest's address after redirects: what an install fetches.
    pub url: String,
    pub version: String,
    pub channel: String,
    pub summary: String,
    pub notes_url: String,
    /// Why the updater would refuse it (the boot level), for people.
    pub blocked: Option<String>,
    /// It is for another model, as the updater says; not offered.
    pub other_model: Option<String>,
}

#[derive(Debug, Default)]
struct Checks {
    source: Source,
    checking: bool,
    finished_ms: u64,
    error: String,
    no_release: bool,
    next_ms: u64,
    found: Option<Found>,
}

pub struct Updater {
    paths: UpdaterPaths,
    state: Arc<Mutex<State>>,
    checks: Mutex<Checks>,
    /// One check at a time.
    check_lock: tokio::sync::Mutex<()>,
    /// Bumped whenever what GetUpdates shows may have changed.
    changed: Arc<watch::Sender<u64>>,
}

fn lock(state: &Mutex<State>) -> MutexGuard<'_, State> {
    state.lock().unwrap_or_else(|e| e.into_inner())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

fn lock_checks(c: &Mutex<Checks>) -> MutexGuard<'_, Checks> {
    c.lock().unwrap_or_else(|e| e.into_inner())
}

fn bump(changed: &watch::Sender<u64>) {
    changed.send_modify(|n| *n = n.wrapping_add(1));
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
                stage: InstallStage::INSTALL_STAGE_UNSPECIFIED,
                fraction: None,
            })),
            checks: Mutex::new(Checks::default()),
            check_lock: tokio::sync::Mutex::new(()),
            changed: Arc::new(watch::channel(0).0),
        }
    }

    /// Changes to what GetUpdates shows: checks, installs, slot switches.
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.changed.subscribe()
    }

    pub fn development_build(&self) -> bool {
        self.paths.development_build
    }

    /// The slots, the running version, the checks and the release they
    /// found; `suggested_manifest` is the caller's.
    pub async fn get_updates(&self) -> GetUpdatesResponse {
        let paths = self.paths.clone();
        let mut resp = tokio::task::spawn_blocking(move || {
            let mut resp = read_slots(&paths);
            resp.development_overlay = paths.overlay.exists();
            resp.restart_pending = paths.restart_mark.exists();
            resp
        })
        .await
        .unwrap_or_else(|e| unavailable(format!("reading the slots failed: {e}")));
        resp.development_build = self.paths.development_build;
        {
            let state = lock(&self.state);
            resp.installing = state.running;
            resp.last_install = state
                .last
                .clone()
                .map_or_else(MessageField::none, MessageField::some);
        }
        let checks = lock_checks(&self.checks);
        if let Source::Manifest(url) = &checks.source {
            resp.manifest_override.clone_from(url);
        }
        resp.check = MessageField::some(UpdateCheck {
            checking: checks.checking,
            finished_unix_ms: checks.finished_ms,
            error: checks.error.clone(),
            no_release: checks.no_release,
            next_unix_ms: if checks.source.url().is_some() {
                checks.next_ms
            } else {
                0
            },
            ..Default::default()
        });
        if let Some(found) = &checks.found {
            let booted = resp.booted_version.clone();
            resp.release = MessageField::some(release_view(
                found,
                &booted,
                &resp.slots,
                resp.development_overlay,
            ));
        }
        resp
    }

    /// The install's stage and download fraction while one runs.
    pub fn install_progress(&self) -> Option<(InstallStage, Option<f32>)> {
        let state = lock(&self.state);
        state.running.then_some((state.stage, state.fraction))
    }

    /// Start `pinnactl update -- <manifest>`; it runs to the end whoever watches.
    pub fn install(&self, manifest: &str) -> Result<(), ConnectError> {
        self.spawn_install(vec![manifest.to_owned()], manifest, Origin::Dashboard)
    }

    /// `pinnactl update <args…>`; `args` ends with the manifest.
    fn spawn_install(
        &self,
        args: Vec<String>,
        manifest: &str,
        origin: Origin,
    ) -> Result<(), ConnectError> {
        let mut state = lock(&self.state);
        if state.running {
            return Err(ConnectError::failed_precondition(
                "An update is already being installed.",
            ));
        }
        let (flags, url) = args.split_at(args.len() - 1);
        let mut child = Command::new(&self.paths.pinnactl)
            .arg("update")
            .args(flags)
            .arg("--")
            .args(url)
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
        state.stage = InstallStage::INSTALL_STAGE_UNSPECIFIED;
        state.fraction = None;
        drop(state);
        bump(&self.changed);

        let shared = self.state.clone();
        let changed = self.changed.clone();
        let restart_mark = self.paths.restart_mark.clone();
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
                        let stage = stage_of(&line);
                        if stage != InstallStage::INSTALL_STAGE_UNSPECIFIED {
                            state.stage = stage;
                            state.fraction = download_fraction(&line).or(state.fraction);
                            drop(state);
                            bump(&changed);
                        }
                        last_line = line;
                    }
                    (_, true) => out_open = false,
                    (_, false) => err_open = false,
                }
            }
            let ok = child.wait().await.is_ok_and(|s| s.success());
            if ok && origin == Origin::HomeAssistant {
                mark_restart(&restart_mark);
            }
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
            drop(state);
            bump(&changed);
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
        let result = self.set_active_inner(target, true).await;
        bump(&self.changed);
        result
    }

    /// [`Self::set_active`]; without `restart` the other slot only boots next.
    async fn set_active_inner(&self, target: Slot, restart: bool) -> Result<(), ConnectError> {
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
                // A held-back update waits for the owner, not for a quiet moment.
                let _ = std::fs::remove_file(&paths.restart_mark);
                return Ok(());
            }
            if !restart {
                return Ok(());
            }
            if let Err(e) = reboot_blocking(&paths, "switch-slot") {
                // No restart, so no reason for whatever restarts next.
                let _ =
                    std::fs::remove_file(paths.run_dir.join(pinna_bootctl::reboot::REASON_FILE));
                // No record before: the bootloader's default boots the same way.
                let old = old.unwrap_or(Record::KAERU_DEFAULT);
                if let Err(r) = bcb::write(&paths.misc, &old) {
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

/// The update channel (ADR 0022).
impl Updater {
    pub fn source(&self) -> Source {
        lock_checks(&self.checks).source.clone()
    }

    /// Where checks look at start; the first check waits for `run_checks`.
    pub fn init_source(&self, source: Source) {
        lock_checks(&self.checks).source = source;
    }

    /// Set where checks look. A new channel is checked at once; `off`
    /// forgets what the last check found.
    pub fn set_source(self: &Arc<Self>, source: Source) {
        {
            let mut checks = lock_checks(&self.checks);
            if checks.source == source {
                return;
            }
            tracing::info!("updates: {source:?}");
            checks.source = source.clone();
            checks.found = None;
            checks.error.clear();
            checks.no_release = false;
            checks.finished_ms = 0;
        }
        bump(&self.changed);
        if source.url().is_some() {
            let this = self.clone();
            tokio::spawn(async move {
                if let Err(e) = this.check().await {
                    tracing::info!("update check: {}", e.message.unwrap_or_default());
                }
            });
        }
    }

    /// Check the channel now; answers when the check has finished. A failed
    /// check is no error here: GetUpdates shows its cause.
    pub async fn check(&self) -> Result<(), ConnectError> {
        let _one = self.check_lock.lock().await;
        let Some(url) = self.source().url() else {
            return Err(ConnectError::failed_precondition(
                "Updates are off. Choose stable or beta to check for releases.",
            ));
        };
        if lock(&self.state).running {
            return Err(ConnectError::failed_precondition(
                "An update is being installed. Check again when it has finished.",
            ));
        }
        if !self.paths.clock.synchronized() {
            self.finish_check(Err(
                "The satellite's clock is not set yet, so it can't check securely. It sets the \
                 clock from the internet: check that it can reach it, then try again."
                    .into(),
            ));
            return Ok(());
        }
        lock_checks(&self.checks).checking = true;
        bump(&self.changed);
        tracing::info!("update check: {url}");
        let result = run_check(&self.paths.pinnactl, &url).await;
        self.finish_check(result);
        Ok(())
    }

    fn finish_check(&self, result: Result<CheckOutcome, String>) {
        let mut checks = lock_checks(&self.checks);
        checks.checking = false;
        checks.finished_ms = now_ms();
        match result {
            Ok(CheckOutcome::Found(found)) => {
                if checks.source.channel() == Some(UpdateChannel::Stable) && found.channel == "beta"
                {
                    checks.error = format!(
                        "The release service offered the beta {} on stable, so it isn't shown. \
                         Check again later.",
                        found.version
                    );
                } else {
                    tracing::info!("update check: found {}", found.version);
                    checks.error.clear();
                    checks.no_release = false;
                    checks.found = Some(found);
                }
            }
            Ok(CheckOutcome::NoRelease) => {
                tracing::info!("update check: no release on the channel");
                checks.error.clear();
                checks.no_release = true;
                checks.found = None;
            }
            Err(why) => {
                tracing::warn!("update check failed: {why}");
                checks.error = why;
            }
        }
        drop(checks);
        bump(&self.changed);
    }

    /// The checks after start and once a day (ADR 0022). Runs for good on a
    /// satellite image; elsewhere (a developer's computer, the tests) nothing
    /// could be installed, so nothing is checked unless asked.
    pub async fn run_checks(self: Arc<Self>) {
        if !matches!(slot::booted_slot(&self.paths.run_dir), Ok(Some(_))) {
            tracing::info!("not started through pinna-init: no scheduled update checks");
            return;
        }
        // TLS and the schedule need the time: wait until it is set.
        while !self.paths.clock.synchronized() {
            tokio::time::sleep(Duration::from_secs(15)).await;
        }
        let minute = daily_minute(&self.paths.check_minute);
        let mut scheduled = false;
        loop {
            if self.source().url().is_some()
                && let Err(e) = self.check().await
            {
                tracing::info!("update check: {}", e.message.unwrap_or_default());
            }
            if !scheduled {
                tracing::info!(
                    "update checks daily at {:02}:{:02} UTC",
                    minute / 60,
                    minute % 60
                );
                scheduled = true;
            }
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_secs());
            let next = next_daily(minute, now);
            lock_checks(&self.checks).next_ms = next * 1000;
            bump(&self.changed);
            tokio::time::sleep(Duration::from_secs(next.saturating_sub(now))).await;
        }
    }

    /// Install the release the last check found, as GetUpdates showed it with
    /// `version`: by its action (ADR 0022). `older` confirms an older one.
    pub async fn install_release(
        &self,
        version: &str,
        older: bool,
        origin: Origin,
    ) -> Result<(), ConnectError> {
        let resp = self.get_updates().await;
        let Some(release) = resp.release.as_option() else {
            return Err(ConnectError::failed_precondition(
                "No release to install: check for updates first.",
            ));
        };
        if release.version != version {
            return Err(ConnectError::failed_precondition(format!(
                "The satellite found {} since {version} was shown. Look at the release again \
                 before you install it.",
                release.version
            )));
        }
        let Some(found) = lock_checks(&self.checks)
            .found
            .clone()
            .filter(|f| f.version == version)
        else {
            return Err(ConnectError::failed_precondition(
                "The update channel changed since the release was shown. Check again.",
            ));
        };
        let channel = self.source().channel();
        let mut args = Vec::new();
        match release.action.as_known() {
            Some(ReleaseAction::RELEASE_ACTION_INSTALL) => {
                if let Some(c) = channel {
                    args.extend(["--channel".into(), c.as_str().into()]);
                }
            }
            Some(ReleaseAction::RELEASE_ACTION_OLDER) if older => {}
            Some(ReleaseAction::RELEASE_ACTION_OLDER) => {
                return Err(ConnectError::failed_precondition(format!(
                    "{version} is older than the running {}. Confirm going back first.",
                    resp.booted_version
                )));
            }
            Some(ReleaseAction::RELEASE_ACTION_START) => {
                let other = resp
                    .slots
                    .iter()
                    .find(|s| !s.booted)
                    .and_then(|s| Slot::parse(&s.name).ok())
                    .ok_or_else(|| ConnectError::internal("no other slot"))?;
                tracing::info!("updates: {version} is in slot {other} already; it boots next");
                self.set_active_inner(other, false).await?;
                if origin == Origin::HomeAssistant {
                    mark_restart(&self.paths.restart_mark);
                }
                bump(&self.changed);
                return Ok(());
            }
            Some(ReleaseAction::RELEASE_ACTION_RESTART) => {
                if origin == Origin::HomeAssistant {
                    mark_restart(&self.paths.restart_mark);
                    bump(&self.changed);
                }
                return Ok(());
            }
            _ => {
                let why = if release.note.is_empty() {
                    format!("{version} is not newer than the running version.")
                } else {
                    release.note.clone()
                };
                return Err(ConnectError::failed_precondition(why));
            }
        }
        args.extend(["--expect-version".into(), version.into(), found.url.clone()]);
        tracing::info!("updates: installing {version} from {}", found.url);
        self.spawn_install(args, &found.url, origin)
    }

    /// An install from Home Assistant waits for the satellite to be quiet.
    pub fn restart_pending(&self) -> bool {
        self.paths.restart_mark.exists()
    }
}

fn mark_restart(path: &std::path::Path) {
    if let Err(e) = std::fs::write(path, b"") {
        tracing::warn!("marking the restart after the update: {e}");
    }
}

enum CheckOutcome {
    Found(Found),
    /// The channel answered 404: it has no release yet.
    NoRelease,
}

/// `pinnactl update --check -- <url>` and its one line of JSON.
async fn run_check(pinnactl: &std::path::Path, url: &str) -> Result<CheckOutcome, String> {
    let out = tokio::time::timeout(
        CHECK_TIMEOUT,
        Command::new(pinnactl)
            .args(["update", "--check", "--", url])
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| "The check took longer than three minutes. It is tried again later.".to_owned())?
    .map_err(|e| format!("The check couldn't start ({}: {e}).", pinnactl.display()))?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let Some(v) = stdout
        .lines()
        .rev()
        .find_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
    else {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(format!(
            "The check failed: {}",
            stderr.lines().last().unwrap_or("no output")
        ));
    };
    parse_check(&v)
}

fn parse_check(v: &serde_json::Value) -> Result<CheckOutcome, String> {
    let s = |v: &serde_json::Value| v.as_str().unwrap_or_default().to_owned();
    if v["ok"].as_bool() != Some(true) {
        if v["kind"] == "status" && v["status"] == 404 {
            return Ok(CheckOutcome::NoRelease);
        }
        let message = s(&v["message"]);
        return Err(if message.is_empty() {
            "The check failed without saying why.".into()
        } else {
            message
        });
    }
    let m = &v["manifest"];
    let version = s(&m["version"]);
    if version.is_empty() {
        return Err("The check found a manifest without a version.".into());
    }
    Ok(CheckOutcome::Found(Found {
        url: s(&v["url"]),
        version,
        channel: s(&m["channel"]),
        summary: s(&m["summary"]),
        notes_url: s(&m["notes_url"]),
        blocked: match &v["blocked"] {
            // The overlay is looked at live (`release_view`).
            b if b.is_object() && b["kind"] != "overlay" && b["kind"] != "model" => {
                Some(s(&b["message"]))
            }
            _ => None,
        },
        other_model: (v["blocked"]["kind"] == "model").then(|| s(&v["blocked"]["message"])),
    }))
}

/// The minute of the day (UTC) the daily check runs: picked at random once,
/// which spreads the checks of all satellites, and kept.
fn daily_minute(path: &std::path::Path) -> u64 {
    if let Some(m) = std::fs::read_to_string(path)
        .ok()
        .and_then(|t| t.trim().parse::<u64>().ok())
        .filter(|m| *m < 24 * 60)
    {
        return m;
    }
    let m = fastrand::u64(0..24 * 60);
    if let Err(e) = std::fs::write(path, format!("{m}\n")) {
        tracing::warn!("keeping the update check's time in {}: {e}", path.display());
    }
    m
}

/// The next time, in Unix seconds after `now`, that is `minute` past
/// midnight UTC.
fn next_daily(minute: u64, now: u64) -> u64 {
    let today = now - now % 86_400 + minute * 60;
    if today > now { today } else { today + 86_400 }
}

/// What Install does with a release a check found, from the slots as they
/// are now (ADR 0022).
pub fn release_view(found: &Found, booted: &str, slots: &[ApiSlot], overlay: bool) -> Release {
    use ReleaseAction::*;
    let v = &found.version;
    let other = slots.iter().find(|s| !s.booted && s.version == *v);
    let order = version::compare(v, booted);
    let (mut action, mut note) = match (order, other) {
        (Ordering::Equal, _) => (RELEASE_ACTION_UP_TO_DATE, String::new()),
        (Ordering::Greater, Some(o)) if o.state == "unbootable" => (
            RELEASE_ACTION_FAILED,
            format!("{v} failed to start and was rolled back."),
        ),
        (_, Some(o)) if o.state != "unbootable" && o.active => {
            (RELEASE_ACTION_RESTART, String::new())
        }
        (Ordering::Greater, Some(_)) => (RELEASE_ACTION_START, String::new()),
        (Ordering::Less, _) => (RELEASE_ACTION_OLDER, String::new()),
        (Ordering::Greater, None) => match &found.other_model {
            // Not offered (ADR 0022): it can never install here.
            Some(why) => (RELEASE_ACTION_OTHER_MODEL, why.clone()),
            None => (RELEASE_ACTION_INSTALL, String::new()),
        },
    };
    if matches!(action, RELEASE_ACTION_INSTALL | RELEASE_ACTION_START) {
        let on_trial = slots.iter().any(|s| s.booted && s.state == "trial");
        let why = if overlay {
            Some(
                "A development overlay from just push is active: it would hide the new \
                 release's programs. Remove it with scripts/push-dev.sh --remove, on your \
                 computer, then install."
                    .to_owned(),
            )
        } else if on_trial {
            Some(
                "The running version is still on trial. You can install once it has started \
                 successfully, a few minutes after a restart."
                    .to_owned(),
            )
        } else if action == RELEASE_ACTION_INSTALL {
            found.blocked.clone()
        } else {
            None
        };
        if let Some(why) = why {
            action = RELEASE_ACTION_BLOCKED;
            note = why;
        }
    }
    Release {
        version: v.clone(),
        channel: found.channel.clone(),
        summary: found.summary.clone(),
        notes_url: found.notes_url.clone(),
        action: action.into(),
        note,
        ..Default::default()
    }
}

/// Home Assistant's update entity for what GetUpdates shows (ADR 0022):
/// unknown until a check has succeeded and while Updates is off; in progress
/// from the install's start until the restart.
pub fn home_assistant_state(
    resp: &GetUpdatesResponse,
    progress: Option<(InstallStage, Option<f32>)>,
) -> pinna_esphome::UpdateState {
    use ReleaseAction::*;
    let current = resp.booted_version.clone();
    let release = resp.release.as_option();
    let mut st = pinna_esphome::UpdateState {
        missing: false,
        current_version: current.clone(),
        latest_version: current.clone(),
        ..Default::default()
    };
    if let Some((stage, fraction)) = progress {
        st.in_progress = true;
        st.progress = Some(stage_percent(stage, fraction));
        if let Some(r) = release {
            st.latest_version.clone_from(&r.version);
            st.release_summary.clone_from(&r.summary);
            st.release_url.clone_from(&r.notes_url);
        }
    } else if let Some(r) = release {
        st.release_url.clone_from(&r.notes_url);
        match r.action.as_known() {
            Some(RELEASE_ACTION_INSTALL | RELEASE_ACTION_START) => {
                st.latest_version.clone_from(&r.version);
                st.release_summary.clone_from(&r.summary);
            }
            Some(RELEASE_ACTION_BLOCKED) => {
                st.latest_version.clone_from(&r.version);
                st.release_summary.clone_from(&r.note);
            }
            Some(RELEASE_ACTION_RESTART) => {
                st.latest_version.clone_from(&r.version);
                st.in_progress = true;
                st.progress = Some(100.0);
                st.release_summary = if resp.restart_pending {
                    "Installed. The satellite restarts once it is idle.".into()
                } else {
                    "Installed. Restart the satellite to run it.".into()
                };
            }
            Some(RELEASE_ACTION_FAILED) => st.release_summary.clone_from(&r.note),
            _ => {}
        }
    } else if !resp.check.as_option().is_some_and(|c| c.no_release) {
        // No check has succeeded, or Updates is off: the satellite doesn't know.
        st = pinna_esphome::UpdateState {
            current_version: current,
            ..Default::default()
        };
    }
    if !st.missing {
        st.title = format!("Pinna {}", st.latest_version);
    }
    st
}

/// Home Assistant's progress for an install stage.
pub fn stage_percent(stage: InstallStage, fraction: Option<f32>) -> f32 {
    match stage {
        InstallStage::INSTALL_STAGE_CHECK => 2.0,
        InstallStage::INSTALL_STAGE_DOWNLOAD => 5.0 + 80.0 * fraction.unwrap_or(0.0),
        InstallStage::INSTALL_STAGE_WRITE => 88.0,
        InstallStage::INSTALL_STAGE_VERIFY => 95.0,
        InstallStage::INSTALL_STAGE_INSTALLED => 100.0,
        _ => 0.0,
    }
}

/// `  12.0 MiB of 212.4 MiB` → 0.056.
fn download_fraction(line: &str) -> Option<f32> {
    let (done, total) = line.trim().split_once(" of ")?;
    let mib = |t: &str| t.trim().strip_suffix(" MiB")?.parse::<f32>().ok();
    let (done, total) = (mib(done)?, mib(total)?);
    (total > 0.0).then(|| (done / total).clamp(0.0, 1.0))
}

/// Write the slot record that boots `target` next; returns the record before
/// and the booted slot.
fn switch_slot(paths: &UpdaterPaths, target: Slot) -> Result<(Option<Record>, Slot), ConnectError> {
    let internal = |e: anyhow::Error| ConnectError::internal(format!("{e:#}"));
    let booted = slot::booted_slot(&paths.run_dir)
        .map_err(internal)?
        .ok_or_else(|| {
            ConnectError::failed_precondition(
                "not started through pinna-init: there are no slots to switch",
            )
        })?;
    let old = bcb::read(&paths.misc).map_err(internal)?;
    let record = old.unwrap_or(Record::KAERU_DEFAULT);
    if !record.get(booted).good {
        return Err(ConnectError::failed_precondition(format!(
            "the running slot {booted} is still on trial: slot {} is its only fallback",
            booted.other()
        )));
    }
    let new = if target == booted {
        bcb::keep_booted(record, booted).map_err(internal)?
    } else {
        if record.get(target).state() == "unbootable" {
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
        bcb::set_active(record, target, bcb::DEFAULT_ATTEMPTS).map_err(internal)?
    };
    if old != Some(new) {
        bcb::write(&paths.misc, &new).map_err(internal)?;
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
    let record = match bcb::read_or_default(&paths.misc) {
        Ok(r) => r,
        Err(e) => return unavailable(format!("{e:#}")),
    };
    let next = record.next_boot();
    let slots = [Slot::A, Slot::B].map(|s| {
        let m = record.get(s);
        ApiSlot {
            name: s.to_string(),
            version: slot_version(paths, s),
            state: m.state().into(),
            tries_left: m.attempts_left().into(),
            active: next == Some(s),
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

#[cfg(test)]
mod channel_tests {
    use super::*;
    use pinna_api::ReleaseAction::*;

    fn found(version: &str) -> Found {
        Found {
            url: format!("https://updates.example/v1/releases/v{version}/manifest.json"),
            version: version.into(),
            channel: "stable".into(),
            summary: "Summary.".into(),
            notes_url: "https://example.org/notes".into(),
            blocked: None,
            other_model: None,
        }
    }

    fn slot(name: &str, version: &str, state: &str, active: bool, booted: bool) -> ApiSlot {
        ApiSlot {
            name: name.into(),
            version: version.into(),
            state: state.into(),
            active,
            booted,
            ..Default::default()
        }
    }

    /// Booted from a (0.4.0, good); slot b as given.
    fn slots(b_version: &str, b_state: &str, b_next: bool) -> Vec<ApiSlot> {
        vec![
            slot("a", "0.4.0", "good", !b_next, true),
            slot("b", b_version, b_state, b_next, false),
        ]
    }

    fn action(r: &Release) -> ReleaseAction {
        r.action.as_known().unwrap()
    }

    #[test]
    fn what_install_does() {
        let older = slots("0.3.0", "good", false);
        let view = |v: &str, s: &[ApiSlot]| release_view(&found(v), "0.4.0", s, false);
        assert_eq!(action(&view("0.4.0", &older)), RELEASE_ACTION_UP_TO_DATE);
        assert_eq!(action(&view("0.4.1", &older)), RELEASE_ACTION_INSTALL);
        assert_eq!(action(&view("0.3.0", &older)), RELEASE_ACTION_OLDER);
        assert_eq!(
            action(&view("0.4.0", &[])),
            RELEASE_ACTION_UP_TO_DATE,
            "no slots"
        );
        // Installed and waiting for the restart.
        assert_eq!(
            action(&view("0.4.1", &slots("0.4.1", "trial", true))),
            RELEASE_ACTION_RESTART
        );
        // Held back with "Start it later": started, not downloaded again.
        assert_eq!(
            action(&view("0.4.1", &slots("0.4.1", "trial", false))),
            RELEASE_ACTION_START
        );
        // Failed its trial boot.
        let failed = view("0.4.1", &slots("0.4.1", "unbootable", false));
        assert_eq!(action(&failed), RELEASE_ACTION_FAILED);
        assert_eq!(failed.note, "0.4.1 failed to start and was rolled back.");
        // An install that stopped part way leaves the old version unbootable:
        // that is no failed trial of the release.
        assert_eq!(
            action(&view("0.4.1", &slots("0.3.0", "unbootable", false))),
            RELEASE_ACTION_INSTALL
        );
        // A development version from before semantic versions comes first.
        let r = release_view(&found("0.4.1"), "2026.11.17.1405-abc1234", &older, false);
        assert_eq!(action(&r), RELEASE_ACTION_INSTALL);
    }

    #[test]
    fn what_blocks_an_install() {
        let older = slots("0.3.0", "good", false);
        let r = release_view(&found("0.4.1"), "0.4.0", &older, true);
        assert_eq!(action(&r), RELEASE_ACTION_BLOCKED);
        assert!(
            r.note.contains("scripts/push-dev.sh --remove"),
            "{}",
            r.note
        );

        let trial = vec![
            slot("a", "0.4.0", "trial", true, true),
            slot("b", "0.3.0", "good", false, false),
        ];
        let r = release_view(&found("0.4.1"), "0.4.0", &trial, false);
        assert_eq!(action(&r), RELEASE_ACTION_BLOCKED);
        assert!(r.note.contains("still on trial"), "{}", r.note);

        let mut f = found("0.4.1");
        f.blocked = Some("Needs a new boot image: install it over USB.".into());
        let r = release_view(&f, "0.4.0", &older, false);
        assert_eq!(action(&r), RELEASE_ACTION_BLOCKED);
        assert_eq!(r.note, "Needs a new boot image: install it over USB.");
        // Nothing to install, nothing to block.
        assert_eq!(
            action(&release_view(&f, "0.4.1", &older, true)),
            RELEASE_ACTION_UP_TO_DATE
        );

        // For another model: not offered, in Home Assistant neither.
        let mut f = found("0.4.1");
        f.other_model = Some("This release is for echo-dot-2, not for echo-2.".into());
        let r = release_view(&f, "0.4.0", &older, false);
        assert_eq!(action(&r), RELEASE_ACTION_OTHER_MODEL);
        assert_eq!(r.note, "This release is for echo-dot-2, not for echo-2.");
        let st = home_assistant_state(&resp(Some(r), false), None);
        assert!(!st.missing);
        assert_eq!(st.latest_version, "0.4.0");
    }

    fn resp(release: Option<Release>, no_release: bool) -> GetUpdatesResponse {
        GetUpdatesResponse {
            available: true,
            booted_version: "0.4.0".into(),
            release: release.map_or_else(MessageField::none, MessageField::some),
            check: MessageField::some(UpdateCheck {
                no_release,
                finished_unix_ms: 1,
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn home_assistant_sees_what_the_satellite_knows() {
        let older = slots("0.3.0", "good", false);
        // No check has succeeded (or Updates is off): unknown, not "up to date".
        let st = home_assistant_state(&resp(None, false), None);
        assert!(st.missing);
        // The channel has no release: up to date.
        let st = home_assistant_state(&resp(None, true), None);
        assert!(!st.missing);
        assert_eq!(st.latest_version, "0.4.0");
        assert_eq!(st.title, "Pinna 0.4.0");

        let newer = release_view(&found("0.4.1"), "0.4.0", &older, false);
        let st = home_assistant_state(&resp(Some(newer.clone()), false), None);
        assert_eq!(
            (
                st.current_version.as_str(),
                st.latest_version.as_str(),
                st.title.as_str()
            ),
            ("0.4.0", "0.4.1", "Pinna 0.4.1")
        );
        assert_eq!(st.release_summary, "Summary.");
        assert_eq!(st.release_url, "https://example.org/notes");
        assert!(!st.in_progress);

        // Installing: progress follows the stages.
        let st = home_assistant_state(
            &resp(Some(newer), false),
            Some((InstallStage::INSTALL_STAGE_DOWNLOAD, Some(0.5))),
        );
        assert!(st.in_progress);
        assert_eq!(st.progress, Some(45.0));

        // Installed, waiting for the restart: in progress at 100.
        let waiting = release_view(
            &found("0.4.1"),
            "0.4.0",
            &slots("0.4.1", "trial", true),
            false,
        );
        let mut r = resp(Some(waiting), false);
        r.restart_pending = true;
        let st = home_assistant_state(&r, None);
        assert!(st.in_progress);
        assert_eq!(st.progress, Some(100.0));
        assert!(st.release_summary.contains("restarts once it is idle"));

        // Failed: not offered, and the summary says why.
        let failed = release_view(
            &found("0.4.1"),
            "0.4.0",
            &slots("0.4.1", "unbootable", false),
            false,
        );
        let st = home_assistant_state(&resp(Some(failed), false), None);
        assert_eq!(st.latest_version, "0.4.0");
        assert_eq!(
            st.release_summary,
            "0.4.1 failed to start and was rolled back."
        );

        // Blocked: shown, with the reason before anyone presses Install.
        let blocked = release_view(&found("0.4.1"), "0.4.0", &older, true);
        let st = home_assistant_state(&resp(Some(blocked), false), None);
        assert_eq!(st.latest_version, "0.4.1");
        assert!(st.release_summary.contains("push-dev.sh"));

        // Older (beta to stable): nothing offered here.
        let back = release_view(&found("0.3.5"), "0.4.0", &older, false);
        let st = home_assistant_state(&resp(Some(back), false), None);
        assert_eq!(st.latest_version, "0.4.0");
    }

    #[test]
    fn check_output() {
        let ok = serde_json::json!({
            "ok": true,
            "url": "https://u/v1/releases/v0.4.1/manifest.json",
            "manifest": { "version": "0.4.1", "channel": "stable", "summary": "S.",
                          "notes_url": "https://n", "requires": { "boot_level": 1 },
                          "root": { "url": "r", "size": 1, "sha256": "00" } },
            "blocked": { "kind": "boot_level", "message": "Needs a new boot image: install it over USB." },
        });
        let Ok(CheckOutcome::Found(f)) = parse_check(&ok) else {
            panic!("found");
        };
        assert_eq!(f.version, "0.4.1");
        assert_eq!(f.url, "https://u/v1/releases/v0.4.1/manifest.json");
        assert_eq!(
            f.blocked.as_deref(),
            Some("Needs a new boot image: install it over USB.")
        );
        // A model block makes the release another model's, not a blocked one.
        let mut model = ok.clone();
        model["blocked"] = serde_json::json!({ "kind": "model", "message": "For echo-2." });
        let Ok(CheckOutcome::Found(f)) = parse_check(&model) else {
            panic!("found");
        };
        assert_eq!(
            (f.blocked, f.other_model.as_deref()),
            (None, Some("For echo-2."))
        );
        // The overlay is looked at when GetUpdates is asked, not at the check.
        let mut overlay = ok.clone();
        overlay["blocked"] = serde_json::json!({ "kind": "overlay", "message": "x" });
        assert!(matches!(
            parse_check(&overlay),
            Ok(CheckOutcome::Found(Found { blocked: None, .. }))
        ));

        let none = serde_json::json!({ "ok": false, "kind": "status", "status": 404,
                                       "message": "updates.getpinna.dev answered 404: No release on stable yet." });
        assert!(matches!(parse_check(&none), Ok(CheckOutcome::NoRelease)));
        let down = serde_json::json!({ "ok": false, "kind": "no_answer",
                                       "message": "updates.getpinna.dev did not answer." });
        assert_eq!(
            parse_check(&down).err().as_deref(),
            Some("updates.getpinna.dev did not answer.")
        );
    }

    #[test]
    fn the_daily_minute_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("update-check");
        let m = daily_minute(&p);
        assert!(m < 1440);
        assert_eq!(daily_minute(&p), m);
        std::fs::write(&p, "9999\n").unwrap();
        assert!(daily_minute(&p) < 1440, "out of range: picked again");
        // 2026-11-17 12:00 UTC.
        let noon = 1_795_435_200;
        assert_eq!(noon % 86_400, 43_200);
        assert_eq!(next_daily(13 * 60, noon), noon + 3600);
        assert_eq!(next_daily(12 * 60, noon), noon + 86_400);
        assert_eq!(next_daily(0, noon), noon + 12 * 3600);
    }

    #[test]
    fn channels_and_progress_lines() {
        assert_eq!(default_channel(true, "0.4.0"), UpdateChannel::Off);
        assert_eq!(default_channel(false, "0.4.0"), UpdateChannel::Stable);
        assert_eq!(default_channel(false, "0.5.0-beta.1"), UpdateChannel::Beta);
        assert_eq!(UpdateChannel::parse("beta"), Some(UpdateChannel::Beta));
        assert_eq!(UpdateChannel::parse("nightly"), None);
        assert_eq!(
            Source::Channel(UpdateChannel::Beta).url().as_deref(),
            Some("https://updates.getpinna.dev/v1/channels/beta")
        );
        assert_eq!(Source::Channel(UpdateChannel::Off).url(), None);
        assert_eq!(download_fraction("  53.1 MiB of 212.4 MiB"), Some(0.25));
        assert_eq!(download_fraction("downloading x"), None);
    }
}
