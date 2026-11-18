//! The wake word's level before the capture gain, for the measurement that
//! comes before the group's wake word arbitration (ADR 0026, "Measured
//! before it's built").
//!
//! The capture loop logs each wake it passes on with this level, to find out
//! whether the level picks the nearer of two satellites. It is taken before
//! the gain (the AGC of ADR 0004, or a fixed gain), because the AGC lifts a
//! far talker towards a near one's level. It is still after the beamformer
//! and echo cancellation, which differ between the Echo Dot (2nd gen) and the
//! Echo (2nd gen).

use std::fmt;
use std::ops::Range;

use pinna_audio::agc::mean_square_dbfs;
use pinna_audio::dsp::FRAME_SAMPLES;
use pinna_wakeword::{BLOCK_SAMPLES, BlockScore, FEATURE_FRAMES};

// A capture frame never straddles two detector blocks, so a block's frames
// are exactly the ones fed since the block before.
const _: () = assert!(BLOCK_SAMPLES.is_multiple_of(FRAME_SAMPLES));
const FRAMES_PER_BLOCK: usize = BLOCK_SAMPLES / FRAME_SAMPLES;
/// The 1.28 s the classifier saw when it scored the peak block.
const WINDOW_FRAMES: usize = FEATURE_FRAMES * FRAMES_PER_BLOCK;
/// The detector decides on its last three blocks, so a detection's peak
/// block ends up to two blocks before the newest frame.
const RING_FRAMES: usize = WINDOW_FRAMES + 2 * FRAMES_PER_BLOCK;

#[derive(Clone, Copy, Default)]
struct Frame {
    mean_square: f64,
    speech: bool,
}

/// The last frames fed to the wake word detector, measured before the gain
/// and kept by their position in the detector's stream. A fixed ring: the
/// capture loop allocates nothing per frame.
pub struct WakeLevel {
    frames: [Frame; RING_FRAMES],
    /// Frames fed since the detector's last reset, so frame `n` ends at
    /// sample `(n + 1) * FRAME_SAMPLES` of its stream.
    fed: u64,
}

impl Default for WakeLevel {
    fn default() -> Self {
        WakeLevel {
            frames: [Frame::default(); RING_FRAMES],
            fed: 0,
        }
    }
}

/// The level of the frames the classifier saw for one detection.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WordLevel {
    /// Mean level of the speech frames in dBFS, or of all the frames when
    /// none was speech. Averaged as power, never as dB.
    pub dbfs: f32,
    /// How many frames were speech; 0 when all the frames were used.
    pub speech_frames: usize,
}

impl fmt::Display for WordLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "level {:.1} dBFS before the gain ", self.dbfs)?;
        match self.speech_frames {
            0 => write!(f, "(no speech frames, all frames)"),
            n => write!(f, "({n} speech frames)"),
        }
    }
}

impl WakeLevel {
    /// A frame fed to the detector, by its mean square before the gain
    /// ([`pinna_audio::agc::mean_square`]).
    pub fn frame(&mut self, mean_square: f64) {
        self.frames[slot(self.fed)] = Frame {
            mean_square,
            speech: false,
        };
        self.fed += 1;
    }

    /// A block the detector completed. Speech is what the detector's own
    /// energy VAD counted as speech, so the level covers the frames the
    /// detector itself took for the word. That VAD reports once per 80 ms
    /// block, whether it was active at any point in it, so a block's four
    /// frames count as speech together. The VAD itself hears the audio after
    /// the gain.
    pub fn block(&mut self, block: &BlockScore) {
        if !block.vad_active {
            return;
        }
        let end = block.sample_index / FRAME_SAMPLES as u64;
        for n in self.held(end.saturating_sub(FRAMES_PER_BLOCK as u64), end) {
            self.frames[slot(n)].speech = true;
        }
    }

    /// The detector went back to stream position 0 ([`Detector::reset`]):
    /// the frames before never match a position again.
    ///
    /// [`Detector::reset`]: pinna_wakeword::Detector::reset
    pub fn reset(&mut self) {
        self.fed = 0;
    }

    /// The level of the 16 blocks the classifier saw, ending with the peak
    /// block at `peak_end` ([`Detection::sample_index`]). `None` when none of
    /// those frames is still held.
    ///
    /// [`Detection::sample_index`]: pinna_wakeword::Detection::sample_index
    pub fn word(&self, peak_end: u64) -> Option<WordLevel> {
        let end = peak_end / FRAME_SAMPLES as u64;
        let frames = self.held(end.saturating_sub(WINDOW_FRAMES as u64), end);
        if frames.is_empty() {
            return None;
        }
        let count = frames.end - frames.start;
        let (mut all, mut speech, mut speech_frames) = (0.0, 0.0, 0);
        for n in frames {
            let frame = self.frames[slot(n)];
            all += frame.mean_square;
            if frame.speech {
                speech += frame.mean_square;
                speech_frames += 1;
            }
        }
        let mean = match speech_frames {
            0 => all / count as f64,
            n => speech / n as f64,
        };
        Some(WordLevel {
            dbfs: mean_square_dbfs(mean),
            speech_frames,
        })
    }

