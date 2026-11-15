//! The beamformer's live state, shared from the `pinna_beam` LADSPA plugin
//! (inside PipeWire) with pinnad through a small memory-mapped file. A port of
//! the previous firmware's micd beam snapshot (`beam_view`, `format_beams`).
//!
//! The plugin publishes after every processing cycle. A reader takes a
//! consistent copy through a seqlock: the counter is odd while the writer is
//! in the middle of an update. Every field is an atomic, so a torn read is
//! detected rather than undefined. The file never shrinks, so a mapped reader
//! cannot fault when the plugin restarts.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI32, AtomicU32, Ordering, fence};
use std::time::Duration;

use memmap2::{Mmap, MmapMut, MmapOptions};

use crate::beamformer::{Beamformer, MAX_BEAMS};

/// Where the plugin publishes on the Echo Dot: the PipeWire service's runtime
/// directory, which both PipeWire and pinnad (root) can reach.
pub const PATH: &str = "/run/pipewire/pinna-beam";
/// A snapshot older than this means the microphone graph has stopped.
pub const MAX_AGE: Duration = Duration::from_millis(500);
/// "PBV1": identifies the layout below; bump it when the layout changes.
const MAGIC: u32 = u32::from_le_bytes(*b"PBV1");
const READ_TRIES: usize = 100;

#[repr(C)]
struct Shared {
    magic: AtomicU32,
    seq: AtomicU32,
    beam_count: AtomicU32,
    selected_deg: AtomicI32,
    /// CLOCK_MONOTONIC in milliseconds, truncated to 32 bits.
    updated_ms: AtomicU32,
    azimuth_deg: [AtomicI32; MAX_BEAMS],
    /// f32 bits of each beam's smoothed speech-band power.
    power: [AtomicU32; MAX_BEAMS],
}

const SIZE: usize = std::mem::size_of::<Shared>();

fn monotonic_ms() -> u32 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `ts` is a valid out pointer; CLOCK_MONOTONIC always exists on Linux.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    (ts.tv_sec as u64 * 1000 + ts.tv_nsec as u64 / 1_000_000) as u32
}

/// The plugin's side. `publish` only does atomic stores and a clock read, so
/// it is safe on PipeWire's real-time thread.
pub struct BeamViewWriter {
    map: MmapMut,
}

impl BeamViewWriter {
    /// Create or reuse the file at `path`. Not real-time safe: call it when
    /// the plugin is instantiated.
    pub fn create(path: &Path) -> io::Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        // Grow only: shrinking under a reader's mapping would fault it.
        if file.metadata()?.len() < SIZE as u64 {
            file.set_len(SIZE as u64)?;
        }
        // SAFETY: the file is at least SIZE bytes and only ever grows; every
        // access through the mapping is atomic.
        let map = unsafe { MmapOptions::new().len(SIZE).map_mut(&file)? };
        let writer = BeamViewWriter { map };
        writer.shared().magic.store(MAGIC, Ordering::Release);
        Ok(writer)
    }

    fn shared(&self) -> &Shared {
        // SAFETY: the mapping is page-aligned and SIZE bytes long, and Shared
        // is made of atomics only, so any bit pattern is a valid value.
        unsafe { &*self.map.as_ptr().cast::<Shared>() }
    }

    /// Publish the beamformer's current state.
    pub fn publish(&self, bf: &Beamformer) {
        let n = bf.beam_count().min(MAX_BEAMS);
        let mut azimuth = [0i32; MAX_BEAMS];
        let mut power = [0f32; MAX_BEAMS];
        for b in 0..n {
            azimuth[b] = bf.beam_azimuth(b);
            power[b] = bf.beam_power(b);
        }
        self.write(
            monotonic_ms(),
            bf.selected_azimuth(),
            &azimuth[..n],
            &power[..n],
        );
    }

    fn write(&self, now_ms: u32, selected_deg: i32, azimuth: &[i32], power: &[f32]) {
        let s = self.shared();
        // Odd while writing; `| 1` also recovers from a writer that died mid-update.
        let begin = s.seq.load(Ordering::Relaxed) | 1;
        s.seq.store(begin, Ordering::Relaxed);
        fence(Ordering::Release);
        let n = azimuth.len().min(power.len()).min(MAX_BEAMS);
        s.beam_count.store(n as u32, Ordering::Relaxed);
        s.selected_deg.store(selected_deg, Ordering::Relaxed);
        s.updated_ms.store(now_ms, Ordering::Relaxed);
        for b in 0..n {
            s.azimuth_deg[b].store(azimuth[b], Ordering::Relaxed);
            s.power[b].store(power[b].to_bits(), Ordering::Relaxed);
        }
        s.seq.store(begin.wrapping_add(1), Ordering::Release);
    }
}

