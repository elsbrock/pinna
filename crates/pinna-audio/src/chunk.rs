//! Decoding of Sendspin audio chunks (ADR 0027 §8).
//!
//! A chunk carries whole codec units: PCM frames, or one or more complete
//! FLAC frames of a stream that `stream/start` describes (its `codec_header`
//! is the `fLaC` marker and the STREAMINFO block). Each chunk decodes on its
//! own into interleaved stereo i16 at the stream's rate: mono is duplicated,
//! 24-bit audio is rounded to 16 bits.
//!
//! Everything here comes from the network, so malformed input is an error,
//! never a panic: FLAC frames are delimited and checked (header CRC-8, frame
//! CRC-16, block size against STREAMINFO) before the decoder sees them.

use crate::{Error, Result};
use symphonia::core::audio::{AudioBufferRef, Signal};
use symphonia::core::codecs::{CODEC_TYPE_FLAC, CodecParameters, Decoder, DecoderOptions};
use symphonia::core::formats::Packet;

/// The codecs a Pinna player lists (ADR 0027 §8: no Opus).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Codec {
    Pcm,
    Flac,
}

/// Decodes the chunks of one stream format.
pub struct ChunkDecoder {
    rate: u32,
    channels: usize,
    bits: u16,
    flac: Option<Flac>,
}

/// A FLAC stream's decoder, and what its STREAMINFO allows.
struct Flac {
    decoder: Box<dyn Decoder>,
    params: CodecParameters,
    /// The largest block STREAMINFO allows: a frame claiming more would
    /// overrun the decoder's buffer.
    max_block: u32,
}

fn bad(msg: impl Into<String>) -> Error {
    Error::Decode(msg.into())
}

impl ChunkDecoder {
    /// A decoder for the format `stream/start` announced. `codec_header` is
    /// required for FLAC (the `fLaC` marker, then the STREAMINFO block) and
    /// must agree with the other fields.
    pub fn new(
        codec: Codec,
        sample_rate: u32,
        channels: u16,
        bit_depth: u16,
        codec_header: Option<Vec<u8>>,
    ) -> Result<ChunkDecoder> {
        if !(1..=2).contains(&channels) {
            return Err(bad(format!("{channels} channels; Pinna plays 1 or 2")));
        }
        if bit_depth != 16 && bit_depth != 24 {
            return Err(bad(format!("{bit_depth}-bit audio; Pinna plays 16 or 24")));
        }
        if sample_rate == 0 {
            return Err(bad("sample rate 0"));
        }
        let flac = match codec {
            Codec::Pcm => None,
            Codec::Flac => {
                let header =
                    codec_header.ok_or_else(|| bad("FLAC stream without a codec header"))?;
                Some(Flac::new(&header, sample_rate, channels, bit_depth)?)
            }
        };
        Ok(ChunkDecoder {
            rate: sample_rate,
            channels: usize::from(channels),
            bits: bit_depth,
            flac,
        })
    }

    /// The stream's sample rate: what [`decode`](Self::decode) returns.
    pub fn sample_rate(&self) -> u32 {
        self.rate
    }

    /// One chunk's audio as interleaved stereo at the stream's rate.
    pub fn decode(&mut self, data: &[u8]) -> Result<Vec<i16>> {
        match &mut self.flac {
            None => pcm(data, self.channels, self.bits),
            Some(flac) => flac.decode(data, self.channels),
        }
    }
}

/// Little-endian signed PCM, 24-bit packed in 3 bytes.
fn pcm(data: &[u8], channels: usize, bits: u16) -> Result<Vec<i16>> {
    let width = usize::from(bits / 8);
    let frame = width * channels;
    if !data.len().is_multiple_of(frame) {
        return Err(bad(format!(
            "PCM chunk of {} bytes is not whole {frame}-byte frames",
            data.len()
        )));
    }
    let mut out = Vec::with_capacity(data.len() / frame * 2);
    for f in data.chunks_exact(frame) {
        let mut s = f.chunks_exact(width).map(|bytes| match bytes {
            [lo, hi] => i16::from_le_bytes([*lo, *hi]),
            [lo, mid, hi] => round24(i32::from_le_bytes([0, *lo, *mid, *hi]) >> 8),
            _ => 0,
        });
        let l = s.next().unwrap_or(0);
        out.push(l);
        out.push(s.next().unwrap_or(l));
    }
    Ok(out)
}

