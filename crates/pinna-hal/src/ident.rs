//! Device identity: Wi-Fi MAC and per-microphone calibration factors. Which
//! board this is: [`crate::board::detect`].

use std::path::Path;

use crate::board::{DT_IDME_DIR, IDME_DIR};

pub const MICCAL_UNITY: i32 = 16384;
pub const MIC_CHANNELS: usize = 7;
const WLAN0_ADDRESS: &str = "/sys/class/net/wlan0/address";
const DT_IDME_MAC: &str = "/sys/firmware/devicetree/base/idme/mac_addr/value";

/// The Wi-Fi MAC from the bootloader's idme record (device tree
/// `idme/mac_addr`, 12 hex digits), lowercase colon-separated. The Wi-Fi
/// driver gives wlan0 this address; reading it here does not wait for Wi-Fi.
pub fn idme_mac() -> Option<String> {
    idme_mac_from(Path::new(DT_IDME_MAC))
}

pub fn idme_mac_from(path: &Path) -> Option<String> {
    let raw = std::fs::read(path).ok()?;
    let s = std::str::from_utf8(&raw)
        .ok()?
        .trim_end_matches('\0')
        .trim();
    let ok = s.len() == 12
        && s.bytes().all(|b| b.is_ascii_hexdigit())
        && s.bytes().any(|b| b != b'0')
        && !s.eq_ignore_ascii_case("ffffffffffff");
    if !ok {
        return None;
    }
    let s = s.to_ascii_lowercase();
    let octets: Vec<&str> = (0..6).map(|i| &s[2 * i..2 * i + 2]).collect();
    Some(octets.join(":"))
}

/// MAC address of wlan0, lowercase colon-separated.
pub fn wlan0_mac() -> Option<String> {
    mac_from(Path::new(WLAN0_ADDRESS))
}

pub fn mac_from(path: &Path) -> Option<String> {
    let s = std::fs::read_to_string(path).ok()?;
    let s = s.trim().to_ascii_lowercase();
    let ok = s.len() == 17
        && s.split(':').count() == 6
        && s.split(':')
            .all(|o| o.len() == 2 && o.bytes().all(|b| b.is_ascii_hexdigit()));
    ok.then_some(s)
}

/// Q14 calibration factors for the 7 logical mic channels (16384 = unity).
pub fn read_miccal() -> [i32; MIC_CHANNELS] {
    read_miccal_in(Path::new(IDME_DIR), Path::new(DT_IDME_DIR))
}

fn parse_value(text: &str) -> Option<i32> {
    let v: i32 = text.split_whitespace().next()?.parse().ok()?;
    (0..=65535).contains(&v).then_some(v)
}

fn read_value(path: &Path) -> Option<i32> {
    parse_value(&std::fs::read_to_string(path).ok()?)
}

/// Order as micd.c: `idme/miccal.N`, then a whole-file `idme/miccal` list,
/// then `devicetree/idme/miccal.N/value`. Missing channels stay unity.
pub fn read_miccal_in(idme: &Path, dt_idme: &Path) -> [i32; MIC_CHANNELS] {
    let mut out = [MICCAL_UNITY; MIC_CHANNELS];

    let mut count = 0;
    for (i, slot) in out.iter_mut().enumerate() {
        if let Some(v) = read_value(&idme.join(format!("miccal.{i}"))) {
            *slot = v;
            count += 1;
        }
    }
    if count > 0 {
        return out;
    }

    if let Ok(text) = std::fs::read_to_string(idme.join("miccal")) {
        for (slot, tok) in out.iter_mut().zip(text.split_whitespace()) {
            match parse_value(tok) {
                Some(v) => {
                    *slot = v;
                    count += 1;
                }
                None => break,
            }
        }
        if count > 0 {
            return out;
        }
    }

    for (i, slot) in out.iter_mut().enumerate() {
        if let Some(v) = read_value(&dt_idme.join(format!("miccal.{i}")).join("value")) {
            *slot = v;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("pinna-ident-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    /// The calibration is read for 7 microphones (LibreEcho's micd.c).
    #[test]
    fn every_board_has_the_microphones_the_calibration_is_read_for() {
        for m in crate::board::Model::ALL {
            assert_eq!(m.board().mics.count, MIC_CHANNELS, "{m:?}");
        }
    }

    #[test]
    fn default_is_unity() {
        let d = tmp("none");
        assert_eq!(read_miccal_in(&d, &d), [16384; 7]);
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn per_channel_files() {
        let d = tmp("chan");
        fs::write(d.join("miccal.0"), "16000\n").unwrap();
        fs::write(d.join("miccal.3"), "17000\n").unwrap();
        fs::write(d.join("miccal.4"), "70000\n").unwrap(); // out of range: ignored
        let c = read_miccal_in(&d, &d);
        assert_eq!(c, [16000, 16384, 16384, 17000, 16384, 16384, 16384]);
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn whole_file_list() {
        let d = tmp("list");
        fs::write(d.join("miccal"), "1 2 3 4 5 6 7\n").unwrap();
        assert_eq!(read_miccal_in(&d, &d), [1, 2, 3, 4, 5, 6, 7]);
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn devicetree_fallback() {
        let d = tmp("dt");
        let dt = d.join("dt");
        fs::create_dir_all(dt.join("miccal.2")).unwrap();
        fs::write(dt.join("miccal.2").join("value"), "15000").unwrap();
        let c = read_miccal_in(&d.join("idme"), &dt);
        assert_eq!(c[2], 15000);
        assert_eq!(c[0], 16384);
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn idme_mac_parsing() {
        let d = tmp("idme-mac");
        let f = d.join("value");
        fs::write(&f, "40B4CDE32A7F\0").unwrap();
        assert_eq!(idme_mac_from(&f).as_deref(), Some("40:b4:cd:e3:2a:7f"));
        for junk in [
            "000000000000\0",
            "FFFFFFFFFFFF\0",
            "40B4CDE32A\0",
            "40B4CDE32AZZ",
        ] {
            fs::write(&f, junk).unwrap();
            assert_eq!(idme_mac_from(&f), None, "{junk:?}");
        }
        assert_eq!(idme_mac_from(&d.join("missing")), None);
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn mac_parsing() {
        let d = tmp("mac");
        let f = d.join("address");
        fs::write(&f, "AA:bb:cc:00:11:22\n").unwrap();
        assert_eq!(mac_from(&f).as_deref(), Some("aa:bb:cc:00:11:22"));
        fs::write(&f, "garbage\n").unwrap();
        assert_eq!(mac_from(&f), None);
        fs::remove_dir_all(&d).unwrap();
    }
}
