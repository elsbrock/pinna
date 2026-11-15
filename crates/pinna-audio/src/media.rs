//! Media lane: streaming decode of a URL or file into the speaker's mixer.
//!
//! Threading: an async *pump* task reads the HTTP body into a small bounded
//! channel; a dedicated *decoder thread* pulls from it through a blocking
//! `Read`, decodes with symphonia, converts to 48 kHz stereo and pushes
//! 20 ms chunks into a second bounded channel (~1.5 s). The speaker actor
//! drains that channel from its feeder, so a paused lane simply stops
//! draining and backpressure reaches the socket. Dropping a [`Decoder`]
//! closes both channels and aborts the pump: the connection and the thread
//! go away promptly.

use crate::decode::{OUT_RATE, StreamResampler};
use crate::mix::{FADE, Ramp};
use crate::speaker::Cmd;
use crate::{Error, Result};
use bytes::Bytes;
use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, sync_channel};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::{MediaSource, MediaSourceStream, ReadOnlySource};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;

/// Frames per decoded chunk handed to the feeder (20 ms).
const CHUNK_FRAMES: usize = 960;
/// Decoded chunks buffered between decoder and feeder (1.5 s).
const BUFFER_CHUNKS: usize = 75;
/// Body chunks buffered between the HTTP pump and the decoder.
const NET_CHUNKS: usize = 16;
/// Consecutive undecodable packets tolerated before giving up.
const MAX_BAD_PACKETS: u32 = 1000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaPhase {
    Idle,
    /// Opening the stream, or starved mid-stream.
    Buffering,
    Playing,
    Paused,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaState {
    pub phase: MediaPhase,
    pub url: Option<String>,
    pub muted: bool,
    /// Why the last media ended, if it ended badly. Cleared by the next play.
    pub error: Option<String>,
}

impl Default for MediaState {
    fn default() -> Self {
        MediaState {
            phase: MediaPhase::Idle,
            url: None,
            muted: false,
            error: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Decoder thread

pub(crate) enum Item {
    /// Interleaved 48 kHz stereo, up to [`CHUNK_FRAMES`] frames.
    Data(Vec<i16>),
    /// End of stream, with the reason if it failed.
    End(Option<String>),
}

/// A running decode of one stream. Dropping it tears everything down.
pub(crate) struct Decoder {
    rx: Receiver<Item>,
    cancel: Arc<AtomicBool>,
    pump: Option<JoinHandle<()>>,
    /// Held by the decoder thread for its whole life.
    #[cfg_attr(not(test), allow(dead_code))]
    alive: Arc<()>,
}

impl Drop for Decoder {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        if let Some(p) = self.pump.take() {
            p.abort();
        }
    }
}

impl Decoder {
    /// Start decoding `source` on a new thread. The receiver resolves when the
    /// first audio is ready, or with the probe/decode error.
    fn start(
        source: Box<dyn MediaSource>,
        hint: Hint,
        pump: Option<JoinHandle<()>>,
    ) -> Result<(Decoder, oneshot::Receiver<std::result::Result<(), String>>)> {
        let (tx, rx) = sync_channel(BUFFER_CHUNKS);
        let (ready_tx, ready_rx) = oneshot::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let alive = Arc::new(());
        let decoder = Decoder {
            rx,
            cancel: cancel.clone(),
            pump,
            alive: alive.clone(),
        };
        std::thread::Builder::new()
            .name("pinna-media-decode".into())
            .stack_size(512 * 1024)
            .spawn(move || {
                let _alive = alive;
                run_decoder(source, hint, tx, cancel, ready_tx);
            })
            .map_err(|e| Error::Other(format!("spawn decoder: {e}")))?;
        Ok((decoder, ready_rx))
    }

    #[cfg(test)]
    pub(crate) fn detached(rx: Receiver<Item>) -> Decoder {
        Decoder {
            rx,
            cancel: Arc::new(AtomicBool::new(false)),
            pump: None,
            alive: Arc::new(()),
        }
    }
}

/// Chunks decoded audio into the output channel.
struct Out {
    tx: SyncSender<Item>,
    cancel: Arc<AtomicBool>,
    ready: Option<oneshot::Sender<std::result::Result<(), String>>>,
    buf: Vec<i16>,
    sent: bool,
}

impl Out {
    /// `false` once the consumer is gone or the decode was cancelled.
    fn push(&mut self, samples: &[i16]) -> bool {
        self.buf.extend_from_slice(samples);
        let n = CHUNK_FRAMES * 2;
        let mut done = 0;
        while self.buf.len() - done >= n {
            if !self.send(self.buf[done..done + n].to_vec()) {
                return false;
            }
            done += n;
        }
        self.buf.drain(..done);
        true
    }

    fn flush(&mut self) -> bool {
        if self.buf.is_empty() {
            return true;
        }
        let rest = std::mem::take(&mut self.buf);
        self.send(rest)
    }

    fn send(&mut self, chunk: Vec<i16>) -> bool {
        if self.cancel.load(Ordering::Relaxed) || self.tx.send(Item::Data(chunk)).is_err() {
            return false;
        }
        self.sent = true;
        if let Some(r) = self.ready.take() {
            let _ = r.send(Ok(()));
        }
        true
    }
}

fn run_decoder(
    source: Box<dyn MediaSource>,
    hint: Hint,
    tx: SyncSender<Item>,
    cancel: Arc<AtomicBool>,
    ready: oneshot::Sender<std::result::Result<(), String>>,
) {
    let mut out = Out {
        tx: tx.clone(),
        cancel,
        ready: Some(ready),
        buf: Vec::new(),
        sent: false,
    };
    let result = decode_stream(source, hint, &mut out);
    let end = match result {
        Ok(()) if out.sent => None,
        Ok(()) => Some("no audio decoded".to_string()),
        Err(e) => Some(e),
    };
    if let (Some(e), Some(r)) = (&end, out.ready.take()) {
        let _ = r.send(Err(e.clone()));
    }
    if let Some(e) = &end {
        tracing::warn!("media decode ended: {e}");
    }
    let _ = tx.send(Item::End(end));
}

/// Interleaved samples of any channel layout -> interleaved stereo.
fn to_stereo(samples: &[i16], channels: usize, out: &mut Vec<i16>) {
    out.clear();
    for f in samples.chunks_exact(channels.max(1)) {
        out.push(f[0]);
        out.push(if channels > 1 { f[1] } else { f[0] });
    }
}

fn decode_stream(
    source: Box<dyn MediaSource>,
    hint: Hint,
    out: &mut Out,
) -> std::result::Result<(), String> {
    let err = |e: SymError| e.to_string();
    let mss = MediaSourceStream::new(source, Default::default());
    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(|e| format!("unrecognised audio stream: {e}"))?;
    let mut format = probed.format;
    let track = format
        .default_track()
        .ok_or_else(|| "no audio track".to_string())?;
    let track_id = track.id;
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(err)?;

    let mut buf: Option<SampleBuffer<i16>> = None;
    let mut stereo = Vec::new();
    let mut resampled = Vec::new();
    let mut resampler: Option<(u32, StreamResampler)> = None;
    let mut bad = 0u32;
    loop {
        if out.cancel.load(Ordering::Relaxed) {
            return Ok(());
        }
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(SymError::ResetRequired) => break,
            Err(SymError::DecodeError(e)) => {
                bad += 1;
                if bad > MAX_BAD_PACKETS {
                    return Err(format!("decode error: {e}"));
                }
                continue;
            }
            Err(e) => return Err(err(e)),
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            Err(SymError::DecodeError(e)) => {
                bad += 1;
                if bad > MAX_BAD_PACKETS {
                    return Err(format!("decode error: {e}"));
                }
                tracing::debug!("media decode error (skipped): {e}");
                continue;
            }
            Err(e) => return Err(err(e)),
        };
        bad = 0;
        let spec = *decoded.spec();
        if buf
            .as_ref()
            .is_none_or(|b| b.capacity() < decoded.capacity() * spec.channels.count())
        {
            buf = Some(SampleBuffer::new(decoded.capacity() as u64, spec));
        }
        let b = buf.as_mut().unwrap();
        b.copy_interleaved_ref(decoded);
        to_stereo(b.samples(), spec.channels.count(), &mut stereo);
        let ok = if spec.rate == OUT_RATE {
            out.push(&stereo)
        } else {
            if resampler.as_ref().is_none_or(|(r, _)| *r != spec.rate) {
                if let Some((_, mut old)) = resampler.take() {
                    resampled.clear();
                    old.finish(&mut resampled);
                    if !out.push(&resampled) {
                        return Ok(());
                    }
                }
                resampler = Some((spec.rate, StreamResampler::new(spec.rate, OUT_RATE)));
            }
            resampled.clear();
            resampler.as_mut().unwrap().1.push(&stereo, &mut resampled);
            out.push(&resampled)
        };
        if !ok {
            return Ok(());
        }
    }
    if let Some((_, mut r)) = resampler {
        resampled.clear();
        r.finish(&mut resampled);
        if !out.push(&resampled) {
            return Ok(());
        }
    }
    out.flush();
    Ok(())
}

// ---------------------------------------------------------------------------
// Sources

/// Blocking `Read` over the pump's channel.
struct ChanReader {
    rx: mpsc::Receiver<std::io::Result<Bytes>>,
    cur: Bytes,
}

impl Read for ChanReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        while self.cur.is_empty() {
            match self.rx.blocking_recv() {
                Some(Ok(b)) => self.cur = b,
                Some(Err(e)) => return Err(e),
                None => return Ok(0),
            }
        }
        let n = self.cur.len().min(buf.len());
        buf[..n].copy_from_slice(&self.cur[..n]);
        bytes::Buf::advance(&mut self.cur, n);
        Ok(n)
    }
}

