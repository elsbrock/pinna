//! Binary messages once the Noise handshake is done (ADR 0027 §6): the type byte, the
//! reassembly of fragmented messages, and the header of an audio chunk.
//!
//! The reassembler keeps at most one fragmented message, as the specification allows, and
//! closes the connection on one that grows past its limit: 16 KiB for JSON, the player's
//! `buffer_capacity` for audio. Fragments of a type the player does not implement are tracked
//! but not kept.

/// A JSON message in UTF-8.
pub const TYPE_JSON: u8 = 0;
/// A piece of a message too large for one Noise transport message.
pub const TYPE_FRAGMENT: u8 = 1;
/// A player audio chunk.
pub const TYPE_AUDIO: u8 = 4;

/// The type byte, the timestamp and `send_ahead`.
pub const AUDIO_HEADER_LEN: usize = 13;

/// The specification's limit on a JSON message, which the player applies to everything the
/// server sends (ADR 0027 §6).
pub const JSON_MAX: usize = 16384;

const FLAG_LAST: u8 = 0b01;
const FLAG_FIRST: u8 = 0b10;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FrameError {
    #[error("a binary message is empty")]
    Empty,
    #[error("a fragment has no flags byte")]
    NoFlags,
    #[error("a fragment sets reserved flag bits")]
    ReservedFlags,
    #[error("a first fragment has no message type")]
    NoType,
    #[error("a fragment's message type is 1")]
    NestedFragment,
    #[error("a first fragment arrived while another fragmented message was in flight")]
    Overlap,
    #[error("a fragment continues no message")]
    Orphan,
    #[error("a message arrived in the middle of a fragmented message")]
    Interrupted,
    #[error("a fragmented message of type {kind} exceeds {limit} bytes")]
    TooLarge { kind: u8, limit: usize },
    #[error("an audio message is shorter than 13 bytes")]
    ShortAudio,
}

/// One complete binary message: its type and the bytes after the type byte.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub kind: u8,
    pub body: Vec<u8>,
}

/// Turns decrypted binary messages into complete messages.
#[derive(Debug)]
pub struct Reassembler {
    audio_limit: usize,
    in_flight: Option<InFlight>,
}

#[derive(Debug)]
struct InFlight {
    kind: u8,
    /// `None` for a type the player does not implement: its data is dropped as it arrives.
    data: Option<Vec<u8>>,
}

impl Reassembler {
    /// `audio_limit` bounds a reassembled audio chunk: header and payload, as the server
    /// counts it against `buffer_capacity`.
    pub fn new(audio_limit: usize) -> Self {
        Self {
            audio_limit,
            in_flight: None,
        }
    }

    fn limit(&self, kind: u8) -> Option<usize> {
        match kind {
            TYPE_JSON => Some(JSON_MAX),
            // The type byte counts too.
            TYPE_AUDIO => Some(self.audio_limit.saturating_sub(1)),
            _ => None,
        }
    }

    /// Whether a fragmented message is incomplete.
    pub fn in_flight(&self) -> bool {
        self.in_flight.is_some()
    }

