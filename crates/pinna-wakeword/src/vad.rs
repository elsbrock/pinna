//! Energy VAD, a port of LibreEcho's `le_voice_vad_process` (voice_dsp.c) with
//! its default 16 RMS floor. Runs on 160-sample (10 ms) frames: a frame is
//! speech above 6x the tracked noise energy; two consecutive speech frames
//! activate it, 20 non-speech frames of hangover deactivate it, and the noise
//! estimate only adapts (slowly) while inactive.

const FLOOR_RMS: u64 = 16;
const ONSET_FRAMES: u32 = 2;
const HANGOVER_FRAMES: u32 = 20;

pub(crate) struct Vad {
    minimum_energy: u64,
    noise_energy: u64,
    initialized: bool,
    speech_run: u32,
    hangover: u32,
    active: bool,
}

impl Vad {
    pub(crate) fn new() -> Self {
        Self {
            minimum_energy: FLOOR_RMS * FLOOR_RMS,
            noise_energy: 0,
            initialized: false,
            speech_run: 0,
            hangover: 0,
            active: false,
        }
    }

    /// Process one frame and return whether the VAD is active afterwards.
    pub(crate) fn process(&mut self, samples: &[i16]) -> bool {
        if samples.is_empty() {
            return self.active;
        }
        let sum: u64 = samples
            .iter()
            .map(|&s| (i64::from(s) * i64::from(s)) as u64)
            .sum();
        let frame_energy = sum / samples.len() as u64;

        if !self.initialized {
            self.noise_energy = frame_energy.max(self.minimum_energy);
            self.initialized = true;
        }
        let threshold = (self.noise_energy * 6).max(self.minimum_energy * 6);
        if frame_energy > threshold {
            self.speech_run = (self.speech_run + 1).min(ONSET_FRAMES);
            if self.speech_run >= ONSET_FRAMES {
                self.active = true;
                self.hangover = HANGOVER_FRAMES;
            }
        } else {
            self.speech_run = 0;
            if self.active {
                if self.hangover > 0 {
                    self.hangover -= 1;
                } else {
                    self.active = false;
                }
            }
            if !self.active {
                self.noise_energy =
                    ((self.noise_energy * 31 + frame_energy) / 32).max(self.minimum_energy);
            }
        }
        self.active
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silence_stays_inactive_and_loud_speech_activates_with_hangover() {
        let mut vad = Vad::new();
        let quiet = [10i16; 160];
        let loud = [3000i16; 160];
        for _ in 0..10 {
            assert!(!vad.process(&quiet));
        }
        assert!(!vad.process(&loud), "needs two speech frames");
        assert!(vad.process(&loud));
        for _ in 0..HANGOVER_FRAMES {
            assert!(vad.process(&quiet));
        }
        assert!(!vad.process(&quiet));
    }
}
