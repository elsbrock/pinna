//! Time-based LED effects as pure functions of elapsed time.

use crate::led::{FRONT_CENTRE, Frame, RING_PIXELS, Rgb};
use std::f32::consts::TAU;
use std::time::Duration;

/// LibreEcho's esphomed theme colours (esphomed.c profile()).
pub const LISTENING: Rgb = Rgb::new(72, 185, 255);
pub const THINKING: Rgb = Rgb::new(168, 115, 239);
pub const ERROR: Rgb = Rgb::new(239, 80, 80);
pub const MUTED: Rgb = Rgb::new(255, 0, 0);
pub const SPEAKING: Rgb = Rgb::new(90, 220, 230);
pub const AMBER: Rgb = Rgb::new(255, 160, 0);
/// Volume meter colour (buttond.c show_meter on unmute).
pub const VOLUME: Rgb = Rgb::new(120, 200, 255);
/// Music equalizer, bass to treble.
pub const EQUALIZER: [Rgb; RING_PIXELS / 2] = [
    Rgb::new(255, 40, 110),
    Rgb::new(255, 90, 30),
    Rgb::new(255, 170, 0),
    Rgb::new(110, 230, 50),
    Rgb::new(40, 215, 215),
    Rgb::new(70, 120, 255),
];

/// How long the volume meter is held.
pub const VOLUME_HOLD: Duration = Duration::from_millis(1500);
const ERROR_FLASH: Duration = Duration::from_millis(250);
const ERROR_FLASHES: u32 = 3;
const BOOT_DURATION: Duration = Duration::from_millis(1200);

// Listening direction cue (LibreEcho ledd.c, PATTERN_DIRECTION). Pixel i sits
// at (45 + 30 * i) degrees clockwise from the front, so the front-centre pair
// 10/11 is at -15/+15 degrees.
const PIXEL_ZERO_DEG: u32 = 45;
const PIXEL_STEP_DEG: u32 = 30;
/// The rest of the ring stays at this share of the Listening colour.
const DIRECTION_BASE_PERCENT: u32 = 40;
/// The segment's centre is the Listening colour this far towards white.
const DIRECTION_POINTER_PERCENT: u32 = 85;
/// Half the segment's width; brightness falls linearly to the base at the edges.
const DIRECTION_HALF_WIDTH_DEG: u32 = 50;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Effect {
    Idle,
    Listening,
    /// Listening, with a brighter segment pointing at the talker (azimuth in
    /// degrees, 0 = front, clockwise from above).
    ListeningAt(u16),
    Thinking,
    Speaking,
    Error,
    Muted,
    /// Level 0..=100.
    Volume(u8),
    /// Fraction of the timer remaining, 0.0..=1.0.
    Timer(f32),
    TimerRinging,
    Boot,
    /// Music: band levels 0.0..=1.0, bass first, one band per pixel.
    Equalizer([f32; RING_PIXELS]),
}

impl Effect {
    /// Effects with a natural end return the time they finish; the rest run
    /// until replaced.
    pub fn duration(self) -> Option<Duration> {
        match self {
            Effect::Error => Some(ERROR_FLASH * 2 * ERROR_FLASHES),
            Effect::Volume(_) => Some(VOLUME_HOLD),
            Effect::Boot => Some(BOOT_DURATION),
            _ => None,
        }
    }

    pub fn is_finished(self, t: Duration) -> bool {
        self.duration().is_some_and(|d| t >= d)
    }
}

fn solid(c: Rgb) -> Frame {
    [c; RING_PIXELS]
}

/// Smooth 0..1 oscillation with the given period.
fn wave(t: Duration, period_s: f32) -> f32 {
    0.5 - 0.5 * (t.as_secs_f32() / period_s * TAU).cos()
}

/// A bright head with a fading tail rotating around the ring.
fn spin(colour: Rgb, t: Duration, rev_s: f32, tail: f32) -> Frame {
    let pos = (t.as_secs_f32() / rev_s).fract() * RING_PIXELS as f32;
    let mut f = [Rgb::OFF; RING_PIXELS];
    for (i, p) in f.iter_mut().enumerate() {
        // Distance behind the head, in pixels.
        let behind = (pos - i as f32).rem_euclid(RING_PIXELS as f32);
        let level = (1.0 - behind / tail).max(0.0);
        *p = colour.scaled(level * level);
    }
    f
}

