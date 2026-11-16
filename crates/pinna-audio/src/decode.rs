//! Decoding of encoded audio (wav, mp3, flac, ogg/vorbis) into PCM, and
//! conversion to the 48 kHz stereo the speaker kernel path requires.

use crate::{Error, Result};
use std::io::Cursor;
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

pub const OUT_RATE: u32 = 48_000;

/// Interleaved signed 16-bit PCM.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pcm {
    pub rate: u32,
    pub channels: u16,
    pub samples: Vec<i16>,
}

impl Pcm {
    pub fn frames(&self) -> usize {
        self.samples.len() / usize::from(self.channels.max(1))
    }

    pub fn duration(&self) -> std::time::Duration {
        std::time::Duration::from_secs_f64(self.frames() as f64 / f64::from(self.rate.max(1)))
    }

    /// Convert to 48 kHz interleaved stereo (mono is duplicated, channels
    /// beyond the first two are dropped).
    pub fn to_48k_stereo(&self) -> Pcm {
        let ch = usize::from(self.channels.max(1));
        let frames = self.frames();
        let mut left = Vec::with_capacity(frames);
        let mut right = Vec::with_capacity(frames);
        for f in self.samples.chunks_exact(ch) {
            left.push(f[0]);
            right.push(if ch > 1 { f[1] } else { f[0] });
        }
        let (l, r) = if self.rate == OUT_RATE {
            (left, right)
        } else {
            let l = resample(&left, self.rate, OUT_RATE);
            let r = if ch > 1 {
                resample(&right, self.rate, OUT_RATE)
            } else {
                l.clone()
            };
            (l, r)
        };
        let mut samples = Vec::with_capacity(l.len() * 2);
        for (a, b) in l.iter().zip(&r) {
            samples.push(*a);
            samples.push(*b);
        }
        Pcm {
            rate: OUT_RATE,
            channels: 2,
            samples,
        }
    }
}

/// Decode a complete encoded file.
pub fn decode(bytes: &[u8]) -> Result<Pcm> {
    if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WAVE" {
        return decode_wav(bytes);
    }
    decode_symphonia(bytes)
}

fn decode_symphonia(bytes: &[u8]) -> Result<Pcm> {
    let err = |e: SymError| Error::Decode(e.to_string());
    let mss = MediaSourceStream::new(Box::new(Cursor::new(bytes.to_vec())), Default::default());
    let probed = symphonia::default::get_probe()
        .format(
            &Hint::new(),
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(err)?;
    let mut format = probed.format;
    let track = format
        .default_track()
        .ok_or_else(|| Error::Decode("no audio track".into()))?;
    let track_id = track.id;
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(err)?;

    let mut rate = track.codec_params.sample_rate.unwrap_or(0);
    let mut channels = track
        .codec_params
        .channels
        .map(|c| c.count() as u16)
        .unwrap_or(0);
    let mut samples = Vec::new();
    let mut buf: Option<SampleBuffer<i16>> = None;
    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(SymError::ResetRequired) => break,
            Err(e) => return Err(err(e)),
        };
        if packet.track_id() != track_id {
            continue;
        }
        match decoder.decode(&packet) {
            Ok(decoded) => {
                let spec = *decoded.spec();
                rate = spec.rate;
                channels = spec.channels.count() as u16;
                if buf
                    .as_ref()
                    .is_none_or(|b| b.capacity() < decoded.capacity() * spec.channels.count())
                {
                    buf = Some(SampleBuffer::new(decoded.capacity() as u64, spec));
                }
                let b = buf.as_mut().unwrap();
                b.copy_interleaved_ref(decoded);
                samples.extend_from_slice(b.samples());
            }
            // A corrupt packet: skip it and keep going.
            Err(SymError::DecodeError(e)) => tracing::warn!("decode error (skipped): {e}"),
            Err(e) => return Err(err(e)),
        }
    }
    if rate == 0 || channels == 0 || samples.is_empty() {
        return Err(Error::Decode("no audio decoded".into()));
    }
    Ok(Pcm {
        rate,
        channels,
        samples,
    })
}

