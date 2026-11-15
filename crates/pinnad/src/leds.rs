//! LED ring renderer: turns the daemon's state into frames at ~30 fps.

use std::time::{Duration, Instant};

use pinna_audio::{Bands, Spectrum};
use pinna_hal::anim::{Effect, render};
use pinna_hal::led::{Frame, LedHandle, RING_PIXELS, Rgb};
use tokio::sync::watch;

/// How the Home Assistant light entity is shown (its effect list: Solid, Pulse, Flash).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LightEffect {
    #[default]
    Solid,
    /// Breathing, 2 s period.
    Pulse,
    /// On/off at 1 Hz.
    Flash,
}

impl LightEffect {
    pub const NAMES: [&str; 3] = ["Solid", "Pulse", "Flash"];

    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "Solid" | "None" => Some(Self::Solid),
            "Pulse" => Some(Self::Pulse),
            "Flash" => Some(Self::Flash),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        Self::NAMES[self as usize]
    }

    /// Brightness factor at time `t` since the light was shown.
    fn level(self, t: Duration) -> f32 {
        let s = t.as_secs_f32();
        match self {
            Self::Solid => 1.0,
            Self::Pulse => 0.15 + 0.85 * (0.5 - 0.5 * (s * std::f32::consts::PI).cos()),
            Self::Flash => {
                if s.fract() < 0.5 {
                    1.0
                } else {
                    0.0
                }
            }
        }
    }
}

fn scale(rgb: Rgb, k: f32) -> Rgb {
    let f = |c: u8| (f32::from(c) * k).round().clamp(0.0, 255.0) as u8;
    Rgb {
        r: f(rgb.r),
        g: f(rgb.g),
        b: f(rgb.b),
    }
}

/// What the ring should show. The voice state machine owns it.
#[derive(Debug, Clone, PartialEq)]
pub struct LedState {
    /// Persistent effect (Idle, Listening, Thinking, Speaking, Muted, TimerRinging, ...).
    pub base: Effect,
    /// Transient effect shown on top until it finishes (Error, Volume).
    pub overlay: Option<(Effect, Instant)>,
    /// Home Assistant light entity: shown while the base effect is Idle.
    pub light: Option<Rgb>,
    pub light_effect: LightEffect,
    /// Remaining fraction of the soonest timer, shown while idle.
    pub timer: Option<f32>,
    /// Overall ring brightness 0..=100 ("LED brightness" number).
    pub brightness: u8,
}

impl Default for LedState {
    fn default() -> Self {
        Self {
            base: Effect::Boot,
            overlay: None,
            light: None,
            light_effect: LightEffect::Solid,
            timer: None,
            brightness: 100,
        }
    }
}

/// `music`: the band levels sounding now, if music is playing.
pub fn frame_for(
    state: &LedState,
    music: Option<&Bands>,
    base_since: Instant,
    now: Instant,
) -> Frame {
    let mut frame = unscaled_frame(state, music, base_since, now);
    if state.brightness < 100 {
        let k = f32::from(state.brightness) / 100.0;
        for px in &mut frame {
            *px = scale(*px, k);
        }
    }
    frame
}

fn unscaled_frame(
    state: &LedState,
    music: Option<&Bands>,
    base_since: Instant,
    now: Instant,
) -> Frame {
    if let Some((effect, since)) = state.overlay {
        let t = now.saturating_duration_since(since);
        if !effect.is_finished(t) {
            return render(effect, t);
        }
    }
    let t = now.saturating_duration_since(base_since);
    match state.base {
        Effect::Idle | Effect::Boot
            if state.base == Effect::Boot && !Effect::Boot.is_finished(t) =>
        {
            render(Effect::Boot, t)
        }
        Effect::Idle | Effect::Boot => {
            if let Some(rgb) = state.light {
                [scale(rgb, state.light_effect.level(t)); RING_PIXELS]
            } else if let Some(frac) = state.timer {
                render(Effect::Timer(frac), t)
            } else if let Some(bands) = music {
                render(Effect::Equalizer(*bands), t)
            } else {
                render(Effect::Idle, t)
            }
        }
        other => render(other, t),
    }
}

