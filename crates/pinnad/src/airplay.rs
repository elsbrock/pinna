//! AirPlay (ADR 0011): trixie's shairport-sync, AirPlay 1, run by
//! `pinna-airplay.service`, the bridge into the stream socket. pinnad writes
//! its configuration and advertises it through systemd-resolved: avahi-daemon
//! is masked, so shairport-sync never advertises itself.

use std::path::Path;

use anyhow::{Context, Result};
use md5::{Digest, Md5};
use std::time::Duration;

use tokio::sync::{mpsc, watch};

/// shairport-sync's configuration, read by `pinna-airplay.service`.
const CONF: &str = "/run/pinna/airplay/shairport-sync.conf";
/// The `_raop._tcp` record, for systemd-resolved.
const DNSSD: &str = "/run/systemd/dnssd/pinna-airplay.dnssd";
const UNIT: &str = "pinna-airplay.service";
/// AirPlay 1's RTSP port.
const PORT: u16 = 5000;
/// The UDP ports for audio, control and timing: shairport-sync's defaults,
/// set so the documented firewall ports hold across package versions.
const UDP_PORT_BASE: u16 = 6001;
const UDP_PORT_RANGE: u16 = 10;
/// shairport-sync cuts longer names here.
const NAME_MAX: usize = 50;
/// From shairport-sync's output to the speaker: the stream queue (100 ms,
/// plus half a chunk on average), the speaker's 100 ms lead and aplay's
/// ~128 ms buffer. shairport-sync writes this much early, so the sound
/// leaves the speaker on the sender's time. An estimate until measured on
/// the device.
const PIPELINE_LATENCY_S: f64 = 0.34;
/// shairport-sync's AirPlay 1 TXT record, as trixie's 4.3.7 builds it
/// (`rtsp.c`, `build_bonjour_strings`, with metadata) and as configured here
/// (no cover art, no password). `fv` is the package version: a new
/// shairport-sync means checking this again (ADR 0011).
const TXT: [&str; 17] = [
    "sf=0x4",
    "fv=4.3.7",
    "am=ShairportSync",
    "vs=105.1",
    "tp=TCP,UDP",
    "vn=65537",
    "md=0,2",
    "ss=16",
    "sr=44100",
    "da=true",
    "sv=false",
    "et=0,1",
    "ek=1",
    "cn=0,1",
    "ch=2",
    "txtvers=1",
    "pw=false",
];

/// What the receiver is doing, as the dashboard shows it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum State {
    #[default]
    Off,
    /// Switched on; shairport-sync isn't running yet.
    Starting,
    Ready,
    /// The bridge stopped or never started; systemd's result, e.g. "exit-code".
    Failed(String),
    /// No speaker or stream socket: AirPlay can't run here.
    Unavailable,
}

/// The receiver's state and, while it is on, the name senders list it under.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    pub state: State,
    pub name: String,
}

/// How often the worker looks at the bridge while AirPlay is on.
const POLL: Duration = Duration::from_secs(2);

/// The AirPlay receiver as pinnad configures it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AirPlay {
    /// The name senders show, exactly as shairport-sync will use it.
    name: String,
}

impl AirPlay {
    /// `name` is the device's friendly name. `%` goes (shairport-sync and
    /// systemd would both expand it), and the rest is cut to shairport-sync's
    /// limit.
    pub fn new(name: &str) -> AirPlay {
        let mut name: String = name.chars().filter(|&c| c != '%').collect();
        if name.len() > NAME_MAX {
            let mut end = NAME_MAX;
            while !name.is_char_boundary(end) {
                end -= 1;
            }
            name.truncate(end);
        }
        AirPlay { name }
    }

    /// shairport-sync's configuration: audio to stdout for the bridge, the
    /// sender's volume reported as metadata instead of applied.
    pub fn conf(&self) -> String {
        let name = self.name.replace('\\', "\\\\").replace('"', "\\\"");
        format!(
            "// Written by pinnad (ADR 0011).\n\
             general = {{\n\
             \x20 name = \"{name}\";\n\
             \x20 output_backend = \"stdout\";\n\
             \x20 port = {PORT};\n\
             \x20 udp_port_base = {UDP_PORT_BASE};\n\
             \x20 udp_port_range = {UDP_PORT_RANGE};\n\
             \x20 ignore_volume_control = \"yes\";\n\
             \x20 audio_backend_latency_offset_in_seconds = -{PIPELINE_LATENCY_S};\n\
             }};\n\
             sessioncontrol = {{\n\
             \x20 allow_session_interruption = \"yes\";\n\
             }};\n\
             metadata = {{\n\
             \x20 enabled = \"yes\";\n\
             \x20 include_cover_art = \"no\";\n\
             \x20 pipe_name = \"/run/pinna-airplay/metadata\";\n\
             }};\n\
             diagnostics = {{\n\
             \x20 log_output_to = \"stderr\";\n\
             }};\n"
        )
    }

