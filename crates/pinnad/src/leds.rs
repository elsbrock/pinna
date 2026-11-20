//! LED ring renderer: turns the daemon's state into frames at ~30 fps.

use std::time::{Duration, Instant};

use pinna_audio::beamview::BeamSnapshot;
use pinna_audio::{Bands, Spectrum, VoiceLevel};
use pinna_hal::anim::{Effect, render, speaking, speech_level};
use pinna_hal::led::{Frame, LedHandle, RING_PIXELS, Rgb};
use tokio::sync::watch;

/// How fast the speaking ring rises with the voice and falls back: fast
/// enough to show syllables, slow enough not to flicker.
const SPEECH_ATTACK: Duration = Duration::from_millis(25);
const SPEECH_RELEASE: Duration = Duration::from_millis(120);

/// Smooths the reply's level for the speaking ring.
#[derive(Debug, Default)]
struct Envelope {
    value: f32,
    at: Option<Instant>,
}

impl Envelope {
    /// Move towards `target` (0..=1) for the time since the last update.
    fn update(&mut self, target: f32, now: Instant) -> f32 {
        let dt = self
            .at
            .map_or(Duration::ZERO, |t| now.saturating_duration_since(t));
        let tau = if target > self.value {
            SPEECH_ATTACK
        } else {
            SPEECH_RELEASE
        };
        let k = 1.0 - (-dt.as_secs_f32() / tau.as_secs_f32()).exp();
        self.value += (target - self.value) * k;
        self.at = Some(now);
        self.value
    }
}

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
    /// Home Assistant light entity: shown while the base effect is Idle and
    /// neither a timer nor a notification is.
    pub light: Option<Rgb>,
    pub light_effect: LightEffect,
    /// Remaining fraction of the soonest timer, shown while idle.
    pub timer: Option<f32>,
    /// A notification is pending: shown while idle and no timer runs.
    pub notification: bool,
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
            notification: false,
            brightness: 100,
        }
    }
}

/// `music`: the band levels sounding now, if music is playing. `speech`: the
/// reply's smoothed level (0..=1) while it is heard; without it the speaking
/// ring breathes. `boot`: the boot-stage frame while the start is still on the
/// ring (ADR 0012); it shows only where the plain idle ring would.
pub fn frame_for(
    state: &LedState,
    music: Option<&Bands>,
    base_since: Instant,
    now: Instant,
    speech: Option<f32>,
    boot: Option<Frame>,
) -> Frame {
    let mut frame = unscaled_frame(state, music, base_since, now, speech, boot);
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
    speech: Option<f32>,
    boot: Option<Frame>,
) -> Frame {
    if let Some((effect, since)) = state.overlay {
        let t = now.saturating_duration_since(since);
        if !effect.is_finished(t) {
            return render(effect, t);
        }
    }
    if let (Effect::Speaking, Some(level)) = (state.base, speech) {
        return speaking(level);
    }
    let t = now.saturating_duration_since(base_since);
    match state.base {
        Effect::Idle | Effect::Boot
            if state.base == Effect::Boot && !Effect::Boot.is_finished(t) =>
        {
            render(Effect::Boot, t)
        }
        // A running timer was asked of this device just now: its arc shows
        // above a notification, which shows above the light, which shows above
        // the music. The boot stages (ADR 0012) show only where the plain
        // idle ring would, so everything the device is doing wins over them.
        Effect::Idle | Effect::Boot => {
            if let Some(frac) = state.timer {
                render(Effect::Timer(frac), t)
            } else if state.notification {
                render(Effect::Notification, t)
            } else if let Some(rgb) = state.light {
                [scale(rgb, state.light_effect.level(t)); RING_PIXELS]
            } else if let Some(bands) = music {
                render(Effect::Equalizer(*bands), t)
            } else if let Some(frame) = boot {
                frame
            } else {
                render(Effect::Idle, t)
            }
        }
        other => render(other, t),
    }
}

