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
//!
//! The lane also plays *live streams*: raw s16le 48 kHz stereo written in
//! real time to a Unix socket ([`MediaHandle::serve_stream`], ADR 0011). A
//! connection is one session: its first data replaces whatever the lane was
//! playing; EOF or [`STREAM_IDLE`] without data ends it; stop and pause close
//! it. A live source runs on the sender's clock, so its queue is kept short
//! (see [`LiveQueue`]) instead of being pulled ahead like a decode.
//!
//! The same decode feeds the speaker's *voice* lane ([`open_voice`]): replies
//! and announcements sound as they arrive, and a stream that runs dry waits
//! in silence instead of skipping speech (see [`VoiceSource`]).

use crate::decode::{OUT_RATE, StreamResampler};
use crate::mix::{FADE, Ramp};
use crate::speaker::Cmd;
use crate::{Error, Result};
use bytes::Bytes;
use std::collections::VecDeque;
use std::io::Read;
use std::net::Shutdown;
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::os::unix::net::UnixStream;
use std::path::Path;
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
/// Live stream: chunks queued before playback starts, or restarts after
/// running dry (100 ms of jitter buffer).
const LIVE_TARGET_CHUNKS: usize = 5;
/// Live stream: beyond this many chunks (~250 ms) the oldest audio is dropped
/// back to [`LIVE_TARGET_CHUNKS`], so clock drift or the backlog while the
/// speaker opens cannot grow the latency.
const LIVE_MAX_CHUNKS: usize = 13;
/// Voice stream: chunks gathered before it starts sounding (100 ms), then
/// before each restart after running dry (500 ms, then 1.5 s: a source that
/// fell behind once tends to again, and one longer pause beats many).
const VOICE_TARGETS: [usize; 3] = [5, 25, BUFFER_CHUNKS];
/// A stream session ends after this long without data.
pub const STREAM_IDLE: Duration = Duration::from_secs(2);
/// [`MediaState::url`] while a stream plays.
pub const STREAM_URL: &str = "stream:";

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

/// Where the lane's audio comes from.
enum Src {
    /// Decoded ahead of time (a URL or file).
    Decoded(Receiver<Item>),
    /// A live stream from the stream socket.
    Live(Arc<LiveQueue>),
}

/// A running decode of one stream. Dropping it tears everything down.
pub(crate) struct Decoder {
    src: Src,
    cancel: Arc<AtomicBool>,
    pump: Option<JoinHandle<()>>,
    /// A live stream's connection: shut down on drop, which ends the reader
    /// thread's blocking read and tells the client.
    conn: Option<UnixStream>,
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
        if let Some(c) = &self.conn {
            let _ = c.shutdown(Shutdown::Both);
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
            src: Src::Decoded(rx),
            cancel: cancel.clone(),
            pump,
            conn: None,
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
            src: Src::Decoded(rx),
            cancel: Arc::new(AtomicBool::new(false)),
            pump: None,
            conn: None,
            alive: Arc::new(()),
        }
    }

    #[cfg(test)]
    pub(crate) fn live_detached(q: Arc<LiveQueue>) -> Decoder {
        Decoder {
            src: Src::Live(q),
            cancel: Arc::new(AtomicBool::new(false)),
            pump: None,
            conn: None,
            alive: Arc::new(()),
        }
    }

    fn is_live(&self) -> bool {
        matches!(self.src, Src::Live(_))
    }
}

// ---------------------------------------------------------------------------
// Live streams

/// Chunks of a live stream between its reader thread and the feeder.
#[derive(Default)]
pub(crate) struct LiveQueue {
    inner: Mutex<LiveBuf>,
}

#[derive(Default)]
struct LiveBuf {
    /// Interleaved 48 kHz stereo, [`CHUNK_FRAMES`] frames (the last may be shorter).
    chunks: VecDeque<Vec<i16>>,
    /// The stream ended (with the reason if it failed): play out what is queued.
    end: Option<Option<String>>,
}

impl LiveQueue {
    pub(crate) fn push(&self, chunk: Vec<i16>) {
        let mut b = self.inner.lock().unwrap();
        b.chunks.push_back(chunk);
        if b.chunks.len() > LIVE_MAX_CHUNKS {
            let n = b.chunks.len() - LIVE_TARGET_CHUNKS;
            b.chunks.drain(..n);
            tracing::debug!("stream: dropped {} ms of backlog", n * 20);
        }
    }

    pub(crate) fn end(&self, err: Option<String>) {
        self.inner.lock().unwrap().end.get_or_insert(err);
    }
}

/// Fade a chunk linearly to silence across its length.
fn fade_out(chunk: &mut [i16]) {
    let n = chunk.len() / 2;
    for (i, f) in chunk.chunks_exact_mut(2).enumerate() {
        let g = (n - 1 - i) as f32 / n as f32;
        for s in f {
            *s = (f32::from(*s) * g).round() as i16;
        }
    }
}

/// Fade a chunk linearly up from silence across its length.
fn fade_in(chunk: &mut [i16]) {
    let n = chunk.len() / 2;
    for (i, f) in chunk.chunks_exact_mut(2).enumerate() {
        let g = i as f32 / n as f32;
        for s in f {
            *s = (f32::from(*s) * g).round() as i16;
        }
    }
}

fn samples_of(bytes: &[u8]) -> Vec<i16> {
    bytes
        .chunks_exact(2)
        .map(|b| i16::from_le_bytes([b[0], b[1]]))
        .collect()
}