/// WAV parser tolerant of streamed files: ffmpeg writes 0xFFFFFFFF (or 0)
/// for the RIFF and data sizes when it cannot seek. PCM 8/16/24/32-bit and
/// float32/64, including WAVE_FORMAT_EXTENSIBLE.
fn decode_wav(b: &[u8]) -> Result<Pcm> {
    let bad = |m: &str| Error::Decode(format!("wav: {m}"));
    let u16le = |o: usize| u16::from_le_bytes([b[o], b[o + 1]]);
    let u32le = |o: usize| u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);

    let mut pos = 12;
    let mut fmt: Option<(u16, u16, u32, u16)> = None; // tag, channels, rate, bits
    while pos + 8 <= b.len() {
        let id = &b[pos..pos + 4];
        let size = u32le(pos + 4) as usize;
        let body = pos + 8;
        if id == b"fmt " {
            if body + 16 > b.len() {
                return Err(bad("truncated fmt chunk"));
            }
            let mut tag = u16le(body);
            if tag == 0xFFFE && size >= 26 && body + 26 <= b.len() {
                tag = u16le(body + 24); // sub-format GUID starts with the tag
            }
            fmt = Some((tag, u16le(body + 2), u32le(body + 4), u16le(body + 14)));
        } else if id == b"data" {
            let (tag, channels, rate, bits) = fmt.ok_or_else(|| bad("data before fmt"))?;
            let avail = b.len() - body;
            // Unknown (0xFFFFFFFF / 0) or oversized: read to the end.
            let len = if size == 0 || size == 0xFFFF_FFFF || size > avail {
                avail
            } else {
                size
            };
            let data = &b[body..body + len];
            return wav_samples(tag, channels, rate, bits, data);
        }
        // Chunks are word-aligned; an unknown-size chunk can't be skipped.
        if size == 0xFFFF_FFFF {
            break;
        }
        pos = body + size + (size & 1);
    }
    Err(bad("no data chunk"))
}

fn wav_samples(tag: u16, channels: u16, rate: u32, bits: u16, data: &[u8]) -> Result<Pcm> {
    if channels == 0 || rate == 0 {
        return Err(Error::Decode("wav: bad format".into()));
    }
    let conv = |v: f64| (v.clamp(-1.0, 1.0) * 32767.0).round() as i16;
    let samples: Vec<i16> = match (tag, bits) {
        (1, 8) => data.iter().map(|&x| (i16::from(x) - 128) << 8).collect(),
        (1, 16) => data
            .chunks_exact(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]))
            .collect(),
        (1, 24) => data
            .chunks_exact(3)
            .map(|c| i16::from_le_bytes([c[1], c[2]]))
            .collect(),
        (1, 32) => data
            .chunks_exact(4)
            .map(|c| i16::from_le_bytes([c[2], c[3]]))
            .collect(),
        (3, 32) => data
            .chunks_exact(4)
            .map(|c| conv(f64::from(f32::from_le_bytes([c[0], c[1], c[2], c[3]]))))
            .collect(),
        (3, 64) => data
            .chunks_exact(8)
            .map(|c| conv(f64::from_le_bytes(c.try_into().unwrap())))
            .collect(),
        _ => {
            return Err(Error::Decode(format!(
                "wav: unsupported format {tag}/{bits}-bit"
            )));
        }
    };
    let ch = usize::from(channels);
    let whole = samples.len() / ch * ch;
    let mut samples = samples;
    samples.truncate(whole);
    if samples.is_empty() {
        return Err(Error::Decode("wav: no samples".into()));
    }
    Ok(Pcm {
        rate,
        channels,
        samples,
    })
}

// ---------------------------------------------------------------------------
// Resampler: windowed-sinc (Kaiser), table driven.

const ZEROS: f64 = 16.0; // sinc zero crossings per side at unity cutoff
const TABLE_RES: usize = 512; // table entries per input sample
const KAISER_BETA: f64 = 8.0;

fn bessel_i0(x: f64) -> f64 {
    let mut sum = 1.0;
    let mut term = 1.0;
    let q = x * x / 4.0;
    for k in 1..50 {
        term *= q / (f64::from(k) * f64::from(k));
        sum += term;
        if term < sum * 1e-14 {
            break;
        }
    }
    sum
}