pub fn render(effect: Effect, t: Duration) -> Frame {
    match effect {
        Effect::Idle => solid(Rgb::OFF),
        Effect::Listening => solid(LISTENING.scaled(0.3 + 0.7 * wave(t, 1.6))),
        Effect::ListeningAt(azimuth) => direction(LISTENING, u32::from(azimuth) % 360),
        Effect::Thinking => spin(THINKING, t, 1.2, 5.0),
        Effect::Speaking => solid(SPEAKING.scaled(0.25 + 0.55 * wave(t, 2.4))),
        Effect::Error => {
            if effect.is_finished(t) {
                return solid(Rgb::OFF);
            }
            let phase = t.as_millis() / ERROR_FLASH.as_millis();
            if phase.is_multiple_of(2) {
                solid(ERROR)
            } else {
                solid(Rgb::OFF)
            }
        }
        Effect::Muted => solid(MUTED),
        Effect::Volume(level) => {
            if effect.is_finished(t) {
                return solid(Rgb::OFF);
            }
            arc(VOLUME, f32::from(level.min(100)) / 100.0)
        }
        Effect::Timer(remaining) => arc(AMBER, remaining.clamp(0.0, 1.0)),
        Effect::TimerRinging => {
            // Two pulses per second.
            solid(AMBER.scaled(0.15 + 0.85 * wave(t, 0.5)))
        }
        Effect::Boot => {
            if effect.is_finished(t) {
                return solid(Rgb::OFF);
            }
            spin(LISTENING, t, 0.6, 6.0)
        }
        Effect::Equalizer(bands) => equalizer(&bands),
    }
}

/// The bands in pairs (the louder of each) as six, mirrored either side of the
/// front centre: bass at the front, treble at the back.
fn equalizer(bands: &[f32; RING_PIXELS]) -> Frame {
    let mut f = [Rgb::OFF; RING_PIXELS];
    for (k, colour) in EQUALIZER.iter().enumerate() {
        let c = colour.scaled(bands[2 * k].max(bands[2 * k + 1]));
        f[(FRONT_CENTRE[1] + k) % RING_PIXELS] = c;
        f[(FRONT_CENTRE[0] + RING_PIXELS - k) % RING_PIXELS] = c;
    }
    f
}

/// Angle of pixel `i`, degrees clockwise from the front.
fn pixel_angle(i: usize) -> u32 {
    (PIXEL_ZERO_DEG + PIXEL_STEP_DEG * i as u32) % 360
}

/// A dim ring with a brighter segment at `azimuth`, interpolated across
/// neighbouring pixels so twelve pixels still read as a direction. Integer
/// arithmetic as in ledd.c, so the frames match LibreEcho's.
fn direction(colour: Rgb, azimuth: u32) -> Frame {
    let base = |c: u8| u32::from(c) * DIRECTION_BASE_PERCENT / 100;
    let pointer = |c: u8| u32::from(c) + (255 - u32::from(c)) * DIRECTION_POINTER_PERCENT / 100;
    let mut f = [Rgb::OFF; RING_PIXELS];
    for (i, p) in f.iter_mut().enumerate() {
        let d = pixel_angle(i).abs_diff(azimuth);
        let distance = if d > 180 { 360 - d } else { d };
        let level =
            DIRECTION_HALF_WIDTH_DEG.saturating_sub(distance) * 100 / DIRECTION_HALF_WIDTH_DEG;
        let mix = |c: u8| (base(c) + (pointer(c) - base(c)) * level / 100) as u8;
        *p = Rgb::new(mix(colour.r), mix(colour.g), mix(colour.b));
    }
    f
}

