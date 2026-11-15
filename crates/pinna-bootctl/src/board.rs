//! The board's stage conditions (ADR 0012): whether USB and Wi-Fi exist at
//! all, and whether they are up. Shared by the stage helper and pinnad, so
//! both agree on what "skipped" and "done" mean.
//!
//! Every function takes the sysfs (or device-tree) root it looks in, so tests
//! can use a temporary directory.

use std::fs;
use std::path::Path;

pub const UDC_DIR: &str = "/sys/class/udc";
pub const GADGET_DIR: &str = "/sys/kernel/config/usb_gadget/pinna";
pub const DT_COMPATIBLE: &str = "/sys/firmware/devicetree/base/compatible";
pub const NET_DIR: &str = "/sys/class/net";
/// The board string `pinna-wifi.service` is conditioned on.
pub const MT8163: &str = "mediatek,mt8163";

/// A UDC to bind the gadget to exists: boards without USB gadget support
/// (the VMs) skip the USB stage instead of failing it.
pub fn usb_present(udc_dir: &Path) -> bool {
    fs::read_dir(udc_dir).is_ok_and(|mut d| d.next().is_some())
}

/// The gadget is bound to a UDC (`.../usb_gadget/pinna/UDC` is non-empty).
pub fn usb_bound(gadget_dir: &Path) -> bool {
    fs::read_to_string(gadget_dir.join("UDC")).is_ok_and(|s| !s.trim().is_empty())
}

/// The device tree says this is an MT8163 board, so `pinna-wifi.service` will
/// run and the Wi-Fi stage applies.
pub fn wifi_present(compatible: &Path) -> bool {
    let bytes = fs::read(compatible).unwrap_or_default();
    bytes.split(|b| *b == 0).any(|s| s == MT8163.as_bytes())
}

/// The Wi-Fi driver is up: `wlan0` exists.
pub fn wlan_up(net_dir: &Path) -> bool {
    net_dir.join("wlan0").exists()
}

/// What this board skips, evaluated once at startup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Skips {
    pub usb: bool,
    pub wifi: bool,
}

impl Skips {
    pub fn detect() -> Self {
        Skips {
            usb: !usb_present(Path::new(UDC_DIR)),
            wifi: !wifi_present(Path::new(DT_COMPATIBLE)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usb_present_reads_the_udc_dir() {
        let d = tempfile::tempdir().unwrap();
        assert!(!usb_present(&d.path().join("nonexistent")));
        assert!(!usb_present(d.path()));
        fs::create_dir(d.path().join("musb-hdrc.3.auto")).unwrap();
        assert!(usb_present(d.path()));
    }

    #[test]
    fn usb_bound_reads_the_udc_attribute() {
        let d = tempfile::tempdir().unwrap();
        assert!(!usb_bound(d.path()));
        fs::write(d.path().join("UDC"), "\n").unwrap();
        assert!(!usb_bound(d.path()));
        fs::write(d.path().join("UDC"), "musb-hdrc.3.auto\n").unwrap();
        assert!(usb_bound(d.path()));
    }

    #[test]
    fn wifi_present_matches_the_compatible_list() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("compatible");
        assert!(!wifi_present(&f));
        fs::write(&f, b"amazon,radar-puffin\0mediatek,mt8163\0").unwrap();
        assert!(wifi_present(&f));
        fs::write(&f, b"mediatek,mt8163-extra\0").unwrap();
        assert!(!wifi_present(&f));
    }

    #[test]
    fn wlan_up_needs_the_interface() {
        let d = tempfile::tempdir().unwrap();
        assert!(!wlan_up(d.path()));
        fs::create_dir(d.path().join("wlan0")).unwrap();
        assert!(wlan_up(d.path()));
    }
}
