//! Per-sample gain ramps for the speaker's two lanes (voice and media).

use crate::decode::OUT_RATE;
use std::time::Duration;

/// Duck down over this long when the voice lane starts or a duck is held.
pub const DUCK_ATTACK: Duration = Duration::from_millis(150);
/// Stay ducked this long after the voice lane runs dry.
pub const DUCK_HOLD: Duration = Duration::from_millis(300);
/// Come back up over this long.
pub const DUCK_RELEASE: Duration = Duration::from_millis(500);
/// Full-scale ramp for voice gain, pause and mute (click-free, still snappy).
pub const FADE: Duration = Duration::from_millis(20);

pub(crate) fn frames_of(d: Duration) -> f32 {
    d.as_secs_f32() * OUT_RATE as f32
}

/// Linear ramp towards a target, `step` per frame.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Ramp {
    cur: f32,
    target: f32,
    step: f32,
}

impl Ramp {
    pub fn new(value: f32, full_scale: Duration) -> Ramp {
        Ramp {
            cur: value,
            target: value,
            step: 1.0 / frames_of(full_scale).max(1.0),
        }
    }

    pub fn set(&mut self, target: f32) {
        self.target = target;
    }

    pub fn jump(&mut self, value: f32) {
        self.cur = value;
        self.target = value;
    }

    pub fn value(&self) -> f32 {
        self.cur
    }

    pub fn tick(&mut self) -> f32 {
        if self.cur < self.target {
            self.cur = (self.cur + self.step).min(self.target);
        } else if self.cur > self.target {
            self.cur = (self.cur - self.step).max(self.target);
        }
        self.cur
    }
}

/// Media duck gain: 1.0 when clear, `floor` when fully ducked.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Ducker {
    level: f32,
    floor: f32,
    hold_left: u32,
    hold_frames: u32,
    attack_step: f32,
    release_step: f32,
}

impl Ducker {
    pub fn new(db: f32) -> Ducker {
        let floor = 10f32.powf(db.min(0.0) / 20.0);
        let span = 1.0 - floor;
        Ducker {
            level: 1.0,
            floor,
            hold_left: 0,
            hold_frames: frames_of(DUCK_HOLD) as u32,
            attack_step: span / frames_of(DUCK_ATTACK).max(1.0),
            release_step: span / frames_of(DUCK_RELEASE).max(1.0),
        }
    }

    #[cfg(test)]
    pub fn level(&self) -> f32 {
        self.level
    }

    /// Advance one frame. `voice` is whether the voice lane sounds this
    /// frame, `held` an explicit duck (e.g. while listening).
    pub fn tick(&mut self, voice: bool, held: bool) -> f32 {
        if voice {
            self.hold_left = self.hold_frames;
        } else if self.hold_left > 0 {
            self.hold_left -= 1;
        }
        if voice || held || self.hold_left > 0 {
            self.level = (self.level - self.attack_step).max(self.floor);
        } else {
            self.level = (self.level + self.release_step).min(1.0);
        }
        self.level
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(d: &mut Ducker, frames: usize, voice: bool, held: bool) {
        for _ in 0..frames {
            d.tick(voice, held);
        }
    }

    fn ms(n: u64) -> usize {
        frames_of(Duration::from_millis(n)) as usize
    }

    #[test]
    fn ramp_moves_linearly_and_stops() {
        let mut r = Ramp::new(0.0, Duration::from_millis(20));
        r.set(1.0);
        for _ in 0..480 {
            r.tick();
        }
        assert!((r.value() - 0.5).abs() < 1e-3);
        for _ in 0..2000 {
            r.tick();
        }
        assert_eq!(r.value(), 1.0);
        r.set(0.0);
        r.tick();
        assert!(r.value() < 1.0);
    }

    #[test]
    fn duck_attack_hold_release() {
        let mut d = Ducker::new(-20.0);
        assert_eq!(d.level(), 1.0);
        // Halfway through the attack: halfway between 1.0 and 0.1.
        run(&mut d, ms(75), true, false);
        assert!((d.level() - 0.55).abs() < 0.01, "{}", d.level());
        run(&mut d, ms(75), true, false);
        assert!((d.level() - 0.1).abs() < 1e-3);
        // Voice ends: stays ducked through the hold.
        run(&mut d, ms(290), false, false);
        assert!((d.level() - 0.1).abs() < 1e-3);
        // After the hold, release over 500 ms: halfway at +250 ms.
        run(&mut d, ms(10) + ms(250), false, false);
        assert!((d.level() - 0.55).abs() < 0.02, "{}", d.level());
        run(&mut d, ms(260), false, false);
        assert_eq!(d.level(), 1.0);
    }

    #[test]
    fn held_duck_has_no_hold_tail() {
        let mut d = Ducker::new(-20.0);
        run(&mut d, ms(200), false, true);
        assert!((d.level() - 0.1).abs() < 1e-3);
        run(&mut d, ms(250), false, false);
        assert!((d.level() - 0.55).abs() < 0.02, "{}", d.level());
    }

    #[test]
    fn voice_gap_shorter_than_hold_stays_ducked() {
        let mut d = Ducker::new(-20.0);
        run(&mut d, ms(200), true, false);
        run(&mut d, ms(200), false, false);
        run(&mut d, ms(200), true, false);
        assert!((d.level() - 0.1).abs() < 1e-3);
    }

    #[test]
    fn voice_and_held_overlap() {
        let mut d = Ducker::new(-20.0);
        run(&mut d, ms(200), true, true);
        run(&mut d, ms(500), false, true);
        assert!((d.level() - 0.1).abs() < 1e-3);
    }
}
