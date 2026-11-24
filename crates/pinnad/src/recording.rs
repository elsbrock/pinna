//! Recording the microphones from the dashboard (ADR 0030).
//!
//! `RecordAudio` streams what the satellite hears for up to 30 seconds:
//! the frames that the wake word and the request get, tapped in `capture_loop`
//! before and after pinnad's gain, and on the Echo's PipeWire graph the
//! microphone lanes too, from a second `pw-record` that lives only for the
//! recording, and the echo reference cut from them: the loopback lane the
//! speaker plays (ADR 0004). Nothing is stored: the chunks go through one
//! bounded queue of about 2 seconds, and a caller that falls behind ends the
//! recording. The Brain is told while one runs, so the ring shows Listening
//! and an update's restart waits; the phase doesn't change.

use std::net::IpAddr;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use connectrpc::ConnectError;
use pinna_api::{AudioSource, RecordAudioResponse, RecordingEnd};
use tokio::io::{AsyncRead, AsyncReadExt, ReadBuf};
use tokio::process::{Child, ChildStdout, Command};
use tokio::sync::{mpsc, watch};
use tokio::task::AbortHandle;

use crate::voice::{Input, Snapshot};

pub const SAMPLE_RATE: u32 = 16_000;
pub const MAX_SECONDS: u32 = 30;
/// HEARD and BEFORE_GAIN go out in chunks of 5 frames of 20 ms: 100 ms.
const CHUNK_FRAMES: usize = 5;
/// The lanes of `dot_mics`, recorded as S24LE: 100 ms of `lanes` is this many
/// bytes (43200 for the MT8163's 9).
fn lanes_chunk(lanes: u32) -> usize {
    SAMPLE_RATE as usize * lanes as usize * 3 / 10
}
/// What the queue holds, over all sources together.
const QUEUE_SECONDS: usize = 2;
/// The Brain forgets a recording this long after it should have ended, if
/// nothing told it so.
pub const BRAIN_GRACE: Duration = Duration::from_secs(5);

const MUTED: &str = "The microphones are muted. Turn the microphones on first.";
const CALIBRATING: &str = "Wake word calibration is running. Stop it first.";
const RUNNING: &str = "A recording is running already. Wait for it to end, or stop it.";
const SECONDS: &str = "The length must be 1 to 30 seconds. Choose one in that range.";
const NO_BRAIN: &str = "pinnad is shutting down";

/// Check the requested length.
pub fn check_seconds(seconds: u32) -> Result<(), ConnectError> {
    if (1..=MAX_SECONDS).contains(&seconds) {
        Ok(())
    } else {
        Err(ConnectError::invalid_argument(SECONDS))
    }
}

/// One chunk of one source.
type Item = (AudioSource, Vec<u8>);

/// Why a recording ended, and for a failure the sentence that says so.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finish {
    pub reason: RecordingEnd,
    pub detail: String,
}

/// The state shared by the taps, the debug readers and the stream of one
/// recording. The first end wins.
struct Run {
    finish: watch::Sender<Option<Finish>>,
}

impl Run {
    fn end(&self, reason: RecordingEnd, detail: impl Into<String>) {
        self.finish.send_if_modified(|f| {
            if f.is_some() {
                return false;
            }
            *f = Some(Finish {
                reason,
                detail: detail.into(),
            });
            true
        });
    }

    fn finished(&self) -> bool {
        self.finish.borrow().is_some()
    }
}

struct Taps {
    tx: mpsc::Sender<Item>,
    run: Arc<Run>,
    /// BEFORE_GAIN is a debug stream: the Echo's PipeWire graph only.
    debug: bool,
    before: Vec<u8>,
    heard: Vec<u8>,
    frames: usize,
}

/// What `capture_loop` feeds. Costs one atomic read per frame while no
/// recording runs.
#[derive(Default)]
pub struct Tap {
    on: AtomicBool,
    slot: Mutex<Option<Taps>>,
}