/// One connection's reader thread: socket -> chunks -> `queue`. Hands
/// `decoder` to the lane with the first chunk; until then the connection
/// does not touch the lane.
fn read_stream(
    mut conn: UnixStream,
    queue: Arc<LiveQueue>,
    cancel: Arc<AtomicBool>,
    media: MediaHandle,
    mut decoder: Option<Decoder>,
) {
    let mut buf = vec![0u8; CHUNK_FRAMES * 4];
    let mut have = 0;
    let mut idle = false;
    let start = |decoder: &mut Option<Decoder>| {
        if let Some(d) = decoder.take() {
            tracing::info!("stream: session started");
            media.attach_stream(d);
        }
    };
    let end = loop {
        if cancel.load(Ordering::Relaxed) {
            break None;
        }
        match conn.read(&mut buf[have..]) {
            Ok(0) => {
                // Whole frames only; a torn last frame is dropped.
                let whole = have - have % 4;
                if whole > 0 {
                    queue.push(samples_of(&buf[..whole]));
                    start(&mut decoder);
                }
                break None;
            }
            Ok(n) => {
                have += n;
                if have == buf.len() {
                    queue.push(samples_of(&buf));
                    have = 0;
                    start(&mut decoder);
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                idle = true;
                break None;
            }
            Err(e) => break Some(format!("stream: {e}")),
        }
    };
    let started = decoder.is_none();
    match &end {
        Some(e) => tracing::warn!("{e}"),
        None if started && idle => tracing::info!(
            "stream: no data for {} s, session ended",
            STREAM_IDLE.as_secs()
        ),
        None if started => tracing::info!("stream: session ended"),
        None => {}
    }
    queue.end(end);
    // Tell the client now, even while the lane still plays out the queue.
    let _ = conn.shutdown(Shutdown::Both);
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

    /// The HTTP client media and voice streams are fetched with.
    pub(crate) fn client(&self) -> &reqwest::Client {
        self.shared.client()
    }

    /// Accept live streams on a Unix socket at `path` (raw s16le, 48 kHz
    /// stereo, written in real time; mode 0660). A stale socket there is
    /// replaced. Must be called inside a tokio runtime.
    pub fn serve_stream(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        match std::fs::symlink_metadata(path) {
            Ok(m) if m.file_type().is_socket() => std::fs::remove_file(path)?,
            Ok(_) => {
                return Err(Error::Other(format!(
                    "{} exists and is not a socket",
                    path.display()
                )));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        let listener = tokio::net::UnixListener::bind(path)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o660))?;
        let media = self.clone();
        tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((s, _)) => {
                        if let Err(e) = media.start_stream(s) {
                            tracing::warn!("stream: {e}");
                        }
                    }
                    Err(e) => {
                        tracing::warn!("stream accept: {e}");
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                }
            }
        });
        Ok(())
    }

    /// Start a reader thread for one connection.
    fn start_stream(&self, s: tokio::net::UnixStream) -> Result<()> {
        let conn = s.into_std()?;
        conn.set_nonblocking(false)?;
        conn.set_read_timeout(Some(STREAM_IDLE))?;
        let queue = Arc::new(LiveQueue::default());
        let cancel = Arc::new(AtomicBool::new(false));
        let alive = Arc::new(());
        let decoder = Decoder {
            src: Src::Live(queue.clone()),
            cancel: cancel.clone(),
            pump: None,
            conn: Some(conn.try_clone()?),
            alive: alive.clone(),
        };
        let media = self.clone();
        std::thread::Builder::new()
            .name("pinna-media-stream".into())
            .stack_size(256 * 1024)
            .spawn(move || {
                let _alive = alive;
                read_stream(conn, queue, cancel, media, Some(decoder));
            })
            .map_err(|e| Error::Other(format!("spawn stream reader: {e}")))?;
        Ok(())
    }

    /// Replace the current media with a live stream that has data.
    fn attach_stream(&self, decoder: Decoder) {
        let gen_ = self.shared.send_gen(|gen_| Cmd::MediaBegin {
            gen_,
            url: STREAM_URL.to_string(),
        });
        let _ = self.shared.tx.send(Cmd::MediaAttach { gen_, decoder });
    }

    /// Pause; a live stream cannot be paused (yet) and ends instead.
    pub fn pause(&self) {
        let _ = self.shared.tx.send(Cmd::MediaPause);
    }

    pub fn resume(&self) {
        let _ = self.shared.tx.send(Cmd::MediaResume);
    }

    /// Stop the current media (and any play still opening): it fades out
    /// over 20 ms, then its stream closes.
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
// Voice streams

/// What a [`VoiceSource`] has for the voice lane right now.
#[derive(Debug, PartialEq)]
pub(crate) enum Pull {
    /// The next chunk (interleaved 48 kHz stereo).
    Chunk(Vec<i16>),
    /// Nothing to sound yet: gathering before the start, or after running dry.
    Wait,
    /// The stream is over, with the reason if it failed.
    End(Option<String>),
}

/// A URL decoding for the voice lane (a reply or an announcement). Speech,
/// so nothing decoded is ever dropped:
///
/// | Event | The source |
/// | --- | --- |
/// | opened | gathers the first of [`VOICE_TARGETS`] (or the whole stream, if shorter), then starts at full level |
/// | runs dry mid-stream | the last chunk it has fades out; waits for the next, larger target, then fades in |
/// | ends | plays to the end |
/// | fails after audio | the last chunk fades out, then it ends with the error |
///
/// It holds at most the target ahead and leaves the rest in the decoder's
/// channel, so backpressure still reaches the socket.
pub(crate) struct VoiceSource {
    decoder: Decoder,
    ahead: VecDeque<Vec<i16>>,
    end: Option<Option<String>>,
    /// Waiting for the target before (re)starting.
    gathering: bool,
    /// A chunk has been handed out: a later restart fades in.
    sounded: bool,
    /// Times it ran dry; picks the target.
    dry: usize,
    /// Restarts after running dry (a dry spell at the very end is not one).
    stalls: u32,
}

impl VoiceSource {
    pub(crate) fn new(decoder: Decoder) -> VoiceSource {
        VoiceSource {
            decoder,
            ahead: VecDeque::new(),
            end: None,
            gathering: true,
            sounded: false,
            dry: 0,
            stalls: 0,
        }
    }

    fn target(&self) -> usize {
        VOICE_TARGETS[self.dry.min(VOICE_TARGETS.len() - 1)]
    }

    /// How often it fell silent mid-stream and came back.
    pub(crate) fn stalls(&self) -> u32 {
        self.stalls
    }

    fn top_up(&mut self) {
        let Src::Decoded(rx) = &self.decoder.src else {
            self.end.get_or_insert(Some("not a decoded stream".into()));
            return;
        };
        while self.end.is_none() && self.ahead.len() < self.target() {
            match rx.try_recv() {
                Ok(Item::Data(v)) if v.len() >= 2 => self.ahead.push_back(v),
                Ok(Item::Data(_)) => {}
                Ok(Item::End(err)) => self.end = Some(err),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.end = Some(Some("decoder stopped unexpectedly".into()));
                }
            }
        }
    }

    /// The next chunk to sound, or why there is none. Never blocks.
    pub(crate) fn pull(&mut self) -> Pull {
        self.top_up();
        if self.gathering && self.end.is_none() && self.ahead.len() < self.target() {
            return Pull::Wait;
        }
        let Some(mut chunk) = self.ahead.pop_front() else {
            return match &self.end {
                Some(err) => Pull::End(err.clone()),
                None => {
                    self.gathering = true;
                    Pull::Wait
                }
            };
        };
        let resuming = std::mem::replace(&mut self.gathering, false) && self.sounded;
        self.top_up();
        if self.ahead.is_empty() && !matches!(self.end, Some(None)) {
            // The last audio there is: fade it out rather than cut it off.
            fade_out(&mut chunk);
            if self.end.is_none() {
                self.gathering = true;
                self.dry += 1;
            }
        }
        if resuming {
            fade_in(&mut chunk);
            self.stalls += 1;
        }
        self.sounded = true;
        Pull::Chunk(chunk)
    }
}

/// Open `url` (http, https, `file://` or a plain path) for the voice lane.
/// Resolves once the first audio is decoded, or with the open/probe error
/// (an empty stream is an error); nothing keeps running after an error.
pub(crate) async fn open_voice(client: &reqwest::Client, url: &str) -> Result<VoiceSource> {
    let (decoder, ready) = open(client, url).await?;
    match ready.await {
        Ok(Ok(())) => Ok(VoiceSource::new(decoder)),
        Ok(Err(msg)) => Err(Error::Other(msg)),
        Err(_) => Err(Error::Other("decoder stopped".into())),
    }
}

// ---------------------------------------------------------------------------
// Feeder-side lane state

/// A source stopped or replaced mid-signal: it plays on from where it was
/// for one fade ([`FADE`]), then is dropped, which closes its stream.
struct Outgoing {
    decoder: Decoder,
    cur: Vec<i16>,
    pos: usize,
    /// May take further chunks (false if the lane was starved: whatever is
    /// queued then never started sounding).
    more: bool,
    gain: Ramp,
}

