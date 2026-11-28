use std::path::{Path, PathBuf};

use super::*;
use crate::mic::{MIC_QUEUE, MicRouter, MicTap};
use crate::people::speaker::Decision;
use crate::people::{Paths, PersonEdit};

const MODEL: &str =
    "../../blobs/models/speaker/3dspeaker_speech_campplus_sv_zh_en_16k-common_advanced.onnx";

/// `frames` frames of 10 ms, each speech or not, with the speech kept so far
/// as the tap counts it. Returns the runs that started, with the frame.
fn drive(trigger: &mut Trigger, len: &mut usize, frames: usize, speech: bool) -> Vec<(u8, usize)> {
    let mut started = Vec::new();
    for at in 0..frames {
        if speech {
            *len = (*len + FRAME).min(WINDOW_SAMPLES);
        }
        started.extend(trigger.frame(*len, speech).map(|run| (run, at)));
    }
    started
}

#[test]
fn a_pause_after_half_a_second_of_speech_starts_run_one() {
    let (mut t, mut len) = (Trigger::default(), 0);
    // 0.6 s of speech: nothing starts while it goes on.
    assert!(drive(&mut t, &mut len, 60, true).is_empty());
    assert_eq!(drive(&mut t, &mut len, 3, false), [(1, 0)]);
    // Only once.
    assert!(drive(&mut t, &mut len, 50, false).is_empty());
}

#[test]
fn a_pause_before_half_a_second_waits_for_the_next() {
    let (mut t, mut len) = (Trigger::default(), 0);
    assert!(drive(&mut t, &mut len, 30, true).is_empty());
    assert!(drive(&mut t, &mut len, 5, false).is_empty());
    assert!(drive(&mut t, &mut len, 30, true).is_empty());
    assert_eq!(drive(&mut t, &mut len, 2, false), [(1, 0)]);
}

#[test]
fn continuous_speech_starts_run_one_at_one_and_a_half_seconds() {
    let (mut t, mut len) = (Trigger::default(), 0);
    // The 150th frame is the one that reaches 1.5 s.
    assert_eq!(drive(&mut t, &mut len, 200, true), [(1, 149)]);
}

#[test]
fn run_two_follows_only_a_short_run_one_and_never_a_third() {
    let (mut t, mut len) = (Trigger::default(), 0);
    assert!(drive(&mut t, &mut len, 70, true).is_empty());
    assert_eq!(drive(&mut t, &mut len, 2, false), [(1, 0)]);
    // 0.7 s kept; speech goes on until it reaches 1.5 s.
    assert_eq!(drive(&mut t, &mut len, 200, true), [(2, 79)]);
    for _ in 0..4 {
        assert!(drive(&mut t, &mut len, 30, false).is_empty());
        assert!(drive(&mut t, &mut len, 200, true).is_empty());
    }
}

#[test]
fn run_two_never_follows_a_run_one_with_the_whole_window() {
    let (mut t, mut len) = (Trigger::default(), 0);
    assert_eq!(drive(&mut t, &mut len, 150, true), [(1, 149)]);
    assert!(drive(&mut t, &mut len, 300, true).is_empty());
}

/// 16 kHz mono s16le after a 44-byte header.
fn wav(name: &str) -> Vec<i16> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../pinna-wakeword/tests/fixtures")
        .join(name);
    std::fs::read(path).unwrap()[44..]
        .chunks_exact(2)
        .map(|b| i16::from_le_bytes([b[0], b[1]]))
        .collect()
}

/// 20 ms frames, as the capture loop hands them.
fn frames(pcm: &[i16]) -> Vec<Bytes> {
    pcm.chunks(320)
        .map(|c| Bytes::from(c.iter().flat_map(|s| s.to_le_bytes()).collect::<Vec<u8>>()))
        .collect()
}

struct Rig {
    people: Arc<People>,
    speaker: Arc<Speaker>,
    inputs: mpsc::Receiver<Input>,
    tap: Option<Tap>,
    _dir: tempfile::TempDir,
}

impl Rig {
    fn new(model: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let people = People::open(
            &Paths::beside(&dir.path().join("settings.toml")),
            model.into(),
        );
        let speaker = Arc::new(Speaker::default());
        let (tx, inputs) = mpsc::channel(16);
        let tap = Tap::new(people.clone(), speaker.clone(), tx, Counters::default());
        Self {
            people,
            speaker,
            inputs,
            tap: Some(tap),
            _dir: dir,
        }
    }

    fn add(&self, name: &str) {
        self.people
            .set_person(PersonEdit {
                id: None,
                revision: 0,
                name: name.into(),
                instructions: String::new(),
            })
            .unwrap();
    }