fn le_bytes(samples: &[i16], out: &mut Vec<u8>) {
    for s in samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
}

impl Tap {
    /// A recording runs.
    pub fn active(&self) -> bool {
        self.on.load(Ordering::Relaxed)
    }

    /// One 20 ms frame, as the echo canceller gave it (`before`) and as the
    /// wake word and the request get it (`heard`).
    pub fn frame(&self, before: &[i16], heard: &[i16]) {
        let mut slot = self.slot.lock().unwrap();
        let Some(taps) = slot.as_mut().filter(|t| !t.run.finished()) else {
            return;
        };
        if taps.debug {
            le_bytes(before, &mut taps.before);
        }
        le_bytes(heard, &mut taps.heard);
        taps.frames += 1;
        if taps.frames < CHUNK_FRAMES {
            return;
        }
        taps.frames = 0;
        if taps.debug {
            let before = std::mem::take(&mut taps.before);
            offer(
                &taps.tx,
                &taps.run,
                AudioSource::AUDIO_SOURCE_BEFORE_GAIN,
                before,
            );
        }
        let heard = std::mem::take(&mut taps.heard);
        offer(&taps.tx, &taps.run, AudioSource::AUDIO_SOURCE_HEARD, heard);
    }

    fn install(&self, tx: mpsc::Sender<Item>, run: Arc<Run>, debug: bool) {
        *self.slot.lock().unwrap() = Some(Taps {
            tx,
            run,
            debug,
            before: Vec::new(),
            heard: Vec::new(),
            frames: 0,
        });
        self.on.store(true, Ordering::SeqCst);
    }

    /// Turn the taps off, unless another recording took them over.
    fn remove(&self, run: &Arc<Run>) {
        let mut slot = self.slot.lock().unwrap();
        if slot.as_ref().is_some_and(|t| Arc::ptr_eq(&t.run, run)) {
            *slot = None;
            self.on.store(false, Ordering::SeqCst);
        }
    }
}

/// Queue a chunk; a full queue means the caller fell behind.
fn offer(tx: &mpsc::Sender<Item>, run: &Run, source: AudioSource, data: Vec<u8>) {
    if let Err(mpsc::error::TrySendError::Full(_)) = tx.try_send((source, data)) {
        run.end(RecordingEnd::RECORDING_END_FELL_BEHIND, "");
    }
}

/// The debug captures of the Echo's PipeWire graph. A seam: tests can't run
/// PipeWire.
pub trait DebugCapture: Send + Sync {
    /// How many lanes `dot_mics` has: the board's capture channels.
    fn lanes(&self) -> u32;
    /// Raw bytes of `source` (LANES) until dropped.
    fn open(&self, source: AudioSource) -> std::io::Result<Box<dyn AsyncRead + Send + Unpin>>;
    /// The lane that carries what the speaker plays, the echo canceller's
    /// reference (`pinna_board::Board::speaker_lane`).
    fn reference_lane(&self) -> usize;
}

/// `pw-record`, which lives as long as the reader.
pub struct PwRecord {
    pub bin: String,
    pub reference_lane: usize,
    pub lanes: u32,
}

struct PwStream {
    _child: Child,
    out: ChildStdout,
}

impl AsyncRead for PwStream {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.out).poll_read(cx, buf)
    }
}

impl DebugCapture for PwRecord {
    fn reference_lane(&self) -> usize {
        self.reference_lane
    }

    fn lanes(&self) -> u32 {
        self.lanes
    }

    fn open(&self, source: AudioSource) -> std::io::Result<Box<dyn AsyncRead + Send + Unpin>> {
        // The node name is in 10-echo-dot.conf and 10-echo-dot-5.conf;
        // pw-record converts the rate and format.
        let lanes = self.lanes.to_string();
        let args: &[&str] = match source {
            AudioSource::AUDIO_SOURCE_LANES => &[
                "--raw",
                "--rate",
                "16000",
                "--channels",
                &lanes,
                "--format",
                "s24",
                "--target",
                "dot_mics",
                "-",
            ],
            _ => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Unsupported,
                    "only the lanes are recorded",
                ));
            }
        };
        let mut child = Command::new(&self.bin)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()?;
        let out = child.stdout.take().expect("piped");
        Ok(Box::new(PwStream { _child: child, out }))
    }
}