    /// The `_raop._tcp` record shairport-sync would publish on a host whose
    /// first network interface has `mac`.
    pub fn record(&self, mac: [u8; 6]) -> String {
        format!(
            "# Written by pinnad: shairport-sync's AirPlay 1 record (ADR 0011).\n\
             [Service]\nName={}@{}\nType=_raop._tcp\nPort={PORT}\nTxtText={}\n",
            prefix(&self.name, mac),
            self.name,
            TXT.join(" ")
        )
    }
}

/// The name's prefix, as shairport-sync makes it outside AirPlay 2 mode:
/// MD5 over the name and its 8-byte `hw_addr` (the MAC, two zero bytes),
/// the first 6 bytes in hex. It answers Apple challenges with the same bytes.
fn prefix(name: &str, mac: [u8; 6]) -> String {
    let mut h = Md5::new();
    h.update(name.as_bytes());
    h.update(mac);
    h.update([0, 0]);
    h.finalize()[..6]
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect()
}

/// The MAC shairport-sync takes: the first interface other than `lo` that
/// `getifaddrs` lists with a link-layer address.
fn device_mac() -> Option<[u8; 6]> {
    let mut list: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: getifaddrs fills `list`, which is freed below and only read in
    // between; every pointer is checked before use.
    unsafe {
        if libc::getifaddrs(&mut list) != 0 {
            return None;
        }
        let mut found = None;
        let mut ifa = list;
        while !ifa.is_null() && found.is_none() {
            let a = (*ifa).ifa_addr;
            if !a.is_null() && i32::from((*a).sa_family) == libc::AF_PACKET {
                let name = std::ffi::CStr::from_ptr((*ifa).ifa_name);
                if name.to_bytes() != b"lo" {
                    let ll = &*(a as *const libc::sockaddr_ll);
                    let mut mac = [0u8; 6];
                    let n = usize::from(ll.sll_halen).min(6);
                    mac[..n].copy_from_slice(&ll.sll_addr[..n]);
                    found = Some(mac);
                }
            }
            ifa = (*ifa).ifa_next;
        }
        libc::freeifaddrs(list);
        found
    }
}

pub(crate) fn systemctl(args: &[&str]) {
    match std::process::Command::new("systemctl").args(args).status() {
        Ok(s) if s.success() => {}
        other => tracing::warn!("systemctl {}: {other:?}", args.join(" ")),
    }
}

/// A task that applies the receiver as pinnad wants it, `Some` on under that
/// name or `None` off, one change at a time (changes that pile up collapse
/// into the latest), and while it is on watches the bridge and reports.
pub fn worker(report: watch::Sender<Report>) -> mpsc::UnboundedSender<Option<AirPlay>> {
    let (tx, mut rx) = mpsc::unbounded_channel::<Option<AirPlay>>();
    tokio::spawn(async move {
        let mut want: Option<AirPlay> = None;
        let mut poll = tokio::time::interval(POLL);
        loop {
            tokio::select! {
                next = rx.recv() => {
                    let Some(mut next) = next else { return };
                    while let Ok(later) = rx.try_recv() {
                        next = later;
                    }
                    want = next.clone();
                    report.send_replace(Report {
                        state: if want.is_some() { State::Starting } else { State::Off },
                        name: want.as_ref().map(|a| a.name.clone()).unwrap_or_default(),
                    });
                    let applied = tokio::task::spawn_blocking(move || match &next {
                        Some(a) => apply(a, true),
                        None => {
                            off();
                            Ok(())
                        }
                    });
                    match applied.await {
                        Ok(Ok(())) => {}
                        Ok(Err(e)) => tracing::warn!("airplay: {e:#}"),
                        Err(e) => tracing::warn!("airplay: {e}"),
                    }
                }
                _ = poll.tick(), if want.is_some() => {
                    if let Ok(state) = tokio::task::spawn_blocking(|| unit_state(UNIT)).await {
                        report.send_if_modified(|r| {
                            let changed = r.state != state;
                            if changed && let State::Failed(why) = &state {
                                // The dashboard sends people to this log.
                                tracing::warn!(
                                    "airplay: the receiver stopped ({why}); \
                                     journalctl -u pinna-airplay says why"
                                );
                            }
                            r.state = state;
                            changed
                        });
                    }
                }
            }
        }
    });
    tx
}

/// A bridge's unit as systemd sees it, while its source is on.
pub(crate) fn unit_state(unit: &str) -> State {
    let out = std::process::Command::new("systemctl")
        .args(["show", "-p", "ActiveState", "-p", "Result", unit])
        .output();
    let Ok(out) = out else {
        return State::Failed("systemctl".into());
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let field = |k: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(k)?.strip_prefix('='))
            .unwrap_or("")
            .to_owned()
    };
    unit_state_of(&field("ActiveState"), &field("Result"))
}

