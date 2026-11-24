//! The LED ring while pinna-init runs (ADR 0012): a thread draws the boot
//! stages, a failure code or rescue mode, about 20 frames a second.
//!
//! The ring driver may probe after pinna-init starts; the thread looks for it
//! once a second until it appears. Without a ring nothing is drawn.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use pinna_bootctl::ring;
use pinna_bootctl::stages::{self, Frame};

const TICK: Duration = Duration::from_millis(50);
const RESCAN: Duration = Duration::from_secs(1);

/// What the ring shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Show {
    /// This many stages are complete; the next one is in progress.
    Progress(u8),
    /// This stage missed its deadline.
    Code(u8),
    /// Rescue mode; `host` once a computer has configured the USB gadget.
    Rescue { host: bool },
}

impl Show {
    fn frame(self, t: Duration) -> Frame {
        match self {
            Show::Progress(done) => stages::progress(done, t),
            Show::Code(n) => stages::code(n, t),
            Show::Rescue { host } => stages::rescue(host, t),
        }
    }
}

struct Shared {
    show: Mutex<(Show, Instant)>,
    stop: AtomicBool,
}

pub struct Ring {
    shared: Arc<Shared>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl Ring {
    pub fn start() -> Ring {
        let ring = Ring {
            shared: Arc::new(Shared {
                show: Mutex::new((Show::Progress(0), Instant::now())),
                stop: AtomicBool::new(false),
            }),
            thread: Mutex::new(None),
        };
        ring.ensure_running();
        ring
    }

    fn ensure_running(&self) {
        let mut thread = self.thread.lock().unwrap_or_else(|e| e.into_inner());
        if thread.is_none() {
            self.shared.stop.store(false, Ordering::SeqCst);
            let shared = Arc::clone(&self.shared);
            *thread = thread::Builder::new()
                .name("ring".into())
                .spawn(move || draw(&shared))
                .ok();
        }
    }

    /// Show `s` from now on (restarting the drawing after [`Ring::hold`]).
    pub fn show(&self, s: Show) {
        {
            let mut cur = self.shared.show.lock().unwrap_or_else(|e| e.into_inner());
            if cur.0 != s {
                *cur = (s, Instant::now());
            }
        }
        self.ensure_running();
    }

    /// The stage in progress: one past the completed ones.
    pub fn in_progress(&self) -> u8 {
        match self.shared.show.lock().unwrap_or_else(|e| e.into_inner()).0 {
            Show::Progress(done) => done.saturating_add(1).min(stages::STAGES),
            _ => stages::STORAGE,
        }
    }

    /// Stage `n` is complete.
    pub fn done(&self, n: u8) {
        self.show(Show::Progress(n));
    }

    /// Stop drawing and leave the still frame of `done` stages in the LED
    /// controller, for whatever takes the ring over after pinna-init.
    pub fn hold(&self, done: u8) {
        self.shared.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = t.join();
        }
        if let Some(path) = ring::find(Path::new(ring::SYSFS_I2C_DIR)) {
            let _ = ring::write(&path, &stages::progress(done, Duration::ZERO));
        }
    }
}

fn draw(shared: &Shared) {
    let mut ring: Option<ring::Ring> = None;
    let mut looked = None::<Instant>;
    let mut last: Option<Frame> = None;
    while !shared.stop.load(Ordering::SeqCst) {
        if ring.is_none() && looked.is_none_or(|l| l.elapsed() >= RESCAN) {
            ring = ring::find(Path::new(ring::SYSFS_I2C_DIR));
            looked = Some(Instant::now());
            last = None;
        }
        if let Some(path) = &ring {
            let (show, since) = *shared.show.lock().unwrap_or_else(|e| e.into_inner());
            let frame = show.frame(since.elapsed());
            if last != Some(frame) {
                if ring::write(path, &frame).is_err() {
                    ring = None;
                }
                last = Some(frame);
            }
        }
        thread::sleep(TICK);
    }
}
