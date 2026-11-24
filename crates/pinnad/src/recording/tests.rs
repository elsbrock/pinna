use std::net::Ipv4Addr;

use connectrpc::ErrorCode;
use pinna_api::record_audio_response::Event;
use tokio::io::{AsyncWriteExt, DuplexStream};

use super::*;
use crate::voice::{CalibrationSnapshot, Shared};

const WHO: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 9));

struct Rig {
    recorder: Arc<Recorder>,
    shared: Arc<Shared>,
    inputs: mpsc::Receiver<Input>,
    status: watch::Sender<Snapshot>,
}

fn rig(debug: Option<Arc<dyn DebugCapture>>) -> Rig {
    let shared = Arc::new(Shared::default());
    let (tx, inputs) = mpsc::channel(8);
    let (status, status_rx) = watch::channel(Snapshot::default());
    Rig {
        recorder: Recorder::new(shared.clone(), tx, status_rx, debug),
        shared,
        inputs,
        status,
    }
}

impl Rig {
    /// `frames` frames of 20 ms, as the capture loop feeds them: the sample
    /// is `value` before the gain and twice that after it.
    fn frames(&self, frames: usize, value: i16) {
        for _ in 0..frames {
            self.shared
                .recording
                .frame(&[value; 320], &[value * 2; 320]);
        }
    }
}

fn code(e: ConnectError) -> ErrorCode {
    e.code
}

async fn next(recording: &mut Recording) -> Event {
    tokio::time::timeout(Duration::from_secs(5), recording.next())
        .await
        .expect("a message in time")
        .expect("not done")
        .event
        .expect("an event")
}

fn chunk(event: Event) -> (AudioSource, Vec<u8>) {
    let Event::Chunk(c) = event else {
        panic!("not a chunk: {event:?}");
    };
    (c.source.as_known().unwrap(), c.data)
}

fn ended(event: Event) -> (RecordingEnd, String) {
    let Event::Ended(e) = event else {
        panic!("not the end: {event:?}");
    };
    (e.reason.as_known().unwrap(), e.message)
}

/// A pw-record that the test feeds.
#[derive(Default)]
struct Fake {
    lanes: u32,
    writers: Mutex<Vec<(AudioSource, DuplexStream)>>,
}

impl Fake {
    fn with_lanes(lanes: u32) -> Self {
        Fake {
            lanes,
            writers: Mutex::default(),
        }
    }
}

impl DebugCapture for Fake {
    /// The last lane, as on the MT8163 (8 of 9).
    fn reference_lane(&self) -> usize {
        self.lanes as usize - 1
    }

    fn lanes(&self) -> u32 {
        self.lanes
    }

    fn open(&self, source: AudioSource) -> std::io::Result<Box<dyn AsyncRead + Send + Unpin>> {
        let (writer, reader) = tokio::io::duplex(1 << 20);
        self.writers.lock().unwrap().push((source, writer));
        Ok(Box::new(reader))
    }
}

#[test]
fn the_length_is_1_to_30_seconds() {
    for ok in [1, 15, 30] {
        check_seconds(ok).unwrap();
    }
    for bad in [0, 31, 3600] {
        assert_eq!(
            code(check_seconds(bad).unwrap_err()),
            ErrorCode::InvalidArgument
        );
    }
}

#[tokio::test]
async fn it_starts_with_a_description_and_streams_heard_and_before_the_gain_in_100_ms_chunks() {
    let mut rig = rig(None);
    let mut recording = rig.recorder.start(WHO, 5).await.unwrap();
    let Some(Input::RecordingStarted { from, until, .. }) = rig.inputs.recv().await else {
        panic!("the Brain was not told");
    };
    assert_eq!(from, WHO);
    assert!(until > Instant::now() + Duration::from_secs(4));
    assert!(rig.shared.recording.active());

    let Event::Started(started) = next(&mut recording).await else {
        panic!("not started");
    };
    assert_eq!(
        (
            started.sample_rate,
            started.seconds,
            started.debug,
            started.lanes
        ),
        (16000, 5, false, 0)
    );

    // Four frames are not a chunk yet; the fifth makes one. Without the
    // Echo's PipeWire graph there is HEARD only.
    rig.frames(4, 100);
    assert!(
        tokio::time::timeout(Duration::from_millis(50), recording.next())
            .await
            .is_err()
    );
    rig.frames(6, 100);
    let (source, heard) = chunk(next(&mut recording).await);
    assert_eq!(source, AudioSource::AUDIO_SOURCE_HEARD);
    assert_eq!(heard.len(), 3200);
    assert_eq!(i16::from_le_bytes([heard[0], heard[1]]), 200);
    let (source, _) = chunk(next(&mut recording).await);
    assert_eq!(source, AudioSource::AUDIO_SOURCE_HEARD, "the second chunk");
}