/// A 24-bit sample rounded to 16 bits, saturating at full scale.
fn round24(s: i32) -> i16 {
    ((s + 0x80) >> 8).clamp(-32768, 32767) as i16
}

/// A sample at 32-bit full scale (as the FLAC decoder gives it) rounded to
/// 16 bits.
fn round32(s: i32) -> i16 {
    ((i64::from(s) + 0x8000) >> 16).clamp(-32768, 32767) as i16
}

impl Flac {
    fn new(header: &[u8], rate: u32, channels: u16, bits: u16) -> Result<Flac> {
        let info = streaminfo(header)?;
        let (info_rate, info_channels, info_bits, max_block) = parse_streaminfo(info)?;
        if (info_rate, info_channels, info_bits) != (rate, channels, bits) {
            return Err(bad(format!(
                "FLAC header says {info_rate} Hz, {info_channels} channels, {info_bits}-bit; \
                 the stream {rate} Hz, {channels} channels, {bits}-bit"
            )));
        }
        let mut params = CodecParameters::new();
        params
            .for_codec(CODEC_TYPE_FLAC)
            .with_extra_data(Box::from(info));
        let decoder = make_flac(&params)?;
        Ok(Flac {
            decoder,
            params,
            max_block,
        })
    }

    fn decode(&mut self, data: &[u8], channels: usize) -> Result<Vec<i16>> {
        let mut out = Vec::new();
        let mut start = 0;
        while start < data.len() {
            let len = frame_len(&data[start..])?;
            let frame = &data[start..start + len];
            start += len;
            let header = frame_header(frame).ok_or_else(|| bad("damaged FLAC frame header"))?;
            if header.block > self.max_block {
                return Err(bad(format!(
                    "FLAC frame of {} samples, more than the stream's {}",
                    header.block, self.max_block
                )));
            }
            if usize::from(header.channels) != channels {
                return Err(bad(format!(
                    "FLAC frame with {} channels in a {channels}-channel stream",
                    header.channels
                )));
            }
            let packet = Packet::new_from_slice(0, 0, 0, frame);
            // The decoder indexes by what the frame claims; the checks above
            // rule out what is known to overrun it. A frame that still makes
            // it panic is damaged data, not a reason to take pinnad down.
            let decoded = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                match self.decoder.decode(&packet) {
                    Ok(AudioBufferRef::S32(buf)) => {
                        let frames = buf.frames();
                        let left = buf.chan(0);
                        let right = if buf.spec().channels.count() > 1 {
                            buf.chan(1)
                        } else {
                            left
                        };
                        let mut v = Vec::with_capacity(frames * 2);
                        for (l, r) in left.iter().zip(right) {
                            v.push(round32(*l));
                            v.push(round32(*r));
                        }
                        Ok(v)
                    }
                    Ok(_) => Err(bad("FLAC decoder gave an unexpected sample format")),
                    Err(e) => Err(bad(format!("FLAC: {e}"))),
                }
            }));
            match decoded {
                Ok(v) => out.extend(v?),
                Err(_) => {
                    // Start again from a fresh decoder: the old one stopped
                    // halfway through a frame.
                    self.decoder = make_flac(&self.params)?;
                    return Err(bad("damaged FLAC frame"));
                }
            }
        }
        Ok(out)
    }
}

fn make_flac(params: &CodecParameters) -> Result<Box<dyn Decoder>> {
    symphonia::default::get_codecs()
        .make(params, &DecoderOptions::default())
        .map_err(|e| bad(format!("FLAC: {e}")))
}