/// The 16-bit samples of `lane` in a chunk of `lanes` lanes (S24LE, the top
/// 16 bits).
fn lane_s16(chunk: &[u8], lanes: u32, lane: usize) -> Vec<u8> {
    chunk
        .chunks_exact(lanes as usize * 3)
        .flat_map(|f| [f[lane * 3 + 1], f[lane * 3 + 2]])
        .collect()
}

/// Read the lanes in chunks of 100 ms, and offer each with the reference cut
/// from it, until the recording ends or the read fails.
async fn read_lanes(
    mut reader: Box<dyn AsyncRead + Send + Unpin>,
    lanes: u32,
    reference_lane: usize,
    tx: mpsc::Sender<Item>,
    run: Arc<Run>,
) {
    let what = "the microphone lanes";
    loop {
        let mut buf = vec![0u8; lanes_chunk(lanes)];
        if let Err(e) = reader.read_exact(&mut buf).await {
            let why = if e.kind() == std::io::ErrorKind::UnexpectedEof {
                "pw-record ended".to_owned()
            } else {
                e.to_string()
            };
            run.end(
                RecordingEnd::RECORDING_END_FAILED,
                format!("Recording {what} stopped: {why}"),
            );
            return;
        }
        if run.finished() {
            return;
        }
        let reference = lane_s16(&buf, lanes, reference_lane);
        offer(&tx, &run, AudioSource::AUDIO_SOURCE_LANES, buf);
        offer(&tx, &run, AudioSource::AUDIO_SOURCE_REFERENCE, reference);
    }
}

struct Current {
    id: u64,
    run: Arc<Run>,
}

/// Starts and stops recordings; one at a time.
pub struct Recorder {
    shared: Arc<crate::voice::Shared>,
    inputs: mpsc::Sender<Input>,
    status: watch::Receiver<Snapshot>,
    debug: Option<Arc<dyn DebugCapture>>,
    current: Mutex<Option<Current>>,
    next_id: AtomicU64,
}

impl Recorder {
    /// `debug` is set on the Echo's PipeWire graph only.
    pub fn new(
        shared: Arc<crate::voice::Shared>,
        inputs: mpsc::Sender<Input>,
        status: watch::Receiver<Snapshot>,
        debug: Option<Arc<dyn DebugCapture>>,
    ) -> Arc<Self> {
        Arc::new(Self {
            shared,
            inputs,
            status,
            debug,
            current: Mutex::new(None),
            next_id: AtomicU64::new(1),
        })
    }

    /// Why a recording can't start now, if it can't.
    pub fn refusal(&self) -> Option<ConnectError> {
        let current = self.current.lock().unwrap();
        self.refusal_with(&current)
    }

    fn refusal_with(&self, current: &Option<Current>) -> Option<ConnectError> {
        let status = self.status.borrow();
        let sentence = if status.hw_muted || status.sw_muted {
            MUTED
        } else if status.calibration.is_some() {
            CALIBRATING
        } else if current.is_some() {
            RUNNING
        } else {
            return None;
        };
        Some(ConnectError::failed_precondition(sentence))
    }

    /// End the running recording, if one runs.
    pub fn stop(&self) {
        if let Some(current) = &*self.current.lock().unwrap() {
            current
                .run
                .end(RecordingEnd::RECORDING_END_STOPPED, String::new());
        }
    }