    /// The positions in `start..end` that are fed and not yet overwritten.
    fn held(&self, start: u64, end: u64) -> Range<u64> {
        let start = start.max(self.fed.saturating_sub(RING_FRAMES as u64));
        let end = end.min(self.fed);
        start..end.max(start)
    }
}

fn slot(position: u64) -> usize {
    (position % RING_FRAMES as u64) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    const BLOCK: u64 = BLOCK_SAMPLES as u64;

    /// The mean square of a frame at `dbfs`.
    fn power(dbfs: f32) -> f64 {
        10f64.powf(f64::from(dbfs) / 10.0)
    }

    /// Feeds one block of four frames at `dbfs` and closes it, as the
    /// capture loop and the detector do.
    fn feed_block(levels: &mut WakeLevel, dbfs: f32, speech: bool) -> u64 {
        for _ in 0..FRAMES_PER_BLOCK {
            levels.frame(power(dbfs));
        }
        let end = levels.fed * FRAME_SAMPLES as u64;
        levels.block(&BlockScore {
            score: 0.0,
            keyword: None,
            sample_index: end,
            vad_active: speech,
            playback: false,
        });
        end
    }

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 0.05
    }

    #[test]
    fn the_level_is_the_mean_of_the_speech_frames() {
        let mut levels = WakeLevel::default();
        for _ in 0..4 {
            feed_block(&mut levels, -70.0, false);
        }
        let peak = feed_block(&mut levels, -30.0, true);
        let word = levels.word(peak).unwrap();
        assert!(close(word.dbfs, -30.0), "{word:?}");
        assert_eq!(word.speech_frames, FRAMES_PER_BLOCK);
    }

    #[test]
    fn speech_frames_are_averaged_as_power_not_as_decibels() {
        let mut levels = WakeLevel::default();
        feed_block(&mut levels, -20.0, true);
        let peak = feed_block(&mut levels, -40.0, true);
        let word = levels.word(peak).unwrap();
        // The mean of 0.01 and 0.0001 is 0.00505: -23.0 dBFS, not -30.
        assert!(close(word.dbfs, -22.97), "{word:?}");
        assert_eq!(word.speech_frames, 2 * FRAMES_PER_BLOCK);
    }

    #[test]
    fn without_speech_every_frame_counts() {
        let mut levels = WakeLevel::default();
        feed_block(&mut levels, -50.0, false);
        let peak = feed_block(&mut levels, -50.0, false);
        let word = levels.word(peak).unwrap();
        assert!(close(word.dbfs, -50.0), "{word:?}");
        assert_eq!(word.speech_frames, 0);
        assert_eq!(
            word.to_string(),
            "level -50.0 dBFS before the gain (no speech frames, all frames)"
        );
    }

    #[test]
    fn only_the_sixteen_blocks_up_to_the_peak_count() {
        let mut levels = WakeLevel::default();
        // Loud speech 17 blocks before the peak is outside the window.
        feed_block(&mut levels, -10.0, true);
        for _ in 0..FEATURE_FRAMES {
            feed_block(&mut levels, -40.0, true);
        }
        let peak = levels.fed * FRAME_SAMPLES as u64;
        // Two more blocks after the peak, before the detector decides.
        feed_block(&mut levels, -10.0, true);
        feed_block(&mut levels, -10.0, true);
        let word = levels.word(peak).unwrap();
        assert!(close(word.dbfs, -40.0), "{word:?}");
        assert_eq!(word.speech_frames, WINDOW_FRAMES);
        assert_eq!(
            word.to_string(),
            "level -40.0 dBFS before the gain (64 speech frames)"
        );
    }

    #[test]
    fn a_peak_further_back_than_the_ring_holds_is_cut_short() {
        let mut levels = WakeLevel::default();
        for _ in 0..FEATURE_FRAMES {
            feed_block(&mut levels, -40.0, true);
        }
        let peak = levels.fed * FRAME_SAMPLES as u64;
        for _ in 0..3 {
            feed_block(&mut levels, -10.0, true);
        }
        // Three blocks later, the window's first block is overwritten.
        let word = levels.word(peak).unwrap();
        assert!(close(word.dbfs, -40.0), "{word:?}");
        assert_eq!(word.speech_frames, WINDOW_FRAMES - FRAMES_PER_BLOCK);
    }

    #[test]
    fn a_reset_forgets_the_frames_before_it() {
        let mut levels = WakeLevel::default();
        for _ in 0..5 {
            feed_block(&mut levels, -10.0, true);
        }
        levels.reset();
        assert_eq!(levels.word(3 * BLOCK), None);
        feed_block(&mut levels, -60.0, false);
        let peak = feed_block(&mut levels, -30.0, true);
        let word = levels.word(peak).unwrap();
        assert!(close(word.dbfs, -30.0), "{word:?}");
        assert_eq!(word.speech_frames, FRAMES_PER_BLOCK);
    }

    #[test]
    fn digital_silence_is_the_floor() {
        let mut levels = WakeLevel::default();
        for _ in 0..FRAMES_PER_BLOCK {
            levels.frame(0.0);
        }
        assert_eq!(levels.word(BLOCK).unwrap().dbfs, -100.0);
        assert_eq!(WakeLevel::default().word(BLOCK), None);
    }
}