/// Kernel over u in [-ZEROS, ZEROS]: sinc(u) * kaiser(u / ZEROS).
fn kernel_table() -> Vec<f32> {
    let n = (ZEROS as usize) * TABLE_RES;
    let denom = bessel_i0(KAISER_BETA);
    (0..=n)
        .map(|i| {
            let u = i as f64 / TABLE_RES as f64; // 0..ZEROS
            let sinc = if u == 0.0 {
                1.0
            } else {
                (std::f64::consts::PI * u).sin() / (std::f64::consts::PI * u)
            };
            let r = u / ZEROS;
            let w = bessel_i0(KAISER_BETA * (1.0 - r * r).max(0.0).sqrt()) / denom;
            (sinc * w) as f32
        })
        .collect()
}

/// Resample mono audio from `from` Hz to `to` Hz. Band-limits to the lower of
/// the two Nyquist frequencies. Output length is `len * to / from`.
pub fn resample(input: &[i16], from: u32, to: u32) -> Vec<i16> {
    if from == to || input.is_empty() {
        return input.to_vec();
    }
    let ratio = f64::from(to) / f64::from(from);
    let cutoff = ratio.min(1.0); // fraction of input Nyquist kept
    let half = ZEROS / cutoff; // kernel half-width in input samples
    let table = kernel_table();
    let out_len = (input.len() as f64 * ratio).round() as usize;
    let mut out = Vec::with_capacity(out_len);
    let n = input.len() as isize;
    for j in 0..out_len {
        let x = j as f64 / ratio; // position in input samples
        let first = (x - half).ceil() as isize;
        let last = (x + half).floor() as isize;
        let mut acc = 0.0f32;
        for k in first.max(0)..=last.min(n - 1) {
            let u = ((x - k as f64) * cutoff).abs();
            let pos = u * TABLE_RES as f64;
            let i = pos as usize;
            let frac = (pos - i as f64) as f32;
            let w = match (table.get(i), table.get(i + 1)) {
                (Some(a), Some(b)) => a + (b - a) * frac,
                _ => 0.0,
            };
            acc += w * f32::from(input[k as usize]);
        }
        out.push((acc * cutoff as f32).round().clamp(-32768.0, 32767.0) as i16);
    }
    out
}

/// Incremental stereo resampler: same kernel as [`resample`], fed in pieces.
/// Output over `push` + `finish` equals a one-shot `resample` per channel.
pub(crate) struct StreamResampler {
    ratio: f64,
    cutoff: f64,
    half: f64,
    table: Vec<f32>,
    buf: Vec<[f32; 2]>,
    /// Absolute input index of `buf[0]`.
    base: i64,
    total_in: i64,
    produced: i64,
}

impl StreamResampler {
    pub fn new(from: u32, to: u32) -> StreamResampler {
        let ratio = f64::from(to) / f64::from(from);
        let cutoff = ratio.min(1.0);
        StreamResampler {
            ratio,
            cutoff,
            half: ZEROS / cutoff,
            table: kernel_table(),
            buf: Vec::new(),
            base: 0,
            total_in: 0,
            produced: 0,
        }
    }

    /// Feed interleaved stereo; appends converted interleaved stereo to `out`.
    pub fn push(&mut self, input: &[i16], out: &mut Vec<i16>) {
        for f in input.chunks_exact(2) {
            self.buf.push([f32::from(f[0]), f32::from(f[1])]);
        }
        self.total_in += (input.len() / 2) as i64;
        self.emit(false, out);
        // Drop input no later output can reach.
        let keep_from = ((self.produced as f64 / self.ratio) - self.half).ceil() as i64;
        let drop = (keep_from - self.base).clamp(0, self.buf.len() as i64) as usize;
        self.buf.drain(..drop);
        self.base += drop as i64;
    }

    /// End of stream: emit the tail.
    pub fn finish(&mut self, out: &mut Vec<i16>) {
        self.emit(true, out);
    }

