//! ESPHome native API framing.
//!
//! Plaintext frames are `0x00 | varint(len) | varint(type) | payload`. Noise frames are
//! `0x01 | u16 BE len | payload`; after the handshake the payload is the ciphertext of
//! `u16 BE type | u16 BE data len | protobuf`. The server only ever talks to a
//! [`FrameCodec`], never to the wire format directly.

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
    #[error("noise handshake failed")]
    Handshake,
    #[error("noise frame failed to decrypt")]
    Decrypt,
    #[error("decrypted noise frame is malformed")]
    BadInner,
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

/// Noise protocol used by ESPHome.
pub const NOISE_PROTOCOL: &str = "Noise_NNpsk0_25519_ChaChaPoly_SHA256";
const NOISE_PROLOGUE: &[u8] = b"NoiseAPIInit\x00\x00";
const NOISE_TAG: usize = 16;
/// Largest protobuf payload that fits one encrypted frame (u16 length, tag, 4 byte header).
const NOISE_MAX_DATA: usize = u16::MAX as usize - NOISE_TAG - 4;

/// Append one raw Noise frame (`0x01 | u16 BE len | payload`).
pub fn put_noise_frame(out: &mut BytesMut, payload: &[u8]) -> Result<(), FrameError> {
    let len = u16::try_from(payload.len()).map_err(|_| FrameError::TooLarge(payload.len()))?;
    out.put_u8(1);
    out.put_u16(len);
    out.put_slice(payload);
    Ok(())
}

/// Remove one raw Noise frame from `buf`; `Ok(None)` when more bytes are needed.
pub fn take_noise_frame(buf: &mut BytesMut) -> Result<Option<Bytes>, FrameError> {
    let Some(&ind) = buf.first() else {
        return Ok(None);
    };
    if ind != 1 {
        return Err(FrameError::BadPreamble(ind));
    }
    if buf.len() < 3 {
        return Ok(None);
    }
    let len = usize::from(u16::from_be_bytes([buf[1], buf[2]]));
    if buf.len() < 3 + len {
        return Ok(None);
    }
    buf.advance(3);
    Ok(Some(buf.split_to(len).freeze()))
}

/// Encrypted framing, usable once the handshake is done.
pub struct NoiseCodec {
    transport: snow::TransportState,
}

impl FrameCodec for NoiseCodec {
    fn encode(&mut self, id: u32, payload: &[u8], out: &mut BytesMut) -> Result<(), FrameError> {
        let id = u16::try_from(id).map_err(|_| FrameError::BadId(u64::from(id)))?;
        if payload.len() > NOISE_MAX_DATA {
            return Err(FrameError::TooLarge(payload.len()));
        }
        let mut plain = Vec::with_capacity(4 + payload.len());
        plain.extend_from_slice(&id.to_be_bytes());
        plain.extend_from_slice(&(payload.len() as u16).to_be_bytes());
        plain.extend_from_slice(payload);
        let mut enc = vec![0u8; plain.len() + NOISE_TAG];
        let n = self
            .transport
            .write_message(&plain, &mut enc)
            .map_err(|_| FrameError::Decrypt)?;
        put_noise_frame(out, &enc[..n])
    }

    fn decode(&mut self, buf: &mut BytesMut) -> Result<Option<Frame>, FrameError> {
        let Some(raw) = take_noise_frame(buf)? else {
            return Ok(None);
        };
        let mut plain = vec![0u8; raw.len()];
        let n = self
            .transport
            .read_message(&raw, &mut plain)
            .map_err(|_| FrameError::Decrypt)?;
        plain.truncate(n);
        if plain.len() < 4
            || usize::from(u16::from_be_bytes([plain[2], plain[3]])) != plain.len() - 4
        {
            return Err(FrameError::BadInner);
        }
        let id = u32::from(u16::from_be_bytes([plain[0], plain[1]]));
        Ok(Some(Frame {
            id,
            payload: Bytes::from(plain).slice(4..),
        }))
    }
}

/// Progress of [`NoiseHandshake::feed`].
pub enum HandshakeStep {
    /// More bytes are needed; flush the output written so far.
    Pending,
    /// Handshake done; flush the output, then switch to the codec.
    Done(NoiseCodec),
    /// Handshake refused; flush the output (an error frame), then close.
    Rejected,
}