/// The STREAMINFO body in a codec header: `fLaC`, then the STREAMINFO
/// metadata block (its 4-byte header, then 34 bytes). A bare 34-byte body
/// after the marker is taken too.
fn streaminfo(header: &[u8]) -> Result<&[u8]> {
    let rest = header
        .strip_prefix(b"fLaC")
        .ok_or_else(|| bad("FLAC codec header does not start with fLaC"))?;
    match rest {
        [kind, 0, 0, 34, body @ ..] if kind & 0x7F == 0 && body.len() >= 34 => Ok(&body[..34]),
        body if body.len() == 34 => Ok(body),
        _ => Err(bad("FLAC codec header has no STREAMINFO block")),
    }
}

/// Sample rate, channels, bits per sample and the largest block size.
fn parse_streaminfo(b: &[u8]) -> Result<(u32, u16, u16, u32)> {
    let max_block = u32::from(u16::from_be_bytes([b[2], b[3]]));
    let packed = u64::from_be_bytes([b[10], b[11], b[12], b[13], b[14], b[15], b[16], b[17]]);
    let rate = (packed >> 44) as u32;
    let channels = ((packed >> 41) & 0x7) as u16 + 1;
    let bits = ((packed >> 36) & 0x1F) as u16 + 1;
    if max_block < 16 {
        return Err(bad("FLAC STREAMINFO block size below 16"));
    }
    Ok((rate, channels, bits, max_block))
}

/// What a FLAC frame header says.
struct FrameHeader {
    block: u32,
    channels: u8,
}

/// Parse and check (CRC-8) the frame header at the start of `b`.
fn frame_header(b: &[u8]) -> Option<FrameHeader> {
    if b.len() < 6 || b[0] != 0xFF || b[1] & 0xFE != 0xF8 || b[3] & 1 != 0 {
        return None;
    }
    let block_code = b[2] >> 4;
    let rate_code = b[2] & 0xF;
    let assignment = b[3] >> 4;
    if block_code == 0 || rate_code == 15 || assignment > 10 || (b[3] >> 1) & 7 == 3 {
        return None;
    }
    // The coded frame or sample number: UTF-8 style, 1 to 7 bytes.
    let lead = b[4];
    let extra = match lead.leading_ones() {
        0 => 0,
        n @ 2..=7 => n as usize - 1,
        _ => return None,
    };
    let mut at = 5 + extra;
    if b.len() < at || b[5..at].iter().any(|c| c & 0xC0 != 0x80) {
        return None;
    }
    let mut take = |n: usize| -> Option<u32> {
        let v = b
            .get(at..at + n)?
            .iter()
            .fold(0, |a, &c| (a << 8) | u32::from(c));
        at += n;
        Some(v)
    };
    let block = match block_code {
        1 => 192,
        2..=5 => 576 << (block_code - 2),
        6 => take(1)? + 1,
        7 => take(2)? + 1,
        _ => 256 << (block_code - 8),
    };
    match rate_code {
        12 => {
            take(1)?;
        }
        13 | 14 => {
            take(2)?;
        }
        _ => {}
    }
    if *b.get(at)? != crc8(&b[..at]) {
        return None;
    }
    Some(FrameHeader {
        block,
        channels: if assignment < 8 { assignment + 1 } else { 2 },
    })
}

/// Length of the FLAC frame at the start of `b`: up to the next frame
/// header such that the bytes before it end in their own CRC-16, or the
/// whole rest.
fn frame_len(b: &[u8]) -> Result<usize> {
    if frame_header(b).is_none() {
        return Err(bad("FLAC chunk does not start at a frame"));
    }
    let mut crc = 0u16;
    for (i, &byte) in b.iter().enumerate() {
        // The CRC-16 over a whole frame, its own CRC included, is 0.
        if i > 2 && crc == 0 && byte == 0xFF && frame_header(&b[i..]).is_some() {
            return Ok(i);
        }
        crc = crc16_step(crc, byte);
    }
    if crc == 0 {
        Ok(b.len())
    } else {
        Err(bad("damaged or truncated FLAC frame"))
    }
}

