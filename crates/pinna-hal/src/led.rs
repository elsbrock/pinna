//! LED ring (IS31FL3236) driven through the kernel driver's sysfs files.
//!
//! Never talk to `/dev/i2c`: the kernel driver owns the chip. Output shaping
//! (gamma, per-channel weights, frame power budget) is a port of the previous firmware's
//! `led_output.c`.

use crate::{Error, Result};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

pub const RING_PIXELS: usize = 12;
/// The two pixels at the front centre of the ring.
pub const FRONT_CENTRE: [usize; 2] = [10, 11];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const OFF: Rgb = Rgb { r: 0, g: 0, b: 0 };

    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Rgb { r, g, b }
    }

    /// Scale all channels by `f` (clamped to 0..=1).
    pub fn scaled(self, f: f32) -> Rgb {
        let f = f.clamp(0.0, 1.0);
        let s = |v: u8| (f32::from(v) * f).round() as u8;
        Rgb::new(s(self.r), s(self.g), s(self.b))
    }
}

pub type Frame = [Rgb; RING_PIXELS];

pub trait LedRing: Send {
    fn set(&mut self, frame: &Frame) -> Result<()>;
    fn clear(&mut self) -> Result<()> {
        self.set(&[Rgb::OFF; RING_PIXELS])
    }
}

/// Encode a frame as the driver expects: 12 x `%02x%02x%02x` plus newline.
pub fn encode_frame(frame: &Frame) -> String {
    let mut s = String::with_capacity(RING_PIXELS * 6 + 1);
    for p in frame {
        s.push_str(&format!("{:02x}{:02x}{:02x}", p.r, p.g, p.b));
    }
    s.push('\n');
    s
}

// ---------------------------------------------------------------------------
// Output shaping (led_output.c)