    /// Start recording for `seconds` (already checked) for the browser at
    /// `from`.
    pub async fn start(
        self: &Arc<Self>,
        from: IpAddr,
        seconds: u32,
    ) -> Result<Recording, ConnectError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (finish_tx, finish) = watch::channel(None);
        let run = Arc::new(Run { finish: finish_tx });
        {
            // Taken before anything else starts, so two callers can't both.
            let mut current = self.current.lock().unwrap();
            if let Some(refusal) = self.refusal_with(&current) {
                return Err(refusal);
            }
            *current = Some(Current {
                id,
                run: run.clone(),
            });
        }
        // From here the guard undoes whatever fails.
        let mut guard = Guard {
            recorder: self.clone(),
            id,
            run: run.clone(),
            began: Instant::now(),
            tasks: Vec::new(),
            logged: false,
        };
        let sources = if self.debug.is_some() { 4 } else { 1 };
        let (tx, rx) = mpsc::channel(QUEUE_SECONDS * 10 * sources);
        if let Some(debug) = &self.debug {
            let reader = debug.open(AudioSource::AUDIO_SOURCE_LANES).map_err(|e| {
                ConnectError::unavailable(format!(
                    "Recording the microphone lanes didn't start: {e}. Try again."
                ))
            })?;
            let task = tokio::spawn(read_lanes(
                reader,
                debug.lanes(),
                debug.reference_lane(),
                tx.clone(),
                run.clone(),
            ));
            guard.tasks.push(task.abort_handle());
        }
        self.shared
            .recording
            .install(tx, run.clone(), self.debug.is_some());
        let until = Instant::now() + Duration::from_secs(seconds.into());
        self.inputs
            .try_send(Input::RecordingStarted {
                id: guard.id,
                until,
                from,
            })
            .map_err(|_| ConnectError::unavailable(NO_BRAIN))?;
        tracing::info!("recording: started for {seconds} s by {from}");
        guard.logged = true;
        Ok(Recording {
            rx,
            finish,
            run,
            status: self.status.clone(),
            deadline: tokio::time::Instant::now() + Duration::from_secs(seconds.into()),
            seconds,
            debug: self.debug.is_some(),
            lanes: self.debug.as_ref().map_or(0, |d| d.lanes()),
            heard_bytes: 0,
            started_sent: false,
            done: false,
            _guard: guard,
        })
    }
}

/// Undoes a recording however it ends, the caller going away included: the
/// taps, the debug captures and the Brain's state never outlive the stream.
struct Guard {
    recorder: Arc<Recorder>,
    id: u64,
    run: Arc<Run>,
    began: Instant,
    tasks: Vec<AbortHandle>,
    /// The start was logged, so the end is too.
    logged: bool,
}

impl Drop for Guard {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
        self.recorder.shared.recording.remove(&self.run);
        {
            let mut current = self.recorder.current.lock().unwrap();
            if current.as_ref().is_some_and(|c| c.id == self.id) {
                *current = None;
            }
        }
        let ended = Input::RecordingEnded { id: self.id };
        if let Err(mpsc::error::TrySendError::Full(ended)) = self.recorder.inputs.try_send(ended)
            && let Ok(runtime) = tokio::runtime::Handle::try_current()
        {
            let inputs = self.recorder.inputs.clone();
            runtime.spawn(async move {
                let _ = inputs.send(ended).await;
            });
        }
        if self.logged {
            let why = match &*self.run.finish.borrow() {
                Some(f) => match f.reason {
                    RecordingEnd::RECORDING_END_TIME_UP => "time up".to_owned(),
                    RecordingEnd::RECORDING_END_STOPPED => "stopped".to_owned(),
                    RecordingEnd::RECORDING_END_MUTED => "muted".to_owned(),
                    RecordingEnd::RECORDING_END_FELL_BEHIND => "the browser fell behind".to_owned(),
                    _ => format!("failed: {}", f.detail),
                },
                None => "the browser went away".to_owned(),
            };
            tracing::info!(
                "recording: ended after {:.1} s ({why})",
                self.began.elapsed().as_secs_f32()
            );
        }
    }
}

