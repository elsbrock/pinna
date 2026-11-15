//! The raw sysfs path to the LED ring during boot (ADR 0012).
//!
//! pinna-init and the stage helper draw the boot-stage frames themselves, as
//! the ring driver takes them, without the output shaping pinnad applies
//! later. Frames are [`crate::stages::Frame`]s; see [`crate::stages::encode`].

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Where the ring's I2C device shows up in sysfs.
pub const SYSFS_I2C_DIR: &str = "/sys/bus/i2c/devices";

/// Find the ring's `frame` attribute and take it over from the kernel's boot
/// animation, as pinnad does when it starts. `dir` is [`SYSFS_I2C_DIR`].
pub fn find(dir: &Path) -> Option<PathBuf> {
    let mut entries: Vec<_> = fs::read_dir(dir).ok()?.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    entries.into_iter().find_map(|e| {
        let frame = e.path().join("frame");
        if !frame.is_file() {
            return None;
        }
        let _ = fs::write(e.path().join("boot_animation"), "0\n");
        Some(frame)
    })
}

/// Write one frame to the ring.
pub fn write(path: &Path, frame: &crate::stages::Frame) -> std::io::Result<()> {
    fs::OpenOptions::new()
        .write(true)
        .open(path)?
        .write_all(crate::stages::encode(frame).as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stages;
    use std::time::Duration;

    #[test]
    fn finds_and_claims_the_ring() {
        let d = tempfile::tempdir().unwrap();
        fs::create_dir_all(d.path().join("0-0050")).unwrap();
        let ring = d.path().join("0-003f");
        fs::create_dir_all(&ring).unwrap();
        fs::write(ring.join("frame"), "").unwrap();
        fs::write(ring.join("boot_animation"), "1\n").unwrap();
        assert_eq!(find(d.path()), Some(ring.join("frame")));
        assert_eq!(
            fs::read_to_string(ring.join("boot_animation")).unwrap(),
            "0\n"
        );
        assert_eq!(find(&d.path().join("0-0050")), None);
    }

    #[test]
    fn frame_written_in_driver_format() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("frame");
        fs::write(&f, "").unwrap();
        write(&f, &stages::progress(stages::STAGES, Duration::ZERO)).unwrap();
        let s = fs::read_to_string(&f).unwrap();
        assert_eq!(s, stages::encode(&stages::progress(7, Duration::ZERO)));
    }
}
