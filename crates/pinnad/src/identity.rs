//! Device identity (MAC, ESPHome node name) and mDNS publication through
//! systemd-resolved's DNS-SD support.

use std::path::Path;
use std::sync::OnceLock;
use std::sync::mpsc::{Sender, channel};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use pinna_esphome::{DeviceConfig, mdns_txt, parse_encryption_key};

use crate::config;

/// The Noise key: the persisted key file (provisioned by Home Assistant) wins
/// over the configured key; neither = plaintext.
pub fn load_encryption_key(cfg: &config::Esphome) -> Result<Option<[u8; 32]>> {
    if let Ok(text) = std::fs::read_to_string(&cfg.key_file) {
        let key = parse_encryption_key(text.trim())
            .map_err(|e| anyhow::anyhow!("{}: {e:?}", cfg.key_file.display()))?;
        return Ok(Some(key));
    }
    let configured = cfg.encryption_key.trim();
    if configured.is_empty() {
        return Ok(None);
    }
    let key = parse_encryption_key(configured)
        .map_err(|e| anyhow::anyhow!("esphome.encryption_key: {e:?}"))?;
    Ok(Some(key))
}

/// Persist a key pushed by Home Assistant (mode 0600) so it survives restarts.
pub fn store_encryption_key(path: &Path, key: &[u8; 32]) -> Result<()> {
    use base64::Engine as _;
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)
        .with_context(|| format!("writing {}", tmp.display()))?;
    writeln!(
        f,
        "{}",
        base64::engine::general_purpose::STANDARD.encode(key)
    )?;
    f.sync_all()?;
    std::fs::rename(&tmp, path)?;
    if let Some(dir) = path.parent() {
        std::fs::File::open(dir)?.sync_all()?;
    }
    Ok(())
}

pub fn parse_mac(s: &str) -> Option<[u8; 6]> {
    let parts: Vec<u8> = s
        .trim()
        .split(':')
        .map(|p| u8::from_str_radix(p, 16))
        .collect::<Result<_, _>>()
        .ok()?;
    parts.try_into().ok()
}

/// The device MAC: the Wi-Fi MAC on the Echo, from the bootloader's idme
/// record so that pinnad need not wait for Wi-Fi (wlan0 carries the same
/// address), else wlan0's; otherwise a stable, locally administered address
/// derived from the machine id (VMs, laptops).
pub fn device_mac() -> [u8; 6] {
    if let Some(mac) = pinna_hal::ident::idme_mac().and_then(|s| parse_mac(&s)) {
        return mac;
    }
    if !pinna_hal::ident::board_model().is_empty() {
        tracing::warn!("no idme mac_addr in the device tree; the device MAC comes from wlan0");
    }
    if let Some(mac) = pinna_hal::ident::wlan0_mac().and_then(|s| parse_mac(&s)) {
        return mac;
    }
    let seed = std::fs::read_to_string("/etc/machine-id").unwrap_or_else(|_| "pinna".into());
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in seed.trim().bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    let b = h.to_be_bytes();
    [0x02, b[0], b[1], b[2], b[3], b[4]]
}

pub fn default_name(mac: &[u8; 6]) -> String {
    format!("pinna-{:02x}{:02x}{:02x}", mac[3], mac[4], mac[5])
}

/// The ESPHome node name and hostname (ADR 0010): the dashboard's setting,
/// else `[device] name`, else `pinna-` and the MAC suffix.
pub fn node_name(setting: Option<&str>, configured: &str, mac: &[u8; 6]) -> String {
    match setting {
        Some(s) if !s.is_empty() => s.to_owned(),
        _ if !configured.is_empty() => configured.to_owned(),
        _ => default_name(mac),
    }
}

/// Whether `name` can be a hostname: one DNS label, as mDNS announces it.
pub fn valid_hostname(name: &str) -> bool {
    (1..=63).contains(&name.len())
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        && !name.starts_with('-')
        && !name.ends_with('-')
}

