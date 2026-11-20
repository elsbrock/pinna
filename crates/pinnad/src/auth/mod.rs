//! The dashboard password, sessions, tokens and presence confirmation
//! (ADR 0029).
//!
//! [`Auth`] holds the state and answers the `DeviceService` calls about it.
//! [`layer`] puts it in front of the web listener: default-deny, so a route
//! added later is protected without anyone remembering to. The credentials
//! live in `secrets.toml` and are written through the standalone service, the
//! file's one writer (ADR 0017). The loopback API has no layer.

mod confirm;
mod credentials;
pub mod layer;
mod limit;
mod session;

use std::net::IpAddr;
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use connectrpc::ConnectError;
use pinna_api::Security;
use pinna_assist::secrets::DashboardFile;
use pinna_esphome::EsphomeHandle;
use ring::rand::{SecureRandom, SystemRandom};
use tokio::sync::{mpsc, oneshot, watch};

use crate::standalone::service::Service;
use crate::voice::{Input, Presence};
use confirm::{Confirmations, Current, Grants, Progress};
pub use confirm::{Progress as ConfirmProgress, WINDOW as CONFIRM_WINDOW};
pub use credentials::{TokenKind, has_password};

/// How long the ESPHome API takes connections without a key after the owner
/// chose **Let Home Assistant add this satellite** (ADR 0029).
pub const HOME_ASSISTANT_WINDOW: Duration = Duration::from_secs(15 * 60);
/// How long a `Join` is accepted after **Allow adding to a group** (ADR 0029).
pub const GROUP_JOIN_WINDOW: Duration = Duration::from_secs(10 * 60);

/// What the System page and the first-password form look back over.
const LOOK_BACK: Duration = Duration::from_secs(24 * 3600);

const LENGTH: &str = "The password must be 8 to 128 characters. Choose one in that range.";
const WRONG_CURRENT: &str =
    "The current password is wrong. Check it, or press Action at the satellite to confirm instead.";
const NEEDS_CURRENT: &str =
    "Enter the current password, or confirm at the satellite by pressing Action.";
const BAD_GRANT: &str =
    "The confirmation has expired or was already used. Confirm at the satellite again.";
const NEEDS_PRESS: &str = "Setting the first password needs someone at the satellite. Start a confirmation, then press Action while the ring circles white.";
const NO_PASSWORD: &str = "No password is set.";
const HAS_PASSWORD: &str = "A password is set. Remove it first.";
const UNKNOWN_CONFIRMATION: &str = "That confirmation is unknown or has ended. Start a new one.";
const NO_RING: &str = "The satellite can't take a confirmation right now. Try again in a moment.";
const WINDOW_NEEDS_PASSWORD: &str =
    "No password is set, so Home Assistant can add this satellite without opening a window.";
const WINDOW_HAS_KEY: &str =
    "Home Assistant has added this satellite already. Nothing needs opening.";
const GROUP_NEEDS_PASSWORD: &str =
    "No password is set, so the Satellite group switch is all it takes to be added to a group.";
const NO_RANDOM: &str = "The satellite couldn't make random numbers. Try again.";

/// Why a login didn't give a session.
#[derive(Debug, PartialEq, Eq)]
pub enum LoginError {
    NoPassword,
    Wrong,
    /// Seconds until the next attempt is taken.
    Wait(u64),
}

/// How one password check ended.
enum Attempt {
    Right,
    Wrong,
    Wait(u64),
}

pub struct Auth {
    service: Arc<Service>,
    /// To the Brain, which takes the Action press (ADR 0029).
    inputs: mpsc::Sender<Input>,
    cookie: String,
    creds: RwLock<Arc<DashboardFile>>,
    /// Bumped whenever the session key changes; streams opened under the old
    /// key end when they see it.
    ended: watch::Sender<u64>,
    limiter: Mutex<limit::Limiter>,
    /// Serialises changes, so of two made at once the second is checked
    /// against what the first left: a wrong current password.
    change: tokio::sync::Mutex<()>,
    confirmations: Mutex<Confirmations>,
    grants: Mutex<Grants>,
    metrics_read: Mutex<Option<SystemTime>>,
    /// How long a confirmation waits for a press.
    window: Duration,
    /// The ESPHome API, whose keyless connections the password closes.
    esphome: OnceLock<EsphomeHandle>,
    /// When the windows end, while they are open.
    windows: Mutex<Windows>,
}

/// The two windows a password closes until the owner opens them (ADR 0029).
#[derive(Default)]
struct Windows {
    home_assistant: Option<Instant>,
    group_join: Option<Instant>,
}