/// A running recording, as the stream of `RecordAudio` reads it.
pub struct Recording {
    rx: mpsc::Receiver<Item>,
    finish: watch::Receiver<Option<Finish>>,
    run: Arc<Run>,
    status: watch::Receiver<Snapshot>,
    deadline: tokio::time::Instant,
    seconds: u32,
    debug: bool,
    lanes: u32,
    /// Bytes of HEARD sent so far, for "the files hold n s".
    heard_bytes: u64,
    started_sent: bool,
    done: bool,
    _guard: Guard,
}

fn message(event: pinna_api::record_audio_response::Event) -> RecordAudioResponse {
    RecordAudioResponse {
        event: Some(event),
        ..Default::default()
    }
}

impl Recording {
    /// The next message: `started`, the chunks as they come, `ended`.
    pub async fn next(&mut self) -> Option<RecordAudioResponse> {
        use pinna_api::record_audio_response::Event;
        if self.done {
            return None;
        }
        if !self.started_sent {
            self.started_sent = true;
            return Some(message(Event::Started(Box::new(
                pinna_api::RecordingStarted {
                    sample_rate: SAMPLE_RATE,
                    seconds: self.seconds,
                    debug: self.debug,
                    lanes: self.lanes,
                    ..Default::default()
                },
            ))));
        }
        loop {
            let finished = self.finish.borrow().clone();
            if let Some(finish) = finished {
                // What is queued still goes out, unless the caller already
                // fell behind.
                if finish.reason != RecordingEnd::RECORDING_END_FELL_BEHIND
                    && let Ok(item) = self.rx.try_recv()
                {
                    return Some(self.chunk(item));
                }
                self.done = true;
                return Some(message(Event::Ended(Box::new(pinna_api::RecordingEnded {
                    reason: finish.reason.into(),
                    message: self.sentence(&finish),
                    ..Default::default()
                }))));
            }
            tokio::select! {
                biased;
                _ = self.finish.changed() => {}
                changed = self.status.changed() => {
                    if changed.is_err() {
                        self.run.end(RecordingEnd::RECORDING_END_FAILED, NO_BRAIN);
                    } else {
                        let status = self.status.borrow();
                        if status.hw_muted || status.sw_muted {
                            self.run.end(RecordingEnd::RECORDING_END_MUTED, "");
                        }
                    }
                }
                () = tokio::time::sleep_until(self.deadline) => {
                    self.run.end(RecordingEnd::RECORDING_END_TIME_UP, "");
                }
                item = self.rx.recv() => match item {
                    Some(item) => return Some(self.chunk(item)),
                    None => self.run.end(RecordingEnd::RECORDING_END_FAILED, NO_BRAIN),
                },
            }
        }
    }

    fn chunk(&mut self, (source, data): Item) -> RecordAudioResponse {
        if source == AudioSource::AUDIO_SOURCE_HEARD {
            self.heard_bytes += data.len() as u64;
        }
        message(pinna_api::record_audio_response::Event::Chunk(Box::new(
            pinna_api::RecordingChunk {
                source: source.into(),
                data,
                ..Default::default()
            },
        )))
    }

    fn sentence(&self, finish: &Finish) -> String {
        let held = self.heard_bytes / (u64::from(SAMPLE_RATE) * 2);
        match finish.reason {
            RecordingEnd::RECORDING_END_TIME_UP => {
                format!("The recording is complete: the files hold {held} s.")
            }
            RecordingEnd::RECORDING_END_STOPPED => {
                format!("The recording was stopped: the files hold {held} s.")
            }
            RecordingEnd::RECORDING_END_MUTED => {
                format!("The microphones were muted: the files hold {held} s.")
            }
            RecordingEnd::RECORDING_END_FELL_BEHIND => {
                format!("The browser fell behind: the files hold {held} s.")
            }
            _ => finish.detail.clone(),
        }
    }

    pub fn into_stream(
        self,
    ) -> impl futures::Stream<Item = Result<RecordAudioResponse, ConnectError>> {
        futures::stream::unfold(self, |mut recording| async move {
            let next = recording.next().await?;
            Some((Ok(next), recording))
        })
    }
}

#[cfg(test)]
mod tests;