async fn pump(mut resp: reqwest::Response, tx: mpsc::Sender<std::io::Result<Bytes>>) {
    loop {
        let chunk = tokio::select! {
            c = resp.chunk() => c,
            _ = tx.closed() => return,
        };
        match chunk {
            Ok(Some(b)) => {
                if tx.send(Ok(b)).await.is_err() {
                    return;
                }
            }
            Ok(None) => return,
            Err(e) => {
                let _ = tx.send(Err(std::io::Error::other(e))).await;
                return;
            }
        }
    }
}

fn extension_of(url: &str) -> Option<&str> {
    let path = url.split(['?', '#']).next()?;
    let name = path.rsplit('/').next()?;
    let (_, ext) = name.rsplit_once('.')?;
    (!ext.is_empty() && ext.len() <= 5).then_some(ext)
}

fn is_http(url: &str) -> bool {
    let u = url.get(..8).unwrap_or(url).to_ascii_lowercase();
    u.starts_with("http://") || u.starts_with("https://")
}

async fn open(
    client: &reqwest::Client,
    url: &str,
) -> Result<(Decoder, oneshot::Receiver<std::result::Result<(), String>>)> {
    let mut hint = Hint::new();
    if let Some(ext) = extension_of(url) {
        hint.with_extension(ext);
    }
    if is_http(url) {
        let resp = client
            .get(url)
            .send()
            .await
            .map_err(|e| Error::Other(format!("open {url}: {e}")))?;
        if !resp.status().is_success() {
            return Err(Error::Other(format!("open {url}: HTTP {}", resp.status())));
        }
        if let Some(ct) = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
        {
            let mime = ct
                .split(';')
                .next()
                .unwrap_or(ct)
                .trim()
                .to_ascii_lowercase();
            let ext = match mime.as_str() {
                "audio/mpeg" | "audio/mp3" => Some("mp3"),
                "audio/flac" | "audio/x-flac" => Some("flac"),
                "audio/wav" | "audio/x-wav" | "audio/wave" | "audio/vnd.wave" => Some("wav"),
                "audio/ogg" | "application/ogg" | "audio/vorbis" => Some("ogg"),
                _ => None,
            };
            if let Some(ext) = ext {
                hint.with_extension(ext);
            }
            hint.mime_type(&mime);
        }
        let (tx, rx) = mpsc::channel(NET_CHUNKS);
        let pump = tokio::spawn(pump(resp, tx));
        let reader = ChanReader {
            rx,
            cur: Bytes::new(),
        };
        Decoder::start(Box::new(ReadOnlySource::new(reader)), hint, Some(pump))
    } else {
        let path = url.strip_prefix("file://").unwrap_or(url);
        if path.contains("://") {
            return Err(Error::Other(format!("unsupported URL scheme: {url}")));
        }
        let file = tokio::fs::File::open(path)
            .await
            .map_err(|e| Error::Other(format!("open {path}: {e}")))?
            .into_std()
            .await;
        Decoder::start(Box::new(file), hint, None)
    }
}

