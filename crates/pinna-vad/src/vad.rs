//! Energy VAD, a port of the previous firmware's `le_voice_vad_process` (voice_dsp.c)
//! with its default 16 RMS floor. Runs on 160-sample (10 ms) frames: a frame is
//! speech above 6x the tracked noise energy; two consecutive speech frames
//! activate it, 20 non-speech frames of hangover deactivate it, and the noise
//! estimate only adapts (slowly) while inactive.

const FLOOR_RMS: u64 = 16;
const ONSET_FRAMES: u32 = 2;
const HANGOVER_FRAMES: u32 = 20;

pub struct Vad {
    minimum_energy: u64,
    noise_energy: u64,
    initialized: bool,
    speech_run: u32,
    hangover: u32,
    active: bool,
}

impl Vad {
    pub fn new() -> Self {
        Self {
            minimum_energy: FLOOR_RMS * FLOOR_RMS,
            noise_energy: 0,
            initialized: false,
            speech_run: 0,
            hangover: 0,
            active: false,
        }
    }

    /// Start with a known ambient noise energy, for example the wake word's
    /// noise floor measured before the wake word. A seed below the floor is
    /// clamped to it.
    pub fn with_noise(energy: u64) -> Self {
        Self {
            minimum_energy: FLOOR_RMS * FLOOR_RMS,
            noise_energy: energy.max(FLOOR_RMS * FLOOR_RMS),
            initialized: true,
            speech_run: 0,
            hangover: 0,
            active: false,
        }
    }

    /// Process one frame and return whether the VAD is active afterwards.
    pub fn process(&mut self, samples: &[i16]) -> bool {
        if samples.is_empty() {
            return self.active;
        }
        let frame_energy = frame_energy(samples);

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

    /// The raw per-frame decision: is this frame above the speech threshold?
    /// Unlike [`process`](Self::process) there is no onset or hangover, and the
    /// noise estimate adapts on every non-speech frame. The endpointer counts
    /// silence on these decisions, so the hangover can't extend it.
    pub fn classify(&mut self, frame: &[i16]) -> bool {
        if frame.is_empty() {
            return false;
        }
        let energy = frame_energy(frame);
        if !self.initialized {
            self.noise_energy = energy.max(self.minimum_energy);
            self.initialized = true;
        }
        let threshold = (self.noise_energy * 6).max(self.minimum_energy * 6);
        if energy > threshold {
            true
        } else {
            self.noise_energy = ((self.noise_energy * 31 + energy) / 32).max(self.minimum_energy);
            false
        }
    }

    /// The tracked noise energy, or `None` before the VAD has seen a frame.
    pub fn noise_energy(&self) -> Option<u64> {
        self.initialized.then_some(self.noise_energy)
    }
}

impl Default for Vad {
    fn default() -> Self {
        Self::new()
    }
}

fn frame_energy(samples: &[i16]) -> u64 {
    let sum: u64 = samples
        .iter()
        .map(|&s| (i64::from(s) * i64::from(s)) as u64)
        .sum();
    sum / samples.len() as u64
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

    #[test]
    fn classify_is_raw_and_adapts_noise_on_non_speech() {
        let mut vad = Vad::with_noise(256);
        let quiet = [10i16; 160];
        let loud = [3000i16; 160];
        assert!(vad.classify(&loud), "one loud frame is speech, no onset");
        assert!(
            !vad.classify(&quiet),
            "one quiet frame is silence, no hangover"
        );
        assert_eq!(vad.noise_energy(), Some(256), "quiet is below the floor");
    }

    #[test]
    fn a_quiet_seed_hears_the_first_speech_frame_but_new_does_not() {
        let loud = [3000i16; 160];

        let mut seeded = Vad::with_noise(256);
        assert!(seeded.classify(&loud));

        // Without a seed the first loud frame becomes the noise estimate, so the
        // frame is never above 6x it.
        let mut fresh = Vad::new();
        assert!(!fresh.classify(&loud));
        assert!(fresh.noise_energy().unwrap() >= frame_energy(&loud));
    }
}
