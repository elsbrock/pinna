//! End-of-speech detection for standalone mode (ADR 0017). Turns a stream of
//! 16 kHz mono PCM into [`Endpoint`] decisions using the energy VAD. Silence is
//! counted on raw per-frame decisions (see [`Vad::classify`]), so the VAD's
//! hangover does not stretch the configured silence.

use std::time::Duration;

use crate::vad::Vad;

/// VAD frame size (10 ms at 16 kHz), as in the wake word path.
const FRAME_SAMPLES: usize = 160;
const FRAME_MS: u64 = 10;

/// Times that shape one listen. The defaults are the limits in the standalone
/// design (ADR 0017); a follow-up window only shortens `no_speech`.
#[derive(Clone, Copy, Debug)]
pub struct EndpointConfig {
    /// Audio this long after the start is ignored, for the tail of the wake
    /// chime.
    pub ignore: Duration,
    /// Speech this long (after `ignore`) enters the speech state.
    pub onset: Duration,
    /// Silence this long after speech ends the utterance.
    pub silence: Duration,
    /// No speech at all this long ends the listen without speech.
    pub no_speech: Duration,
    /// Total audio this long caps the utterance.
    pub max: Duration,
}

impl EndpointConfig {
    pub const REQUEST: Self = Self {
        ignore: Duration::from_millis(300),
        onset: Duration::from_millis(300),
        silence: Duration::from_millis(800),
        no_speech: Duration::from_secs(5),
        max: Duration::from_secs(15),
    };
    /// A follow-up window closes a little sooner when nobody answers.
    pub const FOLLOW_UP: Self = Self {
        no_speech: Duration::from_secs(4),
        ..Self::REQUEST
    };

    /// The wall-clock bound on a listen: the longest recording plus a margin,
    /// for a microphone route that stalls instead of closing.
    pub const fn backstop(&self) -> Duration {
        self.max.saturating_add(Duration::from_secs(2))
    }
}

/// The endpointer's state after a frame. `Waiting` and `Speech` are live;
/// the rest are terminal and stick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Endpoint {
    Waiting,
    Speech,
    Ended,
    NoSpeech,
    Capped,
}

impl Endpoint {
    /// Whether the listen is over.
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Ended | Self::NoSpeech | Self::Capped)
    }
}

/// Re-chunks audio and reports end-of-speech decisions.
pub struct Endpointer {
    cfg: EndpointConfig,
    vad: Vad,
    frame: [i16; FRAME_SAMPLES],
    frame_len: usize,
    elapsed_ms: u64,
    onset_ms: u64,
    silence_ms: u64,
    state: Endpoint,
}

impl Endpointer {
    /// `noise_seed` is the ambient noise energy measured before the wake word
    /// (for example [`crate::Vad::noise_energy`]). Without it the first frame
    /// becomes the noise estimate, which misreads speech that starts at once.
    pub fn new(cfg: EndpointConfig, noise_seed: Option<u64>) -> Self {
        let vad = match noise_seed {
            Some(energy) => Vad::with_noise(energy),
            None => Vad::new(),
        };
        Self {
            cfg,
            vad,
            frame: [0; FRAME_SAMPLES],
            frame_len: 0,
            elapsed_ms: 0,
            onset_ms: 0,
            silence_ms: 0,
            state: Endpoint::Waiting,
        }
    }

    /// Feed 16 kHz mono samples of any length; returns the state. Terminal
    /// states stick, so later audio is ignored.
    pub fn push(&mut self, pcm: &[i16]) -> Endpoint {
        self.feed(pcm.iter().copied())
    }

    /// [`Self::push`] for little-endian 16-bit bytes, as the microphone
    /// sends them, without converting them into a buffer first: this runs on
    /// every microphone frame.
    pub fn push_le(&mut self, bytes: &[u8]) -> Endpoint {
        self.feed(
            bytes
                .chunks_exact(2)
                .map(|sample| i16::from_le_bytes([sample[0], sample[1]])),
        )
    }

