//! Wi-Fi from the dashboard (ADR 0008), over wpa_supplicant's control socket:
//! scan, list and forget saved networks, and try one, kept only once the
//! satellite is reached on it.
//!
//! The passphrase goes to wpa_supplicant and nowhere else: it is never
//! logged, never put into an error, and never read back.
//!
//! It also tells the setup hotspot (ADR 0028) when it may run: never during a
//! join or a trial, and only while no network is saved.

use std::collections::HashMap;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use buffa::MessageField;
use connectrpc::ConnectError;
use pinna_api::{
    JoinFailure, JoinStage, JoinWifiRequest, JoinWifiResponse, ListWifiResponse, SavedWifi,
    WifiNetwork, WifiSecurity,
};
use tokio::net::UnixDatagram;
use tokio::sync::{OwnedMutexGuard, broadcast, watch};
use tokio::time::Instant;

/// How long a join waits for an address before it goes back.
const JOIN_TIMEOUT: Duration = Duration::from_secs(45);
/// How long a network stays on trial before the satellite goes back.
const KEEP_FOR: Duration = Duration::from_secs(300);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const SCAN_TIMEOUT: Duration = Duration::from_secs(10);
const POLL: Duration = Duration::from_millis(500);
/// `BSS` fields: id, bssid, freq, level, flags, ssid.
const BSS_MASK: &str = "0x1887";
/// Access points read from one scan, at most.
const BSS_MAX: usize = 256;

static NEXT_SOCKET: AtomicU32 = AtomicU32::new(0);

/// One client socket on wpa_supplicant's control interface.
pub struct Ctrl {
    sock: UnixDatagram,
    local: PathBuf,
    attached: bool,
}

impl Ctrl {
    /// Connect to `server` (`/run/wpa_supplicant/wlan0`), with our own socket
    /// in `dir` for the replies.
    pub fn open(server: &Path, dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        let local = dir.join(format!(
            "c-{}-{}",
            std::process::id(),
            NEXT_SOCKET.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_file(&local);
        let sock =
            UnixDatagram::bind(&local).with_context(|| format!("binding {}", local.display()))?;
        let ctrl = Self {
            sock,
            local,
            attached: false,
        };
        ctrl.sock
            .connect(server)
            .with_context(|| format!("connecting to {}", server.display()))?;
        Ok(ctrl)
    }

    /// Send a command and wait for its reply. Errors name only the command's
    /// first word: the rest may be a passphrase.
    pub async fn request(&self, cmd: &str) -> Result<String> {
        let verb = cmd.split_whitespace().next().unwrap_or("");
        self.sock
            .send(cmd.as_bytes())
            .await
            .with_context(|| format!("sending {verb} to wpa_supplicant"))?;
        let mut buf = vec![0u8; 16 * 1024];
        let deadline = Instant::now() + REQUEST_TIMEOUT;
        loop {
            let n = tokio::time::timeout_at(deadline, self.sock.recv(&mut buf))
                .await
                .with_context(|| format!("wpa_supplicant didn't answer {verb}"))??;
            let reply = String::from_utf8_lossy(&buf[..n]);
            // An event on an attached socket, not the reply.
            if !reply.starts_with('<') {
                return Ok(reply.into_owned());
            }
        }
    }

    /// A command whose reply must be `OK`.
    pub async fn ok(&self, cmd: &str) -> Result<()> {
        let reply = self.request(cmd).await?;
        if reply.trim() != "OK" {
            let verb = cmd.split_whitespace().next().unwrap_or("");
            bail!("wpa_supplicant refused {verb}: {}", reply.trim());
        }
        Ok(())
    }

    /// Receive events (`CTRL-EVENT-...`) from here on.
    pub async fn attach(&mut self) -> Result<()> {
        self.ok("ATTACH").await?;
        self.attached = true;
        Ok(())
    }

    /// The next event, without its `<level>` prefix; `None` after `wait`.
    pub async fn event(&self, wait: Duration) -> Option<String> {
        let mut buf = vec![0u8; 4096];
        let deadline = Instant::now() + wait;
        loop {
            let n = tokio::time::timeout_at(deadline, self.sock.recv(&mut buf))
                .await
                .ok()?
                .ok()?;
            let msg = String::from_utf8_lossy(&buf[..n]);
            if let Some(rest) = msg.strip_prefix('<')
                && let Some((_, ev)) = rest.split_once('>')
            {
                return Some(ev.to_owned());
            }
        }
    }
}

impl Drop for Ctrl {
    fn drop(&mut self) {
        if self.attached {
            let _ = self.sock.try_send(b"DETACH");
        }
        let _ = std::fs::remove_file(&self.local);
    }
}

/// Undo wpa_supplicant's escaping of an SSID (`printf_encode`): `\\`, `\"`,
/// `\e`, `\n`, `\r`, `\t` and `\xHH`.
pub fn unescape(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 1 < b.len() {
            i += 1;
            match b[i] {
                b'n' => out.push(b'\n'),
                b'r' => out.push(b'\r'),
                b't' => out.push(b'\t'),
                b'e' => out.push(0x1b),
                b'x' if i + 2 < b.len() => match u8::from_str_radix(&s[i + 1..i + 3], 16) {
                    Ok(v) => {
                        out.push(v);
                        i += 2;
                    }
                    Err(_) => out.extend_from_slice(b"\\x"),
                },
                c => out.push(c),
            }
        } else {
            out.push(b[i]);
        }
        i += 1;
    }
    out
}

/// An SSID for people; a hidden network's (empty or all zero bytes) is `None`.
pub fn ssid_text(escaped: &str) -> Option<String> {
    let bytes = unescape(escaped);
    if bytes.iter().all(|&b| b == 0) {
        return None;
    }
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Whether scan flags offer plain WPA/WPA2-Personal, which this Wi-Fi chip
/// joins: `PSK`, but not `PSK-SHA256` (it needs management frame protection).
fn plain_psk(flags: &str) -> bool {
    flags
        .match_indices("PSK")
        .any(|(i, _)| !flags[i + 3..].starts_with("-SHA256") && !flags[..i].ends_with("FT/"))
}

/// The security a scan's flags (`[WPA2-PSK-CCMP][ESS]`) offer, as Pinna joins
/// it: WPA2 where an access point also offers WPA3. A network that offers only
/// WPA3 or PSK-SHA256 is `SAE`: its driver can do neither (ADR 0008).
pub fn security_from_flags(flags: &str) -> WifiSecurity {
    if plain_psk(flags) {
        WifiSecurity::WIFI_SECURITY_WPA_PSK
    } else if flags.contains("EAP") {
        WifiSecurity::WIFI_SECURITY_ENTERPRISE
    } else if flags.contains("SAE") || flags.contains("PSK") || flags.contains("OWE") {
        WifiSecurity::WIFI_SECURITY_SAE
    } else if flags.contains("WEP") {
        WifiSecurity::WIFI_SECURITY_WEP
    } else {
        WifiSecurity::WIFI_SECURITY_OPEN
    }
}

/// A saved network's security, from its `key_mgmt`.
pub fn security_from_key_mgmt(key_mgmt: &str) -> WifiSecurity {
    if key_mgmt.contains("WPA-PSK") {
        WifiSecurity::WIFI_SECURITY_WPA_PSK
    } else if key_mgmt.contains("SAE") {
        WifiSecurity::WIFI_SECURITY_SAE
    } else if key_mgmt.contains("EAP") || key_mgmt.contains("IEEE8021X") {
        WifiSecurity::WIFI_SECURITY_ENTERPRISE
    } else if key_mgmt.trim() == "NONE" {
        WifiSecurity::WIFI_SECURITY_OPEN
    } else {
        WifiSecurity::WIFI_SECURITY_UNSPECIFIED
    }
}

/// One access point from `BSS ... MASK=0x1887` (`id=`, `freq=`, `level=`,
/// `flags=`, `ssid=` lines); `None` past the last one.
pub fn parse_bss(text: &str) -> Option<(u32, Option<WifiNetwork>)> {
    let f = parse_status(text);
    let id = f.get("id")?.parse().ok()?;
    let net = (|| {
        Some(WifiNetwork {
            ssid: ssid_text(f.get("ssid")?)?,
            signal_dbm: f.get("level")?.parse().ok()?,
            frequency_mhz: f.get("freq")?.parse().ok()?,
            security: security_from_flags(f.get("flags").map_or("", String::as_str)).into(),
            ..Default::default()
        })
    })();
    Some((id, net))
}

/// One network per name, the strongest access point; strongest first.
pub fn strongest(aps: impl IntoIterator<Item = WifiNetwork>) -> Vec<WifiNetwork> {
    let mut best: HashMap<String, WifiNetwork> = HashMap::new();
    for ap in aps {
        match best.get(&ap.ssid) {
            Some(b) if b.signal_dbm >= ap.signal_dbm => {}
            _ => {
                best.insert(ap.ssid.clone(), ap);
            }
        }
    }
    let mut nets: Vec<_> = best.into_values().collect();
    nets.sort_by(|a, b| b.signal_dbm.cmp(&a.signal_dbm).then(a.ssid.cmp(&b.ssid)));
    nets
}

/// A row of `LIST_NETWORKS`.
#[derive(Debug, Clone, PartialEq)]
pub struct Listed {
    pub id: u32,
    pub ssid: String,
    pub current: bool,
}

pub fn parse_list_networks(text: &str) -> Vec<Listed> {
    text.lines()
        .skip(1)
        .filter_map(|line| {
            let f: Vec<&str> = line.split('\t').collect();
            let id = f.first()?.parse().ok()?;
            Some(Listed {
                id,
                ssid: String::from_utf8_lossy(&unescape(f.get(1)?)).into_owned(),
                current: f.get(3).is_some_and(|fl| fl.contains("[CURRENT]")),
            })
        })
        .collect()
}

/// `STATUS`'s (and `BSS`'s) `key=value` lines.
pub fn parse_status(text: &str) -> HashMap<String, String> {
    text.lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect()
}

/// A join request that can be sent: what `check_join` lets through.
#[derive(Debug, Clone)]
pub struct Join {
    pub ssid: String,
    pub security: WifiSecurity,
    pub passphrase: String,
    pub hidden: bool,
}

/// Check a join request; the error is a sentence for the dashboard.
pub fn check_join(req: &JoinWifiRequest) -> Result<Join, String> {
    let ssid = req.ssid.as_str();
    if ssid.trim().is_empty() {
        return Err("Enter the network's name.".into());
    }
    if ssid.len() > 32 {
        return Err("A network name has at most 32 bytes.".into());
    }
    let security = req.security.as_known().unwrap_or_default();
    let pass = req.passphrase.as_str();
    match security {
        WifiSecurity::WIFI_SECURITY_OPEN => {}
        WifiSecurity::WIFI_SECURITY_WPA_PSK => {
            let raw = pass.len() == 64 && pass.bytes().all(|b| b.is_ascii_hexdigit());
            let printable = pass.bytes().all(|b| (0x20..0x7f).contains(&b));
            let passphrase = printable && (8..=63).contains(&pass.len());
            if !(raw || passphrase) {
                return Err(
                    "A WPA password has 8 to 63 characters (letters, digits and symbols, \
                     no accents), or is 64 hexadecimal digits."
                        .into(),
                );
            }
        }
        WifiSecurity::WIFI_SECURITY_SAE => {
            return Err("This satellite's Wi-Fi can't join WPA3-only networks. \
                 Set the network to WPA2/WPA3, if your router offers it."
                .into());
        }
        WifiSecurity::WIFI_SECURITY_ENTERPRISE => {
            return Err("Enterprise (802.1X) networks aren't supported.".into());
        }
        WifiSecurity::WIFI_SECURITY_WEP => {
            return Err("WEP networks aren't supported: WEP isn't safe.".into());
        }
        _ => return Err("Choose the network's security.".into()),
    }
    Ok(Join {
        ssid: ssid.to_owned(),
        security,
        passphrase: if security == WifiSecurity::WIFI_SECURITY_OPEN {
            String::new()
        } else {
            pass.to_owned()
        },
        hidden: req.hidden,
    })
}

/// The `SET_NETWORK` commands for a join, in order. They carry the passphrase:
/// never log them.
fn network_commands(id: u32, j: &Join) -> Vec<String> {
    let mut cmds = vec![format!("SET_NETWORK {id} ssid {}", hex(j.ssid.as_bytes()))];
    if j.security == WifiSecurity::WIFI_SECURITY_WPA_PSK {
        cmds.push(format!("SET_NETWORK {id} key_mgmt WPA-PSK"));
        if j.passphrase.len() == 64 && j.passphrase.bytes().all(|b| b.is_ascii_hexdigit()) {
            cmds.push(format!("SET_NETWORK {id} psk {}", j.passphrase));
        } else {
            cmds.push(format!("SET_NETWORK {id} psk \"{}\"", j.passphrase));
        }
    } else {
        cmds.push(format!("SET_NETWORK {id} key_mgmt NONE"));
    }
    if j.hidden {
        cmds.push(format!("SET_NETWORK {id} scan_ssid 1"));
    }
    cmds
}

/// Where wpa_supplicant and our client sockets are; tests use temp dirs.
#[derive(Debug, Clone)]
pub struct WifiPaths {
    /// The network interface.
    pub iface: String,
    /// wpa_supplicant's `ctrl_interface` directory.
    pub ctrl_dir: PathBuf,
    /// Where our reply sockets go.
    pub client_dir: PathBuf,
    /// The interface's sysfs entry: no entry, no Wi-Fi.
    pub sysfs: PathBuf,
    /// Present while a network is on trial: pinnad goes back at start when it
    /// finds it.
    pub trial_mark: PathBuf,
    /// The board has Wi-Fi hardware (an Echo): no wlan0 means it didn't start.
    pub has_radio: bool,
}

impl Default for WifiPaths {
    fn default() -> Self {
        Self {
            iface: "wlan0".into(),
            ctrl_dir: "/run/wpa_supplicant".into(),
            client_dir: "/run/pinna/wpa".into(),
            sysfs: "/sys/class/net/wlan0".into(),
            trial_mark: "/run/pinna/wifi-trial".into(),
            has_radio: !pinna_hal::ident::board_model().is_empty(),
        }
    }
}

/// A network on trial: in wpa_supplicant's memory, selected, not saved.
struct Trial {
    /// Its TRIAL step, with the address and the fallback.
    step: JoinWifiResponse,
    id: u32,
    /// Saved entries with the same name, replaced when it is kept.
    same_name: Vec<u32>,
    /// The priority it gets when kept: first in line.
    priority: i32,
    until: Instant,
    /// Which trial this is, so a late timer leaves a newer one alone.
    number: u64,
}

struct State {
    joining: bool,
    trial: Option<Trial>,
    trials: u64,
    last: Option<JoinWifiResponse>,
    events: broadcast::Sender<JoinWifiResponse>,
}

/// Runs before a join tries its network: the setup hotspot frees the radio.
pub type BeforeJoin =
    Arc<dyn Fn() -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> + Send + Sync>;

struct Shared {
    paths: WifiPaths,
    /// One wpa_supplicant operation at a time: a scan, a join, a forget, a keep.
    op: Arc<tokio::sync::Mutex<()>>,
    state: Mutex<State>,
    /// Bumped when a join starts or ends, a trial ends or a network is
    /// forgotten: whether the hotspot may run can have changed.
    changed: watch::Sender<u64>,
    before_join: std::sync::OnceLock<BeforeJoin>,
    /// The setup hotspot runs: a join warns first, and a scan that finds
    /// nothing falls back to the one from before it started.
    hotspot_on: std::sync::atomic::AtomicBool,
    /// The last scan that found networks.
    remembered: Mutex<Vec<WifiNetwork>>,
}

fn lock(state: &Mutex<State>) -> MutexGuard<'_, State> {
    state.lock().unwrap_or_else(|e| e.into_inner())
}

fn lock_vec(v: &Mutex<Vec<WifiNetwork>>) -> MutexGuard<'_, Vec<WifiNetwork>> {
    v.lock().unwrap_or_else(|e| e.into_inner())
}