    /// Wait until no run is pending and the outcomes are `count`.
    async fn settled(&self, count: usize) -> Vec<Run> {
        for _ in 0..600 {
            let (runs, pending) = self.speaker.runs();
            if pending == 0 && runs.len() >= count {
                return runs;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("runs didn't settle: {:?}", self.speaker.runs());
    }

    fn published(&mut self) -> Vec<String> {
        let mut out = Vec::new();
        while let Ok(Input::LastSpeaker { value, .. }) = self.inputs.try_recv() {
            out.push(value);
        }
        out
    }
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

#[tokio::test]
async fn nobody_with_a_voiceprint_means_no_run_and_no_model() {
    let mut rig = Rig::new("/nonexistent/speaker.onnx");
    rig.add("Anna");
    let mut tap = rig.tap.take().unwrap();
    tap.start();
    for frame in frames(&wav("alexa-what-time.wav")) {
        tap.frame(&frame);
    }
    tap.end();

    let (runs, pending) = rig.speaker.runs();
    assert_eq!(pending, 0);
    assert_eq!(runs.len(), 1, "{runs:?}");
    assert_eq!(runs[0].outcome, Outcome::NoModel);
    assert!(runs[0].seconds > 0.5, "{}", runs[0].seconds);
    assert_eq!(rig.speaker.decision(), Some(Decision::EveryoneElse));
    assert_eq!(rig.published(), ["Everyone else"]);
}

#[tokio::test]
async fn a_new_session_forgets_the_last_one() {
    let mut rig = Rig::new("/nonexistent/speaker.onnx");
    let mut tap = rig.tap.take().unwrap();
    tap.start();
    tap.end();
    assert_eq!(rig.speaker.decision(), Some(Decision::EveryoneElse));
    tap.start();
    assert_eq!(rig.speaker.decision(), None);
    assert_eq!(rig.speaker.runs().0, []);
}

/// A model that is loaded, with Anna enrolled from `pcm`.
async fn rig_with_anna(model: PathBuf, pcm: Vec<i16>) -> Rig {
    let rig = Rig::new(&model.to_string_lossy());
    rig.add("Anna");
    let hold = rig.people.recognizer().hold();
    let state = rig.people.recognizer().wait_loaded(Duration::from_secs(60));
    assert_eq!(state, State::Ready);
    let computed = match rig.people.recognizer().enroll(pcm).await.unwrap() {
        Ok(computed) => computed,
        Err(e) => panic!("enrollment failed: {e:?}"),
    };
    rig.people
        .save_voiceprint("p1", &computed.enrolled.voiceprint, &computed.model_sha)
        .unwrap();
    drop(hold);
    assert_eq!(rig.people.recognizer().state(), State::Ready);
    rig
}

fn twice_three_times() -> Vec<i16> {
    wav("alexa-twice.wav").repeat(3)
}

#[tokio::test]
async fn under_half_a_second_of_speech_is_too_short() {
    let Some(model) = real_model() else { return };
    let mut rig = rig_with_anna(model, twice_three_times()).await;
    let mut tap = rig.tap.take().unwrap();
    tap.start();
    for frame in frames(&[0; 16_000]) {
        tap.frame(&frame);
    }
    tap.end();
    let runs = rig.settled(1).await;
    assert_eq!(runs.len(), 1, "{runs:?}");
    assert_eq!(runs[0].outcome, Outcome::TooShort);
    assert_eq!(runs[0].seconds, 0.0);
    assert_eq!(rig.speaker.decision(), Some(Decision::EveryoneElse));
}

#[tokio::test]
async fn a_request_through_the_router_is_matched_to_the_enrolled_person() {
    let Some(model) = real_model() else { return };
    let mut rig = rig_with_anna(model, twice_three_times()).await;
    let tap = rig.tap.take().unwrap();
    let mic = MicTap::default();
    let mut router = MicRouter::default().with_speaker_tap(tap);
    // Home Assistant streams the request, then stops.
    for frame in frames(&wav("alexa-what-time.wav")) {
        router.route(frame, true, &mic, None);
    }
    router.route(Bytes::from(vec![0; 320]), false, &mic, None);

    let runs = rig.settled(1).await;
    for run in &runs {
        eprintln!("SCORES: {}", run.describe());
    }
    assert!(runs.len() <= 2, "{runs:?}");
    for run in &runs {
        assert!(
            matches!(
                run.outcome,
                Outcome::Person { .. } | Outcome::Uncertain { .. }
            ),
            "{run:?}"
        );
    }
    assert!(
        runs.iter()
            .any(|r| matches!(&r.outcome, Outcome::Person { name, .. } if name == "Anna")),
        "no run named Anna: {runs:?}"
    );
    assert_eq!(rig.published(), ["Anna"]);
    // A local request starts the next session, and the slot is empty again.
    let (tx, _rx) = mpsc::channel(MIC_QUEUE);
    mic.open(tx);
    router.route(Bytes::from(vec![0; 320]), false, &mic, None);
    assert_eq!(rig.speaker.decision(), None);
}
