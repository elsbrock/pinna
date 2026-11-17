//! Microphone input and the WAV container used by speech-to-text.

use bytes::{Bytes, BytesMut};
use tokio::sync::mpsc;

/// A run's microphone route. Preroll frames are first in `frames`.
pub struct MicInput {
    /// Signed 16-bit little-endian PCM, 16 kHz, mono.
    pub frames: mpsc::Receiver<Bytes>,
    pub noise_seed: Option<u64>,
    pub follow_up: bool,
}

/// Wrap PCM frames in a canonical 16-bit mono WAV container.
pub fn wav(frames: &[Bytes]) -> Bytes {
    let data_len = frames.iter().map(Bytes::len).sum::<usize>();
    let data_len = u32::try_from(data_len).expect("WAV audio is at most 4 GiB");
    let mut out = BytesMut::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16_u32.to_le_bytes());
    out.extend_from_slice(&1_u16.to_le_bytes());
    out.extend_from_slice(&1_u16.to_le_bytes());
    out.extend_from_slice(&16_000_u32.to_le_bytes());
    out.extend_from_slice(&32_000_u32.to_le_bytes());
    out.extend_from_slice(&2_u16.to_le_bytes());
    out.extend_from_slice(&16_u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for frame in frames {
        out.extend_from_slice(frame);
    }
    out.freeze()
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    #[test]
    fn wav_is_readable_pcm_with_all_frames() {
        let pcm = wav(&[
            Bytes::from_static(&[1, 0, 255, 255]),
            Bytes::from_static(&[2, 0]),
        ]);
        let mut reader = hound::WavReader::new(Cursor::new(pcm.clone())).unwrap();
        assert_eq!(reader.spec().channels, 1);
        assert_eq!(reader.spec().sample_rate, 16_000);
        assert_eq!(reader.spec().bits_per_sample, 16);
        assert_eq!(
            reader
                .samples::<i16>()
                .collect::<Result<Vec<_>, _>>()
                .unwrap(),
            [1, -1, 2]
        );
        assert_eq!(reader.duration(), 3);
        assert_eq!(pcm.len(), 44 + 6);
    }
}