// ---------------------------------------------------------------------------
// Public handle

struct Shared {
    tx: mpsc::UnboundedSender<Cmd>,
    /// Generation of the latest play/stop; stale completions are ignored.
    gen_: Mutex<u64>,
    client: OnceLock<reqwest::Client>,
}

impl Shared {
    /// Bump the generation and send the command built from it, in order.
    fn send_gen(&self, mk: impl FnOnce(u64) -> Cmd) -> u64 {
        let mut g = self.gen_.lock().unwrap();
        *g += 1;
        let _ = self.tx.send(mk(*g));
        *g
    }

    fn current(&self) -> u64 {
        *self.gen_.lock().unwrap()
    }

    fn client(&self) -> &reqwest::Client {
        self.client.get_or_init(|| {
            reqwest::Client::builder()
                .user_agent("pinna")
                .connect_timeout(Duration::from_secs(10))
                .read_timeout(Duration::from_secs(20))
                .build()
                .expect("reqwest client")
        })
    }
}

/// Control of the speaker's media lane. Cheap to clone.
#[derive(Clone)]
pub struct MediaHandle {
    shared: Arc<Shared>,
    state: watch::Receiver<MediaState>,
}

impl MediaHandle {
    pub(crate) fn new(
        tx: mpsc::UnboundedSender<Cmd>,
        state: watch::Receiver<MediaState>,
    ) -> MediaHandle {
        MediaHandle {
            shared: Arc::new(Shared {
                tx,
                gen_: Mutex::new(0),
                client: OnceLock::new(),
            }),
            state,
        }
    }

    /// Stream `url` (http, https, `file://` or a plain path), replacing any
    /// current media. Resolves once audio is decoding, or with the
    /// open/probe error (the state then reads `Idle` with `error` set).
    pub async fn play_url(&self, url: &str) -> Result<()> {
        let gen_ = self.shared.send_gen(|gen_| Cmd::MediaBegin {
            gen_,
            url: url.to_string(),
        });
        let fail = |msg: String| {
            let _ = self.shared.tx.send(Cmd::MediaFail {
                gen_,
                msg: msg.clone(),
            });
            Error::Other(msg)
        };
        let (decoder, ready) = match open(self.shared.client(), url).await {
            Ok(v) => v,
            Err(e) => return Err(fail(e.to_string())),
        };
        match ready.await {
            Ok(Ok(())) => {}
            Ok(Err(msg)) => return Err(fail(msg)),
            Err(_) => return Err(fail("decoder stopped".into())),
        }
        if self.shared.current() != gen_ {
            return Err(Error::Other("media replaced".into()));
        }
        self.shared
            .tx
            .send(Cmd::MediaAttach { gen_, decoder })
            .map_err(|_| Error::Other("speaker task ended".into()))
    }

    pub fn pause(&self) {
        let _ = self.shared.tx.send(Cmd::MediaPause);
    }

    pub fn resume(&self) {
        let _ = self.shared.tx.send(Cmd::MediaResume);
    }

    /// Drop the current media (and any play still opening).
    pub fn stop(&self) {
        self.shared.send_gen(|gen_| Cmd::MediaStop { gen_ });
    }

    /// Keep playing but silently; survives track changes.
    pub fn set_muted(&self, muted: bool) {
        let _ = self.shared.tx.send(Cmd::MediaMute(muted));
    }

