//! The setup hotspot (ADR 0028). While no Wi-Fi network is saved, no join
//! runs and nothing is on trial, the satellite runs an access point,
//! "Pinna setup <hostname>", on `ap0` (hostapd in `pinna-hotspot.service`),
//! answers every DNS name with its own address, and the dashboard sends web
//! requests for other names to itself (web.rs), so a phone that joins opens
//! the dashboard by itself. A join stops it first: the chip has one radio.

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use tokio::net::UdpSocket;
use tokio::sync::{Mutex, watch};
use tokio::task::JoinHandle;

use crate::wifi::Wifi;

/// The satellite's address on its hotspot (`30-ap0.network`), next to USB's
/// `10.42.0.1`.
pub const ADDRESS: Ipv4Addr = Ipv4Addr::new(10, 42, 1, 1);
/// A stopgap, published in the docs (ADR 0028).
pub const PASSWORD: &str = "pinnasetup";
const INTERFACE: &str = "ap0";
const UNIT: &str = "pinna-hotspot.service";
pub const NAME_PREFIX: &str = "Pinna setup";
/// How often it looks again without a Wi-Fi change: wpa_supplicant starting
/// late, or a start that failed.
const LOOK_EVERY: Duration = Duration::from_secs(30);
/// After a failed start, the next try waits this long or for a Wi-Fi change:
/// a chip without an access point fails every time.
const RETRY_AFTER: Duration = Duration::from_secs(300);
/// The DNS answers' TTL: short, so names resolve properly once the phone is
/// back on its own network.
const DNS_TTL: u32 = 10;

#[derive(Debug, Clone)]
pub struct HotspotPaths {
    /// hostapd's configuration, written before each start.
    pub conf: PathBuf,
    /// The unit; a satellite image without it has no hotspot.
    pub unit_file: PathBuf,
}

impl Default for HotspotPaths {
    fn default() -> Self {
        Self {
            conf: "/run/pinna/hostapd.conf".into(),
            unit_file: format!("/usr/lib/systemd/system/{UNIT}").into(),
        }
    }
}

/// Whether this machine offers the hotspot: a Pinna image with the unit.
/// Never the computer a developer runs pinnad on.
pub fn available(paths: &HotspotPaths) -> bool {
    crate::identity::on_pinna_image() && paths.unit_file.exists()
}

/// The network's name: "Pinna setup <hostname>", cut to SSID's 32 bytes.
pub fn name(hostname: &str) -> String {
    let mut name = if hostname.is_empty() {
        NAME_PREFIX.to_owned()
    } else {
        format!("{NAME_PREFIX} {hostname}")
    };
    while name.len() > 32 {
        name.pop();
    }
    name
}

/// hostapd's configuration: WPA2-Personal on 2.4 GHz channel 6.
pub fn hostapd_conf(name: &str) -> String {
    format!(
        "# Written by pinnad for the setup hotspot (ADR 0028).\n\
         interface={INTERFACE}\n\
         driver=nl80211\n\
         ctrl_interface=/run/hostapd\n\
         ssid2=\"{}\"\n\
         utf8_ssid=1\n\
         hw_mode=g\n\
         channel=6\n\
         ieee80211n=1\n\
         wmm_enabled=1\n\
         auth_algs=1\n\
         wpa=2\n\
         wpa_key_mgmt=WPA-PSK\n\
         rsn_pairwise=CCMP\n\
         wpa_passphrase={PASSWORD}\n",
        name.replace(['"', '\n'], "")
    )
}

