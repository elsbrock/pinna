use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};

use connectrpc::ErrorCode;
use tokio::sync::{mpsc, watch};

use super::*;
use crate::auth::tests::Rig as AuthRig;
use crate::people::voiceprints::{Voice, synthetic};
use crate::people::{Paths, PersonEdit};
use crate::voice::{CalibrationSnapshot, Input, Shared, Snapshot};

const WHO: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 9));
const SHA: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const MODEL: &str =
    "../../blobs/models/speaker/3dspeaker_speech_campplus_sv_zh_en_16k-common_advanced.onnx";

struct Rig {
    enroller: Enroller,
    people: Arc<People>,
    auth: AuthRig,
    shared: Arc<Shared>,
    recorder: Arc<Recorder>,
    inputs: mpsc::Receiver<Input>,
    status: watch::Sender<Snapshot>,
    dir: tempfile::TempDir,
}

impl Rig {
    /// Anna is on the list, without a voiceprint. The speaker model is a file
    /// that is no model: enough for everything that ends before the
    /// computing.
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let model = dir.path().join("speaker.onnx");
        std::fs::write(&model, b"not a model").unwrap();
        Self::with_model(dir, model)
    }

    fn with_model(dir: tempfile::TempDir, model: PathBuf) -> Self {
        let people = People::open(&Paths::beside(&dir.path().join("settings.toml")), model);
        people
            .set_person(PersonEdit {
                id: None,
                revision: 0,
                name: "Anna".into(),
                instructions: String::new(),
            })
            .unwrap();
        let shared = Arc::new(Shared::default());
        let (tx, inputs) = mpsc::channel(8);
        let (status, status_rx) = watch::channel(Snapshot::default());
        let recorder = Recorder::new(shared.clone(), tx, status_rx, None);
        let auth = crate::auth::tests::rig();
        Self {
            enroller: Enroller::new(people.clone(), recorder.clone(), auth.auth.clone()),
            people,
            auth,
            shared,
            recorder,
            inputs,
            status,
            dir,
        }
    }

    async fn grant(&mut self) -> String {
        self.auth
            .press_for(crate::auth::Purpose::Enroll, None)
            .await
    }

    async fn start(&mut self) -> Enrollment {
        let grant = self.grant().await;
        self.enroller.start(WHO, "p1", &grant, None).await.unwrap()
    }

    /// `frames` frames of 20 ms of `value`, as the capture loop feeds them.
    fn frames(&self, frames: usize, value: i16) {
        for _ in 0..frames {
            self.shared.recording.frame(&[0; 320], &[value; 320]);
        }
    }

    fn file(&self) -> PathBuf {
        self.dir.path().join("voiceprints/p1.vp")
    }

    /// Anna enrolled earlier, with a voiceprint that isn't the model's.
    fn enrolled_earlier(&self) -> Vec<u8> {
        self.people
            .save_voiceprint("p1", &synthetic(&[1.0, 0.0]), SHA)
            .unwrap();
        std::fs::read(self.file()).unwrap()
    }

    /// Nothing was captured, and nothing is running.
    fn nothing_runs(&self) {
        assert!(!self.shared.recording.active());
        assert!(
            self.recorder
                .refusal_for(RecordingKind::Enrollment)
                .is_none()
        );
    }
}

async fn next(enrollment: &mut Enrollment) -> Event {
    tokio::time::timeout(Duration::from_secs(5), enrollment.next())
        .await
        .expect("an event in time")
        .expect("not done")
}

async fn refused(rig: &mut Rig, id: &str, grant: &str) -> ConnectError {
    let Err(e) = rig.enroller.start(WHO, id, grant, None).await else {
        panic!("not refused");
    };
    e
}

#[track_caller]
fn failed(event: Event) -> (Failure, String) {
    let Event::Failed { reason, sentence } = event else {
        panic!("not failed: {event:?}");
    };
    (reason, sentence)
}