    fn emit(&mut self, finish: bool, out: &mut Vec<i16>) {
        let out_len = (self.total_in as f64 * self.ratio).round() as i64;
        let avail_end = self.base + self.buf.len() as i64; // exclusive
        loop {
            let j = self.produced;
            let x = j as f64 / self.ratio;
            let first = (x - self.half).ceil() as i64;
            let last = (x + self.half).floor() as i64;
            if finish {
                if j >= out_len {
                    break;
                }
            } else if last >= avail_end {
                break;
            }
            let mut acc = [0.0f32; 2];
            for k in first.max(0)..=last.min(avail_end - 1) {
                let u = ((x - k as f64) * self.cutoff).abs();
                let pos = u * TABLE_RES as f64;
                let i = pos as usize;
                let frac = (pos - i as f64) as f32;
                let w = match (self.table.get(i), self.table.get(i + 1)) {
                    (Some(a), Some(b)) => a + (b - a) * frac,
                    _ => 0.0,
                };
                let s = self.buf[(k - self.base) as usize];
                acc[0] += w * s[0];
                acc[1] += w * s[1];
            }
            for a in acc {
                out.push((a * self.cutoff as f32).round().clamp(-32768.0, 32767.0) as i16);
            }
            self.produced += 1;
        }
    }
}

/// Streaming stereo resampler whose ratio may change between pushes: a live
/// stream follows the speaker clock (ADR 0011). Same kernel as [`resample`];
/// the cutoff is fixed by the nominal ratio, so an adjustment of a few
/// hundred ppm never moves the filter.
pub(crate) struct VarResampler {
    /// Input frames per output frame at the nominal ratio.
    nominal: f64,
    /// Input frames per output frame now.
    step: f64,
    cutoff: f64,
    half: f64,
    table: Vec<f32>,
    buf: Vec<[f32; 2]>,
    /// Position of the next output frame, in input frames from `buf[0]`.
    pos: f64,
}

impl VarResampler {
    pub fn new(from: u32, to: u32) -> VarResampler {
        let nominal = f64::from(from) / f64::from(to);
        let cutoff = (1.0 / nominal).min(1.0);
        VarResampler {
            nominal,
            step: nominal,
            cutoff,
            half: ZEROS / cutoff,
            table: kernel_table(),
            buf: Vec::new(),
            pos: 0.0,
        }
    }

    /// Run `ppm` parts per million off the nominal ratio: positive consumes
    /// the input faster, so it makes fewer output frames.
    pub fn set_adjust_ppm(&mut self, ppm: f64) {
        self.step = self.nominal * (1.0 + ppm * 1e-6);
    }

