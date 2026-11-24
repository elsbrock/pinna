//! Which board this is, from what the bootloader left: the idme record's
//! `device_type_id`, else the device tree's `compatible` list.
//!
//! Both MT8163 models boot the shared `mt8163-radar-puffin.dtb` (see
//! `kernel/build.sh`), so the device tree names only the chip; the
//! bootloader's idme record tells them apart. A board without the record
//! but with an MT8163 is taken for the Echo Dot (2nd gen), Pinna's first
//! device. An idme id Pinna doesn't know is no board: it reports no model,
//! so a release that names the models it is for isn't installed on it.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::{Board, ECHO_DOT_2, Model, StorageKind};

/// Amazon's idme driver, on Amazon's kernels.
pub const IDME_DIR: &str = "/proc/idme";
/// The idme record LK copies into the device tree, on Pinna's 6.12.
pub const DT_IDME_DIR: &str = "/sys/firmware/devicetree/base/idme";
pub const DT_COMPATIBLE: &str = "/sys/firmware/devicetree/base/compatible";

/// The board this runs on; `None` on hardware Pinna doesn't know: a virtual
/// machine, a computer, an unknown idme id. Read once per process: what the
/// bootloader left doesn't change while the system runs.
pub fn detect() -> Option<&'static Board> {
    static BOARD: OnceLock<Option<&'static Board>> = OnceLock::new();
    *BOARD.get_or_init(|| {
        detect_in(
            Path::new(IDME_DIR),
            Path::new(DT_IDME_DIR),
            Path::new(DT_COMPATIBLE),
        )
    })
}

pub fn detect_in(idme: &Path, dt_idme: &Path, compatible: &Path) -> Option<&'static Board> {
    match device_type_id_in(idme, dt_idme) {
        Some(id) => Model::ALL
            .into_iter()
            .map(Model::board)
            .find(|b| id.eq_ignore_ascii_case(b.device_type_id)),
        None => ECHO_DOT_2.soc_in(compatible).then_some(&ECHO_DOT_2),
    }
}

/// Where this machine keeps its partitions: the board's storage, else the
/// SoC's. The MT8519 keeps everything in UBI volumes (ADR 0035) on every
/// board, so an MT8519 without a board description still boots its root;
/// anything else Pinna doesn't know (the VMs) is GPT, as before.
pub fn storage() -> StorageKind {
    static STORAGE: OnceLock<StorageKind> = OnceLock::new();
    *STORAGE.get_or_init(|| storage_in(detect(), Path::new(DT_COMPATIBLE)))
}

pub fn storage_in(board: Option<&Board>, compatible: &Path) -> StorageKind {
    match board {
        Some(b) => b.storage,
        None if compatible_has(compatible, MT8519_COMPATIBLE) => StorageKind::Ubi,
        None => StorageKind::Gpt,
    }
}

/// The MT8519's entry in the device tree's root `compatible` list.
pub const MT8519_COMPATIBLE: &str = "mediatek,mt8519";

/// The bootloader's idme `device_type_id`: `/proc/idme/device_type_id` on a
/// kernel with Amazon's idme driver, LK's `idme/device_type_id` node in the
/// device tree on 6.12. `None` when the bootloader keeps no record (a VM).
pub fn device_type_id() -> Option<String> {
    device_type_id_in(Path::new(IDME_DIR), Path::new(DT_IDME_DIR))
}

pub fn device_type_id_in(idme: &Path, dt_idme: &Path) -> Option<String> {
    let read = |path: PathBuf| -> Option<String> {
        let raw = std::fs::read(path).ok()?;
        let s = String::from_utf8(raw).ok()?;
        let s = s.trim_end_matches('\0').trim();
        (!s.is_empty()).then(|| s.to_string())
    };
    read(idme.join("device_type_id")).or_else(|| read(dt_idme.join("device_type_id").join("value")))
}