    fn feed(&mut self, samples: impl Iterator<Item = i16>) -> Endpoint {
        if self.state.is_terminal() {
            return self.state;
        }
        for sample in samples {
            self.frame[self.frame_len] = sample;
            self.frame_len += 1;
            if self.frame_len == FRAME_SAMPLES {
                self.frame_len = 0;
                self.step();
                if self.state.is_terminal() {
                    break;
                }
            }
        }
        self.state
    }

    fn step(&mut self) {
        self.elapsed_ms += FRAME_MS;
        let speech = self.vad.classify(&self.frame);
        let in_ignore = self.elapsed_ms <= millis(self.cfg.ignore);

        match self.state {
            Endpoint::Waiting => {
                if speech && !in_ignore {
                    self.onset_ms += FRAME_MS;
                    if self.onset_ms >= millis(self.cfg.onset) {
                        self.state = Endpoint::Speech;
                        self.silence_ms = 0;
                    }
                } else {
                    self.onset_ms = 0;
                }
                if self.state == Endpoint::Waiting && self.elapsed_ms >= millis(self.cfg.no_speech)
                {
                    self.state = Endpoint::NoSpeech;
                }
            }
            Endpoint::Speech => {
                if speech {
                    self.silence_ms = 0;
                } else {
                    self.silence_ms += FRAME_MS;
                    if self.silence_ms >= millis(self.cfg.silence) {
                        self.state = Endpoint::Ended;
                        return;
                    }
                }
            }
            Endpoint::Ended | Endpoint::NoSpeech | Endpoint::Capped => {}
        }

        if self.state == Endpoint::Speech && self.elapsed_ms >= millis(self.cfg.max) {
            self.state = Endpoint::Capped;
        }
    }
}