    /// Feed interleaved stereo; appends converted interleaved stereo to `out`.
    pub fn push(&mut self, input: &[i16], out: &mut Vec<i16>) {
        for f in input.chunks_exact(2) {
            self.buf.push([f32::from(f[0]), f32::from(f[1])]);
        }
        let n = self.buf.len() as i64;
        loop {
            let x = self.pos;
            let last = (x + self.half).floor() as i64;
            if last >= n {
                break;
            }
            let first = (x - self.half).ceil() as i64;
            let mut acc = [0.0f32; 2];
            for k in first.max(0)..=last {
                let u = ((x - k as f64) * self.cutoff).abs();
                let pos = u * TABLE_RES as f64;
                let i = pos as usize;
                let frac = (pos - i as f64) as f32;
                let w = match (self.table.get(i), self.table.get(i + 1)) {
                    (Some(a), Some(b)) => a + (b - a) * frac,
                    _ => 0.0,
                };
                let s = self.buf[k as usize];
                acc[0] += w * s[0];
                acc[1] += w * s[1];
            }
            for a in acc {
                out.push((a * self.cutoff as f32).round().clamp(-32768.0, 32767.0) as i16);
            }
            self.pos += self.step;
        }
        // Keep only what the next output frame still reaches.
        let drop = ((self.pos - self.half).ceil() as i64).clamp(0, n) as usize;
        self.buf.drain(..drop);
        self.pos -= drop as f64;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wav_bytes(rate: u32, channels: u16, samples: &[i16], unknown_sizes: bool) -> Vec<u8> {
        let data_len = (samples.len() * 2) as u32;
        let mut v = Vec::new();
        v.extend_from_slice(b"RIFF");
        v.extend_from_slice(
            &(if unknown_sizes {
                0xFFFF_FFFF
            } else {
                36 + data_len
            })
            .to_le_bytes(),
        );
        v.extend_from_slice(b"WAVEfmt ");
        v.extend_from_slice(&16u32.to_le_bytes());
        v.extend_from_slice(&1u16.to_le_bytes());
        v.extend_from_slice(&channels.to_le_bytes());
        v.extend_from_slice(&rate.to_le_bytes());
        v.extend_from_slice(&(rate * u32::from(channels) * 2).to_le_bytes());
        v.extend_from_slice(&(channels * 2).to_le_bytes());
        v.extend_from_slice(&16u16.to_le_bytes());
        v.extend_from_slice(b"data");
        v.extend_from_slice(&(if unknown_sizes { 0xFFFF_FFFF } else { data_len }).to_le_bytes());
        for s in samples {
            v.extend_from_slice(&s.to_le_bytes());
        }
        v
    }

    fn sine(freq: f64, rate: u32, n: usize, amp: f64) -> Vec<i16> {
        (0..n)
            .map(|i| {
                (amp * (2.0 * std::f64::consts::PI * freq * i as f64 / f64::from(rate)).sin())
                    as i16
            })
            .collect()
    }

    fn rms(x: &[i16]) -> f64 {
        (x.iter().map(|&s| f64::from(s).powi(2)).sum::<f64>() / x.len() as f64).sqrt()
    }

    #[test]
    fn wav_normal_and_ffmpeg_streamed() {
        let s = [1i16, -2, 3, -4, 5, -6];
        for unknown in [false, true] {
            let pcm = decode(&wav_bytes(24000, 2, &s, unknown)).unwrap();
            assert_eq!((pcm.rate, pcm.channels), (24000, 2));
            assert_eq!(pcm.samples, s);
        }
    }

    #[test]
    fn wav_zero_data_size_reads_to_end() {
        let mut b = wav_bytes(16000, 1, &[7, 8, 9], false);
        let at = b.len() - 6 - 4;
        b[at..at + 4].copy_from_slice(&0u32.to_le_bytes());
        assert_eq!(decode(&b).unwrap().samples, [7, 8, 9]);
    }

    #[test]
    fn wav_skips_unknown_chunks_and_odd_padding() {
        let mut b = wav_bytes(16000, 1, &[1, 2], false);
        // Insert a LIST chunk with odd size (3) + pad byte before "data".
        let data_at = b.windows(4).position(|w| w == b"data").unwrap();
        let mut extra = b"LIST".to_vec();
        extra.extend_from_slice(&3u32.to_le_bytes());
        extra.extend_from_slice(&[1, 2, 3, 0]);
        b.splice(data_at..data_at, extra);
        assert_eq!(decode(&b).unwrap().samples, [1, 2]);
    }

    #[test]
    fn wav_rejects_garbage() {
        assert!(decode(b"RIFF\0\0\0\0WAVEjunk").is_err());
        assert!(decode(b"definitely not audio at all, no way").is_err());
    }

    #[test]
    fn stream_resampler_matches_one_shot() {
        for (from, n) in [(44100u32, 5000usize), (96000, 7000), (16000, 900)] {
            let l = sine(440.0, from, n, 8000.0);
            let r = sine(1500.0, from, n, 5000.0);
            let want_l = resample(&l, from, OUT_RATE);
            let want_r = resample(&r, from, OUT_RATE);
            let mut inter = Vec::new();
            for (a, b) in l.iter().zip(&r) {
                inter.push(*a);
                inter.push(*b);
            }
            let mut rs = StreamResampler::new(from, OUT_RATE);
            let mut out = Vec::new();
            for piece in inter.chunks(2 * 333) {
                rs.push(piece, &mut out);
            }
            rs.finish(&mut out);
            assert_eq!(out.len(), want_l.len() * 2, "{from}");
            for (j, f) in out.chunks_exact(2).enumerate() {
                assert_eq!((f[0], f[1]), (want_l[j], want_r[j]), "{from} frame {j}");
            }
        }
    }

    #[test]
    fn var_resampler_at_nominal_matches_one_shot() {
        for from in [44100u32, 48000] {
            let l = sine(440.0, from, 5000, 8000.0);
            let r = sine(1500.0, from, 5000, 5000.0);
            let (want_l, want_r) = if from == OUT_RATE {
                (l.clone(), r.clone())
            } else {
                (resample(&l, from, OUT_RATE), resample(&r, from, OUT_RATE))
            };
            let inter: Vec<i16> = l.iter().zip(&r).flat_map(|(a, b)| [*a, *b]).collect();
            let mut rs = VarResampler::new(from, OUT_RATE);
            let mut out = Vec::new();
            for piece in inter.chunks(2 * 333) {
                rs.push(piece, &mut out);
            }
            // Only the tail the kernel has not reached yet is missing.
            assert!(out.len() / 2 + 40 >= want_l.len(), "{from}: {}", out.len());
            for (j, f) in out.chunks_exact(2).enumerate() {
                let d = (i32::from(f[0]) - i32::from(want_l[j])).abs()
                    + (i32::from(f[1]) - i32::from(want_r[j])).abs();
                assert!(
                    d <= 2,
                    "{from} frame {j}: {f:?} vs {}/{}",
                    want_l[j],
                    want_r[j]
                );
            }
        }
    }

    #[test]
    fn var_resampler_adjustment_changes_the_output_rate() {
        let input = vec![1000i16; 48000 * 2 * 10];
        for (ppm, want) in [(1000.0, 480_000.0 / 1.001), (-1000.0, 480_000.0 / 0.999)] {
            let mut rs = VarResampler::new(OUT_RATE, OUT_RATE);
            rs.set_adjust_ppm(ppm);
            let mut out = Vec::new();
            for piece in input.chunks(2 * 960) {
                rs.push(piece, &mut out);
            }
            let got = (out.len() / 2) as f64;
            assert!(
                (got - want).abs() < 20.0,
                "{ppm} ppm: {got} frames, want {want}"
            );
            // DC passes at unity, away from the zero-padded start.
            assert!(out[200..].iter().all(|&s| (s - 1000).abs() <= 2));
        }
    }

    #[test]
    fn mono_duplicated_to_stereo_48k() {
        let pcm = Pcm {
            rate: 48000,
            channels: 1,
            samples: vec![1, 2, 3],
        };
        let out = pcm.to_48k_stereo();
        assert_eq!((out.rate, out.channels), (48000, 2));
        assert_eq!(out.samples, [1, 1, 2, 2, 3, 3]);
    }

    #[test]
    fn stereo_extra_channels_dropped() {
        let pcm = Pcm {
            rate: 48000,
            channels: 3,
            samples: vec![1, 2, 3, 4, 5, 6],
        };
        assert_eq!(pcm.to_48k_stereo().samples, [1, 2, 4, 5]);
    }

    #[test]
    fn resample_length_and_tone_preserved() {
        for from in [16000u32, 22050, 24000, 44100] {
            let input = sine(1000.0, from, from as usize, 10000.0);
            let out = resample(&input, from, 48000);
            assert_eq!(out.len(), 48000, "from {from}");
            // Compare the middle (avoid edge transients) against RMS of a pure tone.
            let mid = &out[8000..40000];
            let r = rms(mid);
            assert!(
                (r - 10000.0 / 2f64.sqrt()).abs() < 150.0,
                "from {from}: rms {r}"
            );
        }
    }

    #[test]
    fn resample_rejects_aliasing_when_downsampling() {
        // 20 kHz tone at 48k, down to 16k (Nyquist 8k): must be attenuated.
        let input = sine(20000.0, 48000, 48000, 10000.0);
        let out = resample(&input, 48000, 16000);
        assert!(rms(&out[2000..14000]) < 100.0);
        // An in-band tone passes.
        let input = sine(1000.0, 48000, 48000, 10000.0);
        let out = resample(&input, 48000, 16000);
        assert!((rms(&out[2000..14000]) - 7071.0).abs() < 150.0);
    }

    #[test]
    fn resample_dc_gain_is_unity() {
        let out = resample(&vec![1000i16; 4000], 22050, 48000);
        for &s in &out[1000..out.len() - 1000] {
            assert!((i32::from(s) - 1000).abs() <= 3);
        }
    }

    #[test]
    fn to_48k_stereo_resamples_channels_independently() {
        let l = sine(500.0, 24000, 24000, 8000.0);
        let mut samples = Vec::new();
        for &a in &l {
            samples.push(a);
            samples.push(0);
        }
        let out = Pcm {
            rate: 24000,
            channels: 2,
            samples,
        }
        .to_48k_stereo();
        assert_eq!(out.frames(), 48000);
        let right: Vec<i16> = out.samples.iter().skip(1).step_by(2).copied().collect();
        assert_eq!(rms(&right), 0.0);
    }
}