/// Point the listening ring only when the strongest beam stands this far above
/// the average beam; a diffuse field (noise, nobody talking) gives about 0 dB.
pub const TALKER_MIN_CONTRAST_DB: f64 = 2.0;
/// Move the segment only when the talker's beam moves at least this far.
pub const TALKER_MOVE_DEG: u16 = 12;
/// Go back to the plain pulse after this long without a confident reading.
pub const TALKER_UNSURE_AFTER: Duration = Duration::from_secs(1);

/// Where the listening ring points, from beam snapshots (LibreEcho's
/// esphomed rule, with a fallback). A confident reading points or moves the
/// segment. Short pauses keep it where it is; a second of unsure or missing
/// readings returns the ring to the plain pulse.
#[derive(Debug, Default)]
pub struct Talker {
    direction: Option<u16>,
    unsure_since: Option<Instant>,
}

impl Talker {
    pub fn direction(&self) -> Option<u16> {
        self.direction
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }

    pub fn update(&mut self, snapshot: Option<&BeamSnapshot>, now: Instant) {
        let confident = snapshot
            .filter(|s| s.contrast_db() >= TALKER_MIN_CONTRAST_DB)
            .and_then(|s| u16::try_from(s.selected_deg).ok())
            .filter(|az| *az < 360);
        let Some(azimuth) = confident else {
            if self.direction.is_some() {
                let since = *self.unsure_since.get_or_insert(now);
                if now.saturating_duration_since(since) >= TALKER_UNSURE_AFTER {
                    self.clear();
                }
            }
            return;
        };
        self.unsure_since = None;
        let moved = self.direction.is_none_or(|current| {
            let d = current.abs_diff(azimuth);
            let distance = if d > 180 { 360 - d } else { d };
            distance >= TALKER_MOVE_DEG
        });
        if moved {
            self.direction = Some(azimuth);
        }
    }
}

/// Drive the ring from `state` (and `music`, the speaker's spectrum) until the
/// sender is dropped. The speaking ring follows `voice`, the speaker's level.
/// `boot` drives the boot stages while the start is still on the ring. Every
/// frame also goes to `shown`, for the dashboard (WatchRing), with or without
/// a ring to write it to.
pub async fn run(
    leds: Option<LedHandle>,
    shown: watch::Sender<Frame>,
    mut state: watch::Receiver<LedState>,
    music: Option<Spectrum>,
    voice: Option<VoiceLevel>,
    mut boot: Option<crate::boot::BootRing>,
) {
    let mut current = state.borrow().clone();
    let mut base_since = Instant::now();
    let mut envelope = Envelope::default();
    let mut tick = tokio::time::interval(Duration::from_millis(33));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            changed = state.changed() => {
                if changed.is_err() {
                    show(leds.as_ref(), &shown, [Rgb::OFF; RING_PIXELS]);
                    return;
                }
                let next = state.borrow().clone();
                if next.base != current.base {
                    if leds.is_none() {
                        tracing::debug!("led effect: {:?}", next.base);
                    }
                    base_since = Instant::now();
                }
                current = next;
            }
            _ = tick.tick() => {}
        }
        let now = Instant::now();
        let bands = music.as_ref().and_then(|m| m.at(now));
        let heard = voice
            .as_ref()
            .filter(|_| current.base == Effect::Speaking)
            .and_then(|v| v.at(now));
        let speech = match heard {
            Some(rms) => Some(envelope.update(speech_level(rms), now)),
            None => {
                envelope = Envelope::default();
                None
            }
        };
        let boot_frame = boot.as_mut().and_then(|b| b.frame(now));
        let frame = frame_for(
            &current,
            bands.as_ref(),
            base_since,
            now,
            speech,
            boot_frame,
        );
        show(leds.as_ref(), &shown, frame);
    }
}

