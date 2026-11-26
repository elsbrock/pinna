//! The boot stages on the LED ring (ADR 0012).
//!
//! - Progress: a blue arc grows clockwise from the Action button, one segment
//!   per stage; the segment of the stage in progress gets brighter and dimmer.
//! - A stage that missed its deadline: the whole ring flashes white N times,
//!   pauses, and repeats (N = the stage number).
//! - Rescue mode: green, slowly brighter and dimmer; steady once a computer
//!   has configured the USB gadget.
//!
//! Frames are computed here, without I/O, for pinna-init and the root's stage
//! helper. Values are the raw PWM levels the ring driver takes; every frame
//! stays well below the ring's power budget (a full white ring at 0xff).
//! [`Progress`] tracks how far a boot got, for both the stage helper and
//! pinnad.

use std::time::Duration;

/// LEDs on the ring.
pub const PIXELS: usize = 12;
/// Stages of a boot. Boot progress ends when the boot is marked healthy.
pub const STAGES: u8 = 7;

pub const STORAGE: u8 = 1;
pub const ROOT: u8 = 2;
pub const SYSTEM: u8 = 3;
pub const USB: u8 = 4;
pub const WIFI_DRIVER: u8 = 5;
pub const PINNA: u8 = 6;
pub const HEALTHY: u8 = 7;

/// What stage `n` stands for, as the docs name it.
pub fn name(n: u8) -> &'static str {
    match n {
        STORAGE => "storage found",
        ROOT => "root verified and mounted",
        SYSTEM => "system started",
        USB => "USB ready",
        WIFI_DRIVER => "Wi-Fi driver up",
        PINNA => "Pinna running",
        HEALTHY => "healthy",
        _ => "unknown stage",
    }
}

/// One frame: RGB per LED, in the driver's LED order.
pub type Frame = [[u8; 3]; PIXELS];

/// How long a stage has, as time since boot, before it counts as failed.
/// Stages without a deadline wait for as long as it takes.
pub fn deadline(n: u8) -> Option<Duration> {
    match n {
        USB => Some(Duration::from_secs(30)),
        WIFI_DRIVER => Some(Duration::from_secs(150)),
        PINNA => Some(Duration::from_secs(180)),
        _ => None,
    }
}

/// How far a boot got: `done` stages complete, and the first stage that
/// missed its deadline. The failure is kept for the rest of the boot — the
/// ring flashes it while the device is idle (ADR 0012).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Progress {
    pub done: u8,
    pub failed: Option<u8>,
}

impl Progress {
    pub fn new(done: u8) -> Self {
        Progress { done, failed: None }
    }

    /// Advance through consecutive stages whose condition holds now, then
    /// record the deadline of the stage still pending. `upto` is the last
    /// stage this side can complete: the stage helper reaches [`PINNA`],
    /// pinnad also reaches [`HEALTHY`].
    pub fn update(&mut self, upto: u8, since_boot: Duration, is_done: impl Fn(u8) -> bool) {
        while self.done < upto && is_done(self.done + 1) {
            self.done += 1;
        }
        if self.failed.is_none()
            && self.done < upto
            && let Some(limit) = deadline(self.done + 1)
            && since_boot >= limit
        {
            self.failed = Some(self.done + 1);
        }
    }

    /// The `/run/pinna/boot-stages` text the stage helper writes for pinnad,
    /// so it continues from where the helper stopped.
    pub fn encode(&self) -> String {
        match self.failed {
            Some(n) => format!("stage {}\nfailed {n}\n", self.done),
            None => format!("stage {}\n", self.done),
        }
    }

    /// Read [`Progress::encode`]; unknown lines are ignored, and a missing
    /// stage reads as 0.
    pub fn parse(text: &str) -> Self {
        let number = |line: &str, key: &str| {
            line.strip_prefix(key)?
                .trim()
                .parse::<u8>()
                .ok()
                .filter(|n| *n <= STAGES)
        };
        let mut p = Progress::default();
        for line in text.lines() {
            if let Some(n) = number(line, "stage ") {
                p.done = n;
            } else if let Some(n) = number(line, "failed ") {
                p.failed = Some(n);
            }
        }
        p
    }
}

/// Scale a whole frame by `k` (0..=1), for the healthy full-ring fade.
pub fn scale_frame(frame: &Frame, k: f32) -> Frame {
    frame.map(|c| scaled(c, k))
}

/// Pixel i sits at 45 + 30 i degrees clockwise from the front, seen from above
/// (pinna-hal's ring geometry; a board's driver turns the pixels onto its own
/// LEDs, see `ring::offset_for`). The MT8163 Echos' Action button is at the
/// back, 180 degrees, between pixels 4 and 5, so the arc starts at pixel 5.
/// Where the Echo Dot (5th gen)'s button is hasn't been checked.
const ARC_START: usize = 5;