    /// Takes one decrypted binary message. Returns the message it completes, if any: a plain
    /// message at once, a fragmented one with its last fragment. Every error closes the
    /// connection.
    pub fn push(&mut self, mut plain: Vec<u8>) -> Result<Option<Message>, FrameError> {
        let Some(&kind) = plain.first() else {
            return Err(FrameError::Empty);
        };
        if kind != TYPE_FRAGMENT {
            if self.in_flight.is_some() {
                return Err(FrameError::Interrupted);
            }
            plain.remove(0);
            return Ok(Some(Message { kind, body: plain }));
        }
        let Some(&flags) = plain.get(1) else {
            return Err(FrameError::NoFlags);
        };
        if flags & !(FLAG_FIRST | FLAG_LAST) != 0 {
            return Err(FrameError::ReservedFlags);
        }
        let data_at = if flags & FLAG_FIRST != 0 {
            if self.in_flight.is_some() {
                return Err(FrameError::Overlap);
            }
            let Some(&orig) = plain.get(2) else {
                return Err(FrameError::NoType);
            };
            if orig == TYPE_FRAGMENT {
                return Err(FrameError::NestedFragment);
            }
            let data = self.limit(orig).map(|_| Vec::new());
            self.in_flight = Some(InFlight { kind: orig, data });
            3
        } else {
            if self.in_flight.is_none() {
                return Err(FrameError::Orphan);
            }
            2
        };
        let limit = self
            .in_flight
            .as_ref()
            .and_then(|f| self.limit(f.kind))
            .unwrap_or(usize::MAX);
        let Some(in_flight) = self.in_flight.as_mut() else {
            return Err(FrameError::Orphan);
        };
        if let Some(data) = in_flight.data.as_mut() {
            let chunk = &plain[data_at..];
            if data.len() + chunk.len() > limit {
                return Err(FrameError::TooLarge {
                    kind: in_flight.kind,
                    limit,
                });
            }
            data.extend_from_slice(chunk);
        }
        if flags & FLAG_LAST == 0 {
            return Ok(None);
        }
        let done = self.in_flight.take();
        Ok(done.and_then(|f| f.data.map(|body| Message { kind: f.kind, body })))
    }
}

/// The header of an audio chunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioHeader {
    /// Server time in µs at which the chunk's first sample is to be heard.
    pub timestamp_us: i64,
    /// µs from the chunk's transmission to `timestamp_us`. Only for measuring arrival delay.
    pub send_ahead_us: u32,
}