/// Whole seconds until `end`, rounded up; 0 once it passed or none is open.
fn left(end: Option<Instant>, now: Instant) -> u32 {
    end.map_or(0, |end| {
        limit::ceil_secs(end.saturating_duration_since(now)) as u32
    })
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn wait_sentence(seconds: u64) -> String {
    format!("Too many attempts. Try again in {seconds} s.")
}

impl Auth {
    /// `mac` names the cookie. `inputs` reaches the Brain.
    pub fn new(service: Arc<Service>, mac: &str, inputs: mpsc::Sender<Input>) -> Arc<Self> {
        Self::waiting(service, mac, inputs, CONFIRM_WINDOW)
    }

    fn waiting(
        service: Arc<Service>,
        mac: &str,
        inputs: mpsc::Sender<Input>,
        window: Duration,
    ) -> Arc<Self> {
        Arc::new(Self {
            window,
            creds: RwLock::new(Arc::new(service.dashboard())),
            service,
            inputs,
            cookie: session::cookie_name(mac),
            ended: watch::channel(0).0,
            limiter: Mutex::default(),
            change: tokio::sync::Mutex::new(()),
            confirmations: Mutex::default(),
            grants: Mutex::default(),
            metrics_read: Mutex::new(None),
            esphome: OnceLock::new(),
            windows: Mutex::default(),
        })
    }

    /// The ESPHome API this closes to keyless connections while a password is
    /// set and its window is shut. Applies the state now.
    pub fn attach_esphome(&self, esphome: EsphomeHandle) {
        if self.esphome.set(esphome).is_ok() {
            self.sync_esphome();
        }
    }

    /// Tell the ESPHome API whether it takes connections without a key: not
    /// with a password set, except inside the window. With a key it doesn't
    /// matter.
    fn sync_esphome(&self) {
        let Some(esphome) = self.esphome.get() else {
            return;
        };
        let open = left(self.windows.lock().unwrap().home_assistant, Instant::now()) > 0;
        let closed = credentials::has_password(&self.credentials()) && !open;
        esphome.set_keyless_closed(closed);
    }

    /// Let Home Assistant add this satellite for [`HOME_ASSISTANT_WINDOW`].
    pub fn open_home_assistant_window(self: &Arc<Self>) -> Result<(), ConnectError> {
        if !credentials::has_password(&self.credentials()) {
            return Err(ConnectError::failed_precondition(WINDOW_NEEDS_PASSWORD));
        }
        if self.esphome.get().is_some_and(|e| e.has_key()) {
            return Err(ConnectError::failed_precondition(WINDOW_HAS_KEY));
        }
        self.windows.lock().unwrap().home_assistant = Some(Instant::now() + HOME_ASSISTANT_WINDOW);
        self.sync_esphome();
        // The API closes again when the window ends, also over a connection
        // that is still open.
        let auth = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(HOME_ASSISTANT_WINDOW).await;
            auth.sync_esphome();
        });
        Ok(())
    }

    /// Allow a `Join` for [`GROUP_JOIN_WINDOW`].
    pub fn allow_group_join(&self) -> Result<(), ConnectError> {
        if !credentials::has_password(&self.credentials()) {
            return Err(ConnectError::failed_precondition(GROUP_NEEDS_PASSWORD));
        }
        self.windows.lock().unwrap().group_join = Some(Instant::now() + GROUP_JOIN_WINDOW);
        Ok(())
    }

    /// The owner allowed a `Join` and its window is still open.
    pub fn group_join_open(&self) -> bool {
        left(self.windows.lock().unwrap().group_join, Instant::now()) > 0
    }

    /// A password, set without a press or the full cost of a hash, for tests
    /// of what a password changes elsewhere.
    #[cfg(test)]
    pub(crate) async fn set_password_for_test(&self, password: &str) {
        let mut hashed = DashboardFile::default();
        credentials::set_password(&mut hashed, password, 1000).unwrap();
        self.save(|file| {
            file.hash = hashed.hash;
            file.salt = hashed.salt;
            file.iterations = hashed.iterations;
        })
        .await
        .unwrap();
    }

    pub fn credentials(&self) -> Arc<DashboardFile> {
        self.creds.read().unwrap().clone()
    }

    /// Streams hold one of these and end when it changes. Take it before
    /// reading the credentials, so a change in between isn't missed.
    pub fn subscribe_end(&self) -> watch::Receiver<u64> {
        self.ended.subscribe()
    }

    /// The request carries a session that is valid now.
    pub fn signed_in(&self, headers: &axum::http::HeaderMap) -> bool {
        let key = credentials::session_key(&self.credentials());
        session::read(headers, &self.cookie).is_some_and(|v| session::valid(&key, v, unix_now()))
    }

    /// The request carries the `kind` token as a bearer token.
    pub fn bearer_ok(&self, kind: TokenKind, headers: &axum::http::HeaderMap) -> bool {
        headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split_once(' '))
            .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
            .is_some_and(|(_, token)| {
                credentials::token_matches(&self.credentials(), kind, token.trim())
            })
    }

    pub fn note_metrics_read(&self) {
        *self.metrics_read.lock().unwrap() = Some(SystemTime::now());
    }

    fn apply(&self, file: DashboardFile) {
        let key_changed = {
            let mut creds = self.creds.write().unwrap();
            let changed = creds.session_key != file.session_key;
            *creds = Arc::new(file);
            changed
        };
        // After the new key is in place: a stream that read the old key
        // subscribed before this and sees it.
        if key_changed {
            self.ended.send_modify(|generation| *generation += 1);
        }
        // Without a password the windows mean nothing, and a later password
        // starts with them shut.
        if !credentials::has_password(&self.credentials()) {
            *self.windows.lock().unwrap() = Windows::default();
        }
        self.sync_esphome();
    }

    async fn save(&self, edit: impl FnOnce(&mut DashboardFile)) -> Result<(), ConnectError> {
        match self.service.edit_dashboard(edit).await {
            Ok(file) => {
                self.apply(file);
                Ok(())
            }
            Err(refusal) => Err(ConnectError::failed_precondition(refusal.sentence())),
        }
    }

    /// The access state, for `signed_in` as the caller is. With a password set,
    /// a caller without a session gets only what the sign-in form needs: the
    /// failures, their address, the tokens' ends, the agents' names and the
    /// windows are for the System page, not for anyone on the LAN (ADR 0029).
    pub fn security(&self, signed_in: bool) -> Security {
        let creds = self.credentials();
        let now = Instant::now();
        if credentials::has_password(&creds) && !signed_in {
            let wait = self.limiter.lock().unwrap().wait(now);
            return Security {
                password_set: true,
                login_retry_after_s: limit::ceil_secs(wait) as u32,
                confirmation_cooldown_s: self.confirmations.lock().unwrap().cooldown(now) as u32,
                ..Default::default()
            };
        }
        let (since_login, failure, wait) = {
            let limiter = self.limiter.lock().unwrap();
            (
                limiter.since_login(),
                limiter.last_failure(),
                limiter.wait(now),
            )
        };
        let recent = |at: SystemTime| at.elapsed().is_ok_and(|age| age < LOOK_BACK);
        let mut clients: Vec<String> = Vec::new();
        for agent in self.service.agents() {
            if recent(agent.last_seen) && !clients.contains(&agent.name) {
                clients.push(agent.name);
            }
        }
        let password_set = credentials::has_password(&creds);
        let (home_assistant_window_left_s, group_join_left_s) = {
            let windows = self.windows.lock().unwrap();
            (
                left(windows.home_assistant, Instant::now()),
                left(windows.group_join, Instant::now()),
            )
        };
        Security {
            password_set,
            left_open: creds.left_open,
            offer_pending: !password_set && !creds.left_open,
            signed_in,
            metrics_token_last4: creds.metrics_last4.clone(),
            agent_token_last4: creds.agent_last4.clone(),
            failures_since_login: since_login,
            last_failure_address: failure.map(|f| f.address.to_string()).unwrap_or_default(),
            last_failure_unix_ms: failure
                .and_then(|f| f.at.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_millis() as u64),
            login_retry_after_s: limit::ceil_secs(wait) as u32,
            confirmation_cooldown_s: self.confirmations.lock().unwrap().cooldown(now) as u32,
            mcp_clients: clients,
            metrics_read: self.metrics_read.lock().unwrap().is_some_and(recent),
            home_assistant_window_left_s,
            group_join_left_s,
            ..Default::default()
        }
    }

    /// Check `password` against the stored one, within the rate limit. A
    /// refused attempt doesn't run PBKDF2. A failure is logged and counted.
    async fn attempt(&self, from: IpAddr, password: String) -> Attempt {
        if let Err(seconds) = self.limiter.lock().unwrap().admit(Instant::now()) {
            return Attempt::Wait(seconds);
        }
        let creds = self.credentials();
        let right =
            tokio::task::spawn_blocking(move || credentials::check_password(&creds, &password))
                .await
                .unwrap_or(false);
        let mut limiter = self.limiter.lock().unwrap();
        if right {
            limiter.succeeded();
            Attempt::Right
        } else {
            limiter.failed(from, SystemTime::now());
            tracing::info!("dashboard: wrong password from {from}");
            Attempt::Wrong
        }
    }

    /// The cookie value of a new session, for the right password.
    pub async fn login(&self, from: IpAddr, password: String) -> Result<String, LoginError> {
        if !credentials::has_password(&self.credentials()) {
            return Err(LoginError::NoPassword);
        }
        match self.attempt(from, password).await {
            Attempt::Right => {
                let key = credentials::session_key(&self.credentials());
                Ok(session::issue(&key, unix_now() + session::MAX_AGE_S))
            }
            Attempt::Wrong => Err(LoginError::Wrong),
            Attempt::Wait(seconds) => Err(LoginError::Wait(seconds)),
        }
    }

    pub fn session_cookie(&self, value: &str) -> String {
        session::set_cookie(&self.cookie, value)
    }

    pub fn clear_session_cookie(&self) -> String {
        session::clear_cookie(&self.cookie)
    }

    /// What lets a change through: the current password when one is set, or
    /// a grant.
    async fn allow(
        &self,
        from: IpAddr,
        password_set: bool,
        current: String,
        grant: String,
    ) -> Result<(), ConnectError> {
        if password_set && !current.is_empty() {
            return match self.attempt(from, current).await {
                Attempt::Right => Ok(()),
                Attempt::Wrong => Err(ConnectError::permission_denied(WRONG_CURRENT)),
                Attempt::Wait(seconds) => {
                    Err(ConnectError::resource_exhausted(wait_sentence(seconds)))
                }
            };
        }
        if !grant.is_empty() {
            let granted = self.grants.lock().unwrap().consume(&grant, Instant::now());
            return if granted {
                Ok(())
            } else {
                Err(ConnectError::permission_denied(BAD_GRANT))
            };
        }
        Err(if password_set {
            ConnectError::invalid_argument(NEEDS_CURRENT)
        } else {
            ConnectError::failed_precondition(NEEDS_PRESS)
        })
    }

    /// A change that was asked for with the current password, after another
    /// change replaced it: the password it names is no longer the current
    /// one, so it fails as a wrong current password (and counts as a failed
    /// login) without another PBKDF2.
    fn outdated(&self, from: IpAddr, seen: &str, current: &str) -> Result<(), ConnectError> {
        if current.is_empty() || self.credentials().hash == seen {
            return Ok(());
        }
        self.limiter.lock().unwrap().failed(from, SystemTime::now());
        tracing::info!("dashboard: wrong password from {from}");
        Err(ConnectError::permission_denied(WRONG_CURRENT))
    }

    /// Set or change the password. Ends every session.
    pub async fn set_password(
        &self,
        from: IpAddr,
        new: String,
        current: String,
        grant: String,
    ) -> Result<(), ConnectError> {
        if !(credentials::MIN_CHARS..=credentials::MAX_CHARS).contains(&new.chars().count()) {
            return Err(ConnectError::invalid_argument(LENGTH));
        }
        let seen = self.credentials().hash.clone();
        let _change = self.change.lock().await;
        self.outdated(from, &seen, &current)?;
        let set = credentials::has_password(&self.credentials());
        self.allow(from, set, current, grant).await?;
        let hashed = tokio::task::spawn_blocking(move || {
            let mut file = DashboardFile::default();
            credentials::set_password(&mut file, &new, credentials::ITERATIONS)?;
            Ok::<_, ring::error::Unspecified>(file)
        })
        .await
        .ok()
        .and_then(Result::ok)
        .ok_or_else(|| ConnectError::internal(NO_RANDOM))?;
        let key = credentials::new_session_key().map_err(|_| ConnectError::internal(NO_RANDOM))?;
        self.save(|file| {
            file.hash = hashed.hash;
            file.salt = hashed.salt;
            file.iterations = hashed.iterations;
            file.session_key = key;
            file.left_open = false;
        })
        .await
    }

    /// Remove the password. Ends every session. The owner chose an open
    /// dashboard, so it isn't offered again.
    pub async fn remove_password(
        &self,
        from: IpAddr,
        current: String,
        grant: String,
    ) -> Result<(), ConnectError> {
        let seen = self.credentials().hash.clone();
        let _change = self.change.lock().await;
        self.outdated(from, &seen, &current)?;
        if !credentials::has_password(&self.credentials()) {
            return Err(ConnectError::failed_precondition(NO_PASSWORD));
        }
        self.allow(from, true, current, grant).await?;
        let key = credentials::new_session_key().map_err(|_| ConnectError::internal(NO_RANDOM))?;
        self.save(|file| {
            file.hash.clear();
            file.salt.clear();
            file.iterations = 0;
            file.session_key = key;
            file.left_open = true;
        })
        .await
    }

    pub async fn leave_open(&self) -> Result<(), ConnectError> {
        let _change = self.change.lock().await;
        if credentials::has_password(&self.credentials()) {
            return Err(ConnectError::failed_precondition(HAS_PASSWORD));
        }
        self.save(|file| file.left_open = true).await
    }

    /// A new token of `kind`, shown once. The old one stops working.
    pub async fn create_token(&self, kind: TokenKind) -> Result<String, ConnectError> {
        let _change = self.change.lock().await;
        let mut made = DashboardFile::default();
        let token = credentials::new_token(&mut made, kind)
            .map_err(|_| ConnectError::internal(NO_RANDOM))?;
        self.save(|file| match kind {
            TokenKind::Metrics => {
                file.metrics_sha256 = made.metrics_sha256;
                file.metrics_last4 = made.metrics_last4;
            }
            TokenKind::Agent => {
                file.agent_sha256 = made.agent_sha256;
                file.agent_last4 = made.agent_last4;
            }
        })
        .await?;
        Ok(token)
    }

    /// A new session key: every session ends, and so do the streams they hold.
    pub async fn sign_out_everywhere(&self) -> Result<(), ConnectError> {
        let _change = self.change.lock().await;
        let key = credentials::new_session_key().map_err(|_| ConnectError::internal(NO_RANDOM))?;
        self.save(|file| file.session_key = key).await
    }

    /// Ask for someone at the satellite. The id and the seconds the ring waits.
    pub fn start_confirmation(self: &Arc<Self>) -> Result<(String, u32), ConnectError> {
        let now = Instant::now();
        let mut confirmations = self.confirmations.lock().unwrap();
        let wait = confirmations.cooldown(now);
        if wait > 0 {
            let waiting = confirmations
                .current
                .as_ref()
                .is_some_and(|c| c.ended.is_none());
            return Err(ConnectError::resource_exhausted(if waiting {
                format!(
                    "Another confirmation waits for a press of Action at the satellite. Try again in {wait} s."
                )
            } else {
                format!("A confirmation ended just now. Try again in {wait} s.")
            }));
        }
        let mut id = [0u8; 16];
        SystemRandom::new()
            .fill(&mut id)
            .map_err(|_| ConnectError::internal(NO_RANDOM))?;
        let id = URL_SAFE_NO_PAD.encode(id);
        let (pressed, press) = oneshot::channel();
        self.inputs
            .try_send(Input::Presence(Presence {
                until: now + self.window,
                pressed,
            }))
            .map_err(|_| ConnectError::unavailable(NO_RING))?;
        let (progress, _) = watch::channel(Progress::Waiting);
        confirmations.current = Some(Current {
            id: id.clone(),
            started: now,
            ended: None,
            progress,
        });
        drop(confirmations);
        let auth = self.clone();
        let task_id = id.clone();
        tokio::spawn(async move {
            let pressed = matches!(tokio::time::timeout(auth.window, press).await, Ok(Ok(())));
            auth.finish_confirmation(&task_id, pressed);
        });
        Ok((id, self.window.as_secs() as u32))
    }

    fn finish_confirmation(&self, id: &str, pressed: bool) {
        let mut confirmations = self.confirmations.lock().unwrap();
        let Some(current) = confirmations.current.as_mut().filter(|c| c.id == id) else {
            return;
        };
        let now = Instant::now();
        current.ended = Some(now);
        if pressed {
            let mut grant = [0u8; 32];
            if SystemRandom::new().fill(&mut grant).is_err() {
                current.progress.send_replace(Progress::Expired);
                return;
            }
            let grant = URL_SAFE_NO_PAD.encode(grant);
            self.grants.lock().unwrap().add(grant.clone(), now);
            // A press at the satellite lifts the login limit too.
            self.limiter.lock().unwrap().reset();
            tracing::info!("dashboard: presence confirmed");
            current.progress.send_replace(Progress::Confirmed(grant));
        } else {
            tracing::info!("dashboard: confirmation expired");
            current.progress.send_replace(Progress::Expired);
        }
    }

    /// The progress of confirmation `id`, if it is the latest one.
    pub fn watch_confirmation(
        &self,
        id: &str,
    ) -> Result<watch::Receiver<ConfirmProgress>, ConnectError> {
        self.confirmations
            .lock()
            .unwrap()
            .current
            .as_ref()
            .filter(|c| c.id == id)
            .map(|c| c.progress.subscribe())
            .ok_or_else(|| ConnectError::not_found(UNKNOWN_CONFIRMATION))
    }
}

#[cfg(test)]
mod tests;