/// How long a join waits after saying the hotspot goes off, so that the step
/// reaches the page on the hotspot before it does.
const HOTSPOT_WARNING: Duration = Duration::from_secs(1);

/// The addresses an interface has now (`network::addresses`; tests fake it).
type Addresses = Arc<
    dyn Fn(String) -> std::pin::Pin<Box<dyn std::future::Future<Output = Vec<IpAddr>> + Send>>
        + Send
        + Sync,
>;

pub struct Wifi {
    shared: Arc<Shared>,
    timeout: Duration,
    keep_for: Duration,
    addresses: Addresses,
    /// A look at the interface's addresses runs: one at a time.
    looking: Arc<std::sync::atomic::AtomicBool>,
}

/// The last step a JoinWifi stream sends.
pub fn done(step: &JoinWifiResponse) -> bool {
    matches!(
        step.stage.as_known(),
        Some(
            JoinStage::JOIN_STAGE_TRIAL
                | JoinStage::JOIN_STAGE_JOINED
                | JoinStage::JOIN_STAGE_FAILED
        )
    )
}

fn step(ssid: &str, stage: JoinStage, message: String) -> JoinWifiResponse {
    JoinWifiResponse {
        ssid: ssid.to_owned(),
        stage: stage.into(),
        message,
        ..Default::default()
    }
}

fn failed(ssid: &str, why: JoinFailure, message: String, fallback: &str) -> JoinWifiResponse {
    JoinWifiResponse {
        failure: why.into(),
        fallback: fallback.to_owned(),
        ..step(ssid, JoinStage::JOIN_STAGE_FAILED, message)
    }
}

fn unavailable(what: &str, e: anyhow::Error) -> ConnectError {
    tracing::warn!("Wi-Fi {what}: {e:#}");
    ConnectError::unavailable(format!("{}: {e}", ucfirst(what)))
}

fn ucfirst(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map(|f| f.to_uppercase().chain(c).collect())
        .unwrap_or_default()
}

impl Shared {
    fn ctrl(&self) -> Result<Ctrl, ConnectError> {
        if !self.paths.sysfs.exists() {
            return Err(ConnectError::failed_precondition(if self.paths.has_radio {
                "The Wi-Fi didn't start this time. Restart the satellite to try again."
            } else {
                "This satellite has no Wi-Fi."
            }));
        }
        let server = self.paths.ctrl_dir.join(&self.paths.iface);
        Ctrl::open(&server, &self.paths.client_dir).map_err(|e| {
            tracing::warn!("Wi-Fi: {e:#}");
            ConnectError::unavailable(
                "The Wi-Fi client isn't running. Restart the satellite, or check its log.",
            )
        })
    }

