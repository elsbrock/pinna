//! Tone and chime generator for UI sounds, rendered as 48 kHz stereo.

use crate::decode::{OUT_RATE, Pcm};
use std::f32::consts::TAU;

/// One note: frequency in Hz (0 = silence) for a number of milliseconds.
#[derive(Clone, Copy, Debug)]
pub struct Note {
    pub hz: f32,
    pub ms: u32,
}

const FADE_MS: u32 = 8;

/// Render notes back to back; each gets a short fade in/out to avoid clicks.
/// `amplitude` is 0..=1 of full scale.
pub fn render(notes: &[Note], amplitude: f32) -> Pcm {
    let amp = amplitude.clamp(0.0, 1.0) * f32::from(i16::MAX);
    let mut samples = Vec::new();
    for n in notes {
        let frames = (u64::from(OUT_RATE) * u64::from(n.ms) / 1000) as usize;
        let fade = ((u64::from(OUT_RATE) * u64::from(FADE_MS) / 1000) as usize)
            .min(frames / 2)
            .max(1);
        for i in 0..frames {
            let env = if i < fade {
                i as f32 / fade as f32
            } else if i >= frames - fade {
                (frames - 1 - i) as f32 / fade as f32
            } else {
                1.0
            };
            let s = if n.hz > 0.0 {
                (TAU * n.hz * i as f32 / OUT_RATE as f32).sin() * amp * env
            } else {
                0.0
            };
            let s = s.round() as i16;
            samples.push(s);
            samples.push(s);
        }
    }
    Pcm {
        rate: OUT_RATE,
        channels: 2,
        samples,
    }
}

/// A single sine beep.
pub fn beep(hz: f32, ms: u32, amplitude: f32) -> Pcm {
    render(&[Note { hz, ms }], amplitude)
}

/// Short rising two-note chime: wake word detected.
pub fn wake_chime() -> Pcm {
    render(
        &[
            Note { hz: 880.0, ms: 70 },
            Note {
                hz: 1318.5,
                ms: 110,
            },
        ],
        0.5,
    )
}

/// Low falling two-note tone: something went wrong.
pub fn error_tone() -> Pcm {
    render(
        &[Note { hz: 330.0, ms: 140 }, Note { hz: 220.0, ms: 220 }],
        0.5,
    )
}

/// Timer alarm: three short beeps then a pause (loop it while ringing).
pub fn timer_alarm() -> Pcm {
    let mut notes = Vec::new();
    for _ in 0..3 {
        notes.push(Note {
            hz: 1000.0,
            ms: 120,
        });
        notes.push(Note { hz: 0.0, ms: 100 });
    }
    notes.push(Note { hz: 0.0, ms: 600 });
    render(&notes, 0.6)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn length_and_format() {
        let p = beep(440.0, 100, 0.5);
        assert_eq!((p.rate, p.channels), (48000, 2));
        assert_eq!(p.frames(), 4800);
        assert_eq!(p.samples.len(), 9600);
    }

    #[test]
    fn faded_edges_and_amplitude_cap() {
        let p = beep(440.0, 100, 0.5);
        assert_eq!(p.samples[0], 0);
        assert!(p.samples[p.samples.len() - 2].abs() < 200);
        let peak = p.samples.iter().map(|s| s.unsigned_abs()).max().unwrap();
        assert!(peak <= 16384 && peak > 15000, "peak {peak}");
    }

    #[test]
    fn stereo_channels_match_and_silence_is_silent() {
        let p = render(&[Note { hz: 0.0, ms: 10 }], 1.0);
        assert!(p.samples.iter().all(|&s| s == 0));
        let t = beep(1000.0, 20, 1.0);
        assert!(t.samples.chunks(2).all(|c| c[0] == c[1]));
    }

    #[test]
    fn presets_have_expected_durations() {
        assert_eq!(wake_chime().duration().as_millis(), 180);
        assert_eq!(error_tone().duration().as_millis(), 360);
        assert_eq!(timer_alarm().duration().as_millis(), 3 * 220 + 600);
    }

    #[test]
    fn frequency_is_right() {
        // Count zero crossings of a 1 kHz tone over 100 ms: ~200.
        let p = beep(1000.0, 100, 0.8);
        let mono: Vec<i16> = p.samples.iter().step_by(2).copied().collect();
        let crossings = mono.windows(2).filter(|w| (w[0] < 0) != (w[1] < 0)).count();
        assert!((195..=205).contains(&crossings), "{crossings}");
    }
}
