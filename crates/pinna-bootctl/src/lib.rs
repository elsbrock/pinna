//! The Android bootloader message `command` field in `misc`, as honoured by
//! the Kaeru-patched LK on the Echo devices.
//!
//! Kaeru reads the 32-byte command at `misc` offset 0 and enters fastboot for
//! `bootonce-bootloader`, clearing it itself (one-shot). Nothing else in the
//! partition is touched; the slot record at 0x360 lives in a later sector,
//! see [`bcb`].

pub mod bcb;
pub mod board;
pub mod reboot;
pub mod ring;
pub mod slot;
pub mod stages;

use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::FileExt;
use std::path::Path;

use anyhow::{Context, Result, bail, ensure};

pub const COMMAND_SIZE: usize = 32;
pub const SECTOR: usize = 512;
pub const BOOTONCE_BOOTLOADER: &[u8] = b"bootonce-bootloader";
const MTK_MAGIC: [u8; 4] = [0x88, 0x16, 0x88, 0x58];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Empty,
    BootonceBootloader,
    Other(Vec<u8>),
}

pub fn parse_command(field: &[u8]) -> Command {
    let field = &field[..COMMAND_SIZE];
    if field.iter().all(|&b| b == 0) {
        return Command::Empty;
    }
    let n = BOOTONCE_BOOTLOADER.len();
    if &field[..n] == BOOTONCE_BOOTLOADER && field[n..].iter().all(|&b| b == 0) {
        return Command::BootonceBootloader;
    }
    let end = field.iter().position(|&b| b == 0).unwrap_or(COMMAND_SIZE);
    Command::Other(field[..end].to_vec())
}

/// Is the Kaeru LK payload present in `expdb`? Read-only.
pub fn kaeru_present(expdb: &Path) -> Result<bool> {
    let mut f = match std::fs::File::open(expdb) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e).with_context(|| format!("opening {}", expdb.display())),
    };
    let mut hdr = [0u8; 48];
    if f.read_exact(&mut hdr).is_err() {
        return Ok(false);
    }
    Ok(hdr[..4] == MTK_MAGIC && &hdr[8..11] == b"LK\0")
}

pub fn read_command(misc: &Path) -> Result<Command> {
    let f = std::fs::File::open(misc).with_context(|| format!("opening {}", misc.display()))?;
    let mut sector = [0u8; SECTOR];
    f.read_exact_at(&mut sector, 0)?;
    Ok(parse_command(&sector))
}

fn write_command(misc: &Path, value: &[u8]) -> Result<()> {
    let mut f = OpenOptions::new()
        .read(true)
        .write(true)
        .open(misc)
        .with_context(|| format!("opening {} for writing", misc.display()))?;
    let mut before = vec![0u8; SECTOR * 2];
    f.read_exact_at(&mut before, 0)?;
    let mut sector = before[..SECTOR].to_vec();
    sector[..COMMAND_SIZE].fill(0);
    sector[..value.len()].copy_from_slice(value);
    f.seek(SeekFrom::Start(0))?;
    f.write_all(&sector)?;
    f.sync_all()?;
    let mut after = vec![0u8; SECTOR * 2];
    f.read_exact_at(&mut after, 0)?;
    ensure!(after[..SECTOR] == sector[..], "misc readback mismatch");
    ensure!(
        after[SECTOR..] == before[SECTOR..],
        "misc beyond the command sector changed"
    );
    Ok(())
}

/// Arm `bootonce-bootloader`. Refuses to overwrite a foreign command.
pub fn arm(misc: &Path) -> Result<()> {
    match read_command(misc)? {
        Command::Empty | Command::BootonceBootloader => write_command(misc, BOOTONCE_BOOTLOADER),
        Command::Other(c) => bail!(
            "misc holds another bootloader command ({:?}); leaving it untouched",
            String::from_utf8_lossy(&c)
        ),
    }
}

/// Clear our own request. Anything else is left alone. Returns whether it cleared.
pub fn clear(misc: &Path) -> Result<bool> {
    match read_command(misc)? {
        Command::BootonceBootloader => {
            write_command(misc, &[])?;
            Ok(true)
        }
        _ => Ok(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn misc_with(cmd: &[u8]) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        let mut data = vec![0u8; 1025 * 512];
        data[..cmd.len()].copy_from_slice(cmd);
        data[0x360..0x367].copy_from_slice(&[0, b'A', b'B', b'B', 1, 0x8f, 0]);
        f.write_all(&data).unwrap();
        f
    }

    #[test]
    fn arm_and_clear_roundtrip() {
        let f = misc_with(b"");
        arm(f.path()).unwrap();
        assert_eq!(read_command(f.path()).unwrap(), Command::BootonceBootloader);
        arm(f.path()).unwrap(); // idempotent
        assert!(clear(f.path()).unwrap());
        assert_eq!(read_command(f.path()).unwrap(), Command::Empty);
        assert!(!clear(f.path()).unwrap());
        let data = std::fs::read(f.path()).unwrap();
        assert_eq!(&data[0x360..0x367], &[0, b'A', b'B', b'B', 1, 0x8f, 0]);
    }

    #[test]
    fn foreign_command_untouched() {
        let f = misc_with(b"boot-recovery");
        assert!(arm(f.path()).is_err());
        assert!(!clear(f.path()).unwrap());
        assert_eq!(
            read_command(f.path()).unwrap(),
            Command::Other(b"boot-recovery".to_vec())
        );
    }

    #[test]
    fn kaeru_detection() {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        let mut hdr = vec![0u8; 512];
        hdr[..4].copy_from_slice(&MTK_MAGIC);
        hdr[8..11].copy_from_slice(b"LK\0");
        f.write_all(&hdr).unwrap();
        assert!(kaeru_present(f.path()).unwrap());
        assert!(!kaeru_present(Path::new("/nonexistent/expdb")).unwrap());
    }
}
