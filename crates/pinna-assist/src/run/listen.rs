//! Capture one utterance and detect its end without touching device state.

use bytes::Bytes;
use pinna_vad::{Endpoint, EndpointConfig, Endpointer};

use crate::audio::MicInput;

/// Why a listen produced no audio to upload.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Heard {
    NoSpeech,
}

/// Collect microphone frames until the endpointer finishes. A closed or
/// stalled route is an end of speech only after speech has actually started.
pub async fn listen(mic: &mut MicInput) -> Result<Vec<Bytes>, Heard> {
    let config = if mic.follow_up {
        EndpointConfig::FOLLOW_UP
    } else {
        EndpointConfig::REQUEST
    };

    let mut endpointer = Endpointer::new(config, mic.noise_seed);
    let mut frames = Vec::new();
    let mut speech_started = false;
    let collect = async {
        while let Some(frame) = mic.frames.recv().await {
            let endpoint = endpointer.push_le(&frame);
            speech_started |= matches!(
                endpoint,
                Endpoint::Speech | Endpoint::Ended | Endpoint::Capped
            );
            frames.push(frame);
            match endpoint {
                Endpoint::Ended | Endpoint::Capped => return Ok(std::mem::take(&mut frames)),
                Endpoint::NoSpeech => return Err(Heard::NoSpeech),
                Endpoint::Waiting | Endpoint::Speech => {}
            }
        }
        if speech_started {
            Ok(std::mem::take(&mut frames))
        } else {
            Err(Heard::NoSpeech)
        }
    };

    match tokio::time::timeout(config.backstop(), collect).await {
        Ok(result) => result,
        Err(_) if speech_started => Ok(frames),
        Err(_) => Err(Heard::NoSpeech),
    }
}

#[cfg(test)]
mod tests {
    use tokio::sync::mpsc;

    use super::*;

    const FRAME_SAMPLES: usize = 160;

    fn frame(amplitude: i16) -> Bytes {
        let mut bytes = Vec::with_capacity(FRAME_SAMPLES * 2);
        for _ in 0..FRAME_SAMPLES {
            bytes.extend_from_slice(&amplitude.to_le_bytes());
        }
        Bytes::from(bytes)
    }

    fn input(
        follow_up: bool,
        frames: impl IntoIterator<Item = Bytes>,
        close: bool,
    ) -> (MicInput, Option<mpsc::Sender<Bytes>>) {
        let (tx, rx) = mpsc::channel(2_000);
        for frame in frames {
            tx.try_send(frame).unwrap();
        }
        let tx = (!close).then_some(tx);
        (
            MicInput {
                frames: rx,
                noise_seed: Some(256),
                follow_up,
            },
            tx,
        )
    }

    fn quiet(count: usize) -> impl Iterator<Item = Bytes> {
        std::iter::repeat_n(frame(10), count)
    }

    fn loud(count: usize) -> impl Iterator<Item = Bytes> {
        std::iter::repeat_n(frame(3_000), count)
    }

    #[tokio::test(start_paused = true)]
    async fn speech_then_silence_ends() {
        let (mut mic, _tx) = input(false, loud(110).chain(quiet(100)), true);
        let frames = listen(&mut mic).await.unwrap();
        assert_eq!(frames.len(), 190);
    }

    #[tokio::test(start_paused = true)]
    async fn quiet_audio_is_never_uploaded() {
        let (mut mic, _tx) = input(false, quiet(600), true);
        assert_eq!(listen(&mut mic).await, Err(Heard::NoSpeech));
    }

    #[tokio::test(start_paused = true)]
    async fn a_follow_up_uses_the_shorter_window() {
        let (mut mic, _tx) = input(true, quiet(500), true);
        assert_eq!(listen(&mut mic).await, Err(Heard::NoSpeech));
        assert_eq!(mic.frames.len(), 100, "only the 4 s window was consumed");
    }

    #[tokio::test(start_paused = true)]
    async fn speech_is_capped_at_the_recording_limit() {
        let (mut mic, _tx) = input(false, loud(1_600), true);
        let frames = listen(&mut mic).await.unwrap();
        assert_eq!(frames.len(), 1_500);
    }

    #[tokio::test(start_paused = true)]
    async fn a_closed_channel_before_speech_is_no_speech() {
        let (mut mic, _tx) = input(false, [], true);
        assert_eq!(listen(&mut mic).await, Err(Heard::NoSpeech));
    }

    #[tokio::test(start_paused = true)]
    async fn a_closed_channel_after_speech_ends_the_utterance() {
        let (mut mic, _tx) = input(false, loud(100), true);
        assert_eq!(listen(&mut mic).await.unwrap().len(), 100);
    }

    #[tokio::test(start_paused = true)]
    async fn a_stalled_channel_uses_the_wall_clock_bound() {
        let (mut mic, tx) = input(false, [], false);
        let started = tokio::time::Instant::now();
        assert_eq!(listen(&mut mic).await, Err(Heard::NoSpeech));
        assert_eq!(started.elapsed(), EndpointConfig::REQUEST.backstop());
        drop(tx);

        let (mut mic, _tx) = input(false, loud(100), false);
        let frames = listen(&mut mic).await.unwrap();
        assert_eq!(frames.len(), 100);
    }
}
