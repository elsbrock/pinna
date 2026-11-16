//! Network interfaces for the dashboard: `ip -j addr show`, and `iw dev <if> link`
//! for Wi-Fi.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use pinna_api::{Interface, Wifi};
use serde::Deserialize;
use tokio::process::Command;

const TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Deserialize)]
struct IpLink {
    ifname: String,
    #[serde(default)]
    address: String,
    #[serde(default)]
    operstate: String,
    #[serde(default)]
    flags: Vec<String>,
    #[serde(default)]
    addr_info: Vec<AddrInfo>,
}

#[derive(Deserialize)]
struct AddrInfo {
    local: String,
    prefixlen: u32,
    #[serde(default)]
    scope: String,
}

/// The interfaces `ip -j addr show` lists, without loopback. `wireless` and
/// `wifi` are left for the caller.
pub fn parse_ip_addr(json: &str) -> Result<Vec<Interface>> {
    let links: Vec<IpLink> = serde_json::from_str(json).context("parsing ip output")?;
    Ok(links
        .into_iter()
        .filter(|l| l.ifname != "lo")
        .map(|l| Interface {
            up: l.operstate == "UP"
                || (l.operstate == "UNKNOWN" && l.flags.iter().any(|f| f == "LOWER_UP")),
            addresses: l
                .addr_info
                .iter()
                .filter(|a| a.scope != "link")
                .map(|a| format!("{}/{}", a.local, a.prefixlen))
                .collect(),
            name: l.ifname,
            mac: l.address,
            ..Default::default()
        })
        .collect())
}

/// The link of `iw dev <if> link`; `None` when not connected.
pub fn parse_iw_link(text: &str) -> Option<Wifi> {
    if text.trim_start().starts_with("Not connected") {
        return None;
    }
    let mut wifi = Wifi::default();
    for line in text.lines() {
        let line = line.trim();
        if let Some(v) = line.strip_prefix("SSID: ") {
            wifi.ssid = v.to_owned();
        } else if let Some(v) = line.strip_prefix("freq: ") {
            wifi.frequency_mhz = v.parse::<f64>().ok()? as u32;
        } else if let Some(v) = line.strip_prefix("signal: ") {
            wifi.signal_dbm = v.split_whitespace().next()?.parse().ok()?;
        } else if let Some(v) = line.strip_prefix("tx bitrate: ") {
            wifi.tx_bitrate = match v.find("MBit/s") {
                Some(i) => v[..i + "MBit/s".len()].to_owned(),
                None => v.to_owned(),
            };
        }
    }
    Some(wifi)
}

