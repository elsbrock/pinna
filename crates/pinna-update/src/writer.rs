//! Writing an image into a slot's partition or UBI volume (ADR 0035,
//! Decision 9).
//!
//! A GPT partition takes writes at any offset and keeps what it isn't
//! written over. A UBI volume is replaced whole: `UBI_IOCVOLUP` with the
//! image's exact size erases it, and the data then goes in, in order, until
//! that many bytes arrived. Either way the data can be read back from
//! [`SlotWriter::file`] once [`SlotWriter::finish`] returned.

use std::fs::{File, OpenOptions};
use std::io::{self, Seek, SeekFrom, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileExt, FileTypeExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use pinna_board::StorageKind;
use pinna_image::ReadAt;

/// Starts the replacement of a UBI volume. A trait so a test can record the
/// call: the ioctl needs a real volume.
pub trait VolumeUpdate: Send + Sync {
    /// Erase the volume and expect exactly `size` bytes next.
    fn start(&self, dev: &File, size: u64) -> io::Result<()>;
}

/// `UBI_IOCVOLUP`, on the kernel's UBI volume character device.
pub struct UbiIoctl;

/// `_IOW('O', 0, __s64)` from `<mtd/ubi-user.h>`.
const UBI_IOCVOLUP: u32 = 0x4008_4F00;

impl VolumeUpdate for UbiIoctl {
    fn start(&self, dev: &File, size: u64) -> io::Result<()> {
        let bytes = i64::try_from(size).map_err(|_| io::ErrorKind::InvalidInput)?;
        // SAFETY: the request takes a pointer to an __s64, which `bytes` is.
        let r = unsafe { libc::ioctl(dev.as_raw_fd(), UBI_IOCVOLUP as _, &bytes) };
        if r == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
}

pub struct SlotWriter<'a> {
    file: File,
    path: PathBuf,
    kind: StorageKind,
    ubi: &'a dyn VolumeUpdate,
    /// The size [`SlotWriter::begin`] announced; `None` before it.
    size: Option<u64>,
    written: u64,
}

impl<'a> SlotWriter<'a> {
    /// Open the slot's node for writing, refusing anything in use. Nothing
    /// is erased or written yet.
    pub fn open(path: &Path, kind: StorageKind, ubi: &'a dyn VolumeUpdate) -> Result<Self> {
        Ok(Self {
            file: open_target(path)?,
            path: path.to_owned(),
            kind,
            ubi,
            size: None,
            written: 0,
        })
    }

    /// What the partition or volume holds.
    pub fn capacity(&self) -> Result<u64> {
        Ok(self.file.size()?)
    }

    /// From here on the previous contents are gone (UBI) or being
    /// overwritten (GPT): `size` bytes follow through [`write`](Self::write).
    pub fn begin(&mut self, size: u64) -> Result<()> {
        // `capacity` moved the position to the end. Rewind before the update
        // starts: a UBI volume refuses to seek while it is updated, and the
        // update is then left unfinished ("vol_cdev_llseek: updating", EBUSY;
        // boot_b on a C2N6L4, 2026-11-26).
        (&self.file)
            .seek(SeekFrom::Start(0))
            .with_context(|| format!("rewinding {}", self.path.display()))?;
        if self.kind == StorageKind::Ubi {
            self.ubi
                .start(&self.file, size)
                .with_context(|| format!("starting the update of {}", self.path.display()))?;
        }
        self.size = Some(size);
        self.written = 0;
        Ok(())
    }

    /// The next bytes of the image.
    pub fn write(&mut self, data: &[u8]) -> Result<()> {
        let size = self.size.context("writing before begin")?;
        ensure!(
            self.written + data.len() as u64 <= size,
            "{} bytes written to {}, which was told to expect {size}",
            self.written + data.len() as u64,
            self.path.display()
        );
        match self.kind {
            StorageKind::Gpt => self.file.write_all_at(data, self.written),
            StorageKind::Ubi => (&self.file).write_all(data),
        }
        .with_context(|| format!("writing {}", self.path.display()))?;
        self.written += data.len() as u64;
        Ok(())
    }

    /// All of the image is in: flush it. A UBI volume that got fewer bytes
    /// than announced stays in its update state, so that is an error.
    pub fn finish(&mut self) -> Result<()> {
        let size = self.size.context("finishing before begin")?;
        ensure!(
            self.written == size,
            "{} got {} of {size} bytes",
            self.path.display(),
            self.written
        );
        self.file.sync_all()?;
        // SAFETY: plain fadvise on an open fd.
        unsafe {
            libc::posix_fadvise(self.file.as_raw_fd(), 0, 0, libc::POSIX_FADV_DONTNEED);
        }
        Ok(())
    }

    /// The device, for reading back what was written.
    pub fn file(&self) -> &File {
        &self.file
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// The slot partition, refusing anything in use (mounted, held by dm-verity).
fn open_target(path: &Path) -> Result<File> {
    let real = path
        .canonicalize()
        .with_context(|| format!("resolving {}", path.display()))?;
    let is_dev = std::fs::metadata(&real)?.file_type().is_block_device();
    if is_dev {
        let name = real.file_name().unwrap().to_string_lossy().into_owned();
        let holders = Path::new("/sys/class/block").join(&name).join("holders");
        if let Ok(mut d) = std::fs::read_dir(&holders) {
            ensure!(
                d.next().is_none(),
                "{} is in use (holders in {})",
                real.display(),
                holders.display()
            );
        }
    }
    // O_EXCL on a block device: exclusive claim, EBUSY if mounted or held.
    OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(if is_dev { libc::O_EXCL } else { 0 })
        .open(&real)
        .with_context(|| format!("opening {} for writing", real.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Records the size of each update it was asked to start, and where the
    /// file stood then: the kernel refuses a seek once an update has started.
    #[derive(Default)]
    struct Recorder(Mutex<Vec<(u64, u64)>>);

    impl VolumeUpdate for Recorder {
        fn start(&self, mut file: &File, size: u64) -> io::Result<()> {
            let at = file.stream_position()?;
            self.0.lock().unwrap().push((size, at));
            Ok(())
        }
    }

    fn volume(len: usize) -> tempfile::NamedTempFile {
        let f = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(f.path(), vec![0xbb; len]).unwrap();
        f
    }

    #[test]
    fn a_ubi_volume_is_started_with_the_exact_size_and_filled_in_order() {
        let v = volume(4096);
        let rec = Recorder::default();
        let mut w = SlotWriter::open(v.path(), StorageKind::Ubi, &rec).unwrap();
        assert_eq!(w.capacity().unwrap(), 4096);
        assert!(rec.0.lock().unwrap().is_empty(), "nothing before begin");
        w.begin(10).unwrap();
        w.write(b"01234").unwrap();
        w.write(b"56789").unwrap();
        w.finish().unwrap();
        // Started at the beginning, after capacity() had moved to the end.
        assert_eq!(*rec.0.lock().unwrap(), [(10, 0)]);
        let mut back = [0u8; 10];
        FileExt::read_exact_at(w.file(), &mut back, 0).unwrap();
        assert_eq!(&back, b"0123456789");
    }

    #[test]
    fn a_gpt_partition_is_written_in_place_without_the_ioctl() {
        let v = volume(4096);
        let rec = Recorder::default();
        let mut w = SlotWriter::open(v.path(), StorageKind::Gpt, &rec).unwrap();
        w.begin(4).unwrap();
        w.write(b"ab").unwrap();
        w.write(b"cd").unwrap();
        w.finish().unwrap();
        assert!(rec.0.lock().unwrap().is_empty());
        let all = std::fs::read(v.path()).unwrap();
        assert_eq!(&all[..4], b"abcd");
        assert!(all[4..].iter().all(|&b| b == 0xbb), "the rest stays");
    }

    #[test]
    fn more_or_fewer_bytes_than_announced_is_an_error() {
        let v = volume(64);
        let rec = Recorder::default();
        let mut w = SlotWriter::open(v.path(), StorageKind::Ubi, &rec).unwrap();
        assert!(w.write(b"x").is_err(), "before begin");
        w.begin(3).unwrap();
        w.write(b"ab").unwrap();
        assert!(w.write(b"cd").is_err(), "past the announced size");
        assert!(w.finish().is_err(), "short of it");
    }

    #[test]
    fn the_request_is_the_kernels() {
        // _IOW: write (1 << 30), the size of an __s64, type 'O', number 0.
        assert_eq!(UBI_IOCVOLUP, (1 << 30) | (8 << 16) | (u32::from(b'O') << 8));
    }
}