/// Whether os-release text says this is a Pinna image: `IMAGE_ID` is `pinna`
/// or `pinna-*`, possibly quoted.
fn is_pinna_image(os_release: &str) -> bool {
    os_release.lines().any(|l| {
        l.strip_prefix("IMAGE_ID=").is_some_and(|v| {
            let v = v.trim().trim_matches(|c| c == '"' || c == '\'');
            v == "pinna" || v.starts_with("pinna-")
        })
    })
}

/// Only a satellite image is renamed, never the computer a developer runs
/// pinnad on.
fn on_pinna_image() -> bool {
    let text = std::fs::read_to_string("/etc/os-release")
        .or_else(|_| std::fs::read_to_string("/usr/lib/os-release"))
        .unwrap_or_default();
    is_pinna_image(&text)
}

/// Whether `name` needs setting: this is a Pinna image and it isn't the kernel
/// hostname yet.
fn needs_hostname(name: &str) -> Result<bool> {
    anyhow::ensure!(valid_hostname(name), "{name:?} can't be a hostname");
    if !on_pinna_image() {
        return Ok(false);
    }
    let current = std::fs::read_to_string("/proc/sys/kernel/hostname").unwrap_or_default();
    Ok(current.trim() != name)
}

/// Make `name` the kernel hostname (ADR 0010), which systemd-resolved
/// announces over mDNS and systemd-networkd sends with DHCP. Leaves a name
/// that is already set alone, and a machine that isn't a satellite image.
/// Runs early in boot, before D-Bus, so it can't ask systemd-hostnamed.
pub fn set_hostname(name: &str) -> Result<bool> {
    if !needs_hostname(name)? {
        return Ok(false);
    }
    // SAFETY: a valid pointer and length for the duration of the call.
    let r = unsafe { libc::sethostname(name.as_ptr().cast(), name.len()) };
    if r != 0 {
        return Err(std::io::Error::last_os_error()).context("sethostname");
    }
    Ok(true)
}

/// Rename a running satellite through systemd-hostnamed, which tells
/// systemd-resolved to announce the new name at once. Blocking.
pub fn set_hostname_live(name: &str) -> Result<bool> {
    if !needs_hostname(name)? {
        return Ok(false);
    }
    let out = std::process::Command::new("hostnamectl")
        .args(["--transient", "hostname", name])
        .output()
        .context("running hostnamectl")?;
    anyhow::ensure!(
        out.status.success(),
        "hostnamectl: {}",
        String::from_utf8_lossy(&out.stderr).trim()
    );
    Ok(true)
}

/// The model Home Assistant shows, from `board_model()`'s id. Unknown
/// hardware (a VM, a PC) reports as the Echo Dot, like `board_model()`.
pub fn model_name(model: &str) -> &'static str {
    match model {
        "echo-2" => "Echo (2nd Gen)",
        _ => "Echo Dot (2nd Gen)",
    }
}

/// The name until one is chosen (ADR 0007): the model and the last three MAC
/// bytes, the way Home Assistant Voice PE names itself.
pub fn default_friendly_name(model: &str, mac: &[u8; 6]) -> String {
    let short = match model {
        "echo-2" => "Echo",
        _ => "Echo Dot",
    };
    format!("{short} {:02x}{:02x}{:02x}", mac[3], mac[4], mac[5])
}

/// The speaker profile follows the hardware unless a developer set one.
pub fn speaker_flat(profile: Option<&str>, model: &str) -> bool {
    match profile {
        Some(p) => p.eq_ignore_ascii_case("flat"),
        None => model != "echo-2",
    }
}

