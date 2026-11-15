//! ESPHome native API framing.
//!
//! Plaintext frames are `0x00 | varint(len) | varint(type) | payload`. The
//! [`FrameCodec`] trait is the seam where a Noise transport can be added later:
//! the server only ever talks to a codec, never to the wire format directly.

use bytes::{Buf, BufMut, Bytes, BytesMut};

/// Largest payload accepted from a peer.
pub const MAX_FRAME: usize = 65536;

/// A decoded frame: message id and raw protobuf payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub id: u32,
    pub payload: Bytes,
}

/// Framing failure; the connection must be dropped.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum FrameError {
    #[error("bad preamble byte {0:#04x}")]
    BadPreamble(u8),
    #[error("malformed varint")]
    BadVarint,
    #[error("frame of {0} bytes exceeds the {MAX_FRAME} byte limit")]
    TooLarge(usize),
    #[error("message id {0} does not fit in 32 bits")]
    BadId(u64),
}

/// Transport framing for one connection.
pub trait FrameCodec: Send {
    /// Append one frame to `out`.
    fn encode(&mut self, id: u32, payload: &[u8], out: &mut BytesMut) -> Result<(), FrameError>;
    /// Remove one complete frame from the front of `buf`; `Ok(None)` when more bytes are needed.
    fn decode(&mut self, buf: &mut BytesMut) -> Result<Option<Frame>, FrameError>;
}

/// Append `v` as a protobuf varint.
pub fn put_varint(out: &mut impl BufMut, mut v: u64) {
    while v >= 0x80 {
        out.put_u8((v as u8 & 0x7f) | 0x80);
        v >>= 7;
    }
    out.put_u8(v as u8);
}

/// Read a varint from the start of `buf`: `Ok(Some((value, bytes used)))`,
/// `Ok(None)` if truncated.
pub fn get_varint(buf: &[u8]) -> Result<Option<(u64, usize)>, FrameError> {
    let mut v = 0u64;
    for (i, b) in buf.iter().enumerate() {
        if i >= 10 {
            return Err(FrameError::BadVarint);
        }
        v |= u64::from(b & 0x7f) << (7 * i);
        if b & 0x80 == 0 {
            return Ok(Some((v, i + 1)));
        }
    }
    Ok(None)
}

/// Unencrypted framing (preamble 0x00).
#[derive(Debug, Default, Clone, Copy)]
pub struct Plaintext;

impl FrameCodec for Plaintext {
    fn encode(&mut self, id: u32, payload: &[u8], out: &mut BytesMut) -> Result<(), FrameError> {
        if payload.len() > MAX_FRAME {
            return Err(FrameError::TooLarge(payload.len()));
        }
        out.put_u8(0);
        put_varint(out, payload.len() as u64);
        put_varint(out, u64::from(id));
        out.put_slice(payload);
        Ok(())
    }

    fn decode(&mut self, buf: &mut BytesMut) -> Result<Option<Frame>, FrameError> {
        let Some(&pre) = buf.first() else {
            return Ok(None);
        };
        if pre != 0 {
            return Err(FrameError::BadPreamble(pre));
        }
        let Some((len, n1)) = get_varint(&buf[1..])? else {
            return Ok(None);
        };
        if len > MAX_FRAME as u64 {
            return Err(FrameError::TooLarge(len as usize));
        }
        let Some((id, n2)) = get_varint(&buf[1 + n1..])? else {
            return Ok(None);
        };
        let id = u32::try_from(id).map_err(|_| FrameError::BadId(id))?;
        let header = 1 + n1 + n2;
        let len = len as usize;
        if buf.len() < header + len {
            return Ok(None);
        }
        buf.advance(header);
        let payload = buf.split_to(len).freeze();
        Ok(Some(Frame { id, payload }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varint_roundtrip() {
        for v in [
            0u64,
            1,
            127,
            128,
            300,
            16383,
            16384,
            65536,
            u32::MAX as u64,
            u64::MAX,
        ] {
            let mut b = BytesMut::new();
            put_varint(&mut b, v);
            assert_eq!(get_varint(&b).unwrap(), Some((v, b.len())));
        }
    }

    #[test]
    fn varint_edges() {
        assert_eq!(get_varint(&[]).unwrap(), None);
        assert_eq!(get_varint(&[0x80]).unwrap(), None);
        assert_eq!(get_varint(&[0xac, 0x02]).unwrap(), Some((300, 2)));
        assert_eq!(get_varint(&[0x80; 11]), Err(FrameError::BadVarint));
    }

    #[test]
    fn encode_known_bytes() {
        let mut out = BytesMut::new();
        Plaintext.encode(1, b"abc", &mut out).unwrap();
        assert_eq!(&out[..], &[0, 3, 1, b'a', b'b', b'c']);
        let mut out = BytesMut::new();
        Plaintext.encode(106, &[], &mut out).unwrap();
        assert_eq!(&out[..], &[0, 0, 106]);
    }

    #[test]
    fn decode_roundtrip_multiple() {
        let mut c = Plaintext;
        let mut buf = BytesMut::new();
        c.encode(7, &[], &mut buf).unwrap();
        c.encode(300, &[9u8; 200], &mut buf).unwrap();
        let a = c.decode(&mut buf).unwrap().unwrap();
        assert_eq!((a.id, a.payload.len()), (7, 0));
        let b = c.decode(&mut buf).unwrap().unwrap();
        assert_eq!((b.id, b.payload.len()), (300, 200));
        assert!(buf.is_empty());
        assert_eq!(c.decode(&mut buf).unwrap(), None);
    }

    #[test]
    fn decode_partial_reads_byte_by_byte() {
        let mut enc = BytesMut::new();
        Plaintext.encode(90, &vec![5u8; 300], &mut enc).unwrap();
        let mut c = Plaintext;
        let mut buf = BytesMut::new();
        let mut got = None;
        for (i, b) in enc.iter().enumerate() {
            assert!(got.is_none(), "frame completed early at byte {i}");
            buf.put_u8(*b);
            got = c.decode(&mut buf).unwrap();
        }
        let f = got.unwrap();
        assert_eq!((f.id, f.payload.len()), (90, 300));
        assert!(buf.is_empty());
    }

    #[test]
    fn decode_rejects_bad_input() {
        let mut c = Plaintext;
        assert_eq!(
            c.decode(&mut BytesMut::from(&[1u8, 0, 0][..])),
            Err(FrameError::BadPreamble(1))
        );
        let mut big = BytesMut::new();
        big.put_u8(0);
        put_varint(&mut big, MAX_FRAME as u64 + 1);
        put_varint(&mut big, 1);
        assert_eq!(c.decode(&mut big), Err(FrameError::TooLarge(MAX_FRAME + 1)));
        let mut max = BytesMut::new();
        c.encode(1, &vec![0; MAX_FRAME], &mut max).unwrap();
        assert!(c.decode(&mut max).unwrap().is_some());
        assert!(
            c.encode(1, &vec![0; MAX_FRAME + 1], &mut BytesMut::new())
                .is_err()
        );
    }
}
