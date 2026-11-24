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
/// Where the IS31FL3246's LED class devices show up.
pub const SYSFS_LEDS_DIR: &str = "/sys/class/leds";

const LEDS: usize = crate::stages::PIXELS;
/// The first of the two LEDs at the front centre in a stage frame: frames are
/// drawn for the MT8163 ring (see [`crate::stages`]), pinna-hal's
/// `FRONT_CENTRE`.
const FRAME_FRONT_CENTRE: usize = 10;

/// A ring found in sysfs, and how to write to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ring {
    /// The IS31FL3236's `frame` attribute.
    Frame(PathBuf),
    /// The IS31FL3246's 12 LED class devices in `dir`. `offset` is added to
    /// a frame's LED number to get the device, to turn the frame's front
    /// centre onto the board's.
    Class { dir: PathBuf, offset: usize },
}

/// Find the ring and take it over from the kernel's boot animation, as
/// pinnad does when it starts. `dir` is [`SYSFS_I2C_DIR`]; the class ring is
/// looked for in [`SYSFS_LEDS_DIR`]. Without a board description the class
/// ring is not rotated.
pub fn find(dir: &Path) -> Option<Ring> {
    let offset = pinna_board::detect().map_or(0, |b| offset_for(b.ring.front_centre));
    find_in(dir, Path::new(SYSFS_LEDS_DIR), offset)
}

/// [`find`] with the class directory and the rotation given.
pub fn find_in(i2c_dir: &Path, leds_dir: &Path, offset: usize) -> Option<Ring> {
    find_frame(i2c_dir).or_else(|| find_class(leds_dir, offset))
}

/// The rotation that puts a frame's front centre onto `front_centre`.
fn offset_for(front_centre: [usize; 2]) -> usize {
    (front_centre[0] + LEDS - FRAME_FRONT_CENTRE) % LEDS
}

fn find_frame(dir: &Path) -> Option<Ring> {
    let mut entries: Vec<_> = fs::read_dir(dir).ok()?.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    entries.into_iter().find_map(|e| {
        let frame = e.path().join("frame");
        if !frame.is_file() {
            return None;
        }
        let _ = fs::write(e.path().join("boot_animation"), "0\n");
        Some(Ring::Frame(frame))
    })
}

fn led_dir(dir: &Path, n: usize) -> PathBuf {
    dir.join(format!("is31fl3246:ledmod_{n}"))
}

fn find_class(dir: &Path, offset: usize) -> Option<Ring> {
    if (0..LEDS).any(|n| !led_dir(dir, n).join("color").is_file()) {
        return None;
    }
    // The chip scales the colours by `brightness`, which reads 0 until it is
    // written (TECHO5, one C2N6L4, 2026-11-21).
    for n in 0..LEDS {
        let d = led_dir(dir, n);
        let max = fs::read_to_string(d.join("max_brightness"))
            .ok()
            .and_then(|s| s.trim().parse::<u32>().ok())
            .unwrap_or(255);
        let _ = fs::write(d.join("brightness"), format!("{max}\n"));
    }
    Some(Ring::Class {
        dir: dir.to_path_buf(),
        offset,
    })
}

/// Write one frame to the ring. The class ring takes all 12 colours: boot
/// frames are rare, so it skips none.
pub fn write(ring: &Ring, frame: &crate::stages::Frame) -> std::io::Result<()> {
    match ring {
        Ring::Frame(path) => fs::OpenOptions::new()
            .write(true)
            .open(path)?
            .write_all(crate::stages::encode(frame).as_bytes()),
        Ring::Class { dir, offset } => {
            for (i, [r, g, b]) in frame.iter().enumerate() {
                let color = led_dir(dir, (i + offset) % LEDS).join("color");
                fs::write(color, format!("{r} {g} {b}\n"))?;
            }
            Ok(())
        }
    }
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
        assert_eq!(
            find_in(d.path(), &d.path().join("leds"), 0),
            Some(Ring::Frame(ring.join("frame")))
        );
        assert_eq!(
            fs::read_to_string(ring.join("boot_animation")).unwrap(),
            "0\n"
        );
        assert_eq!(
            find_in(&d.path().join("0-0050"), &d.path().join("leds"), 0),
            None
        );
    }

    fn class_dir(root: &Path) -> PathBuf {
        let leds = root.join("leds");
        for n in 0..LEDS {
            let d = led_dir(&leds, n);
            fs::create_dir_all(&d).unwrap();
            fs::write(d.join("color"), "0 0 0\n").unwrap();
            fs::write(d.join("brightness"), "0\n").unwrap();
            fs::write(d.join("max_brightness"), "200\n").unwrap();
        }
        leds
    }

    fn color(leds: &Path, n: usize) -> String {
        fs::read_to_string(led_dir(leds, n).join("color")).unwrap()
    }

    #[test]
    fn finds_and_claims_the_class_ring() {
        let d = tempfile::tempdir().unwrap();
        let leds = class_dir(d.path());
        fs::remove_file(led_dir(&leds, 4).join("max_brightness")).unwrap();
        assert_eq!(
            find_in(&d.path().join("no-i2c"), &leds, 8),
            Some(Ring::Class {
                dir: leds.clone(),
                offset: 8
            })
        );
        for n in 0..LEDS {
            let want = if n == 4 { "255\n" } else { "200\n" };
            let b = fs::read_to_string(led_dir(&leds, n).join("brightness")).unwrap();
            assert_eq!(b, want, "{n}");
        }
    }

    #[test]
    fn an_incomplete_class_ring_is_not_found() {
        let d = tempfile::tempdir().unwrap();
        let leds = class_dir(d.path());
        fs::remove_file(led_dir(&leds, 11).join("color")).unwrap();
        assert_eq!(find_in(&d.path().join("no-i2c"), &leds, 0), None);
    }

    #[test]
    fn the_frame_ring_wins_over_the_class_ring() {
        let d = tempfile::tempdir().unwrap();
        let leds = class_dir(d.path());
        let ring = d.path().join("0-003f");
        fs::create_dir_all(&ring).unwrap();
        fs::write(ring.join("frame"), "").unwrap();
        assert_eq!(
            find_in(d.path(), &leds, 0),
            Some(Ring::Frame(ring.join("frame")))
        );
    }

    #[test]
    fn the_rotation_puts_the_frames_front_centre_on_the_boards() {
        assert_eq!(offset_for([10, 11]), 0);
        assert_eq!(offset_for([6, 7]), 8);
        assert_eq!((10 + 8) % LEDS, 6);
    }

    #[test]
    fn class_frame_written_in_decimal_and_turned() {
        let d = tempfile::tempdir().unwrap();
        let leds = class_dir(d.path());
        let ring = find_class(&leds, 8).unwrap();
        let mut frame = [[0u8; 3]; LEDS];
        frame[10] = [0, 48, 255];
        frame[3] = [1, 2, 3];
        write(&ring, &frame).unwrap();
        assert_eq!(color(&leds, 6), "0 48 255\n");
        assert_eq!(color(&leds, 11), "1 2 3\n");
        assert_eq!(color(&leds, 10), "0 0 0\n");
    }

    #[test]
    fn frame_written_in_driver_format() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("frame");
        fs::write(&f, "").unwrap();
        write(
            &Ring::Frame(f.clone()),
            &stages::progress(stages::STAGES, Duration::ZERO),
        )
        .unwrap();
        let s = fs::read_to_string(&f).unwrap();
        assert_eq!(s, stages::encode(&stages::progress(7, Duration::ZERO)));
    }
}