impl Outgoing {
    /// Next frame, faded; `None` once faded out or out of audio.
    fn pull(&mut self) -> Option<(f32, f32)> {
        let g = self.gain.tick();
        if g <= 0.0 {
            return None;
        }
        if self.pos + 1 >= self.cur.len() {
            self.cur = self.more.then(|| self.decoder.take_ready()).flatten()?;
            self.pos = 0;
        }
        let l = f32::from(self.cur[self.pos]) * g;
        let r = f32::from(self.cur[self.pos + 1]) * g;
        self.pos += 2;
        Some((l, r))
    }
}

impl Decoder {
    /// The next chunk if one is ready now; never waits and never ends anything.
    fn take_ready(&self) -> Option<Vec<i16>> {
        match &self.src {
            Src::Decoded(rx) => loop {
                match rx.try_recv() {
                    Ok(Item::Data(v)) if v.len() >= 2 => return Some(v),
                    Ok(Item::Data(_)) => {}
                    _ => return None,
                }
            },
            Src::Live(q) => q.inner.lock().unwrap().chunks.pop_front(),
        }
    }
}

/// The media lane's state machine, driven by the speaker actor: commands
/// from [`MediaHandle`] and the stream reader arrive as [`Cmd`]s, and
/// [`pull`](Self::pull) runs once per frame.
///
/// One rule holds for every event: the lane never cuts into a signal.
/// Whatever it stops mid-signal fades out, and whatever it joins mid-signal
/// fades in (each over [`FADE`]).
///
/// | Event | The current source |
/// | --- | --- |
/// | another source starts (URL opening, or stream data) | fades out, then dropped |
/// | stop (Home Assistant, cancel) | fades out, then dropped |
/// | pause, URL | fades to silence and stops draining; kept |
/// | pause, stream | as stop: a live sender cannot be held back |
/// | resume | fades in |
/// | mute / unmute | fades; the source keeps playing |
/// | a URL reaches its end | plays to the end, then dropped (the audio ran out) |
/// | a stream ends (EOF, read error, [`STREAM_IDLE`]) | last chunk fades out, then dropped |
/// | a stream runs dry | fades out, rebuffers 100 ms, fades in |
/// | the speaker fails | dropped at once (nothing plays any more) |
///
/// A new URL starts at full level: it begins at its own beginning. A new
/// stream fades in: it is joined mid-signal. Ducking, volume and the echo
/// reference are applied after the lane (`speaker::Feeder`).
pub(crate) struct MediaLane {
    decoder: Option<Decoder>,
    /// Stopped or replaced sources still fading out.
    outgoing: Vec<Outgoing>,
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
            outgoing: Vec::new(),
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