/// `ceil(v^2 / 255)`, with exact 0 and 255 preserved.
pub fn gamma(value: u8) -> u8 {
    match value {
        0 => 0,
        255 => 255,
        v => {
            let v = u32::from(v);
            ((v * v).div_ceil(255)) as u8
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OutputCalibration {
    pub gain: [u8; 3],
    pub weight: [u32; 3],
    pub max_frame_load: u32,
    /// 0 disables slew limiting.
    pub max_slew_per_frame: u32,
}

impl Default for OutputCalibration {
    fn default() -> Self {
        OutputCalibration {
            gain: [255; 3],
            weight: [100; 3],
            // ~65% of a full-white frame (9180 units); a defensive bound.
            max_frame_load: 5967,
            max_slew_per_frame: 0,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OutputDiag {
    pub raw_load: u32,
    pub frame_load: u32,
    pub limited: bool,
    pub slew_limited: bool,
}

#[derive(Debug, Default)]
pub struct OutputShaper {
    pub cal: OutputCalibration,
    previous_load: Option<u32>,
}

fn scale_brightness(c: u32, brightness: u32) -> u32 {
    if brightness >= 100 {
        c
    } else {
        (c * brightness + 50) / 100
    }
}

fn apply_gain(c: u32, gain: u32) -> u32 {
    if gain >= 255 {
        c
    } else {
        (c * gain + 127) / 255
    }
}

pub fn frame_load(cal: &OutputCalibration, frame: &Frame) -> u32 {
    let mut load: u64 = 0;
    for p in frame {
        load += u64::from(p.r) * u64::from(cal.weight[0]);
        load += u64::from(p.g) * u64::from(cal.weight[1]);
        load += u64::from(p.b) * u64::from(cal.weight[2]);
    }
    (load / 100).min(u64::from(u32::MAX)) as u32
}

fn limit_frame(frame: &mut Frame, num: u32, den: u32) {
    let f = |c: u8| (u64::from(c) * u64::from(num) / u64::from(den)) as u8;
    for p in frame.iter_mut() {
        *p = Rgb::new(f(p.r), f(p.g), f(p.b));
    }
}

impl OutputShaper {
    pub fn new(cal: OutputCalibration) -> Self {
        OutputShaper {
            cal,
            previous_load: None,
        }
    }

    /// Brightness cap, channel gain, gamma, then the whole-frame power budget
    /// and optional slew limit. `brightness` is 0..=100.
    pub fn process(&mut self, logical: &Frame, brightness: u8) -> (Frame, OutputDiag) {
        let brightness = u32::from(brightness.min(100));
        let cal = self.cal;
        let mut diag = OutputDiag::default();
        let mut out = [Rgb::OFF; RING_PIXELS];

        for (o, l) in out.iter_mut().zip(logical) {
            let ch = |v: u8, gain: u8| {
                let v = scale_brightness(u32::from(v), brightness);
                let v = apply_gain(v, u32::from(gain));
                gamma(v as u8)
            };
            *o = Rgb::new(
                ch(l.r, cal.gain[0]),
                ch(l.g, cal.gain[1]),
                ch(l.b, cal.gain[2]),
            );
        }

        let raw_load = frame_load(&cal, &out);
        diag.raw_load = raw_load;
        if cal.max_frame_load > 0 && raw_load > cal.max_frame_load {
            limit_frame(&mut out, cal.max_frame_load, raw_load);
            diag.limited = true;
        }

        if cal.max_slew_per_frame > 0 {
            let load = frame_load(&cal, &out);
            if let Some(prev) = self.previous_load
                && load > prev + cal.max_slew_per_frame
            {
                limit_frame(&mut out, prev + cal.max_slew_per_frame, load);
                diag.slew_limited = true;
            }
        }

        diag.frame_load = frame_load(&cal, &out);
        self.previous_load = Some(diag.frame_load);
        (out, diag)
    }
}

// ---------------------------------------------------------------------------
// Real hardware

const SYSFS_I2C_DIR: &str = "/sys/bus/i2c/devices";
pub const MUTE_LAMP_PATH: &str = "/sys/devices/platform/amz_privacy/mute_lamp";

/// IS31FL3236 ring via the kernel driver's `frame` attribute.
pub struct Is31Sysfs {
    frame_path: PathBuf,
    scaled_frame_path: Option<PathBuf>,
    shaper: OutputShaper,
    brightness: u8,
    mute_lamp: PathBuf,
}

impl Is31Sysfs {
    /// Scan `/sys/bus/i2c/devices/*` for a directory with `frame` and
    /// `boot_animation`, then take over from the kernel boot animation.
    pub fn discover() -> Result<Self> {
        Self::discover_in(Path::new(SYSFS_I2C_DIR))
    }

    pub fn discover_in(dir: &Path) -> Result<Self> {
        let mut entries: Vec<_> = fs::read_dir(dir)?.filter_map(|e| e.ok()).collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let base = e.path();
            let frame = base.join("frame");
            let boot = base.join("boot_animation");
            if !frame.is_file() || !boot.is_file() {
                continue;
            }
            let scaled = base.join("scaled_frame");
            let ring = Is31Sysfs {
                frame_path: frame,
                scaled_frame_path: scaled.is_file().then_some(scaled),
                shaper: OutputShaper::default(),
                brightness: 100,
                mute_lamp: PathBuf::from(MUTE_LAMP_PATH),
            };
            // The kernel driver owns the boot chase until this write.
            fs::write(&boot, "0\n")?;
            tracing::info!(path = %ring.frame_path.display(), "using IS31FL3236 LED backend");
            return Ok(ring);
        }
        Err(Error::NotFound("IS31FL3236 sysfs device"))
    }

    pub fn has_scaled_frame(&self) -> bool {
        self.scaled_frame_path.is_some()
    }

    /// Logical brightness cap, 0..=100 (applied before the power budget).
    pub fn set_brightness(&mut self, brightness: u8) {
        self.brightness = brightness.min(100);
    }

    /// Drive the mute lamp if the kernel exposes it. Returns whether it exists.
    pub fn set_mute_lamp(&self, on: bool) -> Result<bool> {
        set_mute_lamp_at(&self.mute_lamp, on)
    }
}

/// Write the mute lamp attribute at `path`; `Ok(false)` if it does not exist.
pub fn set_mute_lamp_at(path: &Path, on: bool) -> Result<bool> {
    if !path.exists() {
        return Ok(false);
    }
    fs::write(path, if on { "1" } else { "0" })?;
    Ok(true)
}

/// Set the mute lamp at its standard location, if present.
pub fn set_mute_lamp(on: bool) -> Result<bool> {
    set_mute_lamp_at(Path::new(MUTE_LAMP_PATH), on)
}

impl LedRing for Is31Sysfs {
    fn set(&mut self, frame: &Frame) -> Result<()> {
        let (out, _) = self.shaper.process(frame, self.brightness);
        let mut f = fs::OpenOptions::new().write(true).open(&self.frame_path)?;
        f.write_all(encode_frame(&out).as_bytes())?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Mock

/// Renders the ring as ANSI truecolor blocks on stderr (rate limited).
pub struct MockLeds {
    last_print: Option<Instant>,
    min_interval: Duration,
    last: Option<Frame>,
}

impl Default for MockLeds {
    fn default() -> Self {
        MockLeds {
            last_print: None,
            min_interval: Duration::from_millis(100),
            last: None,
        }
    }
}

impl MockLeds {
    pub fn new() -> Self {
        Self::default()
    }

    /// One line of ANSI truecolor blocks, ring order.
    pub fn render_ansi(frame: &Frame) -> String {
        let mut s = String::new();
        for p in frame {
            s.push_str(&format!("\x1b[38;2;{};{};{}m██\x1b[0m", p.r, p.g, p.b));
        }
        s
    }
}

impl LedRing for MockLeds {
    fn set(&mut self, frame: &Frame) -> Result<()> {
        if self.last.as_ref() == Some(frame) {
            return Ok(());
        }
        self.last = Some(*frame);
        let now = Instant::now();
        if self
            .last_print
            .is_none_or(|t| now.duration_since(t) >= self.min_interval)
        {
            self.last_print = Some(now);
            eprint!("\r{} ", Self::render_ansi(frame));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Coalescing writer

/// Minimum spacing between hardware writes (one frame is 37 I2C register
/// writes on a bus shared with the codecs; ledd paces to ~33 ms minimum).
pub const MIN_WRITE_INTERVAL: Duration = Duration::from_millis(33);
const MAX_WRITE_INTERVAL: Duration = Duration::from_millis(125);

struct Shared {
    slot: Mutex<Slot>,
    cv: Condvar,
}

struct Slot {
    pending: Option<Frame>,
    closed: bool,
}

struct Closer(Arc<Shared>);

impl Drop for Closer {
    fn drop(&mut self) {
        self.0.slot.lock().unwrap().closed = true;
        self.0.cv.notify_all();
    }
}

/// Cloneable, non-blocking front end for a ring: frames are coalesced
/// (latest wins) and written from a dedicated thread at <= 30 fps, spending at
/// most half the time on the bus. The thread ends when all handles drop.
#[derive(Clone)]
pub struct LedHandle {
    shared: Arc<Shared>,
    _closer: Arc<Closer>,
}

impl LedHandle {
    pub fn spawn(ring: impl LedRing + 'static) -> LedHandle {
        let shared = Arc::new(Shared {
            slot: Mutex::new(Slot {
                pending: None,
                closed: false,
            }),
            cv: Condvar::new(),
        });
        let thread_shared = shared.clone();
        std::thread::Builder::new()
            .name("led-writer".into())
            .spawn(move || writer_loop(thread_shared, Box::new(ring)))
            .expect("spawn led writer");
        LedHandle {
            _closer: Arc::new(Closer(shared.clone())),
            shared,
        }
    }

    pub fn show(&self, frame: &Frame) {
        self.shared.slot.lock().unwrap().pending = Some(*frame);
        self.shared.cv.notify_one();
    }
}

impl LedRing for LedHandle {
    fn set(&mut self, frame: &Frame) -> Result<()> {
        self.show(frame);
        Ok(())
    }
}

fn writer_loop(shared: Arc<Shared>, mut ring: Box<dyn LedRing>) {
    let mut next_write = Instant::now();
    let mut cost_avg = Duration::ZERO;
    let mut written: Option<Frame> = None;
    loop {
        let frame = {
            let mut slot = shared.slot.lock().unwrap();
            loop {
                if let Some(f) = slot.pending.take() {
                    break Some(f);
                }
                if slot.closed {
                    break None;
                }
                slot = shared.cv.wait(slot).unwrap();
            }
        };
        let Some(mut frame) = frame else { return };
        // Pace: wait out the interval, picking up any newer frame meanwhile.
        loop {
            let now = Instant::now();
            if now >= next_write {
                break;
            }
            let slot = shared.slot.lock().unwrap();
            let (mut slot, _) = shared.cv.wait_timeout(slot, next_write - now).unwrap();
            if let Some(f) = slot.pending.take() {
                frame = f;
            }
        }
        if written.as_ref() == Some(&frame) {
            continue;
        }
        let started = Instant::now();
        match ring.set(&frame) {
            Ok(()) => written = Some(frame),
            Err(e) => {
                tracing::warn!("LED write failed: {e}");
                written = None;
            }
        }
        let cost = started.elapsed();
        cost_avg = if cost_avg.is_zero() {
            cost
        } else {
            (cost_avg * 3 + cost) / 4
        };
        let interval = (cost_avg * 2).clamp(MIN_WRITE_INTERVAL, MAX_WRITE_INTERVAL);
        next_write = started + interval;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gamma_matches_ceil_v2_over_255() {
        assert_eq!(gamma(0), 0);
        assert_eq!(gamma(1), 1);
        assert_eq!(gamma(16), 2); // 256/255 -> ceil 2
        assert_eq!(gamma(128), 65); // 16384/255 = 64.25 -> 65
        assert_eq!(gamma(254), 254); // 64516/255 = 253.01 -> 254
        assert_eq!(gamma(255), 255);
        for v in 0..=255u8 {
            assert!(gamma(v) <= v);
            if v > 0 {
                assert!(gamma(v) > 0);
            }
        }
    }

    #[test]
    fn encode_is_72_hex_plus_newline() {
        let mut f = [Rgb::OFF; 12];
        f[0] = Rgb::new(0x12, 0xab, 0xff);
        f[11] = Rgb::new(1, 2, 3);
        let s = encode_frame(&f);
        assert_eq!(s.len(), 73);
        assert!(s.starts_with("12abff000000"));
        assert!(s.ends_with("010203\n"));
    }

    #[test]
    fn full_white_hits_budget() {
        let mut sh = OutputShaper::default();
        let (out, d) = sh.process(&[Rgb::new(255, 255, 255); 12], 100);
        assert_eq!(d.raw_load, 9180);
        assert!(d.limited);
        // 255 * 5967 / 9180 = 165
        assert_eq!(out[0], Rgb::new(165, 165, 165));
        assert!(d.frame_load <= 5967);
    }

    #[test]
    fn dim_frame_not_limited() {
        let mut sh = OutputShaper::default();
        let (out, d) = sh.process(&[Rgb::new(72, 185, 255); 12], 100);
        assert!(!d.limited);
        assert_eq!(out[0], Rgb::new(gamma(72), gamma(185), 255));
    }

    #[test]
    fn brightness_applies_before_gamma() {
        let mut sh = OutputShaper::default();
        let (out, _) = sh.process(&[Rgb::new(200, 0, 0); 12], 50);
        assert_eq!(out[0].r, gamma(100));
        let (out, _) = sh.process(&[Rgb::new(200, 0, 0); 12], 0);
        assert_eq!(out[0], Rgb::OFF);
    }

    #[test]
    fn weights_change_load() {
        let cal = OutputCalibration {
            weight: [200, 100, 50],
            ..Default::default()
        };
        let mut f = [Rgb::OFF; 12];
        f[0] = Rgb::new(100, 100, 100);
        assert_eq!(frame_load(&cal, &f), 100 * 200 / 100 + 100 + 50);
    }

    #[test]
    fn slew_limit_caps_load_increase() {
        let cal = OutputCalibration {
            max_slew_per_frame: 100,
            ..Default::default()
        };
        let mut sh = OutputShaper::new(cal);
        sh.process(&[Rgb::OFF; 12], 100);
        let (_, d) = sh.process(&[Rgb::new(255, 0, 0); 12], 100);
        assert!(d.slew_limited);
        assert!(d.frame_load <= 100);
    }

    #[test]
    fn discover_in_fake_sysfs() {
        let dir = std::env::temp_dir().join(format!("pinna-led-{}", std::process::id()));
        let dev = dir.join("1-0034");
        fs::create_dir_all(&dev).unwrap();
        fs::create_dir_all(dir.join("i2c-1")).unwrap();
        fs::write(dev.join("frame"), "").unwrap();
        fs::write(dev.join("boot_animation"), "1\n").unwrap();
        let mut ring = Is31Sysfs::discover_in(&dir).unwrap();
        assert_eq!(
            fs::read_to_string(dev.join("boot_animation")).unwrap(),
            "0\n"
        );
        assert!(!ring.has_scaled_frame());
        ring.set(&[Rgb::new(255, 255, 255); 12]).unwrap();
        let written = fs::read_to_string(dev.join("frame")).unwrap();
        assert_eq!(written.len(), 73);
        assert!(written.starts_with("a5a5a5"));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn discover_fails_without_device() {
        let dir = std::env::temp_dir().join(format!("pinna-led-none-{}", std::process::id()));
        fs::create_dir_all(dir.join("i2c-1")).unwrap();
        assert!(matches!(
            Is31Sysfs::discover_in(&dir),
            Err(Error::NotFound(_))
        ));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn mute_lamp_absent_is_ok_false() {
        assert!(!set_mute_lamp_at(Path::new("/nonexistent/mute_lamp"), true).unwrap());
    }

    struct Recorder(Arc<Mutex<Vec<(Instant, Frame)>>>);
    impl LedRing for Recorder {
        fn set(&mut self, f: &Frame) -> Result<()> {
            self.0.lock().unwrap().push((Instant::now(), *f));
            Ok(())
        }
    }

    #[test]
    fn writer_coalesces_and_paces() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let h = LedHandle::spawn(Recorder(log.clone()));
        for i in 0..50u8 {
            h.show(&[Rgb::new(i, 0, 0); 12]);
            std::thread::sleep(Duration::from_millis(2));
        }
        std::thread::sleep(Duration::from_millis(150));
        let log = log.lock().unwrap();
        assert!(log.len() < 15, "writes: {}", log.len());
        assert_eq!(log.last().unwrap().1[0].r, 49);
        for w in log.windows(2) {
            assert!(w[1].0 - w[0].0 >= Duration::from_millis(30));
        }
    }
}