fn crc8(data: &[u8]) -> u8 {
    data.iter().fold(0u8, |mut c, &b| {
        c ^= b;
        for _ in 0..8 {
            c = if c & 0x80 != 0 {
                (c << 1) ^ 0x07
            } else {
                c << 1
            };
        }
        c
    })
}

fn crc16_step(mut c: u16, b: u8) -> u16 {
    c ^= u16::from(b) << 8;
    for _ in 0..8 {
        c = if c & 0x8000 != 0 {
            (c << 1) ^ 0x8005
        } else {
            c << 1
        };
    }
    c
}

#[cfg(test)]
mod tests {
    use super::*;

    fn le16(s: &[i16]) -> Vec<u8> {
        s.iter().flat_map(|x| x.to_le_bytes()).collect()
    }

    fn le24(s: &[i32]) -> Vec<u8> {
        s.iter()
            .flat_map(|x| x.to_le_bytes()[..3].to_vec())
            .collect()
    }

    #[test]
    fn pcm_16_bit_stereo_and_mono() {
        let mut d = ChunkDecoder::new(Codec::Pcm, 48_000, 2, 16, None).unwrap();
        assert_eq!(
            d.decode(&le16(&[1, -2, 300, -400])).unwrap(),
            [1, -2, 300, -400]
        );
        let mut d = ChunkDecoder::new(Codec::Pcm, 44_100, 1, 16, None).unwrap();
        assert_eq!(d.sample_rate(), 44_100);
        assert_eq!(d.decode(&le16(&[5, -6])).unwrap(), [5, 5, -6, -6]);
        assert!(d.decode(&[]).unwrap().is_empty());
    }

    #[test]
    fn pcm_24_bit_rounds_to_16() {
        let mut d = ChunkDecoder::new(Codec::Pcm, 48_000, 2, 24, None).unwrap();
        let s = [0x17F, 0x180, -0x180, -0x181, 0x7F_FFFF, -0x80_0000];
        // 0x17F rounds down to 1, 0x180 (a half) up to 2, and full scale
        // saturates instead of wrapping.
        assert_eq!(
            d.decode(&le24(&s)).unwrap(),
            [1, 2, -1, -2, i16::MAX, i16::MIN]
        );
    }

    #[test]
    fn pcm_torn_frames_and_bad_formats_are_errors() {
        let mut d = ChunkDecoder::new(Codec::Pcm, 48_000, 2, 16, None).unwrap();
        assert!(d.decode(&[1, 2, 3]).is_err());
        let mut d = ChunkDecoder::new(Codec::Pcm, 48_000, 1, 24, None).unwrap();
        assert!(d.decode(&[1, 2, 3, 4]).is_err());
        for (ch, bits) in [(0, 16), (3, 16), (2, 8), (2, 32)] {
            assert!(ChunkDecoder::new(Codec::Pcm, 48_000, ch, bits, None).is_err());
        }
        assert!(ChunkDecoder::new(Codec::Flac, 48_000, 2, 16, None).is_err());
    }

    // -- FLAC, written by hand: verbatim subframes need no encoder ----------

    fn streaminfo_block(rate: u32, channels: u16, bits: u16, max_block: u16) -> Vec<u8> {
        let mut v = b"fLaC".to_vec();
        v.extend_from_slice(&[0x80, 0, 0, 34]);
        v.extend_from_slice(&16u16.to_be_bytes());
        v.extend_from_slice(&max_block.to_be_bytes());
        v.extend_from_slice(&[0; 6]);
        let packed =
            (u64::from(rate) << 44) | (u64::from(channels - 1) << 41) | (u64::from(bits - 1) << 36);
        v.extend_from_slice(&packed.to_be_bytes());
        v.extend_from_slice(&[0; 16]);
        v
    }