/// Write `frame` to the ring, if there is one, and publish it when it changed.
fn show(leds: Option<&LedHandle>, shown: &watch::Sender<Frame>, frame: Frame) {
    if let Some(leds) = leds {
        leds.show(&frame);
    }
    shown.send_if_modified(|f| {
        let changed = *f != frame;
        *f = frame;
        changed
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn frames_are_shown_without_a_ring() {
        let (state_tx, state_rx) = watch::channel(LedState::default());
        let (shown_tx, mut shown_rx) = watch::channel([Rgb::OFF; RING_PIXELS]);
        let task = tokio::spawn(run(None, shown_tx, state_rx, None, None, None));
        state_tx.send_modify(|s| s.base = Effect::Muted);
        let muted = render(Effect::Muted, Duration::ZERO);
        tokio::time::timeout(Duration::from_secs(5), shown_rx.wait_for(|f| *f == muted))
            .await
            .expect("no muted frame")
            .unwrap();
        // The state's sender gone: the ring goes dark and stays so.
        drop(state_tx);
        task.await.unwrap();
        assert_eq!(*shown_rx.borrow(), [Rgb::OFF; RING_PIXELS]);
    }

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
        assert_eq!(
            frame_for(&s, None, now, now, None, None),
            [green; RING_PIXELS]
        );
        s.base = Effect::Muted;
        assert_ne!(
            frame_for(&s, None, now, now, None, None),
            [green; RING_PIXELS]
        );
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
        assert_eq!(frame_for(&s, None, now, now, None, None)[0].r, 100);
        s.brightness = 100;
        s.light_effect = LightEffect::Flash;
        assert_eq!(
            frame_for(&s, None, now, now + Duration::from_millis(100), None, None)[0].r,
            200
        );
        assert_eq!(
            frame_for(&s, None, now, now + Duration::from_millis(600), None, None)[0].r,
            0
        );
        s.light_effect = LightEffect::Pulse;
        let low = frame_for(&s, None, now, now, None, None)[0].r;
        let high = frame_for(&s, None, now, now + Duration::from_secs(1), None, None)[0].r;
        assert!(low < 50 && high == 200, "{low} {high}");
        assert_eq!(LightEffect::from_name("Flash"), Some(LightEffect::Flash));
        assert_eq!(LightEffect::Pulse.name(), "Pulse");
    }

    fn beams(selected_deg: i32, contrast: bool) -> BeamSnapshot {
        let mut power = [1e4f32; 12];
        if contrast {
            power[0] = 1e6;
        }
        BeamSnapshot {
            beam_count: 12,
            azimuth_deg: [0, 30, 60, 90, 120, 150, 180, 210, 240, 270, 300, 330],
            power,
            selected_deg,
            age: Duration::ZERO,
        }
    }

    /// A tracker pointing at `deg`, and the instant it pointed.
    fn pointing(deg: i32) -> (Talker, Instant) {
        let now = Instant::now();
        let mut t = Talker::default();
        t.update(Some(&beams(deg, true)), now);
        (t, now)
    }

    #[test]
    fn follows_the_talker_only_when_confident() {
        let now = Instant::now();
        let mut t = Talker::default();
        // No graph, or a diffuse field: no direction.
        t.update(None, now);
        t.update(Some(&beams(90, false)), now);
        assert_eq!(t.direction(), None);
        // A confident reading points, and moves the segment.
        t.update(Some(&beams(90, true)), now);
        assert_eq!(t.direction(), Some(90));
        t.update(Some(&beams(270, true)), now);
        assert_eq!(t.direction(), Some(270));
        // Out-of-range azimuths count as unsure.
        t.update(Some(&beams(-30, true)), now);
        t.update(Some(&beams(360, true)), now);
        assert_eq!(t.direction(), Some(270));
        let mut fresh = Talker::default();
        fresh.update(Some(&beams(360, true)), now);
        assert_eq!(fresh.direction(), None);
    }

    #[test]
    fn small_moves_keep_the_segment_still() {
        let (mut t, now) = pointing(90);
        t.update(Some(&beams(101, true)), now);
        assert_eq!(t.direction(), Some(90));
        t.update(Some(&beams(102, true)), now);
        assert_eq!(t.direction(), Some(102));
        // Across 0 degrees: 11 degrees apart.
        let (mut t, now) = pointing(5);
        t.update(Some(&beams(354, true)), now);
        assert_eq!(t.direction(), Some(5));
        t.update(Some(&beams(15, true)), now);
        assert_eq!(t.direction(), Some(5));
        t.update(Some(&beams(345, true)), now);
        assert_eq!(t.direction(), Some(345));
    }

    #[test]
    fn pauses_keep_the_direction_a_second_of_doubt_clears_it() {
        let ms = |n| Duration::from_millis(n);
        let (mut t, start) = pointing(90);
        // Unsure for 750 ms, then confident again: the segment never left.
        for n in [250, 500, 750] {
            t.update(Some(&beams(90, false)), start + ms(n));
        }
        t.update(Some(&beams(90, true)), start + ms(1000));
        assert_eq!(t.direction(), Some(90));
        // The doubt timer restarted: 999 ms of missing readings keep it ...
        t.update(None, start + ms(1250));
        t.update(None, start + ms(2249));
        assert_eq!(t.direction(), Some(90));
        // ... a full second returns the ring to the plain pulse.
        t.update(None, start + ms(2250));
        assert_eq!(t.direction(), None);
        // And a new confident reading points again.
        t.update(Some(&beams(180, true)), start + ms(2500));
        assert_eq!(t.direction(), Some(180));
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
        assert_eq!(frame_for(&s, Some(&bands), now, now, None, None), eq);
        assert_eq!(
            frame_for(&s, None, now, now, None, None),
            [Rgb::OFF; RING_PIXELS]
        );
        // A running timer and the light win over the equalizer.
        s.timer = Some(0.5);
        assert_eq!(
            frame_for(&s, Some(&bands), now, now, None, None),
            render(Effect::Timer(0.5), Duration::ZERO)
        );
        s.light = Some(Rgb::new(0, 0, 200));
        assert_eq!(
            frame_for(&s, Some(&bands), now, now, None, None),
            render(Effect::Timer(0.5), Duration::ZERO)
        );
        s.timer = None;
        assert_eq!(
            frame_for(&s, Some(&bands), now, now, None, None),
            [Rgb::new(0, 0, 200); RING_PIXELS]
        );
        // So do the voice states.
        s.base = Effect::Thinking;
        assert_eq!(
            frame_for(&s, Some(&bands), now, now, None, None),
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
        let during = frame_for(&s, None, now, now + Duration::from_millis(100), None, None);
        let after = frame_for(&s, None, now, now + Duration::from_secs(10), None, None);
        assert_ne!(during, after);
        assert_eq!(after, render(Effect::Idle, Duration::from_secs(10)));
    }

    #[test]
    fn timer_arc_shows_above_the_light() {
        let now = Instant::now();
        let mut s = LedState {
            base: Effect::Idle,
            light: Some(Rgb {
                r: 0,
                g: 200,
                b: 40,
            }),
            timer: Some(0.5),
            ..Default::default()
        };
        assert_eq!(
            frame_for(&s, None, now, now, None, None),
            render(Effect::Timer(0.5), Duration::ZERO)
        );
        s.timer = None;
        assert_eq!(
            frame_for(&s, None, now, now, None, None)[0],
            Rgb {
                r: 0,
                g: 200,
                b: 40
            }
        );
    }

    #[test]
    fn speaking_follows_the_reply_when_heard() {
        let now = Instant::now();
        let mut s = LedState {
            base: Effect::Speaking,
            brightness: 50,
            ..Default::default()
        };
        // No level (still downloading, no speaker): the breathing.
        assert_eq!(
            frame_for(&s, None, now, now, None, None),
            frame_for(
                &LedState {
                    brightness: 100,
                    ..s.clone()
                },
                None,
                now,
                now,
                None,
                None
            )
            .map(|p| scale(p, 0.5))
        );
        assert_eq!(
            frame_for(&s, None, now, now, Some(1.0), None),
            [scale(speaking(1.0)[0], 0.5); 12]
        );
        // A transient overlay still wins.
        s.overlay = Some((Effect::Volume(50), now));
        assert_eq!(
            frame_for(&s, None, now, now, Some(1.0), None),
            frame_for(&s, None, now, now, None, None)
        );
        // Only the speaking ring follows the voice.
        s.overlay = None;
        s.base = Effect::Thinking;
        assert_eq!(
            frame_for(&s, None, now, now, Some(1.0), None),
            frame_for(&s, None, now, now, None, None)
        );
    }

    #[test]
    fn envelope_rises_fast_and_falls_slowly() {
        let t0 = Instant::now();
        let ms = |n| t0 + Duration::from_millis(n);
        let mut e = Envelope::default();
        assert_eq!(e.update(1.0, t0), 0.0, "no time has passed yet");
        let up = e.update(1.0, ms(33));
        assert!(up > 0.7, "{up}");
        let mut e = Envelope {
            value: 1.0,
            at: Some(t0),
        };
        let down = e.update(0.0, ms(33));
        assert!(down > 0.7 && down < 0.8, "{down}");
        assert!(e.update(0.0, ms(1000)) < 0.01);
    }

    #[test]
    fn notification_between_timer_arc_and_light() {
        let now = Instant::now();
        let green = Rgb {
            r: 0,
            g: 200,
            b: 40,
        };
        let mut s = LedState {
            base: Effect::Idle,
            light: Some(green),
            timer: Some(0.5),
            notification: true,
            ..Default::default()
        };
        assert_eq!(
            frame_for(&s, None, now, now, None, None),
            render(Effect::Timer(0.5), Duration::ZERO)
        );
        s.timer = None;
        assert_eq!(
            frame_for(&s, None, now, now, None, None),
            render(Effect::Notification, Duration::ZERO)
        );
        // Anything the device is doing, or muted, shows instead.
        for base in [Effect::Muted, Effect::Listening, Effect::TimerRinging] {
            s.base = base;
            assert_eq!(
                frame_for(&s, None, now, now, None, None),
                render(base, Duration::ZERO)
            );
        }
        s.base = Effect::Idle;
        s.notification = false;
        assert_eq!(
            frame_for(&s, None, now, now, None, None),
            [green; RING_PIXELS]
        );
    }

    #[test]
    fn boot_stages_show_where_the_idle_ring_would() {
        let now = Instant::now();
        let green = Rgb {
            r: 0,
            g: 200,
            b: 40,
        };
        let arc = [Rgb::new(0, 0x30, 0xff); RING_PIXELS];
        let s = LedState {
            base: Effect::Idle,
            ..Default::default()
        };
        // Idle: the arc shows instead of the dark ring.
        assert_eq!(frame_for(&s, None, now, now, None, Some(arc)), arc);
        // Anything the device is doing wins: the light, music, a timer, a
        // notification.
        let mut lit = s.clone();
        lit.light = Some(green);
        assert_ne!(frame_for(&lit, None, now, now, None, Some(arc)), arc);
        let bands = [1.0; 12];
        assert_eq!(
            frame_for(&s, Some(&bands), now, now, None, Some(arc)),
            render(Effect::Equalizer(bands), Duration::ZERO)
        );
        let mut timed = s.clone();
        timed.timer = Some(0.5);
        assert_ne!(frame_for(&timed, None, now, now, None, Some(arc)), arc);
        let mut notified = s.clone();
        notified.notification = true;
        assert_ne!(frame_for(&notified, None, now, now, None, Some(arc)), arc);
    }
}