/// Meter / countdown arc starting at pixel 0, with a fractional last pixel and
/// a dim track for the unlit remainder.
fn arc(colour: Rgb, fraction: f32) -> Frame {
    let lit = fraction * RING_PIXELS as f32;
    let mut f = [Rgb::OFF; RING_PIXELS];
    for (i, p) in f.iter_mut().enumerate() {
        let level = (lit - i as f32).clamp(0.0, 1.0);
        *p = if level > 0.0 {
            colour.scaled(level)
        } else {
            colour.scaled(0.04)
        };
    }
    f
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    fn lit(f: &Frame) -> usize {
        f.iter().filter(|p| **p != Rgb::OFF).count()
    }

    #[test]
    fn idle_is_dark() {
        assert_eq!(lit(&render(Effect::Idle, ms(123))), 0);
    }

    #[test]
    fn muted_is_solid_red() {
        assert_eq!(render(Effect::Muted, ms(5)), [Rgb::new(255, 0, 0); 12]);
    }

    #[test]
    fn listening_pulses_between_bounds() {
        let lo = render(Effect::Listening, ms(0))[0];
        let hi = render(Effect::Listening, ms(800))[0];
        assert!(hi.b > lo.b);
        assert_eq!(hi, LISTENING);
        assert!(lo.b > 0);
        // Periodic.
        assert_eq!(
            render(Effect::Listening, ms(100)),
            render(Effect::Listening, ms(1700))
        );
    }

    #[test]
    fn thinking_spins() {
        let a = render(Effect::Thinking, ms(0));
        let b = render(Effect::Thinking, ms(200));
        assert_ne!(a, b);
        assert_eq!(a[0], THINKING); // head at pixel 0 at t=0
        assert_eq!(
            render(Effect::Thinking, ms(0)),
            render(Effect::Thinking, ms(1200))
        );
    }

    #[test]
    fn error_flashes_three_times_then_off() {
        let on = |t| render(Effect::Error, ms(t))[0] == ERROR;
        assert!(on(0) && !on(300) && on(500) && !on(800) && on(1000) && !on(1300));
        assert!(!on(1600));
        assert!(Effect::Error.is_finished(ms(1500)));
        assert!(!Effect::Error.is_finished(ms(1499)));
    }

    #[test]
    fn volume_arc_scales_and_expires() {
        let f = render(Effect::Volume(50), ms(100));
        assert_eq!(f[0], VOLUME);
        assert_eq!(f[5], VOLUME);
        assert_ne!(f[6], VOLUME);
        assert_eq!(render(Effect::Volume(100), ms(0))[11], VOLUME);
        let zero = render(Effect::Volume(0), ms(0));
        assert!(zero.iter().all(|p| p.b <= 10));
        assert_eq!(lit(&render(Effect::Volume(50), ms(1500))), 0);
        assert!(Effect::Volume(50).is_finished(VOLUME_HOLD));
    }

    #[test]
    fn timer_arc_tracks_fraction() {
        let f = render(Effect::Timer(0.25), ms(0));
        assert_eq!(f[0], AMBER);
        assert_eq!(f[2], AMBER);
        assert_ne!(f[3], AMBER);
        let full = render(Effect::Timer(1.0), ms(0));
        assert!(full.iter().all(|p| *p == AMBER));
    }

    #[test]
    fn timer_ringing_pulses_at_2hz() {
        let dim = render(Effect::TimerRinging, ms(0))[0];
        let bright = render(Effect::TimerRinging, ms(250))[0];
        assert!(bright.r > dim.r);
        assert_eq!(
            render(Effect::TimerRinging, ms(100)),
            render(Effect::TimerRinging, ms(600))
        );
    }

    #[test]
    fn boot_spins_then_off() {
        assert!(lit(&render(Effect::Boot, ms(100))) > 0);
        assert_eq!(lit(&render(Effect::Boot, ms(1300))), 0);
    }

    #[test]
    fn listening_points_at_the_talker() {
        let base = Rgb::new(28, 74, 102);
        let front = render(Effect::ListeningAt(0), ms(0));
        // 0 degrees falls between the front-centre pair, 15 degrees off each.
        assert_eq!(front[10], front[11]);
        assert_eq!(front[11], Rgb::new(167, 193, 209));
        // 45 degrees off: still inside the 50-degree half width.
        assert_eq!(front[0], Rgb::new(47, 91, 117));
        assert_eq!(front[9], front[0]);
        assert!(front[1..9].iter().all(|p| *p == base), "{front:?}");
        // Pixel 0 sits at 45 degrees: the segment's full brightness.
        let right = render(Effect::ListeningAt(45), ms(0));
        assert_eq!(right[0], Rgb::new(227, 244, 255));
        assert_eq!(right[0], render(Effect::ListeningAt(405), ms(0))[0]);
        // Behind the device (195 degrees): pixel 5.
        let back = render(Effect::ListeningAt(195), ms(0));
        assert_eq!(back[5], right[0]);
        assert_eq!(back[11], base);
        // Static: the cue does not animate.
        assert_eq!(back, render(Effect::ListeningAt(195), ms(777)));
    }

    #[test]
    fn speaking_breathes() {
        let lo = render(Effect::Speaking, ms(0))[0];
        let hi = render(Effect::Speaking, ms(1200))[0];
        assert!(hi.g > lo.g);
    }

    #[test]
    fn equalizer_mirrors_pairs_bass_at_the_front() {
        assert_eq!(lit(&render(Effect::Equalizer([0.0; 12]), ms(0))), 0);
        let mut bands = [0.0; 12];
        bands[1] = 1.0; // bass pair (0, 1) -> pixels 10 and 11
        bands[10] = 0.5; // treble pair (10, 11) -> pixels 4 and 5, half level
        let f = render(Effect::Equalizer(bands), ms(0));
        assert_eq!(f[10], EQUALIZER[0]);
        assert_eq!(f[11], EQUALIZER[0]);
        assert_eq!(f[4], EQUALIZER[5].scaled(0.5));
        assert_eq!(f[5], EQUALIZER[5].scaled(0.5));
        assert_eq!(lit(&f), 4);
        // Pair k sits at 11 + k and 10 - k.
        let mut bands = [0.0; 12];
        bands[4] = 1.0;
        let f = render(Effect::Equalizer(bands), ms(0));
        assert_eq!((f[1], f[8]), (EQUALIZER[2], EQUALIZER[2]));
        assert_eq!(lit(&f), 2);
    }

    #[test]
    fn open_ended_effects_do_not_finish() {
        for e in [
            Effect::Idle,
            Effect::Listening,
            Effect::ListeningAt(90),
            Effect::Thinking,
            Effect::Speaking,
            Effect::Muted,
            Effect::TimerRinging,
            Effect::Equalizer([1.0; 12]),
        ] {
            assert!(!e.is_finished(Duration::from_secs(3600)));
        }
    }
}