fn quote_txt(k: &str, v: &str) -> String {
    let kv = format!("{k}={v}");
    if kv
        .chars()
        .any(|c| c.is_whitespace() || c == '"' || c == '\\')
    {
        format!("\"{}\"", kv.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        kv
    }
}

/// systemd.dnssd(5) unit advertising the ESPHome native API. The instance is
/// named `%H`, the host name resolved publishes, so resolved moves the record
/// with a rename by itself and withdraws the old one (ADR 0024): a rename
/// never rewrites this file.
pub fn dnssd_unit(cfg: &DeviceConfig) -> String {
    let txt: Vec<String> = mdns_txt(cfg).iter().map(|(k, v)| quote_txt(k, v)).collect();
    format!(
        "# Written by pinnad.\n[Service]\nName=%H\nType=_esphomelib._tcp\nPort={}\nTxtText={}\n",
        cfg.port,
        txt.join(" ")
    )
}

/// Publish via resolved: write /run/systemd/dnssd/ and have resolved read it.
pub fn publish_mdns(cfg: &DeviceConfig) -> Result<()> {
    if !Path::new("/run/systemd/resolve").exists() {
        tracing::info!("systemd-resolved not running; skipping mDNS publication");
        return Ok(());
    }
    let dir = Path::new("/run/systemd/dnssd");
    std::fs::create_dir_all(dir)?;
    let path = dir.join("pinna-esphome.dnssd");
    let unit = dnssd_unit(cfg);
    let old = std::fs::read_to_string(&path).ok();
    if old.as_deref() != Some(unit.as_str()) {
        std::fs::write(&path, &unit).with_context(|| format!("writing {}", path.display()))?;
        tracing::info!("publishing _esphomelib._tcp");
        reread_dnssd(old.is_some());
    }
    Ok(())
}

/// The host name systemd-resolved announces over mDNS, `<name>.local`. It
/// differs from the hostname after resolved lost a name conflict and picked
/// another (ADR 0024). `None` without resolved or busctl. Blocking.
pub fn published_hostname() -> Option<String> {
    let out = std::process::Command::new("busctl")
        .args([
            "get-property",
            "org.freedesktop.resolve1",
            "/org/freedesktop/resolve1",
            "org.freedesktop.resolve1.Manager",
            "LLMNRHostname",
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    busctl_string(&String::from_utf8_lossy(&out.stdout))
}

/// A string property as busctl prints it: `s "echo2"`.
fn busctl_string(out: &str) -> Option<String> {
    let name = out.trim().strip_prefix("s \"")?.strip_suffix('"')?;
    (!name.is_empty()).then(|| name.to_owned())
}

/// How often the announced name is compared with the hostname.
const NAME_CHECK: Duration = Duration::from_secs(60);
/// How long after putting the name back pinnad waits before doing it again.
const NAME_REPAIR_EVERY: Duration = Duration::from_secs(600);

/// Compares the announced name with the hostname, once a minute.
#[derive(Debug, Default)]
struct NameWatch {
    /// Checks in a row that found another name.
    missed: u8,
    repaired: Option<Instant>,
}

impl NameWatch {
    /// Whether to restart resolved now: on the second check in a row that
    /// finds another name (a rename shows a different name for a moment), and
    /// at most once every ten minutes, so a network that keeps answering for
    /// the name doesn't make the satellite announce itself without end.
    fn check(&mut self, hostname: &str, published: &str, now: Instant) -> bool {
        if published.eq_ignore_ascii_case(hostname) {
            self.missed = 0;
            return false;
        }
        self.missed = self.missed.saturating_add(1);
        if self.missed < 2
            || self
                .repaired
                .is_some_and(|t| now.duration_since(t) < NAME_REPAIR_EVERY)
        {
            return false;
        }
        self.missed = 0;
        self.repaired = Some(now);
        true
    }
}

/// Put back a host name that resolved changed after a name conflict (ADR
/// 0024). A restart reads the hostname and the files again; setting the
/// hostname again wouldn't reset a renamed service instance. Runs for as long
/// as pinnad does.
pub async fn watch_name() {
    let mut watch = NameWatch::default();
    loop {
        tokio::time::sleep(NAME_CHECK).await;
        let hostname = crate::network::hostname();
        let Ok(Some(published)) = tokio::task::spawn_blocking(published_hostname).await else {
            continue;
        };
        if !hostname.is_empty() && watch.check(&hostname, &published, Instant::now()) {
            tracing::warn!(
                "mDNS: systemd-resolved announces {published}.local instead of {hostname}.local \
                 after a name conflict; restarting it"
            );
            reread_dnssd(true);
        }
    }
}

/// How long requests gather before resolved reads the files once.
const REREAD_BATCH: Duration = Duration::from_secs(1);

/// Have systemd-resolved read the `.dnssd` files again (ADR 0024). `replaced`:
/// a file that resolved already read changed or went away. resolved 257
/// keeps the records of a reloaded file published, so that takes a restart;
/// a new file only needs a reload, which doesn't announce the host name again
/// (every boot writes the files anew). Requests within a second share one
/// reload or restart: a change to both records (the friendly name) restarts
/// once. Returns at once.
pub fn reread_dnssd(replaced: bool) {
    static TX: OnceLock<Sender<bool>> = OnceLock::new();
    let tx = TX.get_or_init(|| {
        batched(REREAD_BATCH, |restart| {
            let verb = if restart { "restart" } else { "reload" };
            let status = std::process::Command::new("systemctl")
                .args([verb, "systemd-resolved.service"])
                .status();
            match status {
                Ok(s) if s.success() => tracing::info!("systemd-resolved: {verb}"),
                other => tracing::warn!("could not {verb} systemd-resolved: {other:?}"),
            }
        })
    });
    let _ = tx.send(replaced);
}

/// A thread that runs `action` once for each burst of requests (after the
/// first, until none has come for `window`), with whether any of them asked
/// for `true`.
fn batched(window: Duration, action: impl Fn(bool) + Send + 'static) -> Sender<bool> {
    let (tx, rx) = channel::<bool>();
    std::thread::spawn(move || {
        while let Ok(first) = rx.recv() {
            let mut any = first;
            while let Ok(more) = rx.recv_timeout(window) {
                any |= more;
            }
            action(any);
        }
    });
    tx
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mac_parsing() {
        assert_eq!(
            parse_mac("aa:bb:cc:00:11:22\n"),
            Some([0xaa, 0xbb, 0xcc, 0, 0x11, 0x22])
        );
        assert_eq!(parse_mac("nope"), None);
    }

    #[test]
    fn key_file_wins_and_round_trips() {
        let dir = std::env::temp_dir().join(format!("pinnad-key-{}", std::process::id()));
        let file = dir.join("esphome-key");
        let mut cfg = config::Esphome {
            key_file: file.clone(),
            ..Default::default()
        };
        assert_eq!(load_encryption_key(&cfg).unwrap(), None);
        cfg.encryption_key = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=".into();
        let configured = load_encryption_key(&cfg).unwrap().unwrap();
        assert_eq!(configured[1], 1);
        let pushed = [7u8; 32];
        store_encryption_key(&file, &pushed).unwrap();
        assert_eq!(load_encryption_key(&cfg).unwrap(), Some(pushed));
        cfg.encryption_key = "not base64".into();
        std::fs::remove_file(&file).unwrap();
        assert!(load_encryption_key(&cfg).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn node_names_make_hostnames() {
        let mac = [0xf0, 0x27, 0x2d, 0x2a, 0x91, 0x4e];
        assert_eq!(node_name(None, "", &mac), "pinna-2a914e");
        assert_eq!(node_name(None, "kitchen-dot", &mac), "kitchen-dot");
        assert_eq!(node_name(Some("den"), "kitchen-dot", &mac), "den");
        assert_eq!(node_name(Some(""), "", &mac), "pinna-2a914e");
        assert!(valid_hostname("pinna-2a914e"));
        for bad in ["", "-x", "x-", "a.b", "küche", &"a".repeat(64)] {
            assert!(!valid_hostname(bad), "{bad:?}");
        }
        assert!(set_hostname("not a hostname").is_err());
    }

    #[test]
    fn pinna_image_from_os_release() {
        assert!(is_pinna_image("NAME=x\nIMAGE_ID=pinna\n"));
        assert!(is_pinna_image("IMAGE_ID=\"pinna-dev\"\n"));
        assert!(is_pinna_image("IMAGE_ID='pinna'\n"));
        assert!(!is_pinna_image("IMAGE_ID=pinnacle\n"));
        assert!(!is_pinna_image("ID=pinna\nNAME=pinna\n"));
        assert!(!is_pinna_image(""));
    }

    #[test]
    fn names_and_profile_follow_the_hardware() {
        let mac = [0xf0, 0x27, 0x2d, 0x2a, 0x91, 0x4e];
        assert_eq!(default_friendly_name("echo-dot-2", &mac), "Echo Dot 2a914e");
        assert_eq!(default_friendly_name("echo-2", &mac), "Echo 2a914e");
        assert_eq!(default_friendly_name("", &mac), "Echo Dot 2a914e");
        assert_eq!(model_name("echo-2"), "Echo (2nd Gen)");
        assert_eq!(model_name("echo-dot-2"), "Echo Dot (2nd Gen)");
        assert!(!speaker_flat(None, "echo-2"));
        assert!(speaker_flat(None, "echo-dot-2"));
        assert!(speaker_flat(None, ""));
        assert!(speaker_flat(Some("Flat"), "echo-2"));
        assert!(!speaker_flat(Some("Radar"), "echo-dot-2"));
    }

    #[test]
    fn dnssd_quotes_spaces() {
        let mut cfg = DeviceConfig::new("pinna-0a0b0c", [2, 0, 0, 0xa, 0xb, 0xc]);
        cfg.friendly_name = "Echo Dot".into();
        let unit = dnssd_unit(&cfg);
        assert!(unit.contains("\nName=%H\n"), "{unit}");
        assert!(unit.contains("Type=_esphomelib._tcp"));
        assert!(unit.contains("Port=6053"));
        cfg.webserver_port = 80;
        cfg.dashboard_url = Some("https://dot.example.com:8443/".into());
        let unit = dnssd_unit(&cfg);
        assert!(unit.contains(" web_port=80"), "{unit}");
        assert!(
            unit.contains(" url=https://dot.example.com:8443/"),
            "{unit}"
        );
        assert!(unit.contains("\"friendly_name=Echo Dot\""));
        assert!(unit.contains("mac=02:00:00:0a:0b:0c"));
    }

    #[test]
    fn requests_close_together_share_one_run() {
        use std::sync::{Arc, Mutex};
        let runs = Arc::new(Mutex::new(Vec::new()));
        let seen = runs.clone();
        let tx = batched(Duration::from_millis(100), move |restart| {
            seen.lock().unwrap().push(restart);
        });
        let burst = |asks: &[bool]| {
            for &a in asks {
                tx.send(a).unwrap();
                std::thread::sleep(Duration::from_millis(20));
            }
            std::thread::sleep(Duration::from_millis(300));
        };
        burst(&[false, true, false]);
        assert_eq!(*runs.lock().unwrap(), [true], "one run, a restart");
        burst(&[false, false]);
        assert_eq!(*runs.lock().unwrap(), [true, false], "new files: a reload");
    }

    #[test]
    fn reads_the_announced_name() {
        assert_eq!(busctl_string("s \"echo8\"\n").as_deref(), Some("echo8"));
        assert_eq!(busctl_string("s \"\""), None);
        assert_eq!(busctl_string("b true"), None);
    }

    #[test]
    fn a_renamed_host_is_put_back_on_the_second_check_at_most_every_ten_minutes() {
        let mut w = NameWatch::default();
        let t0 = Instant::now();
        let at = |m: u64| t0 + Duration::from_secs(60 * m);
        assert!(!w.check("echo2", "echo2", at(0)));
        assert!(
            !w.check("echo2", "echo8", at(1)),
            "a rename may be under way"
        );
        assert!(w.check("echo2", "echo8", at(2)));
        // Still renamed after the restart: not again within ten minutes.
        for m in 3..12 {
            assert!(!w.check("echo2", "echo9", at(m)), "minute {m}");
        }
        assert!(w.check("echo2", "echo9", at(12)));
        // Back to normal resets the count.
        assert!(!w.check("echo2", "ECHO2", at(13)));
        assert!(!w.check("echo2", "echo3", at(30)));
    }

    #[test]
    fn a_rename_leaves_the_record_alone() {
        let mut cfg = DeviceConfig::new("kitchen", [2, 0, 0, 0xa, 0xb, 0xc]);
        let before = dnssd_unit(&cfg);
        cfg.name = "hall".into();
        assert_eq!(dnssd_unit(&cfg), before);
    }
}