const BLUE: [u8; 3] = [0x00, 0x30, 0xff];
const WHITE: [u8; 3] = [0xa0, 0xa0, 0xa0];
const GREEN: [u8; 3] = [0x00, 0xd0, 0x20];

const FLASH: Duration = Duration::from_millis(300);
const CODE_PAUSE: Duration = Duration::from_secs(2);
const BREATH: Duration = Duration::from_millis(1600);
const RESCUE_BREATH: Duration = Duration::from_secs(3);

fn scaled(c: [u8; 3], k: f32) -> [u8; 3] {
    let k = k.clamp(0.0, 1.0);
    c.map(|v| (f32::from(v) * k).round() as u8)
}

/// 0 at t = 0, 1 half a period later, smooth.
fn wave(t: Duration, period: Duration) -> f32 {
    let phase = t.as_secs_f32() / period.as_secs_f32() * std::f32::consts::TAU;
    0.5 - 0.5 * phase.cos()
}

/// Length of the arc after `done` stages, in LEDs (fractional).
fn arc_len(done: u8) -> f32 {
    f32::from(done.min(STAGES)) * PIXELS as f32 / f32::from(STAGES)
}

/// `done` stages complete; the next one in progress. At `t` = 0 the pending
/// segment is at its dimmest, which is also the frame to leave behind when
/// nothing animates the ring any more.
pub fn progress(done: u8, t: Duration) -> Frame {
    let lit = arc_len(done);
    let next = arc_len(done.saturating_add(1));
    let breath = 0.12 + 0.6 * wave(t, BREATH);
    let mut f = [[0; 3]; PIXELS];
    for k in 0..PIXELS {
        let pos = k as f32;
        let solid = (lit - pos).clamp(0.0, 1.0);
        let pending = (next - pos).clamp(0.0, 1.0) - solid;
        f[(ARC_START + k) % PIXELS] = scaled(BLUE, solid + pending * breath);
    }
    f
}

/// Stage `n` missed its deadline: N white flashes, a pause, repeat.
pub fn code(n: u8, t: Duration) -> Frame {
    let n = u32::from(n.clamp(1, STAGES));
    let flashes = FLASH * 2 * n;
    let period = flashes + CODE_PAUSE;
    let phase = Duration::from_nanos((t.as_nanos() % period.as_nanos()) as u64);
    let on = phase < flashes && (phase.as_nanos() / FLASH.as_nanos()).is_multiple_of(2);
    [if on { WHITE } else { [0; 3] }; PIXELS]
}

/// Rescue mode: green breathing, steady once a computer is connected.
pub fn rescue(host_connected: bool, t: Duration) -> Frame {
    let k = if host_connected {
        0.7
    } else {
        0.1 + 0.6 * wave(t, RESCUE_BREATH)
    };
    [scaled(GREEN, k); PIXELS]
}