    pub fn state(&self) -> watch::Receiver<MediaState> {
        self.state.clone()
    }
}

// ---------------------------------------------------------------------------
// Feeder-side lane state

pub(crate) struct MediaLane {
    decoder: Option<Decoder>,
    cur: Vec<i16>,
    pos: usize,
    gen_: u64,
    /// A play is opening (no decoder attached yet).
    pending: bool,
    url: Option<String>,
    paused: bool,
    muted: bool,
    starved: bool,
    error: Option<String>,
    /// Pause fade; samples keep being consumed until it reaches silence.
    play: Ramp,
    mute: Ramp,
}

impl Default for MediaLane {
    fn default() -> Self {
        MediaLane {
            decoder: None,
            cur: Vec::new(),
            pos: 0,
            gen_: 0,
            pending: false,
            url: None,
            paused: false,
            muted: false,
            starved: false,
            error: None,
            play: Ramp::new(1.0, FADE),
            mute: Ramp::new(1.0, FADE),
        }
    }
}

impl MediaLane {
    pub fn phase(&self) -> MediaPhase {
        if self.decoder.is_some() {
            if self.paused {
                MediaPhase::Paused
            } else if self.starved {
                MediaPhase::Buffering
            } else {
                MediaPhase::Playing
            }
        } else if self.pending {
            MediaPhase::Buffering
        } else {
            MediaPhase::Idle
        }
    }

    #[cfg(test)]
    pub fn position_for_test(&self) -> (usize, usize) {
        (self.cur.len(), self.pos)
    }

    /// Whether the speaker session must be open for this lane.
    pub fn wants_session(&self) -> bool {
        self.pending || (self.decoder.is_some() && !self.paused)
    }

    /// Copy the observable state into `cur`; true if anything changed.
    pub fn publish(&self, cur: &mut MediaState) -> bool {
        let phase = self.phase();
        if cur.phase == phase
            && cur.muted == self.muted
            && cur.url == self.url
            && cur.error == self.error
        {
            return false;
        }
        cur.phase = phase;
        cur.muted = self.muted;
        cur.url.clone_from(&self.url);
        cur.error.clone_from(&self.error);
        true
    }

    fn clear(&mut self) {
        self.decoder = None;
        self.cur.clear();
        self.pos = 0;
        self.pending = false;
        self.url = None;
        self.paused = false;
        self.starved = false;
        self.play.jump(1.0);
    }

    pub fn begin(&mut self, gen_: u64, url: String) {
        self.clear();
        self.gen_ = gen_;
        self.pending = true;
        self.url = Some(url);
        self.error = None;
    }

    pub fn attach(&mut self, gen_: u64, decoder: Decoder) {
        if gen_ != self.gen_ || !self.pending {
            return; // superseded: dropping the decoder tears it down
        }
        self.pending = false;
        self.decoder = Some(decoder);
        self.play.jump(if self.paused { 0.0 } else { 1.0 });
        self.mute.jump(if self.muted { 0.0 } else { 1.0 });
    }

    pub fn open_failed(&mut self, gen_: u64, msg: String) {
        if gen_ == self.gen_ && self.pending {
            self.clear();
            self.error = Some(msg);
        }
    }

    /// Playback broke under us (e.g. the speaker died).
    pub fn fail(&mut self, msg: &str) {
        if self.pending || self.decoder.is_some() {
            self.clear();
            self.error = Some(msg.to_string());
        }
    }

    pub fn stop(&mut self, gen_: u64) {
        self.clear();
        self.gen_ = gen_;
        self.error = None;
    }

    pub fn pause(&mut self) {
        if self.pending || self.decoder.is_some() {
            self.paused = true;
            self.play.set(0.0);
        }
    }

    pub fn resume(&mut self) {
        if self.paused {
            self.paused = false;
            self.play.set(1.0);
        }
    }

    pub fn set_muted(&mut self, muted: bool) {
        self.muted = muted;
        self.mute.set(if muted { 0.0 } else { 1.0 });
    }

    /// Next stereo frame with pause/mute gain applied (silence if none).
    pub fn pull(&mut self) -> (f32, f32) {
        let mg = self.mute.tick();
        let pg = self.play.tick();
        if self.decoder.is_none() || (self.paused && pg <= 0.0) {
            return (0.0, 0.0);
        }
        if self.pos + 1 >= self.cur.len() && !self.refill() {
            return (0.0, 0.0);
        }
        let g = mg * pg;
        let l = f32::from(self.cur[self.pos]) * g;
        let r = f32::from(self.cur[self.pos + 1]) * g;
        self.pos += 2;
        (l, r)
    }

    /// Fetch the next decoded chunk; false if none is available right now.
    fn refill(&mut self) -> bool {
        let Some(d) = &self.decoder else {
            return false;
        };
        loop {
            match d.rx.try_recv() {
                Ok(Item::Data(v)) if v.len() >= 2 => {
                    self.cur = v;
                    self.pos = 0;
                    self.starved = false;
                    return true;
                }
                Ok(Item::Data(_)) => {}
                Ok(Item::End(err)) => {
                    self.finish(err);
                    return false;
                }
                Err(TryRecvError::Empty) => {
                    self.starved = true;
                    return false;
                }
                Err(TryRecvError::Disconnected) => {
                    self.finish(Some("decoder stopped unexpectedly".into()));
                    return false;
                }
            }
        }
    }