/// The reply to one DNS query: every A question is answered with `answer`,
/// every other type with no records. `None` for something that isn't a
/// query.
pub fn dns_reply(query: &[u8], answer: Ipv4Addr) -> Option<Vec<u8>> {
    if query.len() < 12 || query[2] & 0x80 != 0 {
        return None;
    }
    let opcode = (query[2] >> 3) & 0x0f;
    let questions = u16::from_be_bytes([query[4], query[5]]);
    let mut reply = Vec::with_capacity(query.len() + 16);
    reply.extend_from_slice(&query[..2]);
    // QR, the query's opcode and RD; AA.
    reply.push(0x80 | (query[2] & 0x79) | 0x04);
    if opcode != 0 || questions != 1 {
        // NOTIMP for anything but one standard query; FORMERR for none.
        reply.push(if questions == 0 { 1 } else { 4 });
        reply.extend_from_slice(&[0; 8]);
        return Some(reply);
    }
    // The question: labels up to the root, then type and class.
    let mut end = 12;
    loop {
        let len = *query.get(end)? as usize;
        if len & 0xc0 != 0 {
            return None;
        }
        end += 1 + len;
        if len == 0 {
            break;
        }
    }
    let qtype = u16::from_be_bytes([*query.get(end)?, *query.get(end + 1)?]);
    let qclass = u16::from_be_bytes([*query.get(end + 2)?, *query.get(end + 3)?]);
    end += 4;
    let a = qtype == 1 && qclass == 1;
    reply.push(0);
    reply.extend_from_slice(&[0, 1, 0, u8::from(a), 0, 0, 0, 0]);
    reply.extend_from_slice(&query[12..end]);
    if a {
        // A pointer to the question's name, A, IN, the TTL, 4 bytes.
        reply.extend_from_slice(&[0xc0, 0x0c, 0, 1, 0, 1]);
        reply.extend_from_slice(&DNS_TTL.to_be_bytes());
        reply.extend_from_slice(&[0, 4]);
        reply.extend_from_slice(&answer.octets());
    }
    Some(reply)
}