    /// Whether the speaker session must be open for this lane (also while a
    /// fade-out plays, so a stopped stream is always pulled and closed).
    pub fn wants_session(&self) -> bool {
        self.pending || (self.decoder.is_some() && !self.paused) || !self.outgoing.is_empty()
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

    /// Reset to idle; fades already running (`outgoing`) carry on.
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

    /// Stop the current source without cutting into it: if it is sounding it
    /// moves to `outgoing` and fades out from its current level, otherwise it
    /// is dropped now. Either way its stream closes.
    fn retire(&mut self) {
        let Some(decoder) = self.decoder.take() else {
            return;
        };
        let level = self.play.value();
        let mid_chunk = self.pos + 1 < self.cur.len();
        if level <= 0.0 || (self.starved && !mid_chunk) {
            return; // silent (paused or starved): nothing to fade
        }
        let mut gain = Ramp::new(level, FADE);
        gain.set(0.0);
        self.outgoing.push(Outgoing {
            decoder,
            cur: std::mem::take(&mut self.cur),
            pos: self.pos,
            more: !self.starved,
            gain,
        });
    }

    pub fn begin(&mut self, gen_: u64, url: String) {
        self.retire();
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
        if decoder.is_live() && self.paused {
            self.clear(); // paused while starting: a stream cannot pause
            return;
        }
        self.pending = false;
        // A live stream buffers first, then fades in (see `refill_live`).
        self.starved = decoder.is_live();
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

    /// Playback broke under us (e.g. the speaker died): nothing can sound,
    /// so everything is dropped at once.
    pub fn fail(&mut self, msg: &str) {
        self.outgoing.clear();
        if self.pending || self.decoder.is_some() {
            self.clear();
            self.error = Some(msg.to_string());
        }
    }

    pub fn stop(&mut self, gen_: u64) {
        self.retire();
        self.clear();
        self.gen_ = gen_;
        self.error = None;
    }

    pub fn pause(&mut self) {
        if self.decoder.as_ref().is_some_and(Decoder::is_live) {
            // The sender keeps sending: holding its audio back would only
            // replay it late. End the session (closes the connection).
            self.retire();
            self.clear();
            self.error = None;
            return;
        }
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

    /// Next stereo frame: the current source plus any fade-outs, with
    /// pause/mute gain applied (silence if none).
    pub fn pull(&mut self) -> (f32, f32) {
        let mg = self.mute.tick();
        let (mut l, mut r) = self.pull_current();
        self.outgoing.retain_mut(|o| match o.pull() {
            Some((ol, or)) => {
                l += ol;
                r += or;
                true
            }
            None => false, // dropping it closes its stream
        });
        (l * mg, r * mg)
    }

    fn pull_current(&mut self) -> (f32, f32) {
        let mut pg = self.play.tick();
        if self.decoder.is_none() || (self.paused && pg <= 0.0) {
            return (0.0, 0.0);
        }
        if self.pos + 1 >= self.cur.len() {
            if !self.refill() {
                return (0.0, 0.0);
            }
            pg = self.play.value(); // a live stream resuming fades in from here
        }
        let l = f32::from(self.cur[self.pos]) * pg;
        let r = f32::from(self.cur[self.pos + 1]) * pg;
        self.pos += 2;
        (l, r)
    }

    /// Fetch the next decoded chunk; false if none is available right now.
    fn refill(&mut self) -> bool {
        let rx = match self.decoder.as_ref().map(|d| &d.src) {
            None => return false,
            Some(Src::Live(q)) => {
                let q = q.clone();
                return self.refill_live(&q);
            }
            Some(Src::Decoded(rx)) => rx,
        };
        loop {
            match rx.try_recv() {
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

    /// Live stream: play only from a jitter buffer. Waits for
    /// [`LIVE_TARGET_CHUNKS`] before starting, fades in when it starts, and
    /// fades out the last chunk it has (a sender going quiet or ending)
    /// instead of cutting it off. A faded chunk always means a full rebuffer:
    /// a sender just slower than the speaker would otherwise leave the queue
    /// empty after every pop and get every chunk faded.
    fn refill_live(&mut self, q: &LiveQueue) -> bool {
        let mut b = q.inner.lock().unwrap();
        if self.starved && b.end.is_none() && b.chunks.len() < LIVE_TARGET_CHUNKS {
            return false;
        }
        let Some(mut chunk) = b.chunks.pop_front() else {
            match b.end.take() {
                Some(err) => {
                    drop(b);
                    self.finish(err);
                }
                None => self.starved = true,
            }
            return false;
        };
        let resuming = std::mem::replace(&mut self.starved, false);
        if b.chunks.is_empty() {
            fade_out(&mut chunk);
            self.starved = b.end.is_none();
        }
        drop(b);
        if resuming {
            self.play.jump(0.0);
            self.play.set(1.0);
        }
        self.cur = chunk;
        self.pos = 0;
        true
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

    // -- voice lane: streamed URLs -------------------------------------------

    /// One connection, released by the test: a 22.05 kHz mono WAV header
    /// with no data length, then each part sent on the returned channel;
    /// dropping the sender ends the body. With `content_length`, the body
    /// claims that length (so ending early is an error). `closed` is set if
    /// the peer went away while a part was written.
    async fn gated(
        content_length: Option<usize>,
    ) -> (String, mpsc::UnboundedSender<Vec<u8>>, Arc<AtomicBool>) {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/reply.wav", l.local_addr().unwrap());
        let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let closed = Arc::new(AtomicBool::new(false));
        let c = closed.clone();
        tokio::spawn(async move {
            let (mut s, _) = l.accept().await.unwrap();
            let mut req = Vec::new();
            let mut b = [0u8; 1024];
            while !req.windows(4).any(|w| w == b"\r\n\r\n") {
                match s.read(&mut b).await {
                    Ok(0) | Err(_) => return,
                    Ok(n) => req.extend_from_slice(&b[..n]),
                }
            }
            let len = content_length
                .map(|n| format!("Content-Length: {n}\r\n"))
                .unwrap_or_default();
            let mut out = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: audio/wav\r\n{len}Connection: close\r\n\r\n"
            )
            .into_bytes();
            out.extend(wav_header(22050, 1, u32::MAX));
            let mut ok = s.write_all(&out).await.is_ok();
            while ok {
                match rx.recv().await {
                    Some(part) => ok = s.write_all(&part).await.is_ok(),
                    None => break,
                }
            }
            if !ok {
                c.store(true, Ordering::Relaxed);
            }
        });
        (url, tx, closed)
    }

    fn speech(millis: u32) -> Vec<u8> {
        pcm_bytes(&sine(22050, 1, millis, 8000.0))
    }

    async fn within<T>(d: Duration, f: impl std::future::Future<Output = T>) -> T {
        tokio::time::timeout(d, f).await.expect("timed out")
    }

    #[tokio::test]
    async fn voice_url_sounds_before_the_body_ends_and_waits_through_a_stall() {
        let (url, parts, _) = gated(None).await;
        let rig = rig(Duration::from_secs(5));
        parts.send(speech(300)).unwrap();
        let mut p = within(Duration::from_secs(3), rig.speaker.play_url(&url))
            .await
            .unwrap();
        within(Duration::from_secs(3), &mut p.started)
            .await
            .unwrap();
        // The body is still open: it waits, the lane stays busy.
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert!(rig.speaker.is_playing());
        let first = sounding_frames(&rig.sink);
        assert!(first > 10_000, "only {first} frames before the stall");
        parts.send(speech(300)).unwrap();
        drop(parts);
        within(Duration::from_secs(3), p.done)
            .await
            .unwrap()
            .unwrap();
        let all = sounding_frames(&rig.sink);
        assert!(all > first + 10_000, "{first} then {all}");
    }

    #[tokio::test]
    async fn voice_url_stop_mid_stream_closes_the_connection() {
        let srv = serve(vec![]).await;
        let rig = rig(Duration::from_secs(5));
        let mut p = rig.speaker.play_url(&srv.url("/live.wav")).await.unwrap();
        within(Duration::from_secs(3), &mut p.started)
            .await
            .unwrap();
        rig.speaker.stop();
        within(Duration::from_millis(500), p.done)
            .await
            .unwrap()
            .unwrap();
        let st = srv.stats.clone();
        assert!(
            until(Duration::from_secs(3), || st.closed.load(Ordering::Relaxed)
                == 1)
            .await
        );
    }

    #[tokio::test]
    async fn voice_url_errors_queue_nothing() {
        let srv = serve(vec![
            ("/junk.mp3", "audio/mpeg", vec![0x5A; 20_000]),
            ("/empty.wav", "audio/wav", wav(22050, 1, &[])),
        ])
        .await;
        let rig = rig(Duration::from_secs(5));
        for (path, want) in [
            ("/missing.mp3", "404"),
            ("/junk.mp3", ""),
            ("/empty.wav", "no audio"),
        ] {
            let e = match rig.speaker.play_url(&srv.url(path)).await {
                Ok(_) => panic!("{path} played"),
                Err(e) => e,
            };
            assert!(e.to_string().contains(want), "{path}: {e}");
        }
        assert!(!rig.speaker.is_playing());
        assert!(!rig.speaker.is_session_open());
    }

    #[tokio::test]
    async fn voice_url_cut_short_reports_the_error_after_the_audio() {
        let (url, parts, _) = gated(Some(10_000_000)).await;
        let rig = rig(Duration::from_secs(5));
        parts.send(speech(300)).unwrap();
        let mut p = rig.speaker.play_url(&url).await.unwrap();
        within(Duration::from_secs(3), &mut p.started)
            .await
            .unwrap();
        drop(parts); // the connection closes long before its length
        let r = within(Duration::from_secs(3), p.done).await.unwrap();
        assert!(r.is_err(), "{r:?}");
        assert!(sounding_frames(&rig.sink) > 10_000);
    }

    #[tokio::test]
    async fn stop_while_opening_drops_the_stream() {
        let (url, parts, _) = gated(None).await;
        let rig = rig(Duration::from_secs(5));
        let sp = rig.speaker.clone();
        let opening = tokio::spawn(async move { sp.play_url(&url).await });
        tokio::time::sleep(Duration::from_millis(200)).await;
        rig.speaker.stop(); // before any audio arrived
        parts.send(speech(300)).unwrap();
        let p = within(Duration::from_secs(3), opening)
            .await
            .unwrap()
            .unwrap();
        assert!(
            within(Duration::from_secs(1), p.done)
                .await
                .unwrap()
                .is_ok()
        );
        assert!(p.started.await.is_err(), "it never started");
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(sounding_frames(&rig.sink), 0);
    }

    #[tokio::test]
    async fn warm_opens_the_session_until_idle() {
        let rig = rig(Duration::from_millis(300));
        rig.speaker.warm();
        let sp = rig.speaker.clone();
        assert!(until(Duration::from_secs(2), || sp.is_session_open()).await);
        assert!(!rig.speaker.is_playing());
        assert!(until(Duration::from_secs(2), || !sp.is_session_open()).await);
        assert_eq!(sounding_frames(&rig.sink), 0);
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

    // -- live streams: the lane ------------------------------------------------

    fn live_lane() -> (MediaLane, Arc<LiveQueue>) {
        let q = Arc::new(LiveQueue::default());
        let mut lane = MediaLane::default();
        lane.begin(1, STREAM_URL.into());
        lane.attach(1, Decoder::live_detached(q.clone()));
        (lane, q)
    }

    fn chunk(level: i16) -> Vec<i16> {
        vec![level; CHUNK_FRAMES * 2]
    }

    /// Left channel of the next `frames` frames.
    fn pull_left(lane: &mut MediaLane, frames: usize) -> Vec<f32> {
        (0..frames).map(|_| lane.pull().0).collect()
    }

    #[test]
    fn live_lane_buffers_then_fades_in() {
        let (mut lane, q) = live_lane();
        assert_eq!(lane.phase(), MediaPhase::Buffering);
        for _ in 1..LIVE_TARGET_CHUNKS {
            q.push(chunk(10000));
        }
        assert!(pull_left(&mut lane, 100).iter().all(|&s| s == 0.0));
        assert_eq!(lane.phase(), MediaPhase::Buffering);
        q.push(chunk(10000));
        let l = pull_left(&mut lane, CHUNK_FRAMES);
        assert_eq!(lane.phase(), MediaPhase::Playing);
        assert_eq!(l[0], 0.0, "starts from silence");
        assert!(l.windows(2).all(|w| w[1] >= w[0]));
        assert!(l[CHUNK_FRAMES - 1] > 9900.0);
    }

    #[test]
    fn live_lane_fades_out_when_dry_then_rebuffers() {
        let (mut lane, q) = live_lane();
        for _ in 0..LIVE_TARGET_CHUNKS {
            q.push(chunk(10000));
        }
        pull_left(&mut lane, CHUNK_FRAMES * (LIVE_TARGET_CHUNKS - 1));
        // The last chunk there is fades to silence instead of stopping dead.
        let l = pull_left(&mut lane, CHUNK_FRAMES);
        assert!(l[0] > 9900.0 && l[CHUNK_FRAMES - 1] < 20.0, "{l:?}");
        assert!(l.windows(2).all(|w| w[1] <= w[0]));
        assert!(pull_left(&mut lane, 10).iter().all(|&s| s == 0.0));
        assert_eq!(lane.phase(), MediaPhase::Buffering);
        assert!(lane.wants_session());
        // One chunk is not enough to resume; the target is.
        q.push(chunk(5000));
        assert!(pull_left(&mut lane, 100).iter().all(|&s| s == 0.0));
        for _ in 1..LIVE_TARGET_CHUNKS {
            q.push(chunk(5000));
        }
        let l = pull_left(&mut lane, CHUNK_FRAMES);
        assert_eq!(l[0], 0.0);
        assert!(l[CHUNK_FRAMES - 1] > 4900.0);
        assert_eq!(lane.phase(), MediaPhase::Playing);
    }

    #[test]
    fn slow_sender_rebuffers_instead_of_fading_every_chunk() {
        // A sender slightly slower than the speaker: each chunk arrives just
        // before it is needed, so every pop leaves the queue empty.
        let (mut lane, q) = live_lane();
        for _ in 0..LIVE_TARGET_CHUNKS {
            q.push(chunk(10000));
        }
        pull_left(&mut lane, CHUNK_FRAMES * LIVE_TARGET_CHUNKS); // last one faded
        let chunks: Vec<Vec<f32>> = (0..3 * LIVE_TARGET_CHUNKS)
            .map(|_| {
                q.push(chunk(10000)); // just in time
                pull_left(&mut lane, CHUNK_FRAMES)
            })
            .collect();
        // Silent while 100 ms gather again...
        for c in &chunks[..LIVE_TARGET_CHUNKS - 1] {
            assert!(c.iter().all(|&s| s == 0.0));
        }
        // ...then one fade-in, and from then on a 100 ms reserve: no chunk
        // is faded (each would be a 50 Hz flutter).
        let rest = &chunks[LIVE_TARGET_CHUNKS - 1..];
        assert!(rest[0][0] == 0.0 && rest[0][CHUNK_FRAMES - 1] > 9900.0);
        for c in &rest[1..] {
            assert!(c[0] > 9900.0 && c[CHUNK_FRAMES - 1] > 9900.0);
        }
        assert_eq!(lane.phase(), MediaPhase::Playing);
    }

    #[test]
    fn live_eof_plays_out_fades_and_idles() {
        let (mut lane, q) = live_lane();
        q.push(chunk(10000));
        q.push(chunk(10000));
        q.end(None);
        // Fewer chunks than the target, but the stream ended: play them out.
        let l = pull_left(&mut lane, CHUNK_FRAMES * 2);
        assert!(l[CHUNK_FRAMES] > 9900.0);
        assert!(l[2 * CHUNK_FRAMES - 1] < 20.0, "last chunk fades out");
        assert_eq!(pull_left(&mut lane, 1), [0.0]);
        assert_eq!(lane.phase(), MediaPhase::Idle);
        let mut st = MediaState {
            url: Some("x".into()),
            ..Default::default()
        };
        lane.publish(&mut st);
        assert_eq!((st.url, st.error), (None, None));
    }

    #[test]
    fn live_queue_drops_the_oldest_backlog() {
        let q = LiveQueue::default();
        for i in 0..=LIVE_MAX_CHUNKS {
            q.push(chunk(i as i16));
        }
        let levels: Vec<usize> = q
            .inner
            .lock()
            .unwrap()
            .chunks
            .iter()
            .map(|c| c[0] as usize)
            .collect();
        let newest: Vec<usize> =
            (LIVE_MAX_CHUNKS + 1 - LIVE_TARGET_CHUNKS..=LIVE_MAX_CHUNKS).collect();
        assert_eq!(levels, newest);
    }

    // -- stop and replace: fade out, never cut ------------------------------------

    /// A lane playing a decoded source of constant `level`; the sender stays
    /// open, so a failed send means the lane dropped the source.
    fn decoded_lane(gen_: u64, level: i16) -> (MediaLane, SyncSender<Item>) {
        let mut lane = MediaLane::default();
        let tx = attach_decoded(&mut lane, gen_, level);
        (lane, tx)
    }

    fn attach_decoded(lane: &mut MediaLane, gen_: u64, level: i16) -> SyncSender<Item> {
        let (tx, rx) = sync_channel(16);
        for _ in 0..10 {
            tx.send(Item::Data(chunk(level))).unwrap();
        }
        lane.begin(gen_, format!("url{gen_}"));
        lane.attach(gen_, Decoder::detached(rx));
        tx
    }

    fn dropped(tx: &SyncSender<Item>) -> bool {
        tx.try_send(Item::Data(chunk(1))).is_err()
    }

    /// Largest change between neighbouring frames: a click is a big one.
    fn max_step(l: &[f32]) -> f32 {
        l.windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0, f32::max)
    }

    /// One fade's step at full scale (10000 over 20 ms is ~10.4 per frame).
    const FADE_STEP: f32 = 20.0;

    #[test]
    fn stop_fades_out_then_drops_the_source() {
        let (mut lane, tx) = decoded_lane(1, 10000);
        pull_left(&mut lane, 100);
        lane.stop(2);
        assert_eq!(lane.phase(), MediaPhase::Idle, "reported at once");
        assert!(lane.wants_session(), "the fade still has to sound");
        assert!(!dropped(&tx));
        let l = pull_left(&mut lane, 2 * CHUNK_FRAMES);
        assert!(l[0] > 9900.0, "{}", l[0]);
        assert!(l[CHUNK_FRAMES..].iter().all(|&s| s == 0.0), "done in 20 ms");
        assert!(max_step(&l) < FADE_STEP, "click: {}", max_step(&l));
        assert!(dropped(&tx));
        assert!(!lane.wants_session());
    }

    #[test]
    fn a_stream_replacing_a_url_fades_it_out_and_itself_in() {
        let (mut lane, tx) = decoded_lane(1, 10000);
        let mut l = pull_left(&mut lane, 100);
        let q = Arc::new(LiveQueue::default());
        lane.begin(2, STREAM_URL.into());
        lane.attach(2, Decoder::live_detached(q.clone()));
        // The URL fades out while the stream gathers its 100 ms.
        l.extend(pull_left(&mut lane, CHUNK_FRAMES));
        for _ in 0..LIVE_TARGET_CHUNKS {
            q.push(chunk(6000));
        }
        l.extend(pull_left(&mut lane, 2 * CHUNK_FRAMES));
        assert!(dropped(&tx));
        assert!(max_step(&l) < FADE_STEP, "click: {}", max_step(&l));
        assert_eq!(l[l.len() - 1], 6000.0);
        let mut st = MediaState::default();
        lane.publish(&mut st);
        assert_eq!(
            (st.phase, st.url.as_deref()),
            (MediaPhase::Playing, Some(STREAM_URL))
        );
    }

    #[test]
    fn a_url_replacing_a_stream_fades_the_stream_out() {
        let (mut lane, q) = live_lane();
        for _ in 0..LIVE_TARGET_CHUNKS {
            q.push(chunk(10000));
        }
        pull_left(&mut lane, CHUNK_FRAMES + 100); // past the fade-in
        lane.begin(2, "url2".into());
        let l = pull_left(&mut lane, CHUNK_FRAMES + 10);
        assert!(l[0] > 9900.0 && l[CHUNK_FRAMES - 1] < 1.0);
        assert!(max_step(&l) < FADE_STEP, "click: {}", max_step(&l));
        assert_eq!(Arc::strong_count(&q), 1, "stream not dropped");
        // The URL starts at its own beginning, at full level.
        let _tx = attach_decoded(&mut lane, 3, 6000);
        assert_eq!(pull_left(&mut lane, 1), [6000.0]);
    }

    #[test]
    fn silent_sources_are_dropped_at_once() {
        // A paused URL that has faded to silence.
        let (mut lane, tx) = decoded_lane(1, 10000);
        pull_left(&mut lane, 100);
        lane.pause();
        pull_left(&mut lane, CHUNK_FRAMES + 10);
        lane.stop(2);
        assert!(dropped(&tx));
        assert!(!lane.wants_session());
        // A stream still gathering its 100 ms: its queued audio never starts.
        let (mut lane, q) = live_lane();
        q.push(chunk(10000));
        pull_left(&mut lane, 10);
        lane.stop(2);
        assert!(pull_left(&mut lane, 10).iter().all(|&s| s == 0.0));
        assert_eq!(Arc::strong_count(&q), 1);
    }

    #[test]
    fn a_second_stop_during_a_fade_fades_both() {
        let (mut lane, a) = decoded_lane(1, 10000);
        pull_left(&mut lane, 100);
        lane.stop(2);
        pull_left(&mut lane, 300); // A is partly faded
        let b = attach_decoded(&mut lane, 3, 10000);
        pull_left(&mut lane, 100);
        lane.stop(4);
        let l = pull_left(&mut lane, CHUNK_FRAMES + 10);
        assert!(l[0] > 15000.0, "both still sounding: {}", l[0]);
        assert!(l.windows(2).all(|w| w[1] <= w[0]), "both fade together");
        assert!(l[CHUNK_FRAMES - 1] < 1.0);
        assert!(dropped(&a) && dropped(&b));
    }

    #[test]
    fn speaker_failure_drops_fades_at_once() {
        let (mut lane, tx) = decoded_lane(1, 10000);
        pull_left(&mut lane, 100);
        lane.stop(2);
        lane.fail("speaker process died");
        assert!(dropped(&tx));
        assert!(!lane.wants_session());
    }

    #[test]
    fn live_pause_ends_the_session() {
        let (mut lane, q) = live_lane();
        for _ in 0..LIVE_TARGET_CHUNKS {
            q.push(chunk(10000));
        }
        pull_left(&mut lane, CHUNK_FRAMES + 10);
        assert_eq!(lane.phase(), MediaPhase::Playing);
        lane.pause();
        assert_eq!(lane.phase(), MediaPhase::Idle);
        // Ends like a stop: fades out, then the stream is dropped.
        let l = pull_left(&mut lane, CHUNK_FRAMES + 10);
        assert!(l[0] > 9900.0 && l[CHUNK_FRAMES - 1] < 1.0);
        assert!(!lane.wants_session());
        assert_eq!(Arc::strong_count(&q), 1);
    }

    // -- live streams: over the socket -----------------------------------------

    use tokio::net::UnixStream as Client;

    async fn stream_rig(idle: Duration) -> (Rig, MediaHandle, PathBuf) {
        let rig = rig(idle);
        let m = rig.speaker.media();
        let path = rig.dir.join("stream.sock");
        m.serve_stream(&path).unwrap();
        (rig, m, path)
    }

    /// Write `millis` of constant `level` in real time, 20 ms at a time, each
    /// write torn mid-frame. Fails once the ingest has closed the connection.
    async fn send_live(
        w: &mut (impl tokio::io::AsyncWrite + Unpin),
        level: i16,
        millis: u64,
    ) -> std::io::Result<()> {
        let bytes = pcm_bytes(&chunk(level));
        let mut tick = tokio::time::interval(Duration::from_millis(20));
        for _ in 0..millis / 20 {
            tick.tick().await;
            w.write_all(&bytes[..1001]).await?;
            w.write_all(&bytes[1001..]).await?;
        }
        Ok(())
    }

    /// The ingest closed the connection: a read sees EOF within `within`.
    async fn closed(r: &mut (impl tokio::io::AsyncRead + Unpin), within: Duration) -> bool {
        let mut b = [0u8; 16];
        matches!(
            tokio::time::timeout(within, r.read(&mut b)).await,
            Ok(Ok(0))
        )
    }

    fn left_samples(sink: &Path) -> Vec<i16> {
        std::fs::read(sink)
            .unwrap_or_default()
            .chunks_exact(4)
            .map(|f| i16::from_le_bytes([f[0], f[1]]))
            .collect()
    }

    fn url_of(m: &MediaHandle) -> Option<String> {
        m.state().borrow().url.clone()
    }

    #[tokio::test]
    async fn stream_plays_and_fades_out_at_eof() {
        let (rig, m, path) = stream_rig(Duration::from_secs(5)).await;
        let mut c = Client::connect(&path).await.unwrap();
        let sender = tokio::spawn(async move {
            send_live(&mut c, 8000, 600).await.unwrap();
            c.write_all(&[1, 2, 3]).await.unwrap(); // a torn frame, dropped
            c
        });
        wait_phase(&m, MediaPhase::Playing, Duration::from_secs(2)).await;
        assert_eq!(url_of(&m).as_deref(), Some(STREAM_URL));
        drop(sender.await.unwrap()); // EOF
        wait_phase(&m, MediaPhase::Idle, Duration::from_secs(2)).await;
        assert_eq!((url_of(&m), m.state().borrow().error.clone()), (None, None));
        tokio::time::sleep(Duration::from_millis(200)).await; // the player drains

        let got = sounding_frames(&rig.sink);
        assert!(got > 26_000, "only {got} of 28800 frames sounded");
        assert_fades_out(&rig.sink);
    }

    /// The last sound in the sink ramps down over 20 ms instead of cutting off.
    fn assert_fades_out(sink: &Path) {
        let l = left_samples(sink);
        let peak = l.iter().map(|s| s.abs()).max().unwrap();
        let last = l.iter().rposition(|&s| s != 0).unwrap();
        let tail = &l[last + 1 - CHUNK_FRAMES..=last];
        assert!(tail.windows(2).all(|w| w[1] <= w[0]), "tail not monotonic");
        assert!(tail[CHUNK_FRAMES - 1] < peak / 50);
        assert!((tail[CHUNK_FRAMES / 2] - peak / 2).abs() < peak / 10);
    }

    #[tokio::test]
    async fn stop_and_pause_fade_out_and_close_the_connection() {
        let (rig, m, path) = stream_rig(Duration::from_secs(5)).await;
        for pause in [false, true] {
            let (mut r, mut w) = Client::connect(&path).await.unwrap().into_split();
            let sender = tokio::spawn(async move { send_live(&mut w, 8000, 10_000).await });
            wait_phase(&m, MediaPhase::Playing, Duration::from_secs(2)).await;
            tokio::time::sleep(Duration::from_millis(200)).await; // past the fade-in
            if pause {
                m.pause();
            } else {
                m.stop();
            }
            wait_phase(&m, MediaPhase::Idle, Duration::from_secs(1)).await;
            assert!(
                closed(&mut r, Duration::from_secs(1)).await,
                "pause {pause}"
            );
            let sent = tokio::time::timeout(Duration::from_secs(1), sender).await;
            assert!(sent.unwrap().unwrap().is_err(), "writes still accepted");
            assert_eq!(m.state().borrow().phase, MediaPhase::Idle);
            tokio::time::sleep(Duration::from_millis(200)).await; // the player drains
            assert_fades_out(&rig.sink);
        }
    }

    #[tokio::test]
    async fn stop_closes_a_quiet_connection_at_once() {
        // The reader is blocked in a read: stop must not wait for STREAM_IDLE.
        let (_rig, m, path) = stream_rig(Duration::from_secs(5)).await;
        let mut c = Client::connect(&path).await.unwrap();
        let sender = tokio::spawn(async move {
            send_live(&mut c, 8000, 300).await.unwrap();
            c
        });
        wait_phase(&m, MediaPhase::Playing, Duration::from_secs(2)).await;
        let mut c = sender.await.unwrap();
        // Quiet long enough for the reader to block, well short of STREAM_IDLE.
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(m.state().borrow().phase, MediaPhase::Buffering);
        m.stop();
        assert!(closed(&mut c, Duration::from_millis(300)).await);
    }

    #[tokio::test]
    async fn newest_stream_replaces_the_playing_one_silent_clients_do_not() {
        let (_rig, m, path) = stream_rig(Duration::from_secs(5)).await;
        let (mut a_r, mut a_w) = Client::connect(&path).await.unwrap().into_split();
        let a = tokio::spawn(async move { send_live(&mut a_w, 8000, 10_000).await });
        wait_phase(&m, MediaPhase::Playing, Duration::from_secs(2)).await;
        // Connecting without sending changes nothing...
        let mut silent = Client::connect(&path).await.unwrap();
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(!a.is_finished());
        assert_eq!(m.state().borrow().phase, MediaPhase::Playing);
        // ...sending does.
        let (_b_r, mut b_w) = Client::connect(&path).await.unwrap().into_split();
        let b = tokio::spawn(async move { send_live(&mut b_w, 4000, 10_000).await });
        assert!(closed(&mut a_r, Duration::from_secs(2)).await);
        assert!(a.await.unwrap().is_err());
        wait_phase(&m, MediaPhase::Playing, Duration::from_secs(2)).await;
        assert_eq!(url_of(&m).as_deref(), Some(STREAM_URL));
        // The silent client is let go after STREAM_IDLE.
        assert!(closed(&mut silent, STREAM_IDLE + Duration::from_secs(1)).await);
        assert!(!b.is_finished());
        m.stop();
        assert!(b.await.unwrap().is_err());
    }

    #[tokio::test]
    async fn streams_and_urls_replace_each_other() {
        let (rig, m, path) = stream_rig(Duration::from_secs(5)).await;
        let f = rig.dir.join("clip.wav");
        std::fs::write(&f, wav(48000, 2, &sine(48000, 2, 3000, 9000.0))).unwrap();
        let file = f.to_string_lossy().to_string();

        let (mut r, mut w) = Client::connect(&path).await.unwrap().into_split();
        let s1 = tokio::spawn(async move { send_live(&mut w, 8000, 10_000).await });
        wait_phase(&m, MediaPhase::Playing, Duration::from_secs(2)).await;
        m.play_url(&file).await.unwrap();
        assert!(closed(&mut r, Duration::from_secs(1)).await);
        assert!(s1.await.unwrap().is_err());
        assert!(until(Duration::from_secs(1), || url_of(&m) == Some(file.clone())).await);

        let (_r, mut w) = Client::connect(&path).await.unwrap().into_split();
        let s2 = tokio::spawn(async move { send_live(&mut w, 8000, 10_000).await });
        assert!(
            until(Duration::from_secs(2), || url_of(&m).as_deref()
                == Some(STREAM_URL))
            .await
        );
        wait_phase(&m, MediaPhase::Playing, Duration::from_secs(2)).await;
        m.stop();
        assert!(s2.await.unwrap().is_err());
    }

    #[tokio::test]
    async fn quiet_stream_ends_after_idle_and_the_speaker_closes() {
        let (rig, m, path) = stream_rig(Duration::from_millis(400)).await;
        let mut c = Client::connect(&path).await.unwrap();
        let sender = tokio::spawn(async move {
            send_live(&mut c, 8000, 300).await.unwrap();
            c
        });
        wait_phase(&m, MediaPhase::Playing, Duration::from_secs(2)).await;
        let mut c = sender.await.unwrap(); // connected, but quiet from here
        let t0 = Instant::now();
        wait_phase(&m, MediaPhase::Idle, STREAM_IDLE + Duration::from_secs(2)).await;
        assert!(t0.elapsed() > STREAM_IDLE - Duration::from_millis(300));
        assert_eq!(m.state().borrow().error, None);
        assert!(closed(&mut c, Duration::from_millis(500)).await);
        assert!(until(Duration::from_secs(2), || !rig.speaker.is_session_open()).await);
    }

    #[tokio::test]
    async fn streams_are_muted_and_ducked_like_other_media() {
        let (rig, m, path) = stream_rig(Duration::from_secs(5)).await;
        m.set_muted(true);
        let mut st = m.state();
        tokio::time::timeout(Duration::from_secs(1), async {
            while !st.borrow().muted {
                st.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
        let (_r, mut w) = Client::connect(&path).await.unwrap().into_split();
        let s = tokio::spawn(async move { send_live(&mut w, 8000, 10_000).await });
        wait_phase(&m, MediaPhase::Playing, Duration::from_secs(2)).await;
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert_eq!(sounding_frames(&rig.sink), 0);

        m.set_muted(false);
        tokio::time::sleep(Duration::from_millis(500)).await;
        // Peak of the last 200 ms handed to the player.
        let recent = || {
            let l = left_samples(&rig.sink);
            l[l.len().saturating_sub(9600)..]
                .iter()
                .map(|s| s.abs())
                .max()
                .unwrap_or(0)
        };
        let full = recent();
        assert!(full > 0);
        rig.speaker.duck(true);
        tokio::time::sleep(Duration::from_millis(500)).await;
        let ducked = recent();
        assert!(
            ducked < full / 5 && ducked > full / 20,
            "{full} -> {ducked}"
        );
        m.stop();
        assert!(s.await.unwrap().is_err());
    }

    #[tokio::test]
    async fn serve_stream_replaces_a_stale_socket_only() {
        let rig = rig(Duration::from_secs(5));
        let m = rig.speaker.media();
        let p = rig.dir.join("run/stream.sock");
        m.serve_stream(&p).unwrap();
        // A socket left by an earlier run is replaced.
        m.serve_stream(&p).unwrap();
        let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o660);
        assert!(Client::connect(&p).await.is_ok());
        // Anything else at the path is left alone.
        let f = rig.dir.join("not-a-socket");
        std::fs::write(&f, b"x").unwrap();
        assert!(m.serve_stream(&f).is_err());
        assert_eq!(std::fs::read(&f).unwrap(), b"x");
    }

    #[test]
    fn extension_parsing() {
        assert_eq!(extension_of("http://h/a/b.mp3?x=1.wav"), Some("mp3"));
        assert_eq!(extension_of("http://h/stream"), None);
        assert_eq!(extension_of("/tmp/x.FLAC"), Some("FLAC"));
        assert!(is_http("HTTPS://x") && !is_http("file:///x") && !is_http("/x"));
    }

    // -- voice streams -----------------------------------------------------------

    fn voice() -> (VoiceSource, SyncSender<Item>) {
        let (tx, rx) = sync_channel(BUFFER_CHUNKS);
        (VoiceSource::new(Decoder::detached(rx)), tx)
    }

    fn send(tx: &SyncSender<Item>, level: i16, n: usize) {
        for _ in 0..n {
            tx.send(Item::Data(chunk(level))).unwrap();
        }
    }

    fn take(v: &mut VoiceSource) -> Vec<i16> {
        match v.pull() {
            Pull::Chunk(c) => c,
            other => panic!("expected a chunk, got {other:?}"),
        }
    }

    /// Left channel of a chunk.
    fn left(c: &[i16]) -> Vec<i16> {
        c.iter().step_by(2).copied().collect()
    }

    #[test]
    fn voice_gathers_then_starts_at_full_level() {
        let (mut v, tx) = voice();
        send(&tx, 10000, VOICE_TARGETS[0] - 1);
        assert_eq!(v.pull(), Pull::Wait);
        send(&tx, 10000, 2);
        let c = left(&take(&mut v));
        assert!(c.iter().all(|&s| s == 10000), "unfaded start");
    }

    #[test]
    fn voice_fades_out_when_dry_then_waits_longer_each_time_and_fades_in() {
        let (mut v, tx) = voice();
        send(&tx, 10000, VOICE_TARGETS[0]);
        for _ in 1..VOICE_TARGETS[0] {
            assert!(left(&take(&mut v)).iter().all(|&s| s == 10000));
        }
        // The last chunk there is fades to silence.
        let c = left(&take(&mut v));
        assert!(c[0] > 9900 && c[CHUNK_FRAMES - 1] < 20, "{c:?}");
        assert_eq!(v.pull(), Pull::Wait);
        assert_eq!(v.stalls(), 0, "not a stall until it comes back");
        // The first target is not enough any more; the second is.
        send(&tx, 5000, VOICE_TARGETS[0]);
        assert_eq!(v.pull(), Pull::Wait);
        send(&tx, 5000, VOICE_TARGETS[1] - VOICE_TARGETS[0]);
        let c = left(&take(&mut v));
        assert_eq!(c[0], 0, "fades in");
        assert!(c.windows(2).all(|w| w[1] >= w[0]));
        assert!(c[CHUNK_FRAMES - 1] > 4900);
        assert_eq!(v.stalls(), 1);
        // Dry again: now it waits for the largest target.
        while let Pull::Chunk(_) = v.pull() {}
        send(&tx, 5000, VOICE_TARGETS[2] - 1);
        assert_eq!(v.pull(), Pull::Wait);
        send(&tx, 5000, 1);
        assert!(matches!(v.pull(), Pull::Chunk(_)));
        assert_eq!(v.stalls(), 2);
    }

    #[test]
    fn voice_never_drops_audio_across_stalls() {
        // Levels below CHUNK_FRAMES keep their peak through one fade (in or
        // out), so each chunk stays identifiable. Chunks arrive one at a time,
        // pulled after each, as a source that keeps falling behind.
        let (mut v, tx) = voice();
        let mut want = Vec::new();
        let mut got = Vec::new();
        let mut level = 100i16;
        let collect = |v: &mut VoiceSource, got: &mut Vec<i16>| {
            while let Pull::Chunk(c) = v.pull() {
                got.push(*c.iter().max().unwrap());
            }
        };
        for burst in [VOICE_TARGETS[0], VOICE_TARGETS[1] + 3, VOICE_TARGETS[2] + 5] {
            for _ in 0..burst {
                tx.send(Item::Data(chunk(level))).unwrap();
                want.push(level);
                level += 1;
                if let Pull::Chunk(c) = v.pull() {
                    got.push(*c.iter().max().unwrap());
                }
            }
            collect(&mut v, &mut got);
        }
        // A tail shorter than any target plays once the stream ends.
        send(&tx, level, 2);
        want.extend([level, level]);
        tx.send(Item::End(None)).unwrap();
        collect(&mut v, &mut got);
        assert_eq!(got, want);
        assert_eq!(v.pull(), Pull::End(None));
    }

    #[test]
    fn voice_short_stream_plays_at_once_without_fading() {
        let (mut v, tx) = voice();
        send(&tx, 8000, 2);
        tx.send(Item::End(None)).unwrap();
        for _ in 0..2 {
            assert!(left(&take(&mut v)).iter().all(|&s| s == 8000));
        }
        assert_eq!(v.pull(), Pull::End(None));
    }

    #[test]
    fn voice_error_after_audio_fades_then_ends_with_it() {
        let (mut v, tx) = voice();
        send(&tx, 8000, VOICE_TARGETS[0]);
        tx.send(Item::End(Some("cut off".into()))).unwrap();
        for _ in 1..VOICE_TARGETS[0] {
            take(&mut v);
        }
        let c = left(&take(&mut v));
        assert!(c[CHUNK_FRAMES - 1] < 20, "faded");
        assert_eq!(v.pull(), Pull::End(Some("cut off".into())));
    }

    #[test]
    fn voice_decoder_gone_is_an_error() {
        let (mut v, tx) = voice();
        drop(tx);
        assert_eq!(
            v.pull(),
            Pull::End(Some("decoder stopped unexpectedly".into()))
        );
    }
}