    /// One frame: explicit 16-bit block size, independent channels,
    /// verbatim subframes (`frames` are interleaved samples).
    fn verbatim_frame(n: u8, channels: usize, bits: u16, frames: &[i32]) -> Vec<u8> {
        let block = frames.len() / channels;
        let size_code = if bits == 16 { 4 } else { 6 };
        let mut f = vec![
            0xFF,
            0xF8,
            0x70, // explicit 16-bit block size, rate from STREAMINFO
            ((channels as u8 - 1) << 4) | (size_code << 1),
            n,
        ];
        f.extend_from_slice(&((block - 1) as u16).to_be_bytes());
        f.push(crc8(&f));
        for c in 0..channels {
            f.push(0x02); // verbatim, no wasted bits
            for s in frames.iter().skip(c).step_by(channels) {
                let b = s.to_be_bytes();
                f.extend_from_slice(if bits == 16 { &b[2..] } else { &b[1..] });
            }
        }
        let crc = f.iter().fold(0, |c, &b| crc16_step(c, b));
        f.extend_from_slice(&crc.to_be_bytes());
        f
    }

    #[test]
    fn flac_chunks_of_several_frames_decode_in_order() {
        let header = streaminfo_block(48_000, 2, 16, 4096);
        let mut d = ChunkDecoder::new(Codec::Flac, 48_000, 2, 16, Some(header)).unwrap();
        let a: Vec<i32> = (0..200).map(|i| i * 7 - 600).collect();
        let b: Vec<i32> = (0..96).map(|i| 1000 - i * 3).collect();
        let mut chunk = verbatim_frame(0, 2, 16, &a);
        chunk.extend(verbatim_frame(1, 2, 16, &b));
        let want: Vec<i16> = a.iter().chain(&b).map(|&s| s as i16).collect();
        assert_eq!(d.decode(&chunk).unwrap(), want);
        // The next chunk decodes on its own.
        assert_eq!(
            d.decode(&verbatim_frame(2, 2, 16, &b)).unwrap()[..2],
            [1000, 997]
        );
    }

    #[test]
    fn flac_mono_24_bit_is_duplicated_and_rounded() {
        let header = streaminfo_block(44_100, 1, 24, 1024);
        let mut d = ChunkDecoder::new(Codec::Flac, 44_100, 1, 24, Some(header)).unwrap();
        let s = [0x17F, 0x180, -0x181, 0x7F_FFFF, 0, 256, -256, 1];
        let got = d.decode(&verbatim_frame(0, 1, 24, &s)).unwrap();
        assert_eq!(
            got,
            [
                1,
                1,
                2,
                2,
                -2,
                -2,
                i16::MAX,
                i16::MAX,
                0,
                0,
                1,
                1,
                -1,
                -1,
                0,
                0
            ]
        );
    }

    #[test]
    fn flac_header_must_match_the_stream() {
        let header = streaminfo_block(48_000, 2, 16, 4096);
        for (rate, ch, bits) in [(44_100, 2, 16), (48_000, 1, 16), (48_000, 2, 24)] {
            assert!(ChunkDecoder::new(Codec::Flac, rate, ch, bits, Some(header.clone())).is_err());
        }
        assert!(ChunkDecoder::new(Codec::Flac, 48_000, 2, 16, Some(b"OggS".to_vec())).is_err());
        assert!(
            ChunkDecoder::new(Codec::Flac, 48_000, 2, 16, Some(header[..20].to_vec())).is_err()
        );
        // The bare STREAMINFO body after the marker is accepted too.
        let bare = [&header[..4], &header[8..]].concat();
        assert!(ChunkDecoder::new(Codec::Flac, 48_000, 2, 16, Some(bare)).is_ok());
    }