fn unit_state_of(active: &str, result: &str) -> State {
    match active {
        "active" => State::Ready,
        // Starting, or restarting after a crash (Restart=on-failure).
        "activating" | "reloading" => State::Starting,
        _ if result.is_empty() || result == "success" => State::Failed("stopped".into()),
        _ => State::Failed(result.into()),
    }
}

/// AirPlay off: the record goes first, so senders stop listing the device at
/// once however long shairport-sync takes to stop, then the bridge. Blocks.
pub fn off() {
    // Restarted here and now, not batched: the record has to go before the
    // bridge stops (ADR 0011), and a reload would leave it published (ADR 0024).
    if std::fs::remove_file(DNSSD).is_ok() && Path::new("/run/systemd/resolve").exists() {
        systemctl(&["restart", "systemd-resolved.service"]);
    }
    systemctl(&["stop", UNIT]);
    tracing::info!("airplay: off");
}

/// Switch AirPlay on or off: the configuration, the record and the bridge.
/// Blocks (systemctl).
pub fn apply(airplay: &AirPlay, on: bool) -> Result<()> {
    if !on {
        off();
        return Ok(());
    }
    let resolved = Path::new("/run/systemd/resolve").exists();
    let conf = Path::new(CONF);
    if let Some(dir) = conf.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(conf, airplay.conf()).with_context(|| format!("writing {CONF}"))?;
    if resolved {
        match device_mac() {
            Some(mac) => {
                let record = airplay.record(mac);
                let old = std::fs::read_to_string(DNSSD).ok();
                if old.as_deref() != Some(record.as_str()) {
                    std::fs::create_dir_all("/run/systemd/dnssd")?;
                    std::fs::write(DNSSD, record).with_context(|| format!("writing {DNSSD}"))?;
                    crate::identity::reread_dnssd(old.is_some());
                }
            }
            None => tracing::warn!("airplay: no network interface; not advertised"),
        }
    }
    systemctl(&["restart", UNIT]);
    tracing::info!("airplay: on as \"{}\"", airplay.name);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAC: [u8; 6] = [2, 0, 0, 0xa, 0xb, 0xc];

    #[test]
    fn prefix_is_shairport_syncs() {
        // Python: md5(b"Echo Dot" + bytes([2,0,0,10,11,12,0,0])).hexdigest()[:12]
        assert_eq!(prefix("Echo Dot", MAC), "75E7366731AB");
    }

    #[test]
    fn record_names_the_receiver_like_shairport_sync() {
        let r = AirPlay::new("Echo Dot").record(MAC);
        assert!(r.contains("\nName=75E7366731AB@Echo Dot\n"), "{r}");
        assert!(r.contains("\nType=_raop._tcp\nPort=5000\n"));
        assert!(r.contains("TxtText=sf=0x4 fv=4.3.7 am=ShairportSync vs=105.1 "));
        assert!(r.trim_end().ends_with("txtvers=1 pw=false"));
    }

    #[test]
    fn names_lose_percent_signs_and_are_cut_like_shairport_syncs() {
        assert_eq!(AirPlay::new("100% Echo").name, "100 Echo");
        assert_eq!(AirPlay::new(&"x".repeat(60)).name.len(), 50);
        // Never inside a character.
        let n = AirPlay::new(&format!("{}é", "x".repeat(49))).name;
        assert_eq!(n, "x".repeat(49));
    }

    #[test]
    fn conf_quotes_the_name_and_writes_to_stdout() {
        let c = AirPlay::new("Kitchen \"Dot\"").conf();
        assert!(c.contains("  name = \"Kitchen \\\"Dot\\\"\";\n"), "{c}");
        assert!(c.contains("  output_backend = \"stdout\";\n"));
        assert!(c.contains("  ignore_volume_control = \"yes\";\n"));
        // A new sender takes over, as every source does (ADR 0011).
        assert!(c.contains("  allow_session_interruption = \"yes\";\n"));
        assert!(c.contains("  udp_port_base = 6001;\n  udp_port_range = 10;\n"));
        assert!(c.contains("  audio_backend_latency_offset_in_seconds = -0.34;\n"));
        assert!(c.contains("  pipe_name = \"/run/pinna-airplay/metadata\";\n"));
    }

    #[test]
    fn unit_states_map_to_what_the_dashboard_shows() {
        assert_eq!(unit_state_of("active", "success"), State::Ready);
        assert_eq!(unit_state_of("activating", "exit-code"), State::Starting);
        assert_eq!(
            unit_state_of("failed", "start-limit-hit"),
            State::Failed("start-limit-hit".into())
        );
        assert_eq!(
            unit_state_of("inactive", "success"),
            State::Failed("stopped".into())
        );
    }

    #[test]
    fn finds_a_mac_on_this_host() {
        // Any host running the tests has some interface besides lo, unless
        // it is a bare network namespace.
        if let Some(mac) = device_mac() {
            assert_eq!(mac.len(), 6);
        }
    }
}