/// One consistent copy of the beamformer's state.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BeamSnapshot {
    pub beam_count: usize,
    pub azimuth_deg: [i32; MAX_BEAMS],
    pub power: [f32; MAX_BEAMS],
    /// The beam the selector follows (the talker), 0 = front, clockwise from above.
    pub selected_deg: i32,
    pub age: Duration,
}

impl BeamSnapshot {
    /// How far the strongest beam stands above the average beam, in dB: how
    /// directional the sound field is. Diffuse noise gives close to 0 dB.
    pub fn contrast_db(&self) -> f64 {
        let n = self.beam_count.min(MAX_BEAMS);
        if n == 0 {
            return 0.0;
        }
        // micd subtracts a full-scale offset from each level; it cancels here.
        let db = |b: usize| 10.0 * (f64::from(self.power[b]) + 1.0).log10();
        let mean = (0..n).map(db).sum::<f64>() / n as f64;
        let peak = (0..n).map(db).fold(f64::MIN, f64::max);
        peak - mean
    }
}

/// pinnad's side. Opens the file lazily and reopens it after the snapshot
/// goes stale, so it survives PipeWire starting late or restarting.
pub struct BeamViewReader {
    path: PathBuf,
    map: Option<Mmap>,
}

impl BeamViewReader {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        BeamViewReader {
            path: path.into(),
            map: None,
        }
    }

    fn open(&self) -> Option<Mmap> {
        let file = File::open(&self.path).ok()?;
        if file.metadata().ok()?.len() < SIZE as u64 {
            return None;
        }
        // SAFETY: the file is at least SIZE bytes and the writer never shrinks
        // it; every access through the mapping is an atomic load.
        unsafe { MmapOptions::new().len(SIZE).map(&file) }.ok()
    }

    /// The current snapshot, or `None` when the graph is not publishing.
    pub fn read(&mut self) -> Option<BeamSnapshot> {
        self.read_at(monotonic_ms())
    }

    fn read_at(&mut self, now_ms: u32) -> Option<BeamSnapshot> {
        if self.map.is_none() {
            self.map = self.open();
        }
        let snapshot = self.map.as_ref().and_then(|map| {
            // SAFETY: as in BeamViewWriter::shared; loads of 32-bit atomics
            // are fine on a read-only mapping.
            let s = unsafe { &*map.as_ptr().cast::<Shared>() };
            copy(s, now_ms)
        });
        if snapshot.is_none() {
            // Missing, foreign or stale: the plugin may have recreated the file.
            self.map = None;
        }
        snapshot
    }
}