/// Server side of the Noise handshake: client hello, server hello, handshake message.
pub struct NoiseHandshake {
    psk: [u8; 32],
    hello: Vec<u8>,
    got_hello: bool,
}

impl NoiseHandshake {
    /// `name` and `mac` (12 lowercase hex digits) go into the server hello.
    pub fn new(psk: [u8; 32], name: &str, mac: &str) -> Self {
        let mut hello = vec![1];
        hello.extend_from_slice(name.as_bytes());
        hello.push(0);
        hello.extend_from_slice(mac.as_bytes());
        hello.push(0);
        Self {
            psk,
            hello,
            got_hello: false,
        }
    }

    /// Consume input from `buf`, append anything to send to `out`.
    pub fn feed(
        &mut self,
        buf: &mut BytesMut,
        out: &mut BytesMut,
    ) -> Result<HandshakeStep, FrameError> {
        if !self.got_hello {
            match take_noise_frame(buf) {
                // A plaintext client: the 0x01 indicator is what makes it ask for the key.
                Err(FrameError::BadPreamble(0)) => {
                    put_noise_frame(out, b"\x01Bad indicator byte")?;
                    return Ok(HandshakeStep::Rejected);
                }
                Err(e) => return Err(e),
                Ok(None) => return Ok(HandshakeStep::Pending),
                Ok(Some(hello)) if hello.is_empty() => {}
                Ok(Some(_)) => return Err(FrameError::Handshake),
            }
            put_noise_frame(out, &self.hello)?;
            self.got_hello = true;
        }
        let Some(msg) = take_noise_frame(buf)? else {
            return Ok(HandshakeStep::Pending);
        };
        if msg.first() != Some(&0) {
            return Err(FrameError::Handshake);
        }
        match self.respond(&msg[1..]) {
            Ok((reply, codec)) => {
                let mut framed = Vec::with_capacity(1 + reply.len());
                framed.push(0);
                framed.extend_from_slice(&reply);
                put_noise_frame(out, &framed)?;
                Ok(HandshakeStep::Done(codec))
            }
            Err(_) => {
                put_noise_frame(out, b"\x01Handshake MAC failure")?;
                Ok(HandshakeStep::Rejected)
            }
        }
    }