    fn finish(&mut self, err: Option<String>) {
        self.clear();
        self.error = err;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Backend;
    use crate::speaker::{Speaker, SpeakerConfig};
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::AtomicUsize;
    use std::time::Instant;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    // -- fixtures ----------------------------------------------------------

    fn sine(rate: u32, ch: u16, millis: u32, amp: f64) -> Vec<i16> {
        let n = (u64::from(rate) * u64::from(millis) / 1000) as usize;
        let mut v = Vec::new();
        for i in 0..n {
            let s = (amp * (2.0 * std::f64::consts::PI * 440.0 * i as f64 / f64::from(rate)).sin())
                as i16;
            for _ in 0..ch {
                v.push(s);
            }
        }
        v
    }

    fn wav_header(rate: u32, ch: u16, data_len: u32) -> Vec<u8> {
        let mut v = b"RIFF".to_vec();
        v.extend_from_slice(&data_len.saturating_add(36).to_le_bytes());
        v.extend_from_slice(b"WAVEfmt ");
        v.extend_from_slice(&16u32.to_le_bytes());
        v.extend_from_slice(&1u16.to_le_bytes());
        v.extend_from_slice(&ch.to_le_bytes());
        v.extend_from_slice(&rate.to_le_bytes());
        v.extend_from_slice(&(rate * u32::from(ch) * 2).to_le_bytes());
        v.extend_from_slice(&(ch * 2).to_le_bytes());
        v.extend_from_slice(&16u16.to_le_bytes());
        v.extend_from_slice(b"data");
        v.extend_from_slice(&data_len.to_le_bytes());
        v
    }

    fn pcm_bytes(s: &[i16]) -> Vec<u8> {
        s.iter().flat_map(|x| x.to_le_bytes()).collect()
    }

    fn wav(rate: u32, ch: u16, samples: &[i16]) -> Vec<u8> {
        let mut v = wav_header(rate, ch, (samples.len() * 2) as u32);
        v.extend(pcm_bytes(samples));
        v
    }

    fn crc8(data: &[u8]) -> u8 {
        let mut c = 0u8;
        for &b in data {
            c ^= b;
            for _ in 0..8 {
                c = if c & 0x80 != 0 {
                    (c << 1) ^ 0x07
                } else {
                    c << 1
                };
            }
        }
        c
    }

    fn crc16(data: &[u8]) -> u16 {
        let mut c = 0u16;
        for &b in data {
            c ^= u16::from(b) << 8;
            for _ in 0..8 {
                c = if c & 0x8000 != 0 {
                    (c << 1) ^ 0x8005
                } else {
                    c << 1
                };
            }
        }
        c
    }

    /// Mono 16-bit 44.1 kHz FLAC with verbatim subframes (no encoder needed).
    fn flac_44k_mono(samples: &[i16]) -> Vec<u8> {
        let mut v = b"fLaC".to_vec();
        v.extend_from_slice(&[0x80, 0, 0, 34]);
        v.extend_from_slice(&4096u16.to_be_bytes());
        v.extend_from_slice(&4096u16.to_be_bytes());
        v.extend_from_slice(&[0; 6]);
        let packed: u64 = (44100u64 << 44) | (15u64 << 36) | samples.len() as u64;
        v.extend_from_slice(&packed.to_be_bytes());
        v.extend_from_slice(&[0; 16]);
        for (n, block) in samples.chunks(4096).enumerate() {
            assert!(n < 128);
            let mut f = vec![0xFF, 0xF8, 0x79, 0x08, n as u8];
            f.extend_from_slice(&((block.len() - 1) as u16).to_be_bytes());
            f.push(crc8(&f));
            f.push(0x02);
            for s in block {
                f.extend_from_slice(&s.to_be_bytes());
            }
            let c = crc16(&f);
            f.extend_from_slice(&c.to_be_bytes());
            v.extend(f);
        }
        v
    }

    /// Silent MPEG-1 Layer III, 128 kbit/s, 44.1 kHz: 417-byte zero frames.
    fn silent_mp3(frames: usize) -> Vec<u8> {
        let mut v = Vec::new();
        for _ in 0..frames {
            let mut f = vec![0u8; 417];
            f[..4].copy_from_slice(&[0xFF, 0xFB, 0x90, 0x00]);
            v.extend(f);
        }
        v
    }

    // -- server ------------------------------------------------------------

    #[derive(Default)]
    struct Stats {
        /// Connections whose peer went away while we were writing.
        closed: AtomicUsize,
        requests: AtomicUsize,
    }

    struct Server {
        base: String,
        stats: Arc<Stats>,
    }

    impl Server {
        fn url(&self, path: &str) -> String {
            format!("{}{path}", self.base)
        }
    }

    /// `files`: path -> (content type, body). `/live.wav` is an endless
    /// 48 kHz stereo sine, paced in real time after a 1 s head start.
    async fn serve(files: Vec<(&'static str, &'static str, Vec<u8>)>) -> Server {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let stats = Arc::new(Stats::default());
        let files: Arc<HashMap<_, _>> =
            Arc::new(files.into_iter().map(|(p, c, b)| (p, (c, b))).collect());
        let st = stats.clone();
        tokio::spawn(async move {
            loop {
                let Ok((sock, _)) = listener.accept().await else {
                    return;
                };
                tokio::spawn(handle_conn(sock, files.clone(), st.clone()));
            }
        });
        Server { base, stats }
    }

    async fn handle_conn(
        mut s: TcpStream,
        files: Arc<HashMap<&'static str, (&'static str, Vec<u8>)>>,
        stats: Arc<Stats>,
    ) {
        let mut req = Vec::new();
        let mut b = [0u8; 1024];
        while !req.windows(4).any(|w| w == b"\r\n\r\n") {
            match s.read(&mut b).await {
                Ok(0) | Err(_) => return,
                Ok(n) => req.extend_from_slice(&b[..n]),
            }
        }
        stats.requests.fetch_add(1, Ordering::Relaxed);
        let text = String::from_utf8_lossy(&req).to_string();
        let path = text.split_whitespace().nth(1).unwrap_or("/").to_string();
        let ok = if path == "/live.wav" {
            let head = b"HTTP/1.1 200 OK\r\nContent-Type: audio/wav\r\nConnection: close\r\n\r\n";
            let mut out = head.to_vec();
            out.extend(wav_header(48000, 2, u32::MAX));
            let tone = sine(48000, 2, 100, 8000.0);
            let mut sent = 0;
            let mut ok = s.write_all(&out).await.is_ok();
            while ok {
                ok = s.write_all(&pcm_bytes(&tone)).await.is_ok();
                sent += 1;
                if sent > 10 {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
            ok
        } else if let Some((ct, body)) = files.get(path.as_str()) {
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: {ct}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            s.write_all(head.as_bytes()).await.is_ok() && s.write_all(body).await.is_ok()
        } else {
            let _ = s
                .write_all(
                    b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await;
            true
        };
        if !ok {
            stats.closed.fetch_add(1, Ordering::Relaxed);
        }
    }

    // -- speaker with a capturing player -----------------------------------

    struct Rig {
        speaker: Speaker,
        sink: PathBuf,
        dir: PathBuf,
    }

    impl Drop for Rig {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn rig(idle: Duration) -> Rig {
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "pinna-media-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let sink = dir.join("sink.raw");
        // One shared script (written once: exec-ing a file another test thread
        // still holds open for writing fails with ETXTBSY). It appends stdin to
        // the path given as `--target`, the argument before the final `-`.
        static PLAYER: OnceLock<PathBuf> = OnceLock::new();
        let player = PLAYER.get_or_init(|| {
            let p = std::env::temp_dir().join(format!("pinna-media-player-{}", std::process::id()));
            std::fs::write(
                &p,
                "#!/bin/sh\nfor a; do prev=$last; last=$a; done\ncat >> \"$prev\"\n",
            )
            .unwrap();
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
            p
        });
        let mut cfg = SpeakerConfig::new(Backend::PipeWire);
        cfg.pw_play_bin = player.to_string_lossy().into();
        cfg.pw_target = Some(sink.to_string_lossy().into());
        cfg.idle_timeout = idle;
        cfg.initial_volume = 100;
        Rig {
            speaker: Speaker::new(cfg),
            sink,
            dir,
        }
    }

    /// Frames in the sink with any non-zero sample.
    fn sounding_frames(sink: &Path) -> usize {
        let b = std::fs::read(sink).unwrap_or_default();
        b.chunks_exact(4)
            .filter(|f| f.iter().any(|&x| x != 0))
            .count()
    }

    async fn wait_phase(h: &MediaHandle, want: MediaPhase, within: Duration) {
        let mut st = h.state();
        let r = tokio::time::timeout(within, async {
            while st.borrow().phase != want {
                st.changed().await.unwrap();
            }
        })
        .await;
        assert!(r.is_ok(), "never reached {want:?}; at {:?}", *st.borrow());
    }

    async fn until(within: Duration, mut f: impl FnMut() -> bool) -> bool {
        let t0 = Instant::now();
        while t0.elapsed() < within {
            if f() {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        false
    }

    // -- tests -------------------------------------------------------------

    async fn plays_to_end(
        path: &'static str,
        ctype: &'static str,
        body: Vec<u8>,
        min_frames: usize,
    ) {
        let srv = serve(vec![(path, ctype, body)]).await;
        let rig = rig(Duration::from_secs(5));
        let m = rig.speaker.media();
        m.play_url(&srv.url(path)).await.unwrap();
        wait_phase(&m, MediaPhase::Playing, Duration::from_secs(2)).await;
        assert_eq!(
            m.state().borrow().url.as_deref(),
            Some(srv.url(path).as_str())
        );
        wait_phase(&m, MediaPhase::Idle, Duration::from_secs(6)).await;
        let st = m.state().borrow().clone();
        assert_eq!(st.error, None);
        let got = sounding_frames(&rig.sink);
        assert!(got >= min_frames, "only {got} sounding frames");
    }

    #[tokio::test]
    async fn streams_wav_resampled_and_upmixed_then_idles() {
        // 22.05 kHz mono, 1.2 s -> ~57600 frames at 48 kHz stereo.
        let body = wav(22050, 1, &sine(22050, 1, 1200, 12000.0));
        plays_to_end("/a.wav", "audio/wav", body, 50_000).await;
    }

    #[tokio::test]
    async fn streams_flac() {
        let body = flac_44k_mono(&sine(44100, 1, 1000, 12000.0));
        plays_to_end("/a.flac", "audio/flac", body, 44_000).await;
    }

    #[tokio::test]
    async fn streams_mp3() {
        // Silent frames: only checks probe, decode, resample and the end.
        let srv = serve(vec![("/a.mp3", "audio/mpeg", silent_mp3(40))]).await;
        let rig = rig(Duration::from_secs(5));
        let m = rig.speaker.media();
        m.play_url(&srv.url("/a.mp3")).await.unwrap();
        wait_phase(&m, MediaPhase::Idle, Duration::from_secs(6)).await;
        assert_eq!(m.state().borrow().error, None);
        let sunk = std::fs::read(&rig.sink).unwrap().len() / 4;
        assert!(sunk > 40_000, "{sunk}");
    }

    #[tokio::test]
    async fn content_type_hint_without_extension() {
        let body = wav(48000, 2, &sine(48000, 2, 300, 9000.0));
        let srv = serve(vec![("/stream", "audio/x-wav; charset=binary", body)]).await;
        let rig = rig(Duration::from_secs(5));
        let m = rig.speaker.media();
        m.play_url(&srv.url("/stream")).await.unwrap();
        wait_phase(&m, MediaPhase::Idle, Duration::from_secs(4)).await;
        assert_eq!(m.state().borrow().error, None);
        assert!(sounding_frames(&rig.sink) > 10_000);
    }

    #[tokio::test]
    async fn plays_files_and_file_urls() {
        let rig = rig(Duration::from_secs(5));
        let f = rig.dir.join("clip.wav");
        std::fs::write(&f, wav(48000, 2, &sine(48000, 2, 300, 9000.0))).unwrap();
        let m = rig.speaker.media();
        for url in [
            f.to_string_lossy().to_string(),
            format!("file://{}", f.display()),
        ] {
            m.play_url(&url).await.unwrap();
            wait_phase(&m, MediaPhase::Idle, Duration::from_secs(3)).await;
            assert_eq!(m.state().borrow().error, None, "{url}");
        }
        assert!(sounding_frames(&rig.sink) > 20_000);
    }

    #[tokio::test]
    async fn spectrum_moves_while_music_sounds_then_goes_quiet() {
        let rig = rig(Duration::from_secs(5));
        let f = rig.dir.join("clip.wav");
        std::fs::write(&f, wav(48000, 2, &sine(48000, 2, 1500, 9000.0))).unwrap();
        let spectrum = rig.speaker.spectrum();
        let m = rig.speaker.media();
        m.play_url(&f.to_string_lossy()).await.unwrap();
        let lit = || {
            spectrum
                .at(Instant::now())
                .is_some_and(|b| b.iter().any(|&l| l > 0.5))
        };
        assert!(until(Duration::from_secs(2), lit).await, "never lit");
        wait_phase(&m, MediaPhase::Idle, Duration::from_secs(3)).await;
        assert!(
            until(Duration::from_secs(1), || spectrum
                .at(Instant::now())
                .is_none())
            .await,
            "still showing after the music ended"
        );
    }

    #[tokio::test]
    async fn pause_stops_audio_and_resume_continues() {
        let srv = serve(vec![]).await;
        let rig = rig(Duration::from_secs(5));
        let m = rig.speaker.media();
        m.play_url(&srv.url("/live.wav")).await.unwrap();
        assert!(until(Duration::from_secs(2), || sounding_frames(&rig.sink) > 4800).await);
        m.pause();
        wait_phase(&m, MediaPhase::Paused, Duration::from_secs(1)).await;
        // Let what is already queued towards the player drain.
        tokio::time::sleep(Duration::from_millis(400)).await;
        let frozen = sounding_frames(&rig.sink);
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert_eq!(sounding_frames(&rig.sink), frozen);
        assert!(rig.speaker.is_session_open());
        m.resume();
        wait_phase(&m, MediaPhase::Playing, Duration::from_secs(1)).await;
        assert!(
            until(Duration::from_secs(2), || sounding_frames(&rig.sink)
                > frozen + 4800)
            .await
        );
        m.stop();
    }

    #[tokio::test]
    async fn paused_session_closes_after_idle_and_resume_reopens() {
        let srv = serve(vec![]).await;
        let rig = rig(Duration::from_millis(400));
        let m = rig.speaker.media();
        m.play_url(&srv.url("/live.wav")).await.unwrap();
        assert!(until(Duration::from_secs(2), || rig.speaker.is_session_open()).await);
        m.pause();
        assert!(until(Duration::from_secs(3), || !rig.speaker.is_session_open()).await);
        m.resume();
        assert!(until(Duration::from_secs(3), || rig.speaker.is_session_open()).await);
        wait_phase(&m, MediaPhase::Playing, Duration::from_secs(1)).await;
        m.stop();
    }

    #[tokio::test]
    async fn stop_tears_down_the_connection() {
        let srv = serve(vec![]).await;
        let rig = rig(Duration::from_secs(5));
        let m = rig.speaker.media();
        m.play_url(&srv.url("/live.wav")).await.unwrap();
        wait_phase(&m, MediaPhase::Playing, Duration::from_secs(2)).await;
        assert_eq!(srv.stats.closed.load(Ordering::Relaxed), 0);
        m.stop();
        wait_phase(&m, MediaPhase::Idle, Duration::from_secs(1)).await;
        assert!(
            until(Duration::from_secs(3), || srv
                .stats
                .closed
                .load(Ordering::Relaxed)
                == 1)
            .await,
            "server never saw the connection close"
        );
        let st = m.state().borrow().clone();
        assert_eq!((st.url, st.error), (None, None));
    }

    #[tokio::test]
    async fn replacing_media_closes_the_old_stream() {
        let srv = serve(vec![]).await;
        let rig = rig(Duration::from_secs(5));
        let m = rig.speaker.media();
        m.play_url(&srv.url("/live.wav")).await.unwrap();
        m.play_url(&srv.url("/live.wav")).await.unwrap();
        assert!(
            until(Duration::from_secs(3), || srv
                .stats
                .closed
                .load(Ordering::Relaxed)
                == 1)
            .await
        );
        assert_eq!(srv.stats.requests.load(Ordering::Relaxed), 2);
        wait_phase(&m, MediaPhase::Playing, Duration::from_secs(1)).await;
        m.stop();
        assert!(
            until(Duration::from_secs(3), || srv
                .stats
                .closed
                .load(Ordering::Relaxed)
                == 2)
            .await
        );
    }

    #[tokio::test]
    async fn decoder_thread_exits_when_dropped() {
        let srv = serve(vec![]).await;
        let client = reqwest::Client::new();
        let (d, ready) = open(&client, &srv.url("/live.wav")).await.unwrap();
        ready.await.unwrap().unwrap();
        let alive = Arc::downgrade(&d.alive);
        assert!(alive.strong_count() > 0);
        drop(d);
        assert!(
            until(Duration::from_secs(3), || alive.strong_count() == 0).await,
            "decoder thread still running"
        );
    }

    #[tokio::test]
    async fn decoder_thread_exits_when_the_network_stalls() {
        // A server that sends a WAV header then goes silent: the decoder is
        // blocked in a read, and dropping it must still end the thread.
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/x.wav", l.local_addr().unwrap());
        tokio::spawn(async move {
            let (mut s, _) = l.accept().await.unwrap();
            let mut b = [0u8; 1024];
            let _ = s.read(&mut b).await;
            let mut out = b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n".to_vec();
            out.extend(wav_header(48000, 2, u32::MAX));
            let _ = s.write_all(&out).await;
            tokio::time::sleep(Duration::from_secs(60)).await;
        });
        let client = reqwest::Client::new();
        let (d, ready) = open(&client, &url).await.unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        let alive = Arc::downgrade(&d.alive);
        drop(ready);
        drop(d);
        assert!(until(Duration::from_secs(3), || alive.strong_count() == 0).await);
    }

    #[tokio::test]
    async fn http_404_and_garbage_end_idle_with_error() {
        let srv = serve(vec![("/junk.mp3", "audio/mpeg", vec![0x5A; 20_000])]).await;
        let rig = rig(Duration::from_secs(5));
        let m = rig.speaker.media();
        let e = m.play_url(&srv.url("/missing.mp3")).await.unwrap_err();
        assert!(e.to_string().contains("404"), "{e}");
        wait_phase(&m, MediaPhase::Idle, Duration::from_secs(1)).await;
        let st = m.state().borrow().clone();
        assert!(st.error.as_deref().unwrap().contains("404"));

        let e = m.play_url(&srv.url("/junk.mp3")).await.unwrap_err();
        let want = Some(e.to_string());
        assert!(until(Duration::from_secs(1), || m.state().borrow().error == want).await);
        let st = m.state().borrow().clone();
        assert_eq!(st.phase, MediaPhase::Idle);
        assert_eq!(st.error.as_deref(), Some(e.to_string().as_str()));
        assert_eq!(st.url, None);

        let e = m.play_url("/definitely/not/here.wav").await.unwrap_err();
        assert!(e.to_string().contains("open"), "{e}");
        let e = m.play_url("ftp://example.invalid/x.mp3").await.unwrap_err();
        assert!(e.to_string().contains("scheme"), "{e}");
    }

    #[tokio::test]
    async fn voice_lane_works_after_media_failure() {
        let srv = serve(vec![]).await;
        let rig = rig(Duration::from_secs(5));
        assert!(
            rig.speaker
                .media()
                .play_url(&srv.url("/nope.mp3"))
                .await
                .is_err()
        );
        rig.speaker
            .play_pcm(crate::tone::beep(440.0, 100, 0.5))
            .await
            .unwrap();
        assert!(sounding_frames(&rig.sink) > 2000);
    }

    #[tokio::test]
    async fn mute_survives_new_media_and_state_reports_it() {
        let srv = serve(vec![]).await;
        let rig = rig(Duration::from_secs(5));
        let m = rig.speaker.media();
        m.set_muted(true);
        let mut st = m.state();
        tokio::time::timeout(Duration::from_secs(1), async {
            while !st.borrow().muted {
                st.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
        m.play_url(&srv.url("/live.wav")).await.unwrap();
        wait_phase(&m, MediaPhase::Playing, Duration::from_secs(1)).await;
        assert!(m.state().borrow().muted);
        tokio::time::sleep(Duration::from_millis(600)).await;
        assert_eq!(sounding_frames(&rig.sink), 0);
        m.set_muted(false);
        assert!(until(Duration::from_secs(2), || sounding_frames(&rig.sink) > 2000).await);
        m.stop();
    }

    #[test]
    fn extension_parsing() {
        assert_eq!(extension_of("http://h/a/b.mp3?x=1.wav"), Some("mp3"));
        assert_eq!(extension_of("http://h/stream"), None);
        assert_eq!(extension_of("/tmp/x.FLAC"), Some("FLAC"));
        assert!(is_http("HTTPS://x") && !is_http("file:///x") && !is_http("/x"));
    }
}
