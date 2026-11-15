//! The reason for an intentional reboot (ADR 0012).
//!
//! Whatever asks for a reboot records why in `/run/pinna/reboot-reason`. The
//! systemd-shutdown hook (`usr/lib/systemd/system-shutdown/pinna-reboot-reason`)
//! then writes `pinna: reboot (<reason>)` to pstore, so the next boot can say
//! how the last one ended — see pinnad's "the previous boot".

use std::fs;
use std::io;
use std::path::Path;

pub const REASON_FILE: &str = "reboot-reason";

/// Record `reason` for the reboot that is about to happen. Best effort: an
/// unwritable `/run` must not stop the reboot.
pub fn record_at(run_dir: &Path, reason: &str) -> io::Result<()> {
    fs::create_dir_all(run_dir)?;
    fs::write(run_dir.join(REASON_FILE), format!("{reason}\n"))
}

/// The same in the default run directory.
pub fn record(reason: &str) -> io::Result<()> {
    record_at(Path::new(crate::slot::RUN_DIR), reason)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_the_reason() {
        let d = tempfile::tempdir().unwrap();
        record_at(&d.path().join("pinna"), "update").unwrap();
        assert_eq!(
            fs::read_to_string(d.path().join("pinna").join(REASON_FILE)).unwrap(),
            "update\n"
        );
    }
}