fn copy(s: &Shared, now_ms: u32) -> Option<BeamSnapshot> {
    if s.magic.load(Ordering::Acquire) != MAGIC {
        return None;
    }
    for _ in 0..READ_TRIES {
        let begin = s.seq.load(Ordering::Acquire);
        if begin & 1 == 1 {
            std::hint::spin_loop();
            continue;
        }
        let beam_count = s.beam_count.load(Ordering::Relaxed) as usize;
        let selected_deg = s.selected_deg.load(Ordering::Relaxed);
        let updated_ms = s.updated_ms.load(Ordering::Relaxed);
        let mut azimuth_deg = [0i32; MAX_BEAMS];
        let mut power = [0f32; MAX_BEAMS];
        for b in 0..MAX_BEAMS {
            azimuth_deg[b] = s.azimuth_deg[b].load(Ordering::Relaxed);
            power[b] = f32::from_bits(s.power[b].load(Ordering::Relaxed));
        }
        fence(Ordering::Acquire);
        if s.seq.load(Ordering::Relaxed) != begin {
            continue;
        }
        let age = Duration::from_millis(u64::from(now_ms.wrapping_sub(updated_ms)));
        if beam_count == 0 || beam_count > MAX_BEAMS || age > MAX_AGE {
            return None;
        }
        return Some(BeamSnapshot {
            beam_count,
            azimuth_deg,
            power,
            selected_deg,
            age,
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const AZ: [i32; 12] = [0, 30, 60, 90, 120, 150, 180, 210, 240, 270, 300, 330];

    fn snapshot(power: [f32; 12]) -> BeamSnapshot {
        BeamSnapshot {
            beam_count: 12,
            azimuth_deg: AZ,
            power,
            selected_deg: 0,
            age: Duration::ZERO,
        }
    }

    #[test]
    fn round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("beam");
        let writer = BeamViewWriter::create(&path).unwrap();
        let mut reader = BeamViewReader::new(&path);
        let mut power = [1.0f32; 12];
        power[3] = 1000.0;
        writer.write(5_000, 90, &AZ, &power);
        let s = reader.read_at(5_100).unwrap();
        assert_eq!(s.beam_count, 12);
        assert_eq!(s.selected_deg, 90);
        assert_eq!(s.azimuth_deg, AZ);
        assert_eq!(s.power, power);
        assert_eq!(s.age, Duration::from_millis(100));
    }

    #[test]
    fn missing_stale_and_unwritten_read_as_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("beam");
        let mut reader = BeamViewReader::new(&path);
        assert!(reader.read_at(0).is_none(), "no file yet");
        let writer = BeamViewWriter::create(&path).unwrap();
        assert!(reader.read_at(0).is_none(), "no beams published yet");
        writer.write(1_000, 0, &AZ, &[1.0; 12]);
        assert!(reader.read_at(1_500).is_some());
        assert!(reader.read_at(1_501).is_none(), "older than MAX_AGE");
        // The clock wraps after 49 days; ages are taken modulo 2^32.
        writer.write(u32::MAX - 10, 0, &AZ, &[1.0; 12]);
        assert!(reader.read_at(20).is_some());
    }

    #[test]
    fn reader_survives_the_writer_recreating_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("beam");
        let mut reader = BeamViewReader::new(&path);
        BeamViewWriter::create(&path)
            .unwrap()
            .write(1_000, 30, &AZ, &[1.0; 12]);
        assert_eq!(reader.read_at(1_000).unwrap().selected_deg, 30);
        std::fs::remove_file(&path).unwrap();
        let writer = BeamViewWriter::create(&path).unwrap();
        writer.write(9_000, 60, &AZ, &[1.0; 12]);
        // The old mapping goes stale, then the reader opens the new file.
        assert!(reader.read_at(9_000).is_none());
        assert_eq!(reader.read_at(9_000).unwrap().selected_deg, 60);
    }

    #[test]
    fn contrast_is_peak_over_mean() {
        assert!(snapshot([1e6; 12]).contrast_db().abs() < 1e-9, "diffuse");
        let mut power = [1e4f32; 12];
        power[0] = 1e6;
        // Peak 60 dB, mean (60 + 11 * 40) / 12 = 41.67 dB.
        let c = snapshot(power).contrast_db();
        assert!((c - 18.333).abs() < 0.01, "{c}");
    }

    #[test]
    fn publishes_a_live_beamformer() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("beam");
        let writer = BeamViewWriter::create(&path).unwrap();
        let bf = Beamformer::default_echo_dot();
        writer.publish(&bf);
        let s = BeamViewReader::new(&path).read().unwrap();
        assert_eq!(s.beam_count, bf.beam_count());
        assert_eq!(s.selected_deg, bf.selected_azimuth());
        assert_eq!(s.azimuth_deg[1], 30);
    }
}