    fn respond(&self, msg: &[u8]) -> Result<(Vec<u8>, NoiseCodec), snow::Error> {
        let mut hs = snow::Builder::new(NOISE_PROTOCOL.parse()?)
            .psk(0, &self.psk)?
            .prologue(NOISE_PROLOGUE)?
            .build_responder()?;
        let mut scratch = [0u8; 1024];
        hs.read_message(msg, &mut scratch)?;
        let mut reply = vec![0u8; 1024];
        let n = hs.write_message(&[], &mut reply)?;
        reply.truncate(n);
        let transport = hs.into_transport_mode()?;
        Ok((reply, NoiseCodec { transport }))
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

    const PSK: [u8; 32] = [7; 32];

    /// Drive a client through the handshake against `NoiseHandshake`; returns both codecs.
    fn connect(client_psk: [u8; 32]) -> Result<(NoiseCodec, NoiseCodec), BytesMut> {
        let mut init = snow::Builder::new(NOISE_PROTOCOL.parse().unwrap())
            .psk(0, &client_psk)
            .unwrap()
            .prologue(NOISE_PROLOGUE)
            .unwrap()
            .build_initiator()
            .unwrap();
        let mut hs = NoiseHandshake::new(PSK, "node", "020000000042");
        let mut wire = BytesMut::new();
        put_noise_frame(&mut wire, &[]).unwrap();
        let mut msg = vec![0u8; 100];
        let n = init.write_message(&[], &mut msg).unwrap();
        let mut framed = vec![0];
        framed.extend_from_slice(&msg[..n]);
        put_noise_frame(&mut wire, &framed).unwrap();
        let mut out = BytesMut::new();
        let server = match hs.feed(&mut wire, &mut out).unwrap() {
            HandshakeStep::Done(c) => c,
            _ => return Err(out),
        };
        let hello = take_noise_frame(&mut out).unwrap().unwrap();
        assert_eq!(&hello[..], b"\x01node\x00020000000042\x00");
        let reply = take_noise_frame(&mut out).unwrap().unwrap();
        assert!(out.is_empty() && reply[0] == 0);
        let mut scratch = vec![0u8; 100];
        init.read_message(&reply[1..], &mut scratch).unwrap();
        let client = NoiseCodec {
            transport: init.into_transport_mode().unwrap(),
        };
        Ok((client, server))
    }

    #[test]
    fn noise_frame_roundtrip_and_partial() {
        let mut out = BytesMut::new();
        put_noise_frame(&mut out, b"hello").unwrap();
        assert_eq!(&out[..], &[1, 0, 5, b'h', b'e', b'l', b'l', b'o']);
        let mut buf = BytesMut::new();
        let mut got = None;
        for b in out.iter() {
            assert!(got.is_none());
            buf.put_u8(*b);
            got = take_noise_frame(&mut buf).unwrap();
        }
        assert_eq!(&got.unwrap()[..], b"hello");
        assert!(buf.is_empty());
    }

    #[test]
    fn noise_frame_rejects_bad_input() {
        assert_eq!(
            take_noise_frame(&mut BytesMut::from(&[0u8, 0, 0][..])),
            Err(FrameError::BadPreamble(0))
        );
        assert_eq!(
            take_noise_frame(&mut BytesMut::from(&[9u8][..])),
            Err(FrameError::BadPreamble(9))
        );
        let mut out = BytesMut::new();
        put_noise_frame(&mut out, &vec![0; u16::MAX as usize]).unwrap();
        assert_eq!(
            put_noise_frame(&mut out, &vec![0; u16::MAX as usize + 1]),
            Err(FrameError::TooLarge(u16::MAX as usize + 1))
        );
    }

    #[test]
    fn noise_codec_roundtrip_both_ways() {
        let (mut client, mut server) = connect(PSK).unwrap();
        let mut wire = BytesMut::new();
        client.encode(9, b"abc", &mut wire).unwrap();
        client.encode(300, &[], &mut wire).unwrap();
        server
            .encode(11, &vec![5; NOISE_MAX_DATA], &mut wire)
            .unwrap();
        let a = server.decode(&mut wire).unwrap().unwrap();
        assert_eq!((a.id, &a.payload[..]), (9, &b"abc"[..]));
        let b = server.decode(&mut wire).unwrap().unwrap();
        assert_eq!((b.id, b.payload.len()), (300, 0));
        let c = client.decode(&mut wire).unwrap().unwrap();
        assert_eq!((c.id, c.payload.len()), (11, NOISE_MAX_DATA));
        assert!(wire.is_empty());
        assert!(
            server
                .encode(1, &vec![0; NOISE_MAX_DATA + 1], &mut BytesMut::new())
                .is_err()
        );
    }

    #[test]
    fn noise_codec_partial_reads_and_tamper() {
        let (mut client, mut server) = connect(PSK).unwrap();
        let mut enc = BytesMut::new();
        client.encode(90, &[3; 300], &mut enc).unwrap();
        let mut buf = BytesMut::new();
        let mut got = None;
        for b in enc.iter() {
            assert!(got.is_none());
            buf.put_u8(*b);
            got = server.decode(&mut buf).unwrap();
        }
        assert_eq!(got.unwrap().payload.len(), 300);
        let mut enc = BytesMut::new();
        client.encode(1, b"x", &mut enc).unwrap();
        let last = enc.len() - 1;
        enc[last] ^= 1;
        assert_eq!(server.decode(&mut enc), Err(FrameError::Decrypt));
    }

    #[test]
    fn handshake_wrong_key_and_plaintext() {
        let mut out = connect([8; 32]).err().unwrap();
        take_noise_frame(&mut out).unwrap().unwrap();
        assert_eq!(
            &take_noise_frame(&mut out).unwrap().unwrap()[..],
            b"\x01Handshake MAC failure"
        );
        let mut hs = NoiseHandshake::new(PSK, "node", "020000000042");
        let (mut inp, mut out) = (BytesMut::from(&[0u8, 0, 1][..]), BytesMut::new());
        assert!(matches!(
            hs.feed(&mut inp, &mut out).unwrap(),
            HandshakeStep::Rejected
        ));
        assert_eq!(
            &take_noise_frame(&mut out).unwrap().unwrap()[..],
            b"\x01Bad indicator byte"
        );
        let mut inp = BytesMut::from(&[2u8, 0, 0][..]);
        assert!(hs.feed(&mut inp, &mut BytesMut::new()).is_err());
    }
}