/// Answer DNS on `at` with `answer` until the task is aborted. The address
/// appears once networkd has configured `ap0`: binding waits for it.
async fn serve_dns(at: SocketAddrV4, answer: Ipv4Addr) {
    let socket = loop {
        match UdpSocket::bind(at).await {
            Ok(s) => break s,
            Err(e) if e.kind() == std::io::ErrorKind::AddrNotAvailable => {
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            Err(e) => {
                tracing::warn!("hotspot: DNS on {at}: {e}; retrying");
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    };
    tracing::info!("hotspot: answering DNS on {at}");
    let mut buf = [0u8; 512];
    loop {
        let Ok((n, from)) = socket.recv_from(&mut buf).await else {
            continue;
        };
        if let Some(reply) = dns_reply(&buf[..n], answer) {
            let _ = socket.send_to(&reply, from).await;
        }
    }
}

/// Starts and stops the unit (`systemctl`); tests record the calls instead.
pub type Systemctl = Arc<
    dyn Fn(Vec<String>) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send>>
        + Send
        + Sync,
>;

fn systemctl() -> Systemctl {
    Arc::new(|args: Vec<String>| {
        Box::pin(async move {
            let status = tokio::process::Command::new("systemctl")
                .args(&args)
                .status()
                .await
                .context("running systemctl")?;
            if !status.success() {
                bail!("systemctl {} exited with {status}", args.join(" "));
            }
            Ok(())
        })
    })
}

struct Running {
    dns: Option<JoinHandle<()>>,
}

/// What ListWifi says about the hotspot.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct State {
    /// Its name while it runs.
    pub name: Option<String>,
    /// The last start failed, or hostapd stopped by itself: the hotspot may
    /// not work on this satellite.
    pub failing: bool,
}

pub struct Hotspot {
    paths: HotspotPaths,
    systemctl: Systemctl,
    /// Where DNS is answered; tests use a free port on loopback.
    dns_at: SocketAddrV4,
    /// How long hostapd must keep running after a start to count as started:
    /// the unit counts as started once hostapd is forked.
    settle: Duration,
    wifi: Arc<Wifi>,
    /// One start or stop at a time; `Some` while it runs.
    running: Mutex<Option<Running>>,
    state: watch::Sender<State>,
}

impl Hotspot {
    pub fn new(paths: HotspotPaths, wifi: Arc<Wifi>, state: watch::Sender<State>) -> Self {
        Self {
            paths,
            systemctl: systemctl(),
            dns_at: SocketAddrV4::new(ADDRESS, 53),
            settle: Duration::from_secs(3),
            wifi,
            running: Mutex::new(None),
            state,
        }
    }

    #[cfg(test)]
    fn with_systemctl(mut self, systemctl: Systemctl, dns_at: SocketAddrV4) -> Self {
        self.systemctl = systemctl;
        self.dns_at = dns_at;
        self.settle = Duration::ZERO;
        self
    }

    async fn unit(&self, verb: &str) -> Result<()> {
        (self.systemctl)(vec![verb.to_owned(), UNIT.to_owned()]).await
    }

    async fn active(&self) -> bool {
        (self.systemctl)(vec!["is-active".into(), "--quiet".into(), UNIT.into()])
            .await
            .is_ok()
    }

    /// Start it as `name`, unless it runs.
    pub async fn start(&self, name: &str) -> Result<()> {
        let mut running = self.running.lock().await;
        if running.is_some() {
            return Ok(());
        }
        if let Some(dir) = self.paths.conf.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        write_private(&self.paths.conf, &hostapd_conf(name))
            .with_context(|| format!("writing {}", self.paths.conf.display()))?;
        let started = async {
            self.unit("start").await?;
            tokio::time::sleep(self.settle).await;
            if !self.active().await {
                bail!("hostapd stopped right after it started: this Wi-Fi chip may not run an access point");
            }
            Ok(())
        }
        .await;
        if let Err(e) = started {
            // Leave the unit stopped, not failed, for the next try.
            let _ = self.unit("stop").await;
            self.state.send_replace(State {
                name: None,
                failing: true,
            });
            return Err(e);
        }
        let dns = tokio::spawn(serve_dns(self.dns_at, ADDRESS));
        *running = Some(Running { dns: Some(dns) });
        self.wifi.set_hotspot_on(true);
        self.state.send_replace(State {
            name: Some(name.to_owned()),
            failing: false,
        });
        tracing::info!("hotspot: on as {name:?}");
        Ok(())
    }

    /// Stop it, if it runs.
    pub async fn stop(&self) {
        let mut running = self.running.lock().await;
        let Some(mut r) = running.take() else {
            return;
        };
        if let Some(dns) = r.dns.take() {
            dns.abort();
        }
        if let Err(e) = self.unit("stop").await {
            tracing::warn!("hotspot: {e:#}");
        }
        self.wifi.set_hotspot_on(false);
        self.state.send_modify(|s| s.name = None);
        tracing::info!("hotspot: off");
    }

    pub async fn is_on(&self) -> bool {
        self.running.lock().await.is_some()
    }

    /// Whether it still runs, if it should: `false` when hostapd stopped by
    /// itself, which counts as a failed start.
    async fn check(&self) -> bool {
        let mut running = self.running.lock().await;
        let Some(r) = running.as_mut() else {
            return true;
        };
        if self.active().await {
            return true;
        }
        if let Some(dns) = r.dns.take() {
            dns.abort();
        }
        *running = None;
        let _ = self.unit("stop").await;
        self.wifi.set_hotspot_on(false);
        self.state.send_replace(State {
            name: None,
            failing: true,
        });
        tracing::warn!("hotspot: hostapd stopped by itself; see journalctl -u pinna-hotspot");
        false
    }

    /// Run it while the Wi-Fi wants it, for as long as pinnad runs.
    pub async fn run(self: Arc<Self>, hostname: impl Fn() -> String) {
        // A pinnad that stopped while the hotspot ran left it running.
        let _ = self.unit("stop").await;
        let wifi = self.wifi.clone();
        let mut changes = wifi.changes();
        let mut retry_at: Option<tokio::time::Instant> = None;
        loop {
            if !self.check().await {
                retry_at = Some(tokio::time::Instant::now() + RETRY_AFTER);
            }
            let on = self.is_on().await;
            match wifi.wants_hotspot().await {
                Some(true) if !on && retry_at.is_none_or(|t| t <= tokio::time::Instant::now()) => {
                    // Scan first: the chip may not scan while it is an access
                    // point, and the Wi-Fi code lists this scan instead then.
                    if let Err(e) = wifi.scan().await {
                        tracing::info!(
                            "hotspot: scan before starting: {}",
                            e.message.unwrap_or_default()
                        );
                    }
                    // The scan took a while: look again before starting.
                    if wifi.wants_hotspot().await == Some(true) {
                        match self.start(&name(&hostname())).await {
                            Ok(()) => retry_at = None,
                            Err(e) => {
                                tracing::warn!("hotspot: didn't start: {e:#}");
                                retry_at = Some(tokio::time::Instant::now() + RETRY_AFTER);
                            }
                        }
                    }
                }
                Some(false) if on => self.stop().await,
                _ => {}
            }
            tokio::select! {
                r = changes.changed() => {
                    if r.is_err() {
                        return;
                    }
                    retry_at = None;
                }
                () = tokio::time::sleep(LOOK_EVERY) => {}
            }
        }
    }
}

/// Write `text` to `path`, readable by root only.
fn write_private(path: &std::path::Path, text: &str) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(text.as_bytes())
}

/// Where a request for another name on the hotspot goes: the dashboard's
/// Network page, where the owner joins a network.
pub fn portal(local: Option<SocketAddr>) -> Option<String> {
    let local = local?;
    (local.ip().to_canonical() == ADDRESS).then(|| format!("http://{ADDRESS}/#/network"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wifi::fake::{Fake, Outcome};

    #[test]
    fn names_fit_an_ssid() {
        assert_eq!(name("pinna-2a914e"), "Pinna setup pinna-2a914e");
        assert_eq!(name(""), "Pinna setup");
        let long = name("a-very-long-hostname-for-a-kitchen-satellite");
        assert_eq!(long.len(), 32);
        assert!(long.starts_with("Pinna setup a-very"));
    }

    #[test]
    fn hostapd_runs_wpa2_on_ap0() {
        let conf = hostapd_conf("Pinna setup kitchen");
        for line in [
            "interface=ap0",
            "driver=nl80211",
            "ssid2=\"Pinna setup kitchen\"",
            "channel=6",
            "wpa=2",
            "wpa_key_mgmt=WPA-PSK",
            "rsn_pairwise=CCMP",
            "wpa_passphrase=pinnasetup",
        ] {
            assert!(conf.lines().any(|l| l == line), "{line} in {conf}");
        }
        assert!(hostapd_conf("a\"b\nc").contains("ssid2=\"abc\"\n"));
    }

    /// A query for `name` of `qtype`, ID 0x1234, recursion desired.
    fn query(name: &str, qtype: u16) -> Vec<u8> {
        let mut q = vec![0x12, 0x34, 0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0];
        for label in name.split('.') {
            q.push(label.len() as u8);
            q.extend_from_slice(label.as_bytes());
        }
        q.push(0);
        q.extend_from_slice(&qtype.to_be_bytes());
        q.extend_from_slice(&[0, 1]);
        q
    }

    #[test]
    fn every_name_is_the_satellite() {
        let q = query("connectivitycheck.gstatic.com", 1);
        let r = dns_reply(&q, ADDRESS).unwrap();
        assert_eq!(&r[..2], &[0x12, 0x34], "the ID");
        assert_eq!(r[2], 0x85, "a reply, authoritative, recursion desired");
        assert_eq!(r[3] & 0x0f, 0, "no error");
        assert_eq!(&r[4..12], &[0, 1, 0, 1, 0, 0, 0, 0]);
        assert_eq!(&r[12..q.len()], &q[12..], "the question");
        assert_eq!(&r[q.len()..q.len() + 2], &[0xc0, 0x0c]);
        assert_eq!(&r[r.len() - 4..], &[10, 42, 1, 1]);
    }

    #[test]
    fn other_types_get_no_records() {
        let q = query("captive.apple.com", 28);
        let r = dns_reply(&q, ADDRESS).unwrap();
        assert_eq!(r[3] & 0x0f, 0);
        assert_eq!(&r[6..8], &[0, 0], "no answers");
        assert_eq!(r.len(), q.len());
    }

    #[test]
    fn odd_packets() {
        assert_eq!(dns_reply(&[0; 5], ADDRESS), None, "too short");
        let mut reply = query("a.b", 1);
        reply[2] |= 0x80;
        assert_eq!(dns_reply(&reply, ADDRESS), None, "a reply, not a query");
        let mut truncated = query("example.com", 1);
        truncated.truncate(20);
        assert_eq!(dns_reply(&truncated, ADDRESS), None);
        let mut update = query("a.b", 1);
        update[2] = 5 << 3;
        assert_eq!(dns_reply(&update, ADDRESS).unwrap()[3] & 0x0f, 4, "NOTIMP");
    }

    #[test]
    fn only_the_hotspot_address_is_a_portal() {
        assert_eq!(
            portal(Some("10.42.1.1:80".parse().unwrap())).as_deref(),
            Some("http://10.42.1.1/#/network")
        );
        assert_eq!(portal(Some("10.42.0.1:80".parse().unwrap())), None);
        assert!(portal(Some("[::ffff:10.42.1.1]:80".parse().unwrap())).is_some());
        assert_eq!(portal(None), None);
    }

    type Calls = Arc<std::sync::Mutex<Vec<String>>>;

    /// Records the calls; `is-active` answers from `active`.
    fn recording(active: Arc<std::sync::atomic::AtomicBool>) -> (Systemctl, Calls) {
        let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = calls.clone();
        let f: Systemctl = Arc::new(move |args: Vec<String>| {
            let is_active = args[0] == "is-active";
            if !is_active {
                seen.lock().unwrap().push(args.join(" "));
            }
            let up = active.load(std::sync::atomic::Ordering::SeqCst);
            Box::pin(async move {
                if is_active && !up {
                    bail!("inactive");
                }
                Ok(())
            })
        });
        (f, calls)
    }

    fn free_port() -> u16 {
        std::net::UdpSocket::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }

    struct Rig {
        fake: Fake,
        wifi: Arc<Wifi>,
        hotspot: Arc<Hotspot>,
        calls: Calls,
        active: Arc<std::sync::atomic::AtomicBool>,
        state: watch::Receiver<State>,
        paths: HotspotPaths,
        dns_at: SocketAddrV4,
    }

    fn rig() -> Rig {
        let fake = Fake::start(Outcome::Joins);
        let wifi = Arc::new(Wifi::new(fake.paths()));
        let active = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let (systemctl, calls) = recording(active.clone());
        let dns_at = SocketAddrV4::new(Ipv4Addr::LOCALHOST, free_port());
        let (tx, state) = watch::channel(State::default());
        let paths = HotspotPaths {
            conf: fake.dir.join("hostapd.conf"),
            unit_file: fake.dir.join("unit"),
        };
        let hotspot = Arc::new(
            Hotspot::new(paths.clone(), wifi.clone(), tx).with_systemctl(systemctl, dns_at),
        );
        Rig {
            fake,
            wifi,
            hotspot,
            calls,
            active,
            state,
            paths,
            dns_at,
        }
    }

    async fn named(state: &watch::Receiver<State>) -> String {
        let mut rx = state.clone();
        let s = tokio::time::timeout(Duration::from_secs(20), rx.wait_for(|s| s.name.is_some()))
            .await
            .unwrap()
            .unwrap();
        s.name.clone().unwrap()
    }

    #[tokio::test]
    async fn runs_while_nothing_is_saved_and_answers_dns() {
        let r = rig();
        tokio::spawn(r.hotspot.clone().run(|| "kitchen".to_owned()));

        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(
            *r.calls.lock().unwrap(),
            ["stop pinna-hotspot.service"],
            "a leftover from an earlier pinnad is stopped; Home is saved"
        );
        assert_eq!(*r.state.borrow(), State::default());

        r.wifi.forget("Home").await.unwrap();
        assert_eq!(named(&r.state).await, "Pinna setup kitchen");
        assert_eq!(
            *r.calls.lock().unwrap(),
            ["stop pinna-hotspot.service", "start pinna-hotspot.service"]
        );
        assert!(r.fake.log().iter().any(|c| c == "SCAN"), "scanned first");
        let conf = std::fs::read_to_string(&r.paths.conf).unwrap();
        assert!(conf.contains("ssid2=\"Pinna setup kitchen\""));
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&r.paths.conf)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );

        let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        client
            .send_to(&query("example.com", 1), r.dns_at)
            .await
            .unwrap();
        let mut buf = [0u8; 512];
        let (n, _) = tokio::time::timeout(Duration::from_secs(5), client.recv_from(&mut buf))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&buf[n - 4..n], &[10, 42, 1, 1]);

        r.hotspot.stop().await;
        assert_eq!(
            r.calls.lock().unwrap().last().map(String::as_str),
            Some("stop pinna-hotspot.service")
        );
        assert_eq!(*r.state.borrow(), State::default());
    }

    #[tokio::test]
    async fn a_hostapd_that_dies_counts_as_failing() {
        let r = rig();
        r.hotspot.start("Pinna setup kitchen").await.unwrap();
        assert!(r.hotspot.is_on().await);
        r.active.store(false, std::sync::atomic::Ordering::SeqCst);
        assert!(!r.hotspot.check().await);
        assert!(!r.hotspot.is_on().await);
        assert_eq!(
            *r.state.borrow(),
            State {
                name: None,
                failing: true
            }
        );
        // A start whose hostapd doesn't stay up fails too.
        let err = r.hotspot.start("Pinna setup kitchen").await.unwrap_err();
        assert!(
            err.to_string().contains("stopped right after it started"),
            "{err}"
        );
        assert!(!r.hotspot.is_on().await);
        r.active.store(true, std::sync::atomic::Ordering::SeqCst);
        r.hotspot.start("Pinna setup kitchen").await.unwrap();
        assert!(!r.state.borrow().failing);
    }
}