/// Drive the ring from `state` (and `music`, the speaker's spectrum) until the
/// sender is dropped.
pub async fn run(
    leds: Option<LedHandle>,
    mut state: watch::Receiver<LedState>,
    music: Option<Spectrum>,
) {
    let Some(leds) = leds else {
        // No ring: just log transitions.
        let mut last = None;
        while state.changed().await.is_ok() {
            let base = state.borrow().base;
            if last != Some(base) {
                tracing::debug!("led effect: {base:?}");
                last = Some(base);
            }
        }
        return;
    };
    let mut current = state.borrow().clone();
    let mut base_since = Instant::now();
    let mut tick = tokio::time::interval(Duration::from_millis(33));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            changed = state.changed() => {
                if changed.is_err() {
                    leds.show(&[Rgb::OFF; RING_PIXELS]);
                    return;
                }
                let next = state.borrow().clone();
                if next.base != current.base {
                    base_since = Instant::now();
                }
                current = next;
            }
            _ = tick.tick() => {}
        }
        let now = Instant::now();
        let bands = music.as_ref().and_then(|m| m.at(now));
        leds.show(&frame_for(&current, bands.as_ref(), base_since, now));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn light_shown_when_idle_only() {
        let now = Instant::now();
        let green = Rgb {
            r: 0,
            g: 200,
            b: 40,
        };
        let mut s = LedState {
            base: Effect::Idle,
            light: Some(green),
            ..Default::default()
        };
        assert_eq!(frame_for(&s, None, now, now), [green; RING_PIXELS]);
        s.base = Effect::Muted;
        assert_ne!(frame_for(&s, None, now, now), [green; RING_PIXELS]);
    }

    #[test]
    fn brightness_and_light_effects() {
        let now = Instant::now();
        let white = Rgb {
            r: 200,
            g: 200,
            b: 200,
        };
        let mut s = LedState {
            base: Effect::Idle,
            light: Some(white),
            brightness: 50,
            ..Default::default()
        };
        assert_eq!(frame_for(&s, None, now, now)[0].r, 100);
        s.brightness = 100;
        s.light_effect = LightEffect::Flash;
        assert_eq!(
            frame_for(&s, None, now, now + Duration::from_millis(100))[0].r,
            200
        );
        assert_eq!(
            frame_for(&s, None, now, now + Duration::from_millis(600))[0].r,
            0
        );
        s.light_effect = LightEffect::Pulse;
        let low = frame_for(&s, None, now, now)[0].r;
        let high = frame_for(&s, None, now, now + Duration::from_secs(1))[0].r;
        assert!(low < 50 && high == 200, "{low} {high}");
        assert_eq!(LightEffect::from_name("Flash"), Some(LightEffect::Flash));
        assert_eq!(LightEffect::Pulse.name(), "Pulse");
    }

    #[test]
    fn music_shows_only_instead_of_the_dark_idle_ring() {
        let now = Instant::now();
        let bands = [1.0; 12];
        let eq = render(Effect::Equalizer(bands), Duration::ZERO);
        let mut s = LedState {
            base: Effect::Idle,
            ..Default::default()
        };
        assert_eq!(frame_for(&s, Some(&bands), now, now), eq);
        assert_eq!(frame_for(&s, None, now, now), [Rgb::OFF; RING_PIXELS]);
        // A running timer and the light win over the equalizer.
        s.timer = Some(0.5);
        assert_eq!(
            frame_for(&s, Some(&bands), now, now),
            render(Effect::Timer(0.5), Duration::ZERO)
        );
        s.light = Some(Rgb::new(0, 0, 200));
        assert_eq!(
            frame_for(&s, Some(&bands), now, now),
            [Rgb::new(0, 0, 200); RING_PIXELS]
        );
        // So do the voice states.
        s.base = Effect::Thinking;
        assert_eq!(
            frame_for(&s, Some(&bands), now, now),
            render(Effect::Thinking, Duration::ZERO)
        );
    }

    #[test]
    fn overlay_expires() {
        let now = Instant::now();
        let s = LedState {
            base: Effect::Idle,
            overlay: Some((Effect::Volume(50), now)),
            ..Default::default()
        };
        let during = frame_for(&s, None, now, now + Duration::from_millis(100));
        let after = frame_for(&s, None, now, now + Duration::from_secs(10));
        assert_ne!(during, after);
        assert_eq!(after, render(Effect::Idle, Duration::from_secs(10)));
    }
}