/// Whether the NUL-separated `compatible` list at `path` has `entry`.
pub fn compatible_has(path: &Path, entry: &str) -> bool {
    std::fs::read(path).is_ok_and(|bytes| bytes.split(|&b| b == 0).any(|s| s == entry.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ECHO_2;
    use std::fs;

    fn model(b: Option<&'static Board>) -> Option<Model> {
        b.map(|b| b.model)
    }

    #[test]
    fn the_idme_device_type_decides() {
        let d = tempfile::tempdir().unwrap();
        let idme = d.path().join("idme");
        let dt = d.path().join("dt");
        let compatible = d.path().join("compatible");
        fs::create_dir_all(dt.join("device_type_id")).unwrap();
        let dt_value = dt.join("device_type_id").join("value");

        // LK's record in the device tree (6.12), NUL-padded.
        fs::write(&dt_value, "A3S5BH2HU6VAYF\0").unwrap();
        assert_eq!(
            model(detect_in(&idme, &dt, &compatible)),
            Some(Model::EchoDot2)
        );
        fs::write(&dt_value, "A7WXQPH584YP\0").unwrap();
        assert_eq!(
            model(detect_in(&idme, &dt, &compatible)),
            Some(Model::Echo2)
        );
        assert!(std::ptr::eq(
            detect_in(&idme, &dt, &compatible).unwrap(),
            &ECHO_2
        ));

        // /proc/idme (Amazon's idme driver) wins; an unknown id is no board,
        // whatever the device tree says.
        fs::create_dir_all(&idme).unwrap();
        fs::write(idme.join("device_type_id"), "a7wxqph584yp\n").unwrap();
        assert_eq!(
            model(detect_in(&idme, &dt, &compatible)),
            Some(Model::Echo2)
        );
        fs::write(idme.join("device_type_id"), "SOMEBODYELSE\n").unwrap();
        fs::write(&compatible, "amazon,radar-puffin\0mediatek,mt8163\0").unwrap();
        assert_eq!(model(detect_in(&idme, &dt, &compatible)), None);
    }

    #[test]
    fn without_a_record_an_mt8163_is_the_echo_dot() {
        let d = tempfile::tempdir().unwrap();
        let none = d.path().join("none");
        let compatible = d.path().join("compatible");
        fs::write(&compatible, "amazon,whatever\0mediatek,mt8163\0").unwrap();
        assert_eq!(
            model(detect_in(&none, &none, &compatible)),
            Some(Model::EchoDot2)
        );
        fs::write(&compatible, "mediatek,mt8163-extra\0").unwrap();
        assert_eq!(model(detect_in(&none, &none, &compatible)), None);
        fs::write(&compatible, "acme,board\0").unwrap();
        assert_eq!(model(detect_in(&none, &none, &compatible)), None);
        assert_eq!(model(detect_in(&none, &none, &none)), None);
    }

    #[test]
    fn storage_is_the_boards_else_ubi_on_an_mt8519() {
        let d = tempfile::tempdir().unwrap();
        let compatible = d.path().join("compatible");
        fs::write(&compatible, "amzn,cannoli-dvt\0mediatek,mt8519\0").unwrap();
        assert_eq!(storage_in(None, &compatible), StorageKind::Ubi);
        assert_eq!(storage_in(Some(&ECHO_DOT_2), &compatible), StorageKind::Gpt);
        fs::write(&compatible, "amazon,radar-puffin\0mediatek,mt8163\0").unwrap();
        assert_eq!(storage_in(None, &compatible), StorageKind::Gpt);
        assert_eq!(storage_in(None, &d.path().join("none")), StorageKind::Gpt);
    }

    #[test]
    fn the_echo_dot_5_by_its_record_and_an_mt8519_without_one_is_no_board() {
        let d = tempfile::tempdir().unwrap();
        let none = d.path().join("none");
        let dt = d.path().join("dt");
        let compatible = d.path().join("compatible");
        fs::write(&compatible, "amzn,cannoli-dvt\0mediatek,mt8519\0").unwrap();
        // Without the record an MT8519 is no board: only the MT8163 falls
        // back to a model.
        assert_eq!(model(detect_in(&none, &dt, &compatible)), None);
        fs::create_dir_all(dt.join("device_type_id")).unwrap();
        // As the owner's C2N6L4 carries it in the device tree.
        fs::write(dt.join("device_type_id").join("value"), "A4ZXE0RM7LQ7A\0").unwrap();
        let b = detect_in(&none, &dt, &compatible).unwrap();
        assert_eq!(b.model, Model::EchoDot5);
        assert!(b.soc_in(&compatible));
        assert_eq!(storage_in(Some(b), &compatible), StorageKind::Ubi);
    }

    #[test]
    fn an_empty_record_counts_as_none() {
        let d = tempfile::tempdir().unwrap();
        let idme = d.path().join("idme");
        fs::create_dir_all(&idme).unwrap();
        fs::write(idme.join("device_type_id"), "\0\0").unwrap();
        assert_eq!(device_type_id_in(&idme, &d.path().join("dt")), None);
    }
}