/// Splits an audio message's body (after the type byte) into its header and encoded audio.
pub fn split_audio(body: &[u8]) -> Result<(AudioHeader, &[u8]), FrameError> {
    let Some((timestamp, rest)) = body.split_first_chunk::<8>() else {
        return Err(FrameError::ShortAudio);
    };
    let Some((send_ahead, data)) = rest.split_first_chunk::<4>() else {
        return Err(FrameError::ShortAudio);
    };
    Ok((
        AudioHeader {
            timestamp_us: i64::from_be_bytes(*timestamp),
            send_ahead_us: u32::from_be_bytes(*send_ahead),
        },
        data,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frag(flags: u8, orig: Option<u8>, data: &[u8]) -> Vec<u8> {
        let mut v = vec![TYPE_FRAGMENT, flags];
        v.extend(orig);
        v.extend_from_slice(data);
        v
    }

    #[test]
    fn plain_messages_pass_through() {
        let mut r = Reassembler::new(1000);
        assert_eq!(
            r.push(vec![0, b'{', b'}']).unwrap(),
            Some(Message {
                kind: 0,
                body: b"{}".to_vec()
            })
        );
        assert_eq!(
            r.push(vec![200]).unwrap(),
            Some(Message {
                kind: 200,
                body: vec![]
            })
        );
        assert_eq!(r.push(vec![]), Err(FrameError::Empty));
    }

    #[test]
    fn fragments_reassemble_in_order() {
        let mut r = Reassembler::new(1000);
        assert_eq!(r.push(frag(FLAG_FIRST, Some(TYPE_AUDIO), b"ab")), Ok(None));
        assert!(r.in_flight());
        assert_eq!(r.push(frag(0, None, b"cd")), Ok(None));
        assert_eq!(
            r.push(frag(FLAG_LAST, None, b"ef")).unwrap(),
            Some(Message {
                kind: TYPE_AUDIO,
                body: b"abcdef".to_vec()
            })
        );
        assert!(!r.in_flight());
        // First and last at once, and empty data, are both valid.
        assert_eq!(
            r.push(frag(FLAG_FIRST | FLAG_LAST, Some(TYPE_JSON), b""))
                .unwrap(),
            Some(Message {
                kind: TYPE_JSON,
                body: vec![]
            })
        );
    }

    #[test]
    fn unimplemented_types_are_tracked_but_not_kept() {
        let mut r = Reassembler::new(10);
        assert_eq!(r.push(frag(FLAG_FIRST, Some(8), &[0; 100])), Ok(None));
        // Still in flight: anything but a fragment is a malformed sequence.
        assert_eq!(r.push(vec![0]), Err(FrameError::Interrupted));
        let mut r = Reassembler::new(10);
        r.push(frag(FLAG_FIRST, Some(8), &[0; 100])).unwrap();
        assert_eq!(r.push(frag(FLAG_LAST, None, &[0; 100])), Ok(None));
        assert!(!r.in_flight());
    }

    #[test]
    fn limits() {
        // An audio limit of 10 bytes leaves 9 for the data after the type byte.
        let mut r = Reassembler::new(10);
        r.push(frag(FLAG_FIRST, Some(TYPE_AUDIO), &[0; 5])).unwrap();
        r.push(frag(0, None, &[0; 4])).unwrap();
        assert_eq!(
            r.push(frag(FLAG_LAST, None, &[0; 1])),
            Err(FrameError::TooLarge {
                kind: TYPE_AUDIO,
                limit: 9
            })
        );
        let mut r = Reassembler::new(10);
        r.push(frag(FLAG_FIRST, Some(TYPE_AUDIO), &[0; 5])).unwrap();
        assert!(r.push(frag(FLAG_LAST, None, &[0; 4])).unwrap().is_some());

        let mut r = Reassembler::new(usize::MAX);
        r.push(frag(FLAG_FIRST, Some(TYPE_JSON), &[b' '; JSON_MAX]))
            .unwrap();
        assert_eq!(
            r.push(frag(FLAG_LAST, None, b" ")),
            Err(FrameError::TooLarge {
                kind: TYPE_JSON,
                limit: JSON_MAX
            })
        );
    }

    #[test]
    fn malformed_sequences() {
        let mut r = Reassembler::new(100);
        assert_eq!(r.push(vec![TYPE_FRAGMENT]), Err(FrameError::NoFlags));
        assert_eq!(
            r.push(vec![TYPE_FRAGMENT, FLAG_FIRST]),
            Err(FrameError::NoType)
        );
        assert_eq!(
            r.push(frag(FLAG_FIRST, Some(TYPE_FRAGMENT), b"")),
            Err(FrameError::NestedFragment)
        );
        for reserved in [0b100u8, 0b1000_0000, 0b1111_1100] {
            assert_eq!(
                r.push(frag(reserved | FLAG_FIRST, Some(0), b"")),
                Err(FrameError::ReservedFlags)
            );
        }
        assert_eq!(r.push(frag(0, None, b"x")), Err(FrameError::Orphan));
        assert_eq!(r.push(frag(FLAG_LAST, None, b"x")), Err(FrameError::Orphan));

        let mut r = Reassembler::new(100);
        r.push(frag(FLAG_FIRST, Some(TYPE_AUDIO), b"x")).unwrap();
        assert_eq!(
            r.push(frag(FLAG_FIRST, Some(TYPE_AUDIO), b"y")),
            Err(FrameError::Overlap)
        );
        let mut r = Reassembler::new(100);
        r.push(frag(FLAG_FIRST, Some(TYPE_AUDIO), b"x")).unwrap();
        assert_eq!(r.push(vec![TYPE_AUDIO; 20]), Err(FrameError::Interrupted));
    }

    #[test]
    fn audio_header() {
        let mut body = (-5i64).to_be_bytes().to_vec();
        body.extend(70_000u32.to_be_bytes());
        body.extend(b"pcm");
        let (head, data) = split_audio(&body).unwrap();
        assert_eq!(
            head,
            AudioHeader {
                timestamp_us: -5,
                send_ahead_us: 70_000
            }
        );
        assert_eq!(data, b"pcm");
        // Exactly the header, no audio, is not an error here.
        assert!(split_audio(&body[..12]).unwrap().1.is_empty());
        assert_eq!(split_audio(&body[..11]), Err(FrameError::ShortAudio));
        assert_eq!(split_audio(&[]), Err(FrameError::ShortAudio));
    }
}