    /// The operation lock, or a sentence saying what holds it.
    fn begin(&self) -> Result<OwnedMutexGuard<()>, ConnectError> {
        self.op.clone().try_lock_owned().map_err(|_| {
            ConnectError::failed_precondition(if lock(&self.state).joining {
                "The satellite is joining a network. Wait until it has finished."
            } else {
                "The satellite is busy with its Wi-Fi. Try again in a moment."
            })
        })
    }

    fn report(&self, step: JoinWifiResponse) {
        let end = done(&step);
        {
            let mut s = lock(&self.state);
            if matches!(
                step.stage.as_known(),
                Some(JoinStage::JOIN_STAGE_JOINED | JoinStage::JOIN_STAGE_FAILED)
            ) {
                s.last = Some(step.clone());
            }
            let _ = s.events.send(step);
        }
        if end {
            self.bump();
        }
    }

    fn bump(&self) {
        self.changed.send_modify(|n| *n = n.wrapping_add(1));
    }

    fn mark(&self, on: bool) {
        let m = &self.paths.trial_mark;
        let result = if on {
            m.parent()
                .map_or(Ok(()), std::fs::create_dir_all)
                .and_then(|()| std::fs::write(m, b""))
        } else {
            match std::fs::remove_file(m) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                r => r,
            }
        };
        if let Err(e) = result {
            tracing::warn!("Wi-Fi: {}: {e}", m.display());
        }
    }

    /// Back to the saved networks: wpa_supplicant rereads its file, which a
    /// trial never touches.
    async fn go_back(&self, ctrl: &Ctrl) -> Result<()> {
        let r = ctrl.ok("RECONFIGURE").await;
        if r.is_ok() {
            self.mark(false);
        }
        r
    }

    /// End the trial `number`, if it is still on: keep it, or go back.
    async fn end_trial(
        &self,
        keep: bool,
        number: Option<u64>,
    ) -> Result<JoinWifiResponse, ConnectError> {
        let _op = self.op.clone().lock_owned().await;
        let trial = {
            let mut s = lock(&self.state);
            match (&s.trial, number) {
                (Some(t), Some(n)) if t.number != n => None,
                (Some(_), _) => s.trial.take(),
                (None, _) => None,
            }
        };
        let Some(t) = trial else {
            return Err(ConnectError::failed_precondition(
                "The trial has already ended: the satellite kept the network or went back.",
            ));
        };
        let ctrl = self.ctrl()?;
        let ssid = t.step.ssid.clone();
        if !keep {
            self.go_back(&ctrl)
                .await
                .map_err(|e| unavailable("going back", e))?;
            let why = if number.is_some() {
                // The address, for whoever looks again: a .local name that
                // didn't open (Android) is the usual reason.
                let at = if t.step.address.is_empty() {
                    String::new()
                } else {
                    format!(" at http://{}/", t.step.address)
                };
                format!(
                    "Nobody opened the dashboard{at} on {ssid} in time, so the satellite went back."
                )
            } else {
                format!("Went back from {ssid}.")
            };
            let back = if t.step.fallback.is_empty() {
                String::new()
            } else {
                format!(" It is on {} again.", t.step.fallback)
            };
            let end = failed(
                &ssid,
                JoinFailure::JOIN_FAILURE_NOT_KEPT,
                format!("{why}{back}"),
                &t.step.fallback,
            );
            tracing::info!("Wi-Fi: {}", end.message);
            self.report(end.clone());
            return Ok(end);
        }
        // wpa_supplicant renumbers its networks when it rereads its file (a
        // restart, a RECONFIGURE): only keep what is still the trial.
        let listed = parse_list_networks(
            &ctrl
                .request("LIST_NETWORKS")
                .await
                .map_err(|e| unavailable("keeping the network", e))?,
        );
        if !listed.iter().any(|l| l.id == t.id && l.ssid == ssid) {
            self.mark(false);
            let end = failed(
                &ssid,
                JoinFailure::JOIN_FAILURE_OTHER,
                format!("{ssid} is no longer on trial: the Wi-Fi client restarted. Join it again."),
                &t.step.fallback,
            );
            self.report(end.clone());
            return Ok(end);
        }
        let saved = async {
            ctrl.ok(&format!("SET_NETWORK {} priority {}", t.id, t.priority))
                .await?;
            for id in &t.same_name {
                ctrl.ok(&format!("REMOVE_NETWORK {id}")).await?;
            }
            // SELECT_NETWORK disabled the others: enable them before saving.
            ctrl.ok("ENABLE_NETWORK all").await?;
            ctrl.ok("SAVE_CONFIG").await
        }
        .await;
        self.mark(false);
        let end = match saved {
            Ok(()) => JoinWifiResponse {
                address: t.step.address.clone(),
                ..step(
                    &ssid,
                    JoinStage::JOIN_STAGE_JOINED,
                    format!("Kept {ssid}: the satellite joins it from now on."),
                )
            },
            Err(e) => {
                tracing::warn!("Wi-Fi keep: {e:#}");
                JoinWifiResponse {
                    address: t.step.address.clone(),
                    ..step(
                        &ssid,
                        JoinStage::JOIN_STAGE_JOINED,
                        format!(
                            "On {ssid}, but the satellite couldn't save it ({e}). \
                             It goes back to the saved networks when it restarts."
                        ),
                    )
                }
            }
        };
        tracing::info!("Wi-Fi: {}", end.message);
        self.report(end.clone());
        Ok(end)
    }
}

impl Wifi {
    pub fn new(paths: WifiPaths) -> Self {
        Self::with_timeouts(paths, JOIN_TIMEOUT, KEEP_FOR)
    }

