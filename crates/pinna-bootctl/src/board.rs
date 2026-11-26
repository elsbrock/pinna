//! The board's stage conditions (ADR 0012): whether USB and Wi-Fi exist at
//! all, and whether they are up. Shared by the stage helper and pinnad, so
//! both agree on what "skipped" and "done" mean.
//!
//! Every function takes the sysfs (or device-tree) root it looks in, so tests
//! can use a temporary directory.

use std::fs;
use std::path::Path;

use pinna_board::Board;

pub const UDC_DIR: &str = "/sys/class/udc";
pub const GADGET_DIR: &str = "/sys/kernel/config/usb_gadget/pinna";
pub const NET_DIR: &str = "/sys/class/net";

/// A UDC to bind the gadget to exists: boards without USB gadget support
/// (the VMs) skip the USB stage instead of failing it.
pub fn usb_present(udc_dir: &Path) -> bool {
    fs::read_dir(udc_dir).is_ok_and(|mut d| d.next().is_some())
}

/// The gadget is bound to a UDC (`.../usb_gadget/pinna/UDC` is non-empty).
pub fn usb_bound(gadget_dir: &Path) -> bool {
    fs::read_to_string(gadget_dir.join("UDC")).is_ok_and(|s| !s.trim().is_empty())
}

/// This is a board Pinna knows, and every such board has a Wi-Fi chip whose
/// driver brings up `wlan0`, so the Wi-Fi stage applies. Only the VMs, which
/// have no board, skip it.
pub fn wifi_present(board: Option<&Board>) -> bool {
    board.is_some()
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
            wifi: !wifi_present(pinna_board::detect()),
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
    fn wifi_present_for_every_known_board() {
        assert!(!wifi_present(None));
        assert!(wifi_present(Some(&pinna_board::ECHO_DOT_2)));
        assert!(wifi_present(Some(&pinna_board::ECHO_DOT_5)));
    }

    #[test]
    fn wlan_up_needs_the_interface() {
        let d = tempfile::tempdir().unwrap();
        assert!(!wlan_up(d.path()));
        fs::create_dir(d.path().join("wlan0")).unwrap();
        assert!(wlan_up(d.path()));
    }
}