#[tokio::test]
async fn it_is_refused_with_a_sentence_and_nothing_captured() {
    let mut rig = Rig::new();
    let grant = rig.grant().await;

    let e = refused(&mut rig, "p9", &grant).await;
    assert_eq!(e.code, ErrorCode::NotFound);
    assert_eq!(
        e.message.as_deref(),
        Some("This person isn't on the list. Reload the page.")
    );

    // Each refusal leaves the grant good: it didn't cost a press.
    rig.status.send_modify(|s| s.sw_muted = true);
    let e = refused(&mut rig, "p1", &grant).await;
    assert_eq!(e.code, ErrorCode::FailedPrecondition);
    assert!(
        e.message
            .unwrap()
            .contains("Turn the microphones on first.")
    );
    rig.status.send_modify(|s| {
        s.sw_muted = false;
        s.calibration = Some(CalibrationSnapshot {
            started_from: 50,
            events: Vec::new(),
        });
    });
    let e = refused(&mut rig, "p1", &grant).await;
    assert!(e.message.unwrap().contains("calibration"));
    rig.status.send_modify(|s| {
        s.calibration = None;
        s.quiet = false;
    });
    let e = refused(&mut rig, "p1", &grant).await;
    assert_eq!(e.code, ErrorCode::FailedPrecondition);
    assert!(e.message.unwrap().contains("Wait until it is quiet."));
    rig.status.send_modify(|s| s.quiet = true);
    rig.nothing_runs();
    assert!(rig.inputs.try_recv().is_err(), "the Brain was told nothing");

    // A recording that runs, and another enrollment.
    let recording = rig.recorder.start(WHO, 5).await.unwrap();
    let e = refused(&mut rig, "p1", &grant).await;
    assert!(
        e.message
            .unwrap()
            .contains("A recording is running already.")
    );
    drop(recording);
    let first = rig.enroller.start(WHO, "p1", &grant, None).await.unwrap();
    let again = rig.grant().await;
    let e = refused(&mut rig, "p1", &again).await;
    assert!(e.message.unwrap().contains("Another enrollment"));
    // And a recording is refused while it runs.
    let e = rig.recorder.refusal().unwrap();
    assert!(e.message.unwrap().contains("enrolling"));
    drop(first);
    rig.enroller.start(WHO, "p1", &again, None).await.unwrap();
}

#[tokio::test]
async fn the_grant_must_be_present_for_enrolling_and_fresh() {
    let mut rig = Rig::new();
    for grant in [String::new(), "made up".to_owned()] {
        let e = refused(&mut rig, "p1", &grant).await;
        assert_eq!(e.code, ErrorCode::PermissionDenied);
    }
    // A grant for another purpose.
    let password = rig.auth.press().await;
    let e = refused(&mut rig, "p1", &password).await;
    assert_eq!(e.code, ErrorCode::PermissionDenied);
    // One that is older than 10 s.
    rig.auth.enroll_grant_from("old", Duration::from_secs(11));
    let e = refused(&mut rig, "p1", "old").await;
    assert_eq!(e.code, ErrorCode::PermissionDenied);
    rig.nothing_runs();
    // A good one works once.
    let grant = rig.grant().await;
    let started = rig.enroller.start(WHO, "p1", &grant, None).await.unwrap();
    drop(started);
    let e = refused(&mut rig, "p1", &grant).await;
    assert_eq!(e.code, ErrorCode::PermissionDenied);
}

#[tokio::test]
async fn it_starts_and_reports_progress_every_200_ms_of_audio() {
    let mut rig = Rig::new();
    let mut enrollment = rig.start().await;
    let Some(Input::RecordingStarted { from, kind, .. }) = rig.inputs.recv().await else {
        panic!("the Brain was not told");
    };
    assert_eq!((from, kind), (WHO, RecordingKind::Enrollment));
    assert!(matches!(
        next(&mut enrollment).await,
        Event::Started { seconds: 30 }
    ));
    // 200 ms is ten frames: nine are not enough.
    rig.frames(9, 1_000);
    assert!(
        tokio::time::timeout(Duration::from_millis(50), enrollment.next())
            .await
            .is_err()
    );
    rig.frames(1, 1_000);
    let Event::Progress {
        seconds_heard,
        level_dbfs,
        ..
    } = next(&mut enrollment).await
    else {
        panic!("not progress");
    };
    assert!((seconds_heard - 0.2).abs() < 1e-3, "{seconds_heard}");
    let want = 20.0 * (1_000.0f32 / 32_768.0).log10();
    assert!((level_dbfs - want).abs() < 0.1, "{level_dbfs} vs {want}");
}

#[tokio::test]
async fn muting_ends_it_and_keeps_an_earlier_voiceprint() {
    let mut rig = Rig::new();
    let earlier = rig.enrolled_earlier();
    let mut enrollment = rig.start().await;
    next(&mut enrollment).await;
    rig.status.send_modify(|s| s.hw_muted = true);
    let (reason, sentence) = failed(next(&mut enrollment).await);
    assert_eq!(reason, Failure::Ended);
    assert!(sentence.contains("muted"), "{sentence}");
    assert!(sentence.contains("Nothing was saved."), "{sentence}");
    assert!(enrollment.next().await.is_none());
    assert_eq!(std::fs::read(rig.file()).unwrap(), earlier);
}