fn millis(d: Duration) -> u64 {
    d.as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    const QUIET: i16 = 10;
    const LOUD: i16 = 3000;
    const SEED: u64 = 256;

    fn frame(amp: i16) -> [i16; FRAME_SAMPLES] {
        [amp; FRAME_SAMPLES]
    }

    /// Feed frames one at a time and return the frame count when the state
    /// first became terminal, with that state.
    fn run(
        cfg: EndpointConfig,
        seed: Option<u64>,
        amps: &[i16],
    ) -> (usize, Endpoint, Vec<Endpoint>) {
        let mut ep = Endpointer::new(cfg, seed);
        let mut seen = Vec::new();
        for (i, &amp) in amps.iter().enumerate() {
            let state = ep.push(&frame(amp));
            seen.push(state);
            if state.is_terminal() {
                return (i + 1, state, seen);
            }
        }
        (amps.len(), ep.push(&[]), seen)
    }

    #[test]
    fn speech_then_silence_ends_after_the_silence_window() {
        // 1.1 s of speech then silence: Ended once 800 ms of silence passed.
        let mut amps = vec![LOUD; 110];
        amps.extend(std::iter::repeat_n(QUIET, 200));
        let (frames, state, _) = run(EndpointConfig::REQUEST, Some(SEED), &amps);
        assert_eq!(state, Endpoint::Ended);
        assert_eq!(frames, 190, "1.1 s speech + 0.8 s silence");
    }

    #[test]
    fn quiet_ends_with_no_speech_at_the_configured_window() {
        let (frames, state, _) = run(EndpointConfig::REQUEST, None, &vec![QUIET; 600]);
        assert_eq!(state, Endpoint::NoSpeech);
        assert_eq!(frames, 500, "5 s at 10 ms per frame");

        let (frames, state, _) = run(EndpointConfig::FOLLOW_UP, None, &vec![QUIET; 600]);
        assert_eq!(state, Endpoint::NoSpeech);
        assert_eq!(frames, 400, "follow-up window is 4 s");
    }

    #[test]
    fn long_speech_is_capped() {
        let (frames, state, _) = run(EndpointConfig::REQUEST, Some(SEED), &vec![LOUD; 1600]);
        assert_eq!(state, Endpoint::Capped);
        assert_eq!(frames, 1500, "recording cap is 15 s");
    }

    #[test]
    fn loud_audio_inside_the_ignore_window_does_not_count() {
        // 200 ms of noise, all inside the 300 ms ignore window, then quiet.
        let mut amps = vec![LOUD; 20];
        amps.extend(std::iter::repeat_n(QUIET, 600));
        let (_, state, seen) = run(EndpointConfig::REQUEST, Some(SEED), &amps);
        assert_eq!(state, Endpoint::NoSpeech);
        assert!(
            !seen.contains(&Endpoint::Speech),
            "ignored audio must not enter the speech state"
        );
    }

    #[test]
    fn a_quiet_seed_reaches_speech_and_ended_but_a_fresh_vad_does_not() {
        let mut amps = vec![LOUD; 100];
        amps.extend(std::iter::repeat_n(QUIET, 600));

        let (_, state, seen) = run(EndpointConfig::REQUEST, Some(SEED), &amps);
        assert!(seen.contains(&Endpoint::Speech));
        assert_eq!(state, Endpoint::Ended);

        // Without a seed the first loud frame becomes the noise estimate, so the
        // endpointer never hears speech and eventually reports no speech.
        let (_, state, seen) = run(EndpointConfig::REQUEST, None, &amps);
        assert!(!seen.contains(&Endpoint::Speech));
        assert_eq!(state, Endpoint::NoSpeech);
    }

    #[test]
    fn a_terminal_state_sticks() {
        let mut ep = Endpointer::new(EndpointConfig::REQUEST, None);
        assert_eq!(ep.push(&[QUIET; 160 * 500]), Endpoint::NoSpeech);
        assert_eq!(ep.push(&[LOUD; 1600]), Endpoint::NoSpeech);
    }

    #[test]
    fn partial_frames_are_buffered() {
        // 1.1 s of speech then silence, fed in 7-sample chunks, still ends
        // 800 ms after the silence begins.
        let mut signal = vec![LOUD; 110 * FRAME_SAMPLES];
        signal.extend(std::iter::repeat_n(QUIET, 100 * FRAME_SAMPLES));

        let mut ep = Endpointer::new(EndpointConfig::REQUEST, Some(SEED));
        let mut total = 0usize;
        let mut state = Endpoint::Waiting;
        let mut saw_speech = false;
        for chunk in signal.chunks(7) {
            state = ep.push(chunk);
            total += chunk.len();
            saw_speech |= state == Endpoint::Speech;
            if state.is_terminal() {
                break;
            }
        }
        assert!(saw_speech);
        assert_eq!(state, Endpoint::Ended);
        // 1.1 s of speech plus 0.8 s of silence, rounded up to the last chunk.
        let expected = 190 * FRAME_SAMPLES;
        assert!((total as isize - expected as isize).unsigned_abs() <= 7);
    }

    #[test]
    fn bytes_and_samples_reach_the_same_decision() {
        let mut signal = vec![QUIET; 160 * 30];
        signal.extend(vec![LOUD; 160 * 110]);
        signal.extend(vec![QUIET; 160 * 80]);
        let bytes: Vec<u8> = signal.iter().flat_map(|s| s.to_le_bytes()).collect();
        let mut samples = Endpointer::new(EndpointConfig::REQUEST, Some(SEED));
        let mut le = Endpointer::new(EndpointConfig::REQUEST, Some(SEED));
        // Frames of 320 bytes, as the microphone sends them.
        for (pcm, raw) in signal.chunks(160).zip(bytes.chunks(320)) {
            assert_eq!(samples.push(pcm), le.push_le(raw));
        }
        assert_eq!(le.push_le(&[]), Endpoint::Ended);
    }
}