    /// `timeout`: how long a join waits for an address; `keep_for`: how long
    /// a trial lasts. Tests shorten them.
    pub fn with_timeouts(paths: WifiPaths, timeout: Duration, keep_for: Duration) -> Self {
        Self {
            shared: Arc::new(Shared {
                paths,
                op: Arc::new(tokio::sync::Mutex::new(())),
                state: Mutex::new(State {
                    joining: false,
                    trial: None,
                    trials: 0,
                    last: None,
                    events: broadcast::channel(32).0,
                }),
                changed: watch::channel(0).0,
                before_join: std::sync::OnceLock::new(),
                hotspot_on: std::sync::atomic::AtomicBool::new(false),
                remembered: Mutex::new(Vec::new()),
            }),
            timeout,
            keep_for,
            addresses: Arc::new(|iface| {
                Box::pin(async move { crate::network::addresses(&iface).await })
            }),
            looking: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    /// Take the interface's addresses from `f` (tests).
    #[cfg(test)]
    pub fn with_addresses(mut self, f: Addresses) -> Self {
        self.addresses = f;
        self
    }

    /// At start: a trial mark left by a pinnad that stopped mid-join means
    /// wpa_supplicant may still hold an unsaved network; go back.
    pub async fn recover(&self) {
        let s = &self.shared;
        if !s.paths.trial_mark.exists() {
            return;
        }
        tracing::info!("Wi-Fi: a join was cut short; going back to the saved networks");
        match s.ctrl() {
            Ok(ctrl) => {
                if let Err(e) = s.go_back(&ctrl).await {
                    tracing::warn!("Wi-Fi: going back: {e:#}");
                }
            }
            Err(e) => tracing::warn!("Wi-Fi: going back: {e}"),
        }
        s.bump();
    }

    /// Changes after which the setup hotspot looks again (ADR 0028).
    pub fn changes(&self) -> watch::Receiver<u64> {
        self.shared.changed.subscribe()
    }

    /// Run `hook` before every join tries its network. Set once.
    pub fn set_before_join(&self, hook: BeforeJoin) {
        let _ = self.shared.before_join.set(hook);
    }

    /// The setup hotspot started or stopped.
    pub fn set_hotspot_on(&self, on: bool) {
        self.shared.hotspot_on.store(on, Ordering::SeqCst);
    }

    /// Whether the setup hotspot may run (ADR 0028): no join runs, no network
    /// is on trial and wpa_supplicant has none saved. `None` when that can't
    /// be told now (wpa_supplicant isn't up, or another operation runs).
    pub async fn wants_hotspot(&self) -> Option<bool> {
        let s = &self.shared;
        let busy = |s: &Shared| {
            let st = lock(&s.state);
            st.joining || st.trial.is_some()
        };
        if busy(s) {
            return Some(false);
        }
        let _op = s.op.clone().try_lock_owned().ok()?;
        // A join that started before the lock was taken has finished.
        if busy(s) {
            return Some(false);
        }
        let ctrl = s.ctrl().ok()?;
        let listed = ctrl.request("LIST_NETWORKS").await.ok()?;
        Some(parse_list_networks(&listed).is_empty())
    }

    /// A request reached the dashboard at `local`: if that is the trial's
    /// address, the new network works for someone. Keep it.
    pub fn reached(&self, local: IpAddr) {
        let local = local.to_canonical();
        let (number, v4) = {
            let s = lock(&self.shared.state);
            match &s.trial {
                Some(t) => (t.number, t.step.address.parse::<IpAddr>().ok()),
                None => return,
            }
        };
        let keep = move |shared: Arc<Shared>| async move {
            tracing::info!("Wi-Fi: the dashboard was opened on the new network");
            let _ = shared.end_trial(true, Some(number)).await;
        };
        if v4 == Some(local) {
            tokio::spawn(keep(self.shared.clone()));
            return;
        }
        // mDNS announces the .local name's IPv6 addresses too: a request to any
        // address of the Wi-Fi interface came over the new network.
        let ordering = std::sync::atomic::Ordering::SeqCst;
        if !local.is_ipv6() || local.is_loopback() || self.looking.swap(true, ordering) {
            return;
        }
        let (addresses, iface) = (self.addresses.clone(), self.shared.paths.iface.clone());
        let (shared, looking) = (self.shared.clone(), self.looking.clone());
        tokio::spawn(async move {
            let on_wifi = addresses(iface).await.contains(&local);
            looking.store(false, ordering);
            if on_wifi {
                keep(shared).await;
            }
        });
    }

    /// Networks in range, from a fresh scan. While the setup hotspot runs, a
    /// scan that fails or finds nothing gives the last one that found
    /// networks: the chip may not scan as an access point (ADR 0028), and
    /// pinnad scans before it starts one.
    pub async fn scan(&self) -> Result<Vec<WifiNetwork>, ConnectError> {
        let s = &self.shared;
        let fresh = self.fresh_scan().await;
        match &fresh {
            Ok(nets) if !nets.is_empty() => *lock_vec(&s.remembered) = nets.clone(),
            _ if s.hotspot_on.load(Ordering::SeqCst) => {
                let remembered = lock_vec(&s.remembered).clone();
                if !remembered.is_empty() {
                    tracing::info!(
                        "Wi-Fi: no scan beside the hotspot; listing the one from before it"
                    );
                    return Ok(remembered);
                }
            }
            _ => {}
        }
        fresh
    }

    async fn fresh_scan(&self) -> Result<Vec<WifiNetwork>, ConnectError> {
        let s = &self.shared;
        let _op = s.begin()?;
        let ctrl = s.ctrl()?;
        let mut events = s.ctrl()?;
        let result = async {
            events.attach().await?;
            let reply = ctrl.request("SCAN").await?;
            // FAIL-BUSY: a scan already runs; its results do as well.
            if reply.trim() != "OK" && !reply.contains("BUSY") {
                bail!("wpa_supplicant refused SCAN: {}", reply.trim());
            }
            let deadline = Instant::now() + SCAN_TIMEOUT;
            while let Some(ev) = events
                .event(deadline.saturating_duration_since(Instant::now()))
                .await
            {
                if ev.starts_with("CTRL-EVENT-SCAN-RESULTS") {
                    break;
                }
            }
            // One access point per reply: a single SCAN_RESULTS holds ~4 KB.
            let mut aps = Vec::new();
            let mut cmd = format!("BSS FIRST MASK={BSS_MASK}");
            for _ in 0..BSS_MAX {
                let Some((id, ap)) = parse_bss(&ctrl.request(&cmd).await?) else {
                    break;
                };
                aps.extend(ap);
                cmd = format!("BSS NEXT-{id} MASK={BSS_MASK}");
            }
            // Setup hotspots, its own and other satellites', aren't networks
            // to join (ADR 0028).
            aps.retain(|ap| !ap.ssid.starts_with(crate::hotspot::NAME_PREFIX));
            let mut nets = strongest(aps);
            let listed = parse_list_networks(&ctrl.request("LIST_NETWORKS").await?);
            for n in &mut nets {
                if let Some(l) = listed.iter().find(|l| l.ssid == n.ssid) {
                    n.saved = true;
                    n.current |= l.current;
                }
            }
            Ok(nets)
        }
        .await;
        result.map_err(|e| unavailable("the scan failed", e))
    }

    async fn saved_with(ctrl: &Ctrl) -> Result<Vec<SavedWifi>> {
        let mut saved = Vec::new();
        for l in parse_list_networks(&ctrl.request("LIST_NETWORKS").await?) {
            let get =
                async |field: &str| ctrl.request(&format!("GET_NETWORK {} {field}", l.id)).await;
            let key_mgmt = get("key_mgmt").await?;
            let priority = get("priority").await?.trim().parse::<i32>().unwrap_or(0);
            let hidden = get("scan_ssid").await?.trim() == "1";
            saved.push((
                priority,
                SavedWifi {
                    ssid: l.ssid,
                    security: security_from_key_mgmt(&key_mgmt).into(),
                    hidden,
                    current: l.current,
                    ..Default::default()
                },
            ));
        }
        // Highest priority first; wpa_supplicant's order among equals.
        saved.sort_by_key(|s| std::cmp::Reverse(s.0));
        Ok(saved.into_iter().map(|(_, s)| s).collect())
    }

    /// Saved networks, the trial, and how the last join ended.
    pub async fn list(&self) -> Result<ListWifiResponse, ConnectError> {
        let s = &self.shared;
        let (joining, last, trial) = {
            let st = lock(&s.state);
            let trial = st.trial.as_ref().map(|t| JoinWifiResponse {
                keep_seconds: t.until.saturating_duration_since(Instant::now()).as_secs() as u32,
                ..t.step.clone()
            });
            (st.joining, st.last.clone(), trial)
        };
        let ctrl = s.ctrl()?;
        let mut saved = Self::saved_with(&ctrl)
            .await
            .map_err(|e| unavailable("reading the saved networks failed", e))?;
        // The trial is in wpa_supplicant's memory, not saved.
        if let Some(t) = &trial {
            saved.retain(|n| !(n.current && n.ssid == t.ssid));
        }
        Ok(ListWifiResponse {
            saved,
            joining,
            last_join: last.map_or_else(MessageField::none, MessageField::some),
            trial: trial.map_or_else(MessageField::none, MessageField::some),
            ..Default::default()
        })
    }

    /// Forget every saved network with this name.
    pub async fn forget(&self, ssid: &str) -> Result<Vec<SavedWifi>, ConnectError> {
        let s = &self.shared;
        let _op = s.begin()?;
        if lock(&s.state).trial.is_some() {
            return Err(ConnectError::failed_precondition(
                "A network is on trial. Keep it or go back first.",
            ));
        }
        let ctrl = s.ctrl()?;
        let result = async {
            let ids: Vec<u32> = parse_list_networks(&ctrl.request("LIST_NETWORKS").await?)
                .into_iter()
                .filter(|l| l.ssid == ssid)
                .map(|l| l.id)
                .collect();
            if ids.is_empty() {
                return Ok(None);
            }
            for id in ids {
                ctrl.ok(&format!("REMOVE_NETWORK {id}")).await?;
            }
            ctrl.ok("ENABLE_NETWORK all").await?;
            ctrl.ok("SAVE_CONFIG").await?;
            tracing::info!("Wi-Fi: forgot {ssid:?}");
            Ok(Some(Self::saved_with(&ctrl).await?))
        }
        .await;
        drop(_op);
        s.bump();
        match result {
            Ok(Some(saved)) => Ok(saved),
            Ok(None) => Err(ConnectError::not_found(format!(
                "{ssid} isn't saved on this satellite."
            ))),
            Err(e) => Err(unavailable("forgetting failed", e)),
        }
    }

    /// Keep the network on trial, or go back now.
    pub async fn keep(&self, keep: bool) -> Result<JoinWifiResponse, ConnectError> {
        self.shared.end_trial(keep, None).await
    }

    /// Start trying a network; it runs to the end whoever watches. The
    /// receiver gets its steps, the last one TRIAL or FAILED.
    pub fn join(&self, j: Join) -> Result<broadcast::Receiver<JoinWifiResponse>, ConnectError> {
        let s = &self.shared;
        let op = s.begin()?;
        let (ctrl, mut events) = (s.ctrl()?, s.ctrl()?);
        let live = {
            let mut st = lock(&s.state);
            if st.trial.is_some() {
                return Err(ConnectError::failed_precondition(
                    "A network is on trial. Keep it or go back first.",
                ));
            }
            st.joining = true;
            st.events.subscribe()
        };
        s.bump();
        tracing::info!("Wi-Fi: trying {:?} (hidden: {})", j.ssid, j.hidden);
        let (shared, timeout, keep_for) = (s.clone(), self.timeout, self.keep_for);
        tokio::spawn(async move {
            // The setup hotspot holds the radio until it is stopped. A page on
            // the hotspot hears that first, so it knows it lost the satellite
            // and not that the join was refused.
            if shared.hotspot_on.load(Ordering::SeqCst) {
                let _ = lock(&shared.state).events.send(step(
                    &j.ssid,
                    JoinStage::JOIN_STAGE_CONNECTING,
                    format!("Turning the setup hotspot off to try {}.", j.ssid),
                ));
                tokio::time::sleep(HOTSPOT_WARNING).await;
            }
            if let Some(hook) = shared.before_join.get() {
                hook().await;
            }
            let outcome = match events.attach().await {
                Ok(()) => run_join(&shared, &ctrl, &events, &j, timeout).await,
                Err(e) => Err(e),
            };
            let end = match outcome {
                Ok(Tried::Trial(mut trial)) => {
                    let number = {
                        let mut st = lock(&shared.state);
                        st.trials += 1;
                        st.trials
                    };
                    trial.number = number;
                    trial.until = Instant::now() + keep_for;
                    trial.step.keep_seconds = keep_for.as_secs() as u32;
                    let step = trial.step.clone();
                    lock(&shared.state).trial = Some(trial);
                    // Nobody kept it in time: go back.
                    let timer = shared.clone();
                    tokio::spawn(async move {
                        tokio::time::sleep(keep_for).await;
                        let _ = timer.end_trial(false, Some(number)).await;
                    });
                    step
                }
                Ok(Tried::Failed(f)) => f,
                Err(e) => {
                    tracing::warn!("Wi-Fi join: {e:#}");
                    // Whatever happened, back to the saved networks.
                    if let Err(e) = shared.go_back(&ctrl).await {
                        tracing::warn!("Wi-Fi: going back: {e:#}");
                    }
                    failed(
                        &j.ssid,
                        JoinFailure::JOIN_FAILURE_OTHER,
                        format!("Joining failed: {e}"),
                        "",
                    )
                }
            };
            tracing::info!("Wi-Fi: {}", end.message);
            lock(&shared.state).joining = false;
            // Free first: the hotspot looks as soon as it hears of the end.
            drop(op);
            shared.report(end);
        });
        Ok(live)
    }
}

enum Tried {
    Trial(Trial),
    Failed(JoinWifiResponse),
}

/// Try `j` (ADR 0008): add it, select it, and wait up to `timeout` for an
/// IPv4 address. With one, it is on trial; without, back to the saved
/// networks with the reason.
async fn run_join(
    shared: &Shared,
    ctrl: &Ctrl,
    events: &Ctrl,
    j: &Join,
    timeout: Duration,
) -> Result<Tried> {
    let before = parse_status(&ctrl.request("STATUS").await?);
    let listed = parse_list_networks(&ctrl.request("LIST_NETWORKS").await?);
    let prev = listed.iter().find(|l| l.current).map(|l| l.ssid.clone());
    let mut top = 0;
    for l in &listed {
        let p = ctrl
            .request(&format!("GET_NETWORK {} priority", l.id))
            .await?;
        top = top.max(p.trim().parse::<i32>().unwrap_or(0));
    }
    // From here on, wpa_supplicant holds a network that isn't saved: going
    // back rereads the file, and a pinnad that stops finds the mark.
    shared.mark(true);
    let id: u32 = ctrl
        .request("ADD_NETWORK")
        .await?
        .trim()
        .parse()
        .context("wpa_supplicant didn't add the network")?;
    for cmd in network_commands(id, j) {
        ctrl.ok(&cmd).await?;
    }
    ctrl.ok(&format!("SELECT_NETWORK {id}")).await?;
    shared.report(step(
        &j.ssid,
        JoinStage::JOIN_STAGE_CONNECTING,
        format!("Connecting to {}…", j.ssid),
    ));

    // The address the satellite had may linger on wlan0 until it disconnects;
    // only an address seen after leaving the old network counts.
    let mut left = before.get("wpa_state").map(String::as_str) != Some("COMPLETED");
    let (mut handshake, mut connected, mut refused) = (false, false, false);
    let deadline = Instant::now() + timeout;
    let mut next_poll = Instant::now();
    let joined = loop {
        let now = Instant::now();
        if now >= deadline {
            break None;
        }
        if let Some(ev) = events.event(next_poll.saturating_duration_since(now)).await {
            let ours = ev.contains(&format!("id={id} "));
            if ours && ev.starts_with("CTRL-EVENT-SSID-TEMP-DISABLED") {
                let reason = ev.split("reason=").nth(1).unwrap_or("");
                if reason.starts_with("WRONG_KEY") || reason.starts_with("AUTH_FAILED") {
                    refused = true;
                    break None;
                }
            }
            continue;
        }
        next_poll = Instant::now() + POLL;
        let st = parse_status(&ctrl.request("STATUS").await?);
        let state = st.get("wpa_state").map(String::as_str).unwrap_or("");
        let ours = st.get("id").is_some_and(|v| v == &id.to_string());
        if state != "COMPLETED" || !ours {
            left = true;
        }
        // Reached the password check: a timeout from here means it failed.
        if matches!(state, "4WAY_HANDSHAKE" | "GROUP_HANDSHAKE") {
            handshake = true;
        }
        if state == "COMPLETED" && ours {
            if !connected {
                connected = true;
                shared.report(step(
                    &j.ssid,
                    JoinStage::JOIN_STAGE_ADDRESS,
                    format!("Connected to {}. Waiting for an address…", j.ssid),
                ));
            }
            if left && let Some(ip) = st.get("ip_address").filter(|ip| !ip.is_empty()) {
                break Some(ip.clone());
            }
        }
    };

    if let Some(ip) = joined {
        let fallback = prev.clone().filter(|p| p != &j.ssid).unwrap_or_default();
        return Ok(Tried::Trial(Trial {
            step: JoinWifiResponse {
                address: ip.clone(),
                fallback,
                ..step(
                    &j.ssid,
                    JoinStage::JOIN_STAGE_TRIAL,
                    format!(
                        "On {} with the address {ip}. Open the dashboard there, or keep it, \
                         to stay.",
                        j.ssid
                    ),
                )
            },
            id,
            same_name: listed
                .iter()
                .filter(|l| l.ssid == j.ssid)
                .map(|l| l.id)
                .collect(),
            priority: top + 1,
            until: Instant::now(),
            number: 0,
        }));
    }

    shared.go_back(ctrl).await?;
    let fallback = prev.unwrap_or_default();
    let (why, text) = if refused || (handshake && !connected) {
        (
            JoinFailure::JOIN_FAILURE_WRONG_PASSWORD,
            format!("{} refused the password.", j.ssid),
        )
    } else if connected {
        (
            JoinFailure::JOIN_FAILURE_NO_ADDRESS,
            format!(
                "Connected to {}, but no address came from its DHCP server.",
                j.ssid
            ),
        )
    } else if j.hidden {
        (
            JoinFailure::JOIN_FAILURE_NOT_FOUND,
            format!("No network called {} answered. Check the name.", j.ssid),
        )
    } else {
        (
            JoinFailure::JOIN_FAILURE_NOT_FOUND,
            format!(
                "{} didn't let the satellite connect. It may be out of range, or require \
                 protected management frames, which this satellite's Wi-Fi can't do.",
                j.ssid
            ),
        )
    };
    let back = if fallback.is_empty() {
        if shared.before_join.get().is_some() {
            " The satellite has no other network now: it runs its setup hotspot again.".to_owned()
        } else {
            " The satellite has no other network now: it is reachable over USB.".to_owned()
        }
    } else {
        format!(" Back on {fallback}.")
    };
    Ok(Tried::Failed(failed(
        &j.ssid,
        why,
        format!("{text}{back}"),
        &fallback,
    )))
}

#[cfg(test)]
pub mod fake {
    //! A scripted wpa_supplicant control socket. Like the real one,
    //! SELECT_NETWORK disables the other networks, SAVE_CONFIG writes what is
    //! in memory, and RECONFIGURE rereads what was saved.

    use super::*;

    /// What the fake does after `SELECT_NETWORK` of an added network.
    #[derive(Debug, Clone, Copy, PartialEq)]
    pub enum Outcome {
        Joins,
        WrongKey,
        NoAddress,
        NotFound,
    }

    #[derive(Debug, Clone, PartialEq)]
    pub struct Net {
        pub id: u32,
        pub ssid: String,
        pub priority: i32,
        pub key_mgmt: String,
        pub hidden: bool,
        pub disabled: bool,
    }

    #[derive(Default)]
    pub struct Wpa {
        pub nets: Vec<Net>,
        /// What SAVE_CONFIG wrote last: the file.
        pub saved: Vec<Net>,
        pub current: Option<u32>,
        selected: Option<(u32, Instant)>,
        pub log: Vec<String>,
        /// SCAN fails, like a chip that can't scan as an access point.
        pub refuse_scan: bool,
    }

    pub struct Fake {
        pub dir: PathBuf,
        pub wpa: Arc<Mutex<Wpa>>,
    }

    /// The access points in range: `BSS` walks them.
    const APS: [(&str, u32, i32, &str); 7] = [
        ("Home", 2412, -70, "[WPA2-PSK-CCMP][ESS]"),
        ("Home", 5180, -50, "[WPA2-PSK-CCMP][ESS]"),
        ("Caf\\xc3\\xa9", 2437, -60, "[ESS]"),
        ("Work", 2462, -40, "[WPA2-EAP-CCMP][ESS]"),
        ("\\x00\\x00", 2462, -45, "[WPA2-PSK-CCMP][ESS]"),
        ("Neighbour", 5200, -80, "[WPA2-SAE-CCMP][ESS]"),
        // Another satellite's setup hotspot: never listed.
        (
            "Pinna setup pinna-2a914e",
            2437,
            -35,
            "[WPA2-PSK-CCMP][ESS]",
        ),
    ];

    impl Fake {
        /// A fake with one saved, current network "Home" (id 0, priority 1).
        pub fn start(outcome: Outcome) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "pinnad-wpa-test-{}-{}",
                std::process::id(),
                NEXT_SOCKET.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(dir.join("ctrl")).unwrap();
            std::fs::create_dir_all(dir.join("sys")).unwrap();
            let sock = UnixDatagram::bind(dir.join("ctrl/wlan0")).unwrap();
            let home = Net {
                id: 0,
                ssid: "Home".into(),
                priority: 1,
                key_mgmt: "WPA-PSK".into(),
                hidden: false,
                disabled: false,
            };
            let wpa = Arc::new(Mutex::new(Wpa {
                nets: vec![home.clone()],
                saved: vec![home],
                current: Some(0),
                ..Default::default()
            }));
            let shared = wpa.clone();
            tokio::spawn(async move {
                let mut attached: Vec<PathBuf> = Vec::new();
                let mut buf = vec![0u8; 4096];
                loop {
                    let Ok((n, from)) = sock.recv_from(&mut buf).await else {
                        return;
                    };
                    let cmd = String::from_utf8_lossy(&buf[..n]).into_owned();
                    let to = from.as_pathname().map(Path::to_path_buf);
                    let mut events: Vec<String> = Vec::new();
                    let reply = {
                        let mut w = shared.lock().unwrap();
                        w.log.push(cmd.clone());
                        handle(
                            &mut w,
                            &cmd,
                            outcome,
                            &mut events,
                            &mut attached,
                            to.clone(),
                        )
                    };
                    if let Some(to) = to {
                        let _ = sock.send_to(reply.as_bytes(), &to).await;
                    }
                    for ev in events {
                        for a in &attached {
                            let _ = sock.send_to(ev.as_bytes(), a).await;
                        }
                    }
                }
            });
            Self { dir, wpa }
        }

        pub fn paths(&self) -> WifiPaths {
            WifiPaths {
                iface: "wlan0".into(),
                ctrl_dir: self.dir.join("ctrl"),
                client_dir: self.dir.join("client"),
                sysfs: self.dir.join("sys"),
                trial_mark: self.dir.join("trial"),
                has_radio: true,
            }
        }

        pub fn log(&self) -> Vec<String> {
            self.wpa.lock().unwrap().log.clone()
        }

        pub fn saved(&self) -> Vec<Net> {
            self.wpa.lock().unwrap().saved.clone()
        }
    }

    fn handle(
        w: &mut Wpa,
        cmd: &str,
        outcome: Outcome,
        events: &mut Vec<String>,
        attached: &mut Vec<PathBuf>,
        from: Option<PathBuf>,
    ) -> String {
        let now = Instant::now();
        // The selected network's fate, a moment after it was selected.
        let state = match w.selected {
            Some((id, at)) if now - at > Duration::from_millis(300) => match outcome {
                Outcome::Joins | Outcome::NoAddress => {
                    w.current = Some(id);
                    "COMPLETED"
                }
                Outcome::WrongKey => {
                    events.push(format!(
                        "<3>CTRL-EVENT-SSID-TEMP-DISABLED id={id} ssid=\"x\" auth_failures=1 \
                         duration=10 reason=WRONG_KEY"
                    ));
                    "DISCONNECTED"
                }
                Outcome::NotFound => "SCANNING",
            },
            Some(_) => "SCANNING",
            None if w.current.is_some() => "COMPLETED",
            None => "DISCONNECTED",
        };
        let mut words = cmd.split_whitespace();
        let verb = words.next().unwrap_or("");
        let (a, b) = (words.next(), words.next());
        let id = || a.and_then(|v| v.parse::<u32>().ok());
        match verb {
            "ATTACH" => {
                attached.extend(from);
                "OK\n".into()
            }
            "DETACH" | "ENABLE_NETWORK" if a == Some("all") || verb == "DETACH" => {
                if verb == "ENABLE_NETWORK" {
                    w.nets.iter_mut().for_each(|n| n.disabled = false);
                }
                "OK\n".into()
            }
            "SCAN" if w.refuse_scan => "FAIL\n".into(),
            "SCAN" => {
                events.push("<2>CTRL-EVENT-SCAN-RESULTS ".into());
                "OK\n".into()
            }
            "BSS" => {
                let i = match a {
                    Some("FIRST") => 0,
                    Some(next) => next
                        .strip_prefix("NEXT-")
                        .and_then(|v| v.parse::<usize>().ok())
                        .map_or(APS.len(), |v| v + 1),
                    None => APS.len(),
                };
                match APS.get(i) {
                    Some((ssid, freq, level, flags)) => format!(
                        "id={i}\nbssid=aa:00:00:00:00:0{i}\nfreq={freq}\nlevel={level}\n\
                         flags={flags}\nssid={ssid}\n"
                    ),
                    None => String::new(),
                }
            }
            "LIST_NETWORKS" => {
                let mut s = "network id / ssid / bssid / flags\n".to_owned();
                for n in &w.nets {
                    let mut flags = String::new();
                    if w.current == Some(n.id) {
                        flags += "[CURRENT]";
                    }
                    if n.disabled {
                        flags += "[DISABLED]";
                    }
                    s += &format!("{}\t{}\tany\t{flags}\n", n.id, n.ssid);
                }
                s
            }
            "STATUS" => {
                let mut s = format!("wpa_state={state}\n");
                if state == "COMPLETED"
                    && let Some(id) = w.current
                {
                    s += &format!("id={id}\n");
                    let trial = w.selected.is_some_and(|(sid, _)| sid == id);
                    if outcome != Outcome::NoAddress || !trial {
                        s += &format!("ip_address=192.168.{id}.20\n");
                    }
                }
                s
            }
            "ADD_NETWORK" => {
                let id = w.nets.iter().map(|n| n.id).max().map_or(0, |m| m + 1);
                w.nets.push(Net {
                    id,
                    ssid: String::new(),
                    priority: 0,
                    key_mgmt: "WPA-PSK".into(),
                    hidden: false,
                    disabled: false,
                });
                format!("{id}\n")
            }
            "SET_NETWORK" => {
                let value = cmd.splitn(4, ' ').nth(3).unwrap_or("").to_owned();
                let Some(n) = id().and_then(|id| w.nets.iter_mut().find(|n| n.id == id)) else {
                    return "FAIL\n".into();
                };
                match b.unwrap_or("") {
                    "ssid" => {
                        let bytes: Vec<u8> = (0..value.len())
                            .step_by(2)
                            .map(|i| u8::from_str_radix(&value[i..i + 2], 16).unwrap())
                            .collect();
                        n.ssid = String::from_utf8(bytes).unwrap();
                    }
                    "priority" => n.priority = value.parse().unwrap(),
                    "key_mgmt" => n.key_mgmt = value,
                    "scan_ssid" => n.hidden = value == "1",
                    _ => {}
                }
                "OK\n".into()
            }
            "GET_NETWORK" => match id().and_then(|id| w.nets.iter().find(|n| n.id == id)) {
                Some(n) => match b.unwrap_or("") {
                    "priority" => n.priority.to_string(),
                    "key_mgmt" => n.key_mgmt.clone(),
                    "scan_ssid" => if n.hidden { "1" } else { "0" }.to_owned(),
                    _ => "FAIL\n".into(),
                },
                None => "FAIL\n".into(),
            },
            "SELECT_NETWORK" => {
                let Some(id) = id() else {
                    return "FAIL\n".into();
                };
                for n in &mut w.nets {
                    n.disabled = n.id != id;
                }
                w.selected = Some((id, now));
                w.current = None;
                "OK\n".into()
            }
            "REMOVE_NETWORK" => {
                let Some(id) = id() else {
                    return "FAIL\n".into();
                };
                w.nets.retain(|n| n.id != id);
                if w.current == Some(id) {
                    w.current = None;
                }
                "OK\n".into()
            }
            "SAVE_CONFIG" => {
                w.saved = w.nets.clone();
                "OK\n".into()
            }
            "RECONFIGURE" => {
                w.nets = w.saved.clone();
                w.selected = None;
                w.current = w
                    .nets
                    .iter()
                    .filter(|n| !n.disabled)
                    .max_by_key(|n| n.priority)
                    .map(|n| n.id);
                "OK\n".into()
            }
            _ => "UNKNOWN COMMAND\n".into(),
        }
    }

    impl Drop for Fake {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::{Fake, Outcome};
    use super::*;

    #[test]
    fn unescapes_ssids() {
        assert_eq!(unescape(r#"Caf\xc3\xa9"#), "Café".as_bytes());
        assert_eq!(unescape(r#"a\\b\"c\td"#), b"a\\b\"c\td");
        assert_eq!(ssid_text(r"\x00\x00"), None);
        assert_eq!(ssid_text(""), None);
        assert_eq!(ssid_text("Home").as_deref(), Some("Home"));
    }

    #[test]
    fn security_follows_the_flags_and_the_chip() {
        use WifiSecurity::*;
        let s = security_from_flags;
        assert_eq!(s("[WPA2-PSK-CCMP][ESS]"), WIFI_SECURITY_WPA_PSK);
        // WPA2/WPA3 mixed: joined as WPA2.
        assert_eq!(s("[WPA2-PSK+SAE-CCMP][ESS]"), WIFI_SECURITY_WPA_PSK);
        assert_eq!(s("[WPA2-PSK+PSK-SHA256-CCMP][ESS]"), WIFI_SECURITY_WPA_PSK);
        // What the MT8163 driver can't do.
        assert_eq!(s("[WPA2-SAE-CCMP][ESS]"), WIFI_SECURITY_SAE);
        assert_eq!(s("[WPA2-PSK-SHA256-CCMP][ESS]"), WIFI_SECURITY_SAE);
        assert_eq!(s("[WPA2-EAP-CCMP][ESS]"), WIFI_SECURITY_ENTERPRISE);
        assert_eq!(s("[WEP][ESS]"), WIFI_SECURITY_WEP);
        assert_eq!(s("[ESS]"), WIFI_SECURITY_OPEN);
        assert_eq!(security_from_key_mgmt("NONE"), WIFI_SECURITY_OPEN);
        assert_eq!(
            security_from_key_mgmt("WPA-PSK WPA-EAP"),
            WIFI_SECURITY_WPA_PSK
        );
    }

    #[test]
    fn bss_entries_and_the_strongest_per_name() {
        let (id, ap) = parse_bss(
            "id=7\nbssid=aa\nfreq=5180\nlevel=-50\nflags=[WPA2-PSK-CCMP][ESS]\nssid=Caf\\xc3\\xa9\n",
        )
        .unwrap();
        assert_eq!(id, 7);
        let ap = ap.unwrap();
        assert_eq!(
            (ap.ssid.as_str(), ap.signal_dbm, ap.frequency_mhz),
            ("Café", -50, 5180)
        );
        // A hidden access point counts for the walk but isn't listed.
        assert!(
            parse_bss("id=8\nfreq=2412\nlevel=-40\nflags=[ESS]\nssid=\n")
                .unwrap()
                .1
                .is_none()
        );
        assert!(parse_bss("").is_none());
        let ap = |ssid: &str, signal_dbm| WifiNetwork {
            ssid: ssid.into(),
            signal_dbm,
            ..Default::default()
        };
        let nets = strongest([ap("Home", -70), ap("Home", -50), ap("Café", -60)]);
        assert_eq!(
            nets.iter()
                .map(|n| (n.ssid.as_str(), n.signal_dbm))
                .collect::<Vec<_>>(),
            [("Home", -50), ("Café", -60)]
        );
    }

    #[test]
    fn join_requests_are_checked() {
        let req = |ssid: &str, security: WifiSecurity, pass: &str| JoinWifiRequest {
            ssid: ssid.into(),
            security: security.into(),
            passphrase: pass.into(),
            ..Default::default()
        };
        use WifiSecurity::*;
        assert!(check_join(&req("Home", WIFI_SECURITY_WPA_PSK, "correct horse")).is_ok());
        assert!(check_join(&req("Home", WIFI_SECURITY_WPA_PSK, &"a".repeat(64))).is_ok());
        assert!(check_join(&req("Home", WIFI_SECURITY_WPA_PSK, "short")).is_err());
        assert!(check_join(&req("Home", WIFI_SECURITY_WPA_PSK, "pässwörter")).is_err());
        assert!(check_join(&req("Home", WIFI_SECURITY_SAE, "anything")).is_err());
        assert!(check_join(&req("", WIFI_SECURITY_OPEN, "")).is_err());
        assert!(check_join(&req(&"x".repeat(33), WIFI_SECURITY_OPEN, "")).is_err());
        assert!(check_join(&req("Work", WIFI_SECURITY_ENTERPRISE, "")).is_err());
        assert!(check_join(&req("Old", WIFI_SECURITY_WEP, "12345")).is_err());
        assert!(check_join(&req("Home", WIFI_SECURITY_UNSPECIFIED, "")).is_err());
        // An open network takes no passphrase, even if one came.
        assert_eq!(
            check_join(&req("Café", WIFI_SECURITY_OPEN, "ignored"))
                .unwrap()
                .passphrase,
            ""
        );
    }

    #[test]
    fn hidden_networks_probe_by_name_and_passwords_stay_quoted() {
        let j = Join {
            ssid: "Büro".into(),
            security: WifiSecurity::WIFI_SECURITY_WPA_PSK,
            passphrase: "a \"quoted\" one".into(),
            hidden: true,
        };
        let cmds = network_commands(3, &j);
        assert_eq!(cmds[0], "SET_NETWORK 3 ssid 42c3bc726f");
        assert!(cmds.contains(&"SET_NETWORK 3 psk \"a \"quoted\" one\"".to_owned()));
        assert!(cmds.contains(&"SET_NETWORK 3 scan_ssid 1".to_owned()));
    }

    fn wifi(fake: &Fake, keep_for: Duration) -> Wifi {
        Wifi::with_timeouts(fake.paths(), Duration::from_secs(3), keep_for)
    }

    /// Try Café and wait for the join's last step.
    async fn try_cafe(wifi: &Wifi) -> JoinWifiResponse {
        let j = Join {
            ssid: "Café".into(),
            security: WifiSecurity::WIFI_SECURITY_WPA_PSK,
            passphrase: "secret passphrase".into(),
            hidden: false,
        };
        let mut live = wifi.join(j).unwrap();
        loop {
            let s = tokio::time::timeout(Duration::from_secs(60), live.recv())
                .await
                .unwrap()
                .unwrap();
            if done(&s) {
                // The join task finishes right after its last step.
                tokio::time::sleep(Duration::from_millis(50)).await;
                return s;
            }
        }
    }

    fn stage(s: &JoinWifiResponse) -> Option<JoinStage> {
        s.stage.as_known()
    }

    #[tokio::test]
    async fn a_join_that_works_is_on_trial_until_kept() {
        let fake = Fake::start(Outcome::Joins);
        let wifi = wifi(&fake, Duration::from_secs(60));
        let end = try_cafe(&wifi).await;
        assert_eq!(stage(&end), Some(JoinStage::JOIN_STAGE_TRIAL), "{end:?}");
        assert_eq!(
            (end.address.as_str(), end.fallback.as_str()),
            ("192.168.1.20", "Home")
        );
        // On trial: nothing saved, and the trial is the thing to settle first.
        assert!(!fake.log().iter().any(|c| c == "SAVE_CONFIG"));
        assert!(fake.dir.join("trial").exists());
        let list = wifi.list().await.unwrap();
        assert_eq!(list.trial.as_option().unwrap().ssid, "Café");
        assert_eq!(list.saved.len(), 1);
        assert_eq!(
            wifi.forget("Home").await.unwrap_err().code,
            connectrpc::ErrorCode::FailedPrecondition
        );

        let kept = wifi.keep(true).await.unwrap();
        assert_eq!(stage(&kept), Some(JoinStage::JOIN_STAGE_JOINED), "{kept:?}");
        // Saved first in line, and Home saved enabled, not disabled by the selection.
        let saved = fake.saved();
        let cafe = saved.iter().find(|n| n.ssid == "Café").unwrap();
        let home = saved.iter().find(|n| n.ssid == "Home").unwrap();
        assert_eq!(
            (cafe.priority, cafe.disabled, home.disabled),
            (2, false, false)
        );
        assert!(!fake.dir.join("trial").exists());
        let list = wifi.list().await.unwrap();
        assert!(list.trial.as_option().is_none());
        assert_eq!(list.saved[0].ssid, "Café");
        assert_eq!(
            list.last_join.into_option().unwrap().address,
            "192.168.1.20"
        );
    }

    #[tokio::test]
    async fn opening_the_dashboard_at_the_new_address_keeps_it() {
        let fake = Fake::start(Outcome::Joins);
        let wifi = wifi(&fake, Duration::from_secs(60));
        try_cafe(&wifi).await;
        // Over USB: proves nothing about the new network.
        wifi.reached("10.42.0.1".parse().unwrap());
        // An IPv6 address that isn't the Wi-Fi interface's.
        wifi.reached("2001:db8::99".parse().unwrap());
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(wifi.list().await.unwrap().trial.as_option().is_some());
        // On the new network, as IPv6 maps it.
        wifi.reached("::ffff:192.168.1.20".parse().unwrap());
        for _ in 0..100 {
            if fake.log().iter().any(|c| c == "SAVE_CONFIG") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(fake.saved().iter().any(|n| n.ssid == "Café"));
        assert!(wifi.list().await.unwrap().trial.as_option().is_none());
    }

    #[tokio::test]
    async fn opening_the_dashboard_over_ipv6_keeps_it_too() {
        let fake = Fake::start(Outcome::Joins);
        let wifi = wifi(&fake, Duration::from_secs(60)).with_addresses(Arc::new(|iface| {
            Box::pin(async move {
                assert_eq!(iface, "wlan0");
                vec!["192.168.1.20".parse().unwrap(), "fe80::1".parse().unwrap()]
            })
        }));
        try_cafe(&wifi).await;
        wifi.reached("fe80::1".parse().unwrap());
        for _ in 0..100 {
            if fake.log().iter().any(|c| c == "SAVE_CONFIG") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(fake.saved().iter().any(|n| n.ssid == "Café"));
    }

    #[tokio::test]
    async fn a_trial_nobody_keeps_goes_back() {
        let fake = Fake::start(Outcome::Joins);
        let wifi = wifi(&fake, Duration::from_millis(300));
        try_cafe(&wifi).await;
        tokio::time::sleep(Duration::from_millis(800)).await;
        let list = wifi.list().await.unwrap();
        assert!(list.trial.as_option().is_none());
        let last = list.last_join.into_option().unwrap();
        assert_eq!(
            last.failure.as_known(),
            Some(JoinFailure::JOIN_FAILURE_NOT_KEPT)
        );
        assert!(fake.log().iter().any(|c| c == "RECONFIGURE"));
        assert!(!fake.log().iter().any(|c| c == "SAVE_CONFIG"));
        assert_eq!(list.saved.len(), 1);
        assert!(list.saved[0].current, "back on Home");
        assert!(!fake.dir.join("trial").exists());
    }

    #[tokio::test]
    async fn going_back_on_request() {
        let fake = Fake::start(Outcome::Joins);
        let wifi = wifi(&fake, Duration::from_secs(60));
        try_cafe(&wifi).await;
        let back = wifi.keep(false).await.unwrap();
        assert_eq!(
            back.failure.as_known(),
            Some(JoinFailure::JOIN_FAILURE_NOT_KEPT)
        );
        assert_eq!(back.fallback, "Home");
        assert_eq!(
            wifi.keep(true).await.unwrap_err().code,
            connectrpc::ErrorCode::FailedPrecondition
        );
    }

    #[tokio::test]
    async fn a_refused_password_goes_back_and_saves_nothing() {
        let fake = Fake::start(Outcome::WrongKey);
        let wifi = wifi(&fake, Duration::from_secs(60));
        let end = try_cafe(&wifi).await;
        assert_eq!(
            (stage(&end), end.failure.as_known(), end.fallback.as_str()),
            (
                Some(JoinStage::JOIN_STAGE_FAILED),
                Some(JoinFailure::JOIN_FAILURE_WRONG_PASSWORD),
                "Home"
            )
        );
        assert!(
            end.message.contains("refused the password"),
            "{}",
            end.message
        );
        // The passphrase reached wpa_supplicant, and nothing else.
        assert!(fake.log().iter().any(|c| c.contains("secret passphrase")));
        assert!(!end.message.contains("secret"));
        assert!(fake.log().iter().any(|c| c == "RECONFIGURE"));
        assert!(!fake.log().iter().any(|c| c == "SAVE_CONFIG"));
        let saved = wifi.list().await.unwrap().saved;
        assert_eq!(saved.len(), 1);
        assert!(saved[0].current);
    }

    #[tokio::test]
    async fn no_address_and_not_found_time_out_and_go_back() {
        let fake = Fake::start(Outcome::NoAddress);
        let end = try_cafe(&wifi(&fake, Duration::from_secs(60))).await;
        assert_eq!(
            end.failure.as_known(),
            Some(JoinFailure::JOIN_FAILURE_NO_ADDRESS),
            "{end:?}"
        );
        let fake = Fake::start(Outcome::NotFound);
        let end = try_cafe(&wifi(&fake, Duration::from_secs(60))).await;
        assert_eq!(
            end.failure.as_known(),
            Some(JoinFailure::JOIN_FAILURE_NOT_FOUND),
            "{end:?}"
        );
        assert!(end.message.ends_with("Back on Home."), "{}", end.message);
    }

    #[tokio::test]
    async fn one_operation_at_a_time() {
        let fake = Fake::start(Outcome::NotFound);
        let wifi = wifi(&fake, Duration::from_secs(60));
        let j = Join {
            ssid: "Café".into(),
            security: WifiSecurity::WIFI_SECURITY_OPEN,
            passphrase: String::new(),
            hidden: false,
        };
        let _live = wifi.join(j.clone()).unwrap();
        assert_eq!(
            wifi.join(j).unwrap_err().code,
            connectrpc::ErrorCode::FailedPrecondition
        );
        let err = wifi.forget("Home").await.unwrap_err();
        assert!(err.message.unwrap_or_default().contains("joining"));
        assert!(wifi.scan().await.is_err());
    }

    #[tokio::test]
    async fn a_trial_left_by_a_stopped_pinnad_is_undone_at_start() {
        let fake = Fake::start(Outcome::Joins);
        std::fs::write(fake.dir.join("trial"), b"").unwrap();
        wifi(&fake, Duration::from_secs(60)).recover().await;
        assert!(fake.log().iter().any(|c| c == "RECONFIGURE"));
        assert!(!fake.dir.join("trial").exists());
    }

    #[tokio::test]
    async fn scan_walks_every_access_point_and_forget_saves() {
        let fake = Fake::start(Outcome::Joins);
        let wifi = wifi(&fake, Duration::from_secs(60));
        let nets = wifi.scan().await.unwrap();
        assert_eq!(
            nets.iter()
                .map(|n| (
                    n.ssid.as_str(),
                    n.saved,
                    n.current,
                    n.security.as_known().unwrap()
                ))
                .collect::<Vec<_>>(),
            [
                ("Work", false, false, WifiSecurity::WIFI_SECURITY_ENTERPRISE),
                ("Home", true, true, WifiSecurity::WIFI_SECURITY_WPA_PSK),
                ("Café", false, false, WifiSecurity::WIFI_SECURITY_OPEN),
                ("Neighbour", false, false, WifiSecurity::WIFI_SECURITY_SAE),
            ]
        );
        let err = wifi.forget("Nope").await.unwrap_err();
        assert_eq!(err.code, connectrpc::ErrorCode::NotFound);
        assert!(wifi.forget("Home").await.unwrap().is_empty());
        let log = fake.log();
        let save = log.iter().position(|c| c == "SAVE_CONFIG").unwrap();
        assert_eq!(log[save - 1], "ENABLE_NETWORK all");
        assert!(fake.saved().is_empty());
    }

    #[tokio::test]
    async fn no_wifi_says_why() {
        let fake = Fake::start(Outcome::Joins);
        let mut paths = fake.paths();
        paths.sysfs = fake.dir.join("none");
        let err = Wifi::new(paths.clone()).scan().await.unwrap_err();
        assert_eq!(err.code, connectrpc::ErrorCode::FailedPrecondition);
        assert!(err.message.unwrap_or_default().contains("didn't start"));
        paths.has_radio = false;
        let err = Wifi::new(paths).scan().await.unwrap_err();
        assert!(err.message.unwrap_or_default().contains("no Wi-Fi"));
    }

    #[tokio::test]
    async fn the_hotspot_runs_only_while_nothing_is_saved() {
        let fake = Fake::start(Outcome::WrongKey);
        let wifi = wifi(&fake, Duration::from_secs(60));
        let mut changes = wifi.changes();
        assert_eq!(wifi.wants_hotspot().await, Some(false), "Home is saved");
        wifi.forget("Home").await.unwrap();
        assert!(changes.has_changed().unwrap(), "a forget is a change");
        changes.mark_unchanged();
        assert_eq!(wifi.wants_hotspot().await, Some(true));

        // A join stops the hotspot before it tries its network, and the
        // hotspot may come back once the join has failed.
        let stopped = fake.wpa.clone();
        wifi.set_before_join(Arc::new(move || {
            let stopped = stopped.clone();
            Box::pin(async move { stopped.lock().unwrap().log.push("hotspot stopped".into()) })
        }));
        let j = Join {
            ssid: "Café".into(),
            security: WifiSecurity::WIFI_SECURITY_WPA_PSK,
            passphrase: "wrong passphrase".into(),
            hidden: false,
        };
        let mut live = wifi.join(j).unwrap();
        assert_eq!(wifi.wants_hotspot().await, Some(false), "joining");
        loop {
            let s = tokio::time::timeout(Duration::from_secs(60), live.recv())
                .await
                .unwrap()
                .unwrap();
            if done(&s) {
                assert_eq!(stage(&s), Some(JoinStage::JOIN_STAGE_FAILED));
                assert!(
                    s.message.ends_with("it runs its setup hotspot again."),
                    "{}",
                    s.message
                );
                break;
            }
        }
        let log = fake.log();
        let hook = log.iter().position(|c| c == "hotspot stopped").unwrap();
        let add = log.iter().position(|c| c == "ADD_NETWORK").unwrap();
        assert!(hook < add, "{log:?}");
        assert!(changes.has_changed().unwrap());
        assert_eq!(wifi.wants_hotspot().await, Some(true));
    }

    #[tokio::test]
    async fn the_hotspot_waits_for_a_trial_to_end() {
        let fake = Fake::start(Outcome::Joins);
        let wifi = wifi(&fake, Duration::from_millis(300));
        wifi.forget("Home").await.unwrap();
        try_cafe(&wifi).await;
        assert_eq!(wifi.wants_hotspot().await, Some(false), "on trial");
        let mut changes = wifi.changes();
        changes.mark_unchanged();
        tokio::time::sleep(Duration::from_millis(800)).await;
        assert!(changes.has_changed().unwrap(), "the trial went back");
        assert_eq!(wifi.wants_hotspot().await, Some(true));
        let last = wifi.list().await.unwrap().last_join.into_option().unwrap();
        assert!(last.message.contains(" at http://"), "{}", last.message);
    }

    #[tokio::test]
    async fn without_wpa_supplicant_the_hotspot_waits() {
        let fake = Fake::start(Outcome::Joins);
        let mut paths = fake.paths();
        paths.ctrl_dir = fake.dir.join("nothing");
        assert_eq!(Wifi::new(paths).wants_hotspot().await, None);
    }

    #[tokio::test]
    async fn beside_the_hotspot_a_failed_scan_lists_the_one_from_before() {
        let fake = Fake::start(Outcome::Joins);
        let wifi = wifi(&fake, Duration::from_secs(60));
        let before = wifi.scan().await.unwrap();
        assert!(!before.is_empty());
        fake.wpa.lock().unwrap().refuse_scan = true;
        assert!(wifi.scan().await.is_err(), "no hotspot: the error stands");
        wifi.set_hotspot_on(true);
        assert_eq!(wifi.scan().await.unwrap(), before);
    }

    #[tokio::test]
    async fn a_join_from_the_hotspot_says_so_before_it_goes() {
        let fake = Fake::start(Outcome::Joins);
        let wifi = wifi(&fake, Duration::from_secs(60));
        wifi.set_hotspot_on(true);
        let mut live = wifi
            .join(Join {
                ssid: "Café".into(),
                security: WifiSecurity::WIFI_SECURITY_OPEN,
                passphrase: String::new(),
                hidden: false,
            })
            .unwrap();
        let first = tokio::time::timeout(Duration::from_secs(5), live.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stage(&first), Some(JoinStage::JOIN_STAGE_CONNECTING));
        assert!(
            first.message.contains("setup hotspot off"),
            "{}",
            first.message
        );
        assert!(
            !fake.log().iter().any(|c| c == "ADD_NETWORK"),
            "the step comes before the network is tried"
        );
    }
}