    #[test]
    fn damaged_flac_is_an_error_not_a_panic() {
        let header = streaminfo_block(48_000, 2, 16, 256);
        let mut d = ChunkDecoder::new(Codec::Flac, 48_000, 2, 16, Some(header)).unwrap();
        let samples: Vec<i32> = (0..400).map(|i| i * 11).collect();
        let good = verbatim_frame(0, 2, 16, &samples);
        // A flipped bit, a cut, garbage, a frame beyond STREAMINFO's block
        // size, a mono frame in a stereo stream.
        let mut flipped = good.clone();
        flipped[40] ^= 0x10;
        let big: Vec<i32> = (0..600).collect();
        let mono: Vec<i32> = (0..100).collect();
        for bad in [
            flipped,
            good[..good.len() - 3].to_vec(),
            vec![0xFF, 0xF8, 1, 2, 3],
            b"definitely not FLAC".to_vec(),
            verbatim_frame(0, 2, 16, &big),
            verbatim_frame(0, 1, 16, &mono),
        ] {
            assert!(d.decode(&bad).is_err());
        }
        // Every truncation and every single-byte corruption fails cleanly.
        for cut in 0..good.len() {
            let _ = d.decode(&good[..cut]);
        }
        for i in 0..good.len() {
            for bit in [0x01, 0x80] {
                let mut b = good.clone();
                b[i] ^= bit;
                let _ = d.decode(&b);
            }
        }
        // And the decoder still works afterwards.
        let want: Vec<i16> = samples.iter().map(|&s| s as i16).collect();
        assert_eq!(d.decode(&good).unwrap(), want);
    }

    // -- FLAC from a real encoder ------------------------------------------

    /// `testdata/sine-48k-stereo.flac`: 3456 frames of the signal below,
    /// encoded by the reference encoder (flac 1.5.0) in three 1152-sample
    /// frames, with LPC and stereo decorrelation. Made with:
    ///
    /// ```text
    /// python3 -c 'import math,struct,sys; sys.stdout.buffer.write(b"".join(
    ///   struct.pack("<hh", round(12000*math.sin(2*math.pi*997*i/48000)),
    ///   round(9000*math.sin(2*math.pi*313*i/48000+1))) for i in range(3456)))' > s.raw
    /// flac --force-raw-format --endian=little --sign=signed --channels=2 \
    ///   --bps=16 --sample-rate=48000 --blocksize=1152 -8 --no-padding \
    ///   --no-seektable -o sine-48k-stereo.flac s.raw
    /// ```
    const FIXTURE: &[u8] = include_bytes!("../testdata/sine-48k-stereo.flac");

    fn fixture_signal() -> Vec<i16> {
        (0..3456)
            .flat_map(|i| {
                let t = f64::from(i) / 48_000.0;
                let tau = std::f64::consts::TAU;
                [
                    (12_000.0 * (tau * 997.0 * t).sin()).round() as i16,
                    (9_000.0 * (tau * 313.0 * t + 1.0).sin()).round() as i16,
                ]
            })
            .collect()
    }

    /// The file split as Sendspin sends it: the header up to the first
    /// frame, then the frames.
    fn fixture_parts() -> (Vec<u8>, Vec<u8>) {
        // fLaC, then metadata blocks; the last one has its top bit set.
        let mut at = 4;
        loop {
            let last = FIXTURE[at] & 0x80 != 0;
            let len = (usize::from(FIXTURE[at + 1]) << 16)
                | (usize::from(FIXTURE[at + 2]) << 8)
                | usize::from(FIXTURE[at + 3]);
            at += 4 + len;
            if last {
                break;
            }
        }
        (FIXTURE[..at].to_vec(), FIXTURE[at..].to_vec())
    }

    #[test]
    fn flac_from_the_reference_encoder_decodes_exactly() {
        let (header, frames) = fixture_parts();
        let mut d = ChunkDecoder::new(Codec::Flac, 48_000, 2, 16, Some(header)).unwrap();
        // All three frames in one chunk.
        let got = d.decode(&frames).unwrap();
        let want = fixture_signal();
        assert_eq!(got.len(), want.len());
        for (i, (g, w)) in got.iter().zip(&want).enumerate() {
            // The test regenerates the signal; libm may round a sample
            // differently from Python's.
            assert!(
                (i32::from(*g) - i32::from(*w)).abs() <= 1,
                "sample {i}: {g} vs {w}"
            );
        }
        // Frame by frame, as separate chunks, gives the same audio.
        let mut split = Vec::new();
        let mut rest = &frames[..];
        while !rest.is_empty() {
            let n = frame_len(rest).unwrap();
            split.extend(d.decode(&rest[..n]).unwrap());
            rest = &rest[n..];
        }
        assert_eq!(split, got);
    }
}