#[tokio::test]
async fn on_the_echo_dot_the_signal_before_the_gain_comes_too() {
    let rig = rig(Some(Arc::new(Fake::with_lanes(9))));
    let mut recording = rig.recorder.start(WHO, 5).await.unwrap();
    let Event::Started(_) = next(&mut recording).await else {
        panic!("not started");
    };
    rig.frames(5, 100);
    let (source, before) = chunk(next(&mut recording).await);
    assert_eq!(source, AudioSource::AUDIO_SOURCE_BEFORE_GAIN);
    let (source, heard) = chunk(next(&mut recording).await);
    assert_eq!(source, AudioSource::AUDIO_SOURCE_HEARD);
    assert_eq!((before.len(), heard.len()), (3200, 3200));
    assert_eq!(i16::from_le_bytes([before[0], before[1]]), 100);
    assert_eq!(i16::from_le_bytes([heard[0], heard[1]]), 200);
}

#[tokio::test]
async fn nothing_is_copied_while_no_recording_runs() {
    let rig = rig(None);
    assert!(!rig.shared.recording.active());
    rig.frames(10, 1);
    let recording = rig.recorder.start(WHO, 5).await.unwrap();
    // Nothing from before it started is queued.
    let mut recording = recording;
    let Event::Started(_) = next(&mut recording).await else {
        panic!("not started");
    };
    assert!(
        tokio::time::timeout(Duration::from_millis(50), recording.next())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn it_is_refused_while_muted_calibrating_or_another_recording_runs() {
    let rig = rig(None);
    rig.status.send_modify(|s| s.sw_muted = true);
    let e = rig.recorder.start(WHO, 5).await.err().unwrap();
    assert_eq!(code(e.clone()), ErrorCode::FailedPrecondition);
    assert!(
        e.message
            .unwrap()
            .contains("Turn the microphones on first.")
    );
    rig.status.send_modify(|s| {
        s.sw_muted = false;
        s.hw_muted = true;
    });
    assert!(rig.recorder.refusal().is_some());
    rig.status.send_modify(|s| {
        s.hw_muted = false;
        s.calibration = Some(CalibrationSnapshot {
            started_from: 50,
            events: Vec::new(),
        });
    });
    let e = rig.recorder.start(WHO, 5).await.err().unwrap();
    assert!(e.message.unwrap().contains("calibration"));
    rig.status.send_modify(|s| s.calibration = None);

    let first = rig.recorder.start(WHO, 5).await.unwrap();
    let e = rig.recorder.start(WHO, 5).await.err().unwrap();
    assert_eq!(code(e.clone()), ErrorCode::FailedPrecondition);
    assert!(e.message.unwrap().contains("running already"));
    // When it is gone, the next may start.
    drop(first);
    rig.recorder.start(WHO, 5).await.unwrap();
}

#[tokio::test]
async fn a_caller_that_falls_behind_ends_it_with_what_it_got() {
    let mut rig = rig(None);
    let mut recording = rig.recorder.start(WHO, 30).await.unwrap();
    let Event::Started(_) = next(&mut recording).await else {
        panic!("not started");
    };
    // Two seconds fit: 20 chunks of HEARD. Nothing is read.
    rig.frames(5 * 20, 1);
    assert!(rig.shared.recording.active());
    // One more chunk than the queue holds.
    rig.frames(5, 1);
    let mut heard = 0;
    let (reason, message) = loop {
        match next(&mut recording).await {
            Event::Ended(e) => break (e.reason.as_known().unwrap(), e.message),
            other => {
                let (source, data) = chunk(other);
                if source == AudioSource::AUDIO_SOURCE_HEARD {
                    heard += data.len();
                }
            }
        }
    };
    assert_eq!(reason, RecordingEnd::RECORDING_END_FELL_BEHIND);
    // Once the queue was full nothing was drained: the files hold what the
    // caller had been sent, here none.
    assert_eq!(heard, 0);
    assert_eq!(message, "The browser fell behind: the files hold 0 s.");
    assert!(rig.inputs.try_recv().is_ok(), "the Brain was told it began");
}

#[tokio::test]
async fn stop_ends_it_and_queued_audio_still_goes_out() {
    let rig = rig(None);
    let mut recording = rig.recorder.start(WHO, 30).await.unwrap();
    let Event::Started(_) = next(&mut recording).await else {
        panic!("not started");
    };
    rig.frames(5, 1);
    rig.recorder.stop();
    assert_eq!(
        chunk(next(&mut recording).await).0,
        AudioSource::AUDIO_SOURCE_HEARD
    );
    let (reason, message) = ended(next(&mut recording).await);
    assert_eq!(reason, RecordingEnd::RECORDING_END_STOPPED);
    assert_eq!(message, "The recording was stopped: the files hold 0 s.");
    assert!(recording.next().await.is_none());
    // Nothing running is not an error.
    drop(recording);
    rig.recorder.stop();
}

#[tokio::test]
async fn mute_ends_it_at_once() {
    let rig = rig(None);
    let mut recording = rig.recorder.start(WHO, 30).await.unwrap();
    let Event::Started(_) = next(&mut recording).await else {
        panic!("not started");
    };
    rig.status.send_modify(|s| s.hw_muted = true);
    let (reason, _) = ended(next(&mut recording).await);
    assert_eq!(reason, RecordingEnd::RECORDING_END_MUTED);
}

#[tokio::test]
async fn the_time_is_capped() {
    let rig = rig(None);
    let mut recording = rig.recorder.start(WHO, 1).await.unwrap();
    let Event::Started(started) = next(&mut recording).await else {
        panic!("not started");
    };
    assert_eq!(started.seconds, 1);
    let began = Instant::now();
    let (reason, _) = ended(next(&mut recording).await);
    assert_eq!(reason, RecordingEnd::RECORDING_END_TIME_UP);
    assert!(began.elapsed() >= Duration::from_millis(900));
}

#[tokio::test]
async fn dropping_the_stream_cleans_up() {
    let mut rig = rig(None);
    let recording = rig.recorder.start(WHO, 30).await.unwrap();
    let Some(Input::RecordingStarted { id, .. }) = rig.inputs.recv().await else {
        panic!("the Brain was not told");
    };
    assert!(rig.shared.recording.active());
    // The caller went away: no message is possible.
    drop(recording);
    assert!(!rig.shared.recording.active(), "the taps are off");
    let Some(Input::RecordingEnded { id: ended }) = rig.inputs.recv().await else {
        panic!("the Brain was not told it ended");
    };
    assert_eq!(ended, id);
    assert!(rig.recorder.refusal().is_none(), "the next may start");
}

#[tokio::test]
async fn the_echo_dot_adds_the_lanes_the_reference_and_the_description() {
    let fake = Arc::new(Fake::with_lanes(9));
    let rig = rig(Some(fake.clone()));
    let mut recording = rig.recorder.start(WHO, 30).await.unwrap();
    let Event::Started(started) = next(&mut recording).await else {
        panic!("not started");
    };
    assert_eq!((started.debug, started.lanes), (true, 9));

    let mut writers = std::mem::take(&mut *fake.writers.lock().unwrap());
    assert_eq!(
        writers.iter().map(|w| w.0).collect::<Vec<_>>(),
        [AudioSource::AUDIO_SOURCE_LANES]
    );
    // Whole sample frames of 100 ms, 9 lanes of 3 bytes; lane 8, the
    // reference, reads 0x0807xx.
    let frame: Vec<u8> = (0..9u8)
        .flat_map(|lane| if lane == 8 { [0x55, 7, 8] } else { [lane; 3] })
        .collect();
    writers[0].1.write_all(&frame.repeat(1600)).await.unwrap();
    let mut got = [
        chunk(next(&mut recording).await),
        chunk(next(&mut recording).await),
    ];
    got.sort_by_key(|(source, _)| *source as i32);
    assert_eq!(got[0].0, AudioSource::AUDIO_SOURCE_LANES);
    assert_eq!(got[0].1.len(), 43200);
    assert_eq!(got[1].0, AudioSource::AUDIO_SOURCE_REFERENCE);
    assert_eq!(got[1].1, [7u8, 8].repeat(1600));
}

#[tokio::test]
async fn the_lanes_follow_the_boards_capture_channels() {
    let fake = Arc::new(Fake::with_lanes(6));
    let rig = rig(Some(fake.clone()));
    let mut recording = rig.recorder.start(WHO, 30).await.unwrap();
    let Event::Started(started) = next(&mut recording).await else {
        panic!("not started");
    };
    assert_eq!(started.lanes, 6);
    let mut writers = std::mem::take(&mut *fake.writers.lock().unwrap());
    // 100 ms of 6 lanes of 3 bytes.
    writers[0].1.write_all(&vec![7u8; 28800]).await.unwrap();
    let (source, data) = chunk(next(&mut recording).await);
    assert_eq!(source, AudioSource::AUDIO_SOURCE_LANES);
    assert_eq!(data.len(), 28800);
}

#[tokio::test]
async fn a_debug_capture_that_fails_ends_it_and_says_which() {
    let fake = Arc::new(Fake::with_lanes(9));
    let rig = rig(Some(fake.clone()));
    let mut recording = rig.recorder.start(WHO, 30).await.unwrap();
    let Event::Started(_) = next(&mut recording).await else {
        panic!("not started");
    };
    // pw-record died: its end of the pipe closed.
    let lanes = fake.writers.lock().unwrap().remove(0);
    drop(lanes);
    let (reason, message) = ended(next(&mut recording).await);
    assert_eq!(reason, RecordingEnd::RECORDING_END_FAILED);
    assert_eq!(
        message,
        "Recording the microphone lanes stopped: pw-record ended"
    );
}

#[tokio::test]
async fn dropping_the_stream_stops_the_debug_captures() {
    let fake = Arc::new(Fake::with_lanes(9));
    let rig = rig(Some(fake.clone()));
    let recording = rig.recorder.start(WHO, 30).await.unwrap();
    drop(recording);
    let writers = std::mem::take(&mut *fake.writers.lock().unwrap());
    for (_, mut writer) in writers {
        // The reader is gone with its task: the pipe is closed.
        let closed = tokio::time::timeout(Duration::from_secs(2), async {
            while writer.write_all(&[0u8; 64]).await.is_ok() {
                tokio::task::yield_now().await;
            }
        })
        .await;
        assert!(closed.is_ok(), "the reader was dropped");
    }
}
