//! A machine id per device (ADR 0015), derived from the idme serial number,
//! so that Dots running the same image don't share one (DHCP client ID,
//! journal). Without a serial (the VMs), the image's machine id stays.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use anyhow::{Context, Result, ensure};
use sha2::{Digest, Sha256};

pub const SERIAL_PATHS: [&str; 2] = [
    "/sys/firmware/devicetree/base/idme/serial/value",
    "/proc/idme/serial",
];

/// Letters and digits only; None when nothing is left.
pub fn clean_serial(raw: &[u8]) -> Option<String> {
    let cleaned: String = raw
        .iter()
        .filter(|b| b.is_ascii_alphanumeric())
        .map(|&b| b as char)
        .collect();
    (!cleaned.is_empty()).then_some(cleaned)
}

/// The first readable, non-empty serial from `paths`.
pub fn read_serial(paths: &[&str]) -> Option<String> {
    for path in paths {
        if let Ok(raw) = fs::read(path)
            && let Some(serial) = clean_serial(&raw)
        {
            return Some(serial);
        }
    }
    None
}

/// 32 lowercase hex digits: sha256("pinna machine-id v1\n" + serial), first
/// 16 bytes, with the version 4 and RFC 4122 variant bits set
/// (byte 6 = (b & 0x0f) | 0x40, byte 8 = (b & 0x3f) | 0x80).
pub fn derive(serial: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(b"pinna machine-id v1\n");
    hash.update(serial.as_bytes());
    let digest = hash.finalize();
    let mut id = [0u8; 16];
    id.copy_from_slice(&digest[..16]);
    id[6] = (id[6] & 0x0f) | 0x40;
    id[8] = (id[8] & 0x3f) | 0x80;
    id.iter().map(|b| format!("{b:02x}")).collect()
}

/// Writes `<run_dir>/machine-id` (the id and a newline, mode 0444) and
/// bind-mounts it over `<sysroot>/etc/machine-id`. Ok(None) when the device
/// has no serial number.
pub fn apply(sysroot: &Path, run_dir: &Path) -> Result<Option<String>> {
    let Some(serial) = read_serial(&SERIAL_PATHS) else {
        return Ok(None);
    };
    let id = derive(&serial);
    let target = sysroot.join("etc/machine-id");
    ensure!(
        target.is_file(),
        "{} is missing in the root",
        target.display()
    );
    let file = run_dir.join("machine-id");
    fs::write(&file, format!("{id}\n")).with_context(|| format!("writing {}", file.display()))?;
    fs::set_permissions(&file, fs::Permissions::from_mode(0o444))
        .with_context(|| format!("chmod {}", file.display()))?;
    crate::mount(
        file.to_str().context("machine-id path is not UTF-8")?,
        target.to_str().context("machine-id path is not UTF-8")?,
        None,
        libc::MS_BIND,
        None,
    )?;
    Ok(Some(id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_serial_drops_everything_but_letters_and_digits() {
        assert_eq!(
            clean_serial(b"G090LF0971521APS\0\0"),
            Some("G090LF0971521APS".to_owned())
        );
        assert_eq!(clean_serial(b"\0\0"), None);
        assert_eq!(clean_serial(b" \n"), None);
    }

    #[test]
    fn derive_is_a_stable_version_4_id() {
        // printf 'pinna machine-id v1\nG090LF0971521APS' | sha256sum
        // 1702cac0e2c4865d2b39f74d2d079f71cd8e0ca3bcdaa55f2351afe11d7d95f6
        // Take the first 32 hex digits (16 bytes); byte 6:
        // (0x86 & 0x0f) | 0x40 = 0x46; byte 8: (0x2b & 0x3f) | 0x80 = 0xab.
        let id = derive("G090LF0971521APS");
        assert_eq!(id, "1702cac0e2c4465dab39f74d2d079f71");
        assert_eq!(id.len(), 32);
        assert!(
            id.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        );
        assert_eq!(id.as_bytes()[12], b'4');
        assert!(matches!(id.as_bytes()[16], b'8' | b'9' | b'a' | b'b'));
        assert_eq!(derive("G090LF0971521APS"), id);
        assert_ne!(derive("G090LF0971522APS"), id);
    }

    #[test]
    fn read_serial_prefers_the_first_usable_path() {
        let dir = std::env::temp_dir().join(format!("pinna-machine-id-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let (a, b) = (dir.join("a"), dir.join("b"));
        let (pa, pb) = (a.to_str().unwrap(), b.to_str().unwrap());

        // First path missing, second present.
        let _ = fs::remove_file(&a);
        fs::write(&b, b"G090LF0971521APS\0\0").unwrap();
        assert_eq!(read_serial(&[pa, pb]), Some("G090LF0971521APS".to_owned()));

        // First present but only NULs, second present.
        fs::write(&a, b"\0\0").unwrap();
        assert_eq!(read_serial(&[pa, pb]), Some("G090LF0971521APS".to_owned()));

        // Both missing.
        let _ = fs::remove_file(&a);
        let _ = fs::remove_file(&b);
        assert_eq!(read_serial(&[pa, pb]), None);

        fs::remove_dir_all(&dir).unwrap();
    }
}