async fn run(program: &str, args: &[&str]) -> Result<String> {
    let out = tokio::time::timeout(TIMEOUT, Command::new(program).args(args).output())
        .await
        .with_context(|| format!("{program} timed out"))?
        .with_context(|| format!("running {program}"))?;
    if !out.status.success() {
        bail!("{program} exited with {}", out.status);
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

pub fn hostname() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .map(|h| h.trim().to_owned())
        .unwrap_or_default()
}

pub async fn interfaces() -> Result<Vec<Interface>> {
    let mut interfaces = parse_ip_addr(&run("ip", &["-j", "addr", "show"]).await?)?;
    for i in &mut interfaces {
        i.wireless = Path::new("/sys/class/net")
            .join(&i.name)
            .join("wireless")
            .exists();
        if i.wireless
            && let Ok(text) = run("iw", &["dev", &i.name, "link"]).await
        {
            i.wifi = parse_iw_link(&text)
                .map_or_else(buffa::MessageField::none, buffa::MessageField::some);
        }
    }
    Ok(interfaces)
}

/// Every address of `ip -j addr show dev <iface>`, link-local included.
pub fn parse_addresses(json: &str) -> Vec<std::net::IpAddr> {
    serde_json::from_str::<Vec<IpLink>>(json)
        .unwrap_or_default()
        .iter()
        .flat_map(|l| &l.addr_info)
        .filter_map(|a| a.local.parse().ok())
        .collect()
}

/// The addresses `iface` has now; none when `ip` fails.
pub async fn addresses(iface: &str) -> Vec<std::net::IpAddr> {
    match run("ip", &["-j", "addr", "show", "dev", iface]).await {
        Ok(json) => parse_addresses(&json),
        Err(_) => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_of_an_interface_include_link_local() {
        let json = r#"[{"ifname":"wlan0","addr_info":[
            {"local":"192.168.1.20","prefixlen":24,"scope":"global"},
            {"local":"fe80::1","prefixlen":64,"scope":"link"},
            {"local":"2001:db8::5","prefixlen":64,"scope":"global"}]}]"#;
        let got = parse_addresses(json);
        assert_eq!(got.len(), 3);
        assert!(got.contains(&"fe80::1".parse().unwrap()));
        assert!(parse_addresses("not json").is_empty());
    }

    const IP: &str = r#"[
{"ifindex":1,"ifname":"lo","flags":["LOOPBACK","UP","LOWER_UP"],"mtu":65536,"operstate":"UNKNOWN","link_type":"loopback","address":"00:00:00:00:00:00","addr_info":[{"family":"inet","local":"127.0.0.1","prefixlen":8,"scope":"host","label":"lo"}]},
{"ifindex":2,"ifname":"wlan0","flags":["BROADCAST","MULTICAST","UP","LOWER_UP"],"mtu":1500,"operstate":"UP","link_type":"ether","address":"02:00:00:aa:bb:cc","addr_info":[{"family":"inet","local":"192.168.1.20","prefixlen":24,"broadcast":"192.168.1.255","scope":"global","dynamic":true,"label":"wlan0"},{"family":"inet6","local":"fe80::ff:feaa:bbcc","prefixlen":64,"scope":"link"}]},
{"ifindex":3,"ifname":"usb0","flags":["BROADCAST","MULTICAST","UP","LOWER_UP"],"mtu":1500,"operstate":"UNKNOWN","link_type":"ether","address":"02:11:22:33:44:55","addr_info":[{"family":"inet","local":"10.42.0.1","prefixlen":24,"scope":"global","label":"usb0"},{"family":"inet6","local":"fe80::11:22ff:fe33:4455","prefixlen":64,"scope":"link"}]},
{"ifindex":4,"ifname":"eth1","flags":["BROADCAST","MULTICAST"],"mtu":1500,"operstate":"DOWN","link_type":"ether","address":"02:99:99:99:99:99","addr_info":[]}
]"#;

    #[test]
    fn ip_addr_skips_loopback_and_link_local() {
        let ifs = parse_ip_addr(IP).unwrap();
        let names: Vec<_> = ifs.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, ["wlan0", "usb0", "eth1"]);
        assert_eq!(ifs[0].mac, "02:00:00:aa:bb:cc");
        assert!(ifs[0].up);
        assert_eq!(ifs[0].addresses, ["192.168.1.20/24"]);
        // UNKNOWN with LOWER_UP counts as up, as it does for USB gadgets.
        assert!(ifs[1].up);
        assert_eq!(ifs[1].addresses, ["10.42.0.1/24"]);
        assert!(!ifs[2].up && ifs[2].addresses.is_empty());
    }

    #[test]
    fn ip_addr_rejects_garbage() {
        assert!(parse_ip_addr("not json").is_err());
    }

    #[test]
    fn iw_link_connected() {
        let text = "Connected to aa:bb:cc:dd:ee:ff (on wlan0)\n\
            \tSSID: Home Net\n\
            \tfreq: 2437\n\
            \tRX: 1234 bytes (10 packets)\n\
            \tTX: 567 bytes (5 packets)\n\
            \tsignal: -52 dBm\n\
            \trx bitrate: 65.0 MBit/s MCS 6\n\
            \ttx bitrate: 72.2 MBit/s MCS 7 short GI\n\
            \n\
            \tbss flags:\tshort-slot-time\n\
            \tdtim period:\t2\n";
        let w = parse_iw_link(text).unwrap();
        assert_eq!(w.ssid, "Home Net");
        assert_eq!(w.frequency_mhz, 2437);
        assert_eq!(w.signal_dbm, -52);
        assert_eq!(w.tx_bitrate, "72.2 MBit/s");
    }

    #[test]
    fn iw_link_fractional_frequency() {
        let w =
            parse_iw_link("Connected to aa:bb (on wlan0)\n\tSSID: x\n\tfreq: 5180.0\n").unwrap();
        assert_eq!(w.frequency_mhz, 5180);
    }

    #[test]
    fn iw_link_not_connected() {
        assert!(parse_iw_link("Not connected.\n").is_none());
    }
}