#[tokio::test]
async fn a_press_of_action_or_an_announcement_ends_it_and_keeps_an_earlier_voiceprint() {
    for (cause, said) in [
        ("Someone pressed Action.", "pressed Action"),
        ("An announcement started.", "announcement"),
        ("A timer alarm started.", "alarm"),
    ] {
        let mut rig = Rig::new();
        let earlier = rig.enrolled_earlier();
        let mut enrollment = rig.start().await;
        next(&mut enrollment).await;
        // What the Brain does.
        assert!(
            rig.shared
                .recording
                .end(RecordingEnd::RECORDING_END_STOPPED, cause)
        );
        let (reason, sentence) = failed(next(&mut enrollment).await);
        assert_eq!(reason, Failure::Ended);
        assert!(sentence.contains(said), "{sentence}");
        assert_eq!(std::fs::read(rig.file()).unwrap(), earlier);
    }
}

#[tokio::test]
async fn the_stream_ending_stores_nothing_and_frees_everything() {
    let mut rig = Rig::new();
    let earlier = rig.enrolled_earlier();
    let mut enrollment = rig.start().await;
    next(&mut enrollment).await;
    rig.frames(50, 1_000);
    drop(enrollment);
    rig.nothing_runs();
    assert_eq!(std::fs::read(rig.file()).unwrap(), earlier);
    // The model is held only while it runs.
    assert_eq!(rig.people.recognizer().state(), State::NotNeeded);
    assert!(rig.recorder.refusal().is_none());
}

#[tokio::test]
async fn the_hold_loads_the_model_for_the_enrollment_and_drops_it_after() {
    let mut rig = Rig::new();
    assert_eq!(rig.people.recognizer().state(), State::NotNeeded);
    let enrollment = rig.start().await;
    assert_ne!(rig.people.recognizer().state(), State::NotNeeded);
    drop(enrollment);
    assert_eq!(rig.people.recognizer().state(), State::NotNeeded);
}

fn alexa_three_times() -> Vec<i16> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../pinna-wakeword/tests/fixtures/alexa-twice.wav");
    let bytes = std::fs::read(path).unwrap();
    // A 44-byte header, then 16 kHz mono s16le.
    let once: Vec<i16> = bytes[44..]
        .chunks_exact(2)
        .map(|b| i16::from_le_bytes([b[0], b[1]]))
        .collect();
    once.repeat(3)
}

fn real_model() -> Option<PathBuf> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(MODEL);
    if path.exists() {
        Some(path)
    } else {
        eprintln!("SKIP: {} is missing; run `just models`", path.display());
        None
    }
}

/// Feed `pcm` as the capture loop would, 100 ms at a time, while the
/// enrollment is read, and return its last event.
async fn enroll_with(rig: &Rig, mut enrollment: Enrollment, pcm: Vec<i16>) -> Event {
    let shared = rig.shared.clone();
    tokio::spawn(async move {
        for chunk in pcm.chunks_exact(1_600).take(300) {
            for frame in chunk.chunks_exact(320) {
                shared.recording.frame(&[0; 320], frame);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    });
    loop {
        match enrollment.next().await.expect("an end") {
            Event::Started { .. } | Event::Progress { .. } => {}
            last => return last,
        }
    }
}

#[tokio::test(start_paused = true)]
async fn thirty_seconds_of_speech_are_saved_as_a_voiceprint() {
    let Some(model) = real_model() else { return };
    let dir = tempfile::tempdir().unwrap();
    let mut rig = Rig::with_model(dir, model);
    let enrollment = rig.start().await;
    assert_eq!(rig.people.view().people[0].voice, Voice::None);
    let last = enroll_with(&rig, enrollment, alexa_three_times()).await;
    let Event::Done {
        person,
        seconds_of_speech,
        close_to,
    } = last
    else {
        panic!("not done: {last:?}");
    };
    assert_eq!(person.person.name, "Anna");
    assert_eq!(person.voice, Voice::Enrolled);
    assert!(seconds_of_speech >= 10.0, "{seconds_of_speech}");
    assert!(close_to.is_empty());
    assert!(rig.file().exists());
    assert_eq!(rig.people.view().people[0].voice, Voice::Enrolled);
    // The voiceprint keeps the model loaded.
    assert_eq!(rig.people.recognizer().state(), State::Ready);
    rig.nothing_runs();
}

#[tokio::test(start_paused = true)]
async fn a_failed_check_says_why_keeps_the_old_voiceprint_and_unloads_the_model() {
    let Some(model) = real_model() else { return };
    let dir = tempfile::tempdir().unwrap();
    let mut rig = Rig::with_model(dir, model);
    let enrollment = rig.start().await;
    // Thirty seconds of silence.
    let last = enroll_with(&rig, enrollment, vec![0; 480_000]).await;
    let (reason, sentence) = failed(last);
    assert_eq!(reason, Failure::TooShort);
    assert!(sentence.starts_with("Too short"), "{sentence}");
    assert!(!rig.file().exists());
    assert_eq!(rig.people.recognizer().state(), State::NotNeeded);
    rig.nothing_runs();
}
