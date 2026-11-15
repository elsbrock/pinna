//! Device identity: Wi-Fi MAC and per-microphone calibration factors.

use std::path::Path;

pub const MICCAL_UNITY: i32 = 16384;
pub const MIC_CHANNELS: usize = 7;
const IDME_DIR: &str = "/proc/idme";
const DT_IDME_DIR: &str = "/sys/firmware/devicetree/base/idme";
const DT_COMPATIBLE: &str = "/sys/firmware/devicetree/base/compatible";
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

/// The board ids in the bootloader's idme `device_type_id` record: what LK's
/// fastboot reports as `BISCUIT` and `RADAR`.
const BOARD_ID_BISCUIT: &str = "A3S5BH2HU6VAYF";
const BOARD_ID_RADAR: &str = "A7WXQPH584YP";

/// The bootloader's idme `device_type_id`: `/proc/idme/device_type_id` on the
/// previous firmware's 6.1 kernel, LK's `idme/device_type_id` node in the
/// device tree on 6.12. None when the bootloader keeps no record (a VM).
pub fn device_type_id() -> Option<String> {
    device_type_id_in(Path::new(IDME_DIR), Path::new(DT_IDME_DIR))
}

pub fn device_type_id_in(idme: &Path, dt_idme: &Path) -> Option<String> {
    let read = |path: std::path::PathBuf| -> Option<String> {
        let raw = std::fs::read(path).ok()?;
        let s = String::from_utf8(raw).ok()?;
        let s = s.trim_end_matches('\0').trim();
        (!s.is_empty()).then(|| s.to_string())
    };
    read(idme.join("device_type_id")).or_else(|| read(dt_idme.join("device_type_id").join("value")))
}

/// Which hardware this is, as the id the dashboard draws it by; empty when
/// unknown.
///
/// Both MT8163 models — the Echo Dot (2nd gen, "biscuit") and the Echo
/// (2nd gen, "radar") — boot the shared `mt8163-radar-puffin.dtb` (see
/// `kernel/build.sh`), so the device tree only names the chip; the
/// bootloader's idme record tells them apart. A board without one (a VM, an
/// unknown boot chain) reports as the Echo Dot, Pinna's first device.
pub fn board_model() -> &'static str {
    board_model_in(
        Path::new(IDME_DIR),
        Path::new(DT_IDME_DIR),
        Path::new(DT_COMPATIBLE),
    )
}

pub fn board_model_in(idme: &Path, dt_idme: &Path, compatible: &Path) -> &'static str {
    match device_type_id_in(idme, dt_idme) {
        Some(id) if id.eq_ignore_ascii_case(BOARD_ID_BISCUIT) => "echo-dot-2",
        Some(id) if id.eq_ignore_ascii_case(BOARD_ID_RADAR) => "echo-2",
        Some(_) => "",
        None => board_model_from(compatible),
    }
}

pub fn board_model_from(path: &Path) -> &'static str {
    let Ok(bytes) = std::fs::read(path) else {
        return "";
    };
    if bytes.split(|&b| b == 0).any(|s| s == b"mediatek,mt8163") {
        "echo-dot-2"
    } else {
        ""
    }
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
    fn board_model_from_compatible() {
        let d = tmp("model");
        let f = d.join("compatible");
        fs::write(&f, "amazon,whatever\0mediatek,mt8163\0").unwrap();
        assert_eq!(board_model_from(&f), "echo-dot-2");
        fs::write(&f, "acme,board\0").unwrap();
        assert_eq!(board_model_from(&f), "");
        assert_eq!(board_model_from(&d.join("missing")), "");
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn board_model_reads_the_idme_device_type() {
        let d = tmp("board");
        let idme = d.join("idme");
        let dt = d.join("dt");
        let compatible = d.join("compatible");
        fs::create_dir_all(dt.join("device_type_id")).unwrap();
        let dt_value = dt.join("device_type_id").join("value");

        // LK's record in the device tree (6.12), NUL-padded.
        fs::write(&dt_value, "A3S5BH2HU6VAYF\0").unwrap();
        assert_eq!(board_model_in(&idme, &dt, &compatible), "echo-dot-2");
        fs::write(&dt_value, "A7WXQPH584YP\0").unwrap();
        assert_eq!(board_model_in(&idme, &dt, &compatible), "echo-2");

        // The 6.1 kernel's /proc/idme wins; an unknown id stays unknown.
        fs::create_dir_all(&idme).unwrap();
        fs::write(idme.join("device_type_id"), "A7WXQPH584YP\n").unwrap();
        assert_eq!(board_model_in(&idme, &dt, &compatible), "echo-2");
        fs::write(idme.join("device_type_id"), "SOMEBODYELSE\n").unwrap();
        assert_eq!(board_model_in(&idme, &dt, &compatible), "");

        // No idme record at all: the compatible string decides.
        fs::write(&compatible, "amazon,radar-puffin\0mediatek,mt8163\0").unwrap();
        assert_eq!(
            board_model_in(&d.join("none"), &dt.join("none"), &compatible),
            "echo-dot-2"
        );
        fs::write(&compatible, "acme,board\0").unwrap();
        assert_eq!(
            board_model_in(&d.join("none"), &dt.join("none"), &compatible),
            ""
        );
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
