//! Where a partition's name leads on this board (ADR 0035, Decision 9): the
//! GPT partition under `/dev/disk/by-partlabel`, or the UBI volume's
//! character device. The updater, pinnad, `pinnactl` and `pinna-bootctl`
//! share it, so "no override" means "the board's".

use std::path::{Path, PathBuf};

use pinna_board::StorageKind;

use crate::ubi;

pub const GPT_DIR: &str = "/dev/disk/by-partlabel";
pub const SYS_UBI: &str = "/sys/class/ubi";

/// A set of named partitions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Parts {
    /// Files or device nodes named after the partitions in one directory.
    Dir(PathBuf),
    /// UBI volumes by name, found in this sysfs directory.
    Ubi(PathBuf),
}

impl Parts {
    /// This board's partitions.
    pub fn board() -> Parts {
        Parts::of(pinna_board::storage())
    }

    pub fn of(kind: StorageKind) -> Parts {
        match kind {
            StorageKind::Gpt => Parts::Dir(GPT_DIR.into()),
            StorageKind::Ubi => Parts::Ubi(SYS_UBI.into()),
        }
    }

    /// Partitions as files in `dir`: an override, and the tests' fakes.
    pub fn in_dir(dir: impl Into<PathBuf>) -> Parts {
        Parts::Dir(dir.into())
    }

    pub fn kind(&self) -> StorageKind {
        match self {
            Parts::Dir(_) => StorageKind::Gpt,
            Parts::Ubi(_) => StorageKind::Ubi,
        }
    }

    /// The node of the partition `name`, `None` when a UBI volume of that
    /// name doesn't exist. A GPT name always resolves; whether it exists is
    /// for the caller to find out by opening it.
    pub fn node(&self, name: &str) -> Option<PathBuf> {
        match self {
            Parts::Dir(d) => Some(d.join(name)),
            Parts::Ubi(sys) => ubi::volume(sys, name),
        }
    }

    /// The node, or for a missing UBI volume a path that can't exist and
    /// names it (`ubi:misc`), so opening it fails with that name in the error.
    pub fn path(&self, name: &str) -> PathBuf {
        self.node(name)
            .unwrap_or_else(|| PathBuf::from(format!("ubi:{name}")))
    }

    pub fn exists(&self, name: &str) -> bool {
        self.node(name).is_some_and(|p| p.exists())
    }

    /// The directory of a [`Parts::Dir`].
    pub fn dir(&self) -> Option<&Path> {
        match self {
            Parts::Dir(d) => Some(d),
            Parts::Ubi(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn gpt_names_are_files_in_the_directory() {
        let p = Parts::in_dir("/x");
        assert_eq!(p.kind(), StorageKind::Gpt);
        assert_eq!(p.node("misc"), Some(PathBuf::from("/x/misc")));
        assert_eq!(p.path("boot_a"), PathBuf::from("/x/boot_a"));
        assert_eq!(Parts::of(StorageKind::Gpt), Parts::Dir(GPT_DIR.into()));
    }

    #[test]
    fn ubi_names_are_the_volumes_char_devices() {
        let d = tempfile::tempdir().unwrap();
        for (dev, name) in [("ubi0_6", "misc"), ("ubi0_9", "system_a")] {
            fs::create_dir(d.path().join(dev)).unwrap();
            fs::write(d.path().join(dev).join("name"), format!("{name}\n")).unwrap();
        }
        let p = Parts::Ubi(d.path().into());
        assert_eq!(p.kind(), StorageKind::Ubi);
        assert_eq!(p.node("system_a"), Some(PathBuf::from("/dev/ubi0_9")));
        assert_eq!(p.node("system_b"), None);
        assert_eq!(p.path("system_b"), PathBuf::from("ubi:system_b"));
        assert!(!p.exists("system_b"));
        assert_eq!(Parts::of(StorageKind::Ubi), Parts::Ubi(SYS_UBI.into()));
    }
}