/// The ring driver's `frame` format: 12 x `rrggbb`, then a newline.
pub fn encode(frame: &Frame) -> String {
    let mut s = String::with_capacity(PIXELS * 6 + 1);
    for [r, g, b] in frame {
        s.push_str(&format!("{r:02x}{g:02x}{b:02x}"));
    }
    s.push('\n');
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    /// LEDs along the arc, from the Action button clockwise.
    fn along_arc(f: &Frame) -> Vec<[u8; 3]> {
        (0..PIXELS).map(|k| f[(ARC_START + k) % PIXELS]).collect()
    }

    #[test]
    fn arc_starts_at_the_action_button_and_grows_clockwise() {
        let f = along_arc(&progress(3, ms(0)));
        // 3 of 7 stages: 5.14 LEDs solid.
        assert!(f[..5].iter().all(|p| *p == BLUE), "{f:?}");
        assert!(f[5][2] > 0 && f[5][2] < BLUE[2]);
        assert!(f[8..].iter().all(|p| *p == [0; 3]), "{f:?}");
        assert_eq!(progress(3, ms(0))[5], BLUE);
        assert_eq!(progress(3, ms(0))[4], [0; 3]);
    }

    #[test]
    fn pending_segment_breathes() {
        let dim = along_arc(&progress(1, ms(0)))[2];
        let bright = along_arc(&progress(1, BREATH / 2))[2];
        assert!(bright[2] > dim[2] && dim[2] > 0, "{dim:?} {bright:?}");
        // The completed part does not move.
        assert_eq!(along_arc(&progress(1, ms(0)))[0], BLUE);
        assert_eq!(along_arc(&progress(1, BREATH / 2))[0], BLUE);
    }

    #[test]
    fn healthy_is_a_full_ring() {
        assert_eq!(progress(STAGES, ms(0)), [BLUE; PIXELS]);
        assert_eq!(progress(STAGES, ms(777)), [BLUE; PIXELS]);
        assert_eq!(progress(0, ms(0))[ARC_START + 2], [0; 3]);
    }

    #[test]
    fn code_flashes_the_stage_number_then_pauses() {
        let on = |t| code(4, ms(t))[0] == WHITE;
        // One period: 4 flashes of 300 ms (3 samples each at 100 ms), 2 s off.
        let lit_samples = (0..4400).step_by(100).filter(|&t| on(t)).count();
        assert_eq!(lit_samples, 4 * 3);
        assert!(on(0) && !on(300) && on(600) && on(1800) && !on(2100));
        assert!(!on(2500) && !on(4300), "pause after the flashes");
        assert!(on(4400), "repeats");
        assert!(code(4, ms(0)).iter().all(|p| *p == WHITE));
    }

    #[test]
    fn frames_stay_below_the_power_budget() {
        // pinna-hal caps a frame at 5967 units (65% of full white).
        let load = |f: Frame| f.iter().flatten().map(|&c| u32::from(c)).sum::<u32>();
        assert!(load(code(1, ms(0))) < 5967);
        assert!(load(progress(STAGES, ms(0))) < 5967);
        assert!(load(rescue(true, ms(0))) < 5967);
    }

    #[test]
    fn rescue_is_green_and_steady_with_a_host() {
        assert_eq!(rescue(true, ms(0)), rescue(true, ms(1234)));
        assert_ne!(rescue(false, ms(0)), rescue(false, RESCUE_BREATH / 2));
        assert!(rescue(false, ms(0)).iter().all(|p| p[1] > 0 && p[0] == 0));
    }

    #[test]
    fn encoding() {
        let mut f = [[0; 3]; PIXELS];
        f[0] = [1, 0x2a, 0xff];
        let s = encode(&f);
        assert!(s.starts_with("012aff000000"));
        assert_eq!(s.len(), PIXELS * 6 + 1);
        assert!(s.ends_with('\n'));
    }

    #[test]
    fn names() {
        assert_eq!(name(STORAGE), "storage found");
        assert_eq!(name(HEALTHY), "healthy");
        assert_eq!(name(9), "unknown stage");
    }

    #[test]
    fn deadlines() {
        assert_eq!(deadline(USB), Some(Duration::from_secs(30)));
        assert_eq!(deadline(WIFI_DRIVER), Some(Duration::from_secs(150)));
        assert_eq!(deadline(PINNA), Some(Duration::from_secs(180)));
        assert_eq!(deadline(STORAGE), None);
        assert_eq!(deadline(HEALTHY), None);
    }

    #[test]
    fn progress_advances_and_records_the_first_missed_deadline() {
        let secs = Duration::from_secs;
        // Nothing is up: storage..system are done, USB pends, no deadline yet.
        let mut p = Progress::new(SYSTEM);
        p.update(PINNA, secs(10), |_| false);
        assert_eq!(p, Progress::new(SYSTEM));
        // USB is late: at its deadline the code is recorded and stays.
        p.update(PINNA, secs(30), |_| false);
        assert_eq!(p.failed, Some(USB));
        // USB comes up later anyway: the arc advances, the code stays.
        p.update(PINNA, secs(60), |n| n == USB);
        assert_eq!(
            p,
            Progress {
                done: USB,
                failed: Some(USB)
            }
        );
        // Wi-Fi pends; its deadline is later, and the first failure wins.
        p.update(PINNA, secs(200), |n| n == USB);
        assert_eq!(
            p,
            Progress {
                done: USB,
                failed: Some(USB)
            }
        );
        // The helper never counts healthy; pinnad does.
        let mut q = Progress::new(PINNA);
        q.update(HEALTHY, secs(0), |_| false);
        assert_eq!(q.done, PINNA);
        q.update(HEALTHY, secs(0), |n| n == HEALTHY);
        assert_eq!(q.done, HEALTHY);
    }

    #[test]
    fn progress_round_trips_through_its_text() {
        assert_eq!(Progress::new(6).encode(), "stage 6\n");
        assert_eq!(
            Progress {
                done: 4,
                failed: Some(5)
            }
            .encode(),
            "stage 4\nfailed 5\n"
        );
        assert_eq!(Progress::parse("stage 6\n"), Progress::new(6));
        assert_eq!(
            Progress::parse("stage 4\nfailed 5\n"),
            Progress {
                done: 4,
                failed: Some(5)
            }
        );
        assert_eq!(Progress::parse(""), Progress::new(0));
        assert_eq!(
            Progress::parse("stage x\nother 3\nfailed 9\n"),
            Progress::new(0)
        );
    }

    #[test]
    fn scale_frame_fades() {
        let full = progress(STAGES, Duration::ZERO);
        assert_eq!(scale_frame(&full, 1.0), full);
        assert_eq!(scale_frame(&full, 0.0), [[0; 3]; PIXELS]);
        let half = scale_frame(&progress(1, Duration::ZERO), 0.5);
        assert_eq!(half[ARC_START], scaled(BLUE, 0.5));
    }
}
