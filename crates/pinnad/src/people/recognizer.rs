//! The speaker model's lifecycle and the thread that computes voiceprints
//! (ADR 0038).
//!
//! The model loads only while someone has a usable voiceprint, on a thread of
//! its own: loading takes about 9 s on the Echo Dot (2nd gen), and neither the
//! tokio runtime nor the audio may wait for it. The same thread then answers
//! [`Recognizer::identify`] until the model is dropped. Both run at a lower
//! priority than the audio. Enrollment holds the model for as long as it runs
//! ([`Recognizer::hold`]), so the load overlaps the recording, and computes
//! the voiceprint on the same thread.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Instant;

use pinna_speaker::{EnrollError, Enrolled, Model, Voiceprint};
use sha2::{Digest, Sha256};
use tokio::sync::oneshot;

use crate::timeline;

/// Voiceprint jobs that may wait while one runs.
const QUEUE: usize = 2;

/// Niceness of the loading and voiceprint thread. Pinnad's other threads keep
/// the default, so the audio and the wake word win the cores.
const NICE: libc::c_int = 5;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    /// Nobody has a usable voiceprint, so the model isn't loaded.
    NotNeeded,
    Loading,
    Ready,
    /// The model can't be used; the sentence says why and what to do.
    Unavailable(String),
}

enum Job {
    Identify {
        window: Vec<i16>,
        reply: oneshot::Sender<Result<Voiceprint, String>>,
    },
    Enroll {
        pcm: Vec<i16>,
        reply: oneshot::Sender<Result<Computed, Unenrolled>>,
    },
}

/// What an enrollment computed, and which model computed it.
pub struct Computed {
    pub enrolled: Enrolled,
    /// The model's sha256 (lowercase hex), which the voiceprint file records.
    pub model_sha: String,
}

/// Why an enrollment computed nothing.
#[derive(Debug)]
pub enum Unenrolled {
    /// The model isn't loaded.
    NotLoaded,
    /// Voiceprint jobs already wait.
    Busy,
    /// A check failed, or the model failed to run.
    Failed(EnrollError),
}

struct Shared {
    state: State,
    /// Of the installed model file; known once the loading thread hashed it.
    sha: Option<String>,
    /// The models of the voiceprint files that can be read.
    wanted: Vec<String>,
    /// Enrollments that run: each needs the model, whoever is enrolled.
    holds: usize,
    /// Counts loads and drops, so a load that finishes late knows it was
    /// dropped meanwhile.
    generation: u64,
    /// Present while the model is ready; dropping it ends the thread, which
    /// frees the model.
    jobs: Option<SyncSender<Job>>,
}

impl Shared {
    /// Before the model is hashed, any readable voiceprint may be usable.
    fn needed(&self) -> bool {
        if self.holds > 0 {
            return true;
        }
        match &self.sha {
            Some(sha) => self.wanted.contains(sha),
            None => !self.wanted.is_empty(),
        }
    }

    /// Drop the model when nothing needs it.
    fn drop_unneeded(&mut self) {
        if !self.needed() && self.state != State::NotNeeded {
            self.generation += 1;
            self.jobs = None;
            self.state = State::NotNeeded;
        }
    }
}

/// Keeps the model loaded while it lives, for an enrollment. Dropping the
/// last hold drops the model unless a voiceprint still needs it.
pub struct Hold(Arc<Mutex<Shared>>);

impl Drop for Hold {
    fn drop(&mut self) {
        let mut shared = lock(&self.0);
        shared.holds -= 1;
        shared.drop_unneeded();
    }
}

pub struct Recognizer {
    path: PathBuf,
    shared: Arc<Mutex<Shared>>,
}

fn lock(shared: &Mutex<Shared>) -> MutexGuard<'_, Shared> {
    shared.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Recognizer {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            shared: Arc::new(Mutex::new(Shared {
                state: State::NotNeeded,
                sha: None,
                wanted: Vec::new(),
                holds: 0,
                generation: 0,
                jobs: None,
            })),
        }
    }

    pub fn state(&self) -> State {
        lock(&self.shared).state.clone()
    }

    /// The installed model's sha256 (lowercase hex), once it was hashed.
    pub fn sha(&self) -> Option<String> {
        lock(&self.shared).sha.clone()
    }

    /// Say which models the readable voiceprint files were made with, after
    /// every change to the people. Starts the load when the first usable
    /// voiceprint appears and drops the model when the last one is gone.
    pub fn set_wanted(&self, wanted: Vec<String>) {
        let mut shared = lock(&self.shared);
        shared.wanted = wanted;
        match (shared.needed(), &shared.state) {
            (false, State::NotNeeded) | (true, State::Loading | State::Ready) => {}
            // A failure stays until the reason is gone, so a new enrollment
            // tries again without a restart.
            (true, State::Unavailable(_)) => {}
            (false, _) => shared.drop_unneeded(),
            (true, State::NotNeeded) => self.start_load(&mut shared),
        }
    }

    /// Load the model now if it isn't, and keep it until the hold is
    /// dropped.
    pub fn hold(&self) -> Hold {
        let mut shared = lock(&self.shared);
        shared.holds += 1;
        if shared.state == State::NotNeeded {
            self.start_load(&mut shared);
        }
        Hold(self.shared.clone())
    }

    /// Wait until the model has loaded or failed, for at most `limit`, and
    /// say where it stands. Blocks: call it off the runtime.
    pub fn wait_loaded(&self, limit: std::time::Duration) -> State {
        let end = Instant::now() + limit;
        loop {
            let state = self.state();
            if state != State::Loading || Instant::now() >= end {
                return state;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    fn start_load(&self, shared: &mut Shared) {
        shared.generation += 1;
        shared.state = State::Loading;
        let generation = shared.generation;
        let (path, handle) = (self.path.clone(), self.shared.clone());
        let spawned = std::thread::Builder::new()
            .name("speaker-model".into())
            .spawn(move || run(&path, &handle, generation));
        if let Err(e) = spawned {
            let reason = format!("Can't start the thread that loads the speaker model: {e}.");
            tracing::warn!("{reason}");
            shared.state = State::Unavailable(reason);
        }
    }

    /// The voiceprint of a 1.5 s window, computed on the model's thread. It
    /// answers at once with an error when the model isn't loaded or two
    /// windows already wait.
    pub fn identify(&self, window: Vec<i16>) -> oneshot::Receiver<Result<Voiceprint, String>> {
        let (reply, answer) = oneshot::channel();
        let shared = lock(&self.shared);
        let refused = match &shared.jobs {
            None => Some((reply, "the speaker model isn't loaded")),
            Some(jobs) => match jobs.try_send(Job::Identify { window, reply }) {
                Ok(()) => None,
                Err(TrySendError::Full(Job::Identify { reply, .. })) => Some((reply, "busy")),
                Err(TrySendError::Disconnected(Job::Identify { reply, .. })) => {
                    Some((reply, "the speaker model isn't loaded"))
                }
                Err(_) => unreachable!("the job sent is an identify"),
            },
        };
        if let Some((reply, why)) = refused {
            let _ = reply.send(Err(why.into()));
        }
        answer
    }

    /// The voiceprint of a whole enrollment recording, checks included,
    /// computed on the model's thread: 30 s of speech takes 15 to 20 windows
    /// of about 764 ms on the Echo Dot (2nd gen). It answers at once when
    /// the model isn't loaded or voiceprint jobs already wait.
    pub fn enroll(&self, pcm: Vec<i16>) -> oneshot::Receiver<Result<Computed, Unenrolled>> {
        let (reply, answer) = oneshot::channel();
        let shared = lock(&self.shared);
        let refused = match &shared.jobs {
            None => Some((reply, Unenrolled::NotLoaded)),
            Some(jobs) => match jobs.try_send(Job::Enroll { pcm, reply }) {
                Ok(()) => None,
                Err(TrySendError::Full(Job::Enroll { reply, .. })) => {
                    Some((reply, Unenrolled::Busy))
                }
                Err(TrySendError::Disconnected(Job::Enroll { reply, .. })) => {
                    Some((reply, Unenrolled::NotLoaded))
                }
                Err(_) => unreachable!("the job sent is an enroll"),
            },
        };
        if let Some((reply, why)) = refused {
            let _ = reply.send(Err(why));
        }
        answer
    }
}

fn lower_priority() {
    // SAFETY: setpriority only reads its arguments. Linux niceness is per
    // thread, so this lowers this thread and no other.
    unsafe {
        let tid = libc::syscall(libc::SYS_gettid) as libc::id_t;
        libc::setpriority(libc::PRIO_PROCESS, tid, NICE);
    }
}

fn sha256_of(path: &Path) -> Result<String, String> {
    let bytes = std::fs::read(path).map_err(|e| {
        pinna_speaker::Error::Read {
            path: path.display().to_string(),
            cause: e.to_string(),
        }
        .to_string()
    })?;
    Ok(Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

/// The thread: hash, load, then answer jobs until the model is dropped.
fn run(path: &Path, shared: &Mutex<Shared>, generation: u64) {
    lower_priority();
    let started = Instant::now();
    let loaded = sha256_of(path).and_then(|sha| {
        let mut s = lock(shared);
        if s.generation != generation {
            return Ok(None);
        }
        s.sha = Some(sha);
        if !s.needed() {
            // Every voiceprint is from another model: nothing to load.
            s.state = State::NotNeeded;
            return Ok(None);
        }
        drop(s);
        Model::load(path).map(Some).map_err(|e| e.to_string())
    });
    let model = match loaded {
        Ok(Some(model)) => model,
        Ok(None) => return,
        Err(reason) => {
            let mut s = lock(shared);
            if s.generation == generation {
                tracing::warn!("{reason}");
                timeline::stage(
                    || format!("speaker model: unavailable ({reason})"),
                    started.elapsed(),
                    0,
                );
                s.state = State::Unavailable(reason);
            }
            return;
        }
    };
    let (jobs, queue) = mpsc::sync_channel::<Job>(QUEUE);
    {
        let mut s = lock(shared);
        if s.generation != generation {
            return;
        }
        s.jobs = Some(jobs);
        s.state = State::Ready;
    }
    tracing::info!(
        "speaker model loaded in {:.1} s",
        started.elapsed().as_secs_f32()
    );
    timeline::stage(|| "speaker model: loaded".into(), started.elapsed(), 0);
    while let Ok(job) = queue.recv() {
        match job {
            Job::Identify { window, reply } => {
                let _ = reply.send(model.voiceprint(&window).map_err(|e| e.to_string()));
            }
            Job::Enroll { pcm, reply } => {
                let _ = reply.send(
                    pinna_speaker::enroll(&model, &pcm)
                        .map(|enrolled| Computed {
                            enrolled,
                            model_sha: model.sha256().to_owned(),
                        })
                        .map_err(Unenrolled::Failed),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    const MODEL: &str =
        "../../blobs/models/speaker/3dspeaker_speech_campplus_sv_zh_en_16k-common_advanced.onnx";

    fn wait_for(r: &Recognizer, done: impl Fn(&State) -> bool) -> State {
        for _ in 0..600 {
            let state = r.state();
            if done(&state) {
                return state;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        panic!("the state never settled: {:?}", r.state());
    }

    #[test]
    fn a_missing_model_file_is_unavailable_with_the_reason() {
        let r = Recognizer::new("/nonexistent/speaker.onnx".into());
        r.set_wanted(vec!["aa".repeat(32)]);
        let State::Unavailable(reason) = wait_for(&r, |s| *s != State::Loading) else {
            panic!("expected unavailable");
        };
        assert!(reason.contains("/nonexistent/speaker.onnx"), "{reason}");
        assert!(reason.contains("Check that the file exists"), "{reason}");
    }

    #[test]
    fn nobody_enrolled_loads_nothing() {
        let r = Recognizer::new("/nonexistent/speaker.onnx".into());
        r.set_wanted(vec![]);
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(r.state(), State::NotNeeded);
        assert_eq!(r.sha(), None, "the file was never read");
    }

    #[tokio::test]
    async fn identify_without_a_model_answers_at_once() {
        let r = Recognizer::new("/nonexistent/speaker.onnx".into());
        let err = r.identify(vec![0; 24_000]).await.unwrap().unwrap_err();
        assert_eq!(err, "the speaker model isn't loaded");
    }

    #[test]
    fn voiceprints_of_another_model_load_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("speaker.onnx");
        std::fs::write(&file, b"not a model").unwrap();
        let r = Recognizer::new(file);
        r.set_wanted(vec!["bb".repeat(32)]);
        wait_for(&r, |s| *s == State::NotNeeded && r.sha().is_some());
        assert_eq!(r.state(), State::NotNeeded);
    }

    #[tokio::test]
    async fn the_real_model_loads_answers_and_is_dropped() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(MODEL);
        if !path.exists() {
            eprintln!("SKIP: {} is missing; run `just models`", path.display());
            return;
        }
        let r = Recognizer::new(path.clone());
        r.set_wanted(vec![sha256_of(&path).unwrap()]);
        assert_eq!(r.state(), State::Loading);
        wait_for(&r, |s| *s != State::Loading);
        assert_eq!(r.state(), State::Ready);
        let window: Vec<i16> = (0..24_000)
            .map(|i| ((i as f32 * 0.05).sin() * 8_000.0) as i16)
            .collect();
        let vp = r.identify(window).await.unwrap().unwrap();
        assert!(vp.dim() > 0);
        r.set_wanted(vec![]);
        assert_eq!(r.state(), State::NotNeeded);
        let err = r.identify(vec![0; 24_000]).await.unwrap().unwrap_err();
        assert_eq!(err, "the speaker model isn't loaded");
    }

    #[test]
    fn a_hold_loads_the_model_and_drops_it_when_nobody_needs_it() {
        let r = Recognizer::new("/nonexistent/speaker.onnx".into());
        let hold = r.hold();
        assert_eq!(r.state(), State::Loading);
        let State::Unavailable(_) = wait_for(&r, |s| *s != State::Loading) else {
            panic!("expected unavailable");
        };
        // Nobody is enrolled: letting go forgets the failure too.
        drop(hold);
        assert_eq!(r.state(), State::NotNeeded);
    }

    #[test]
    fn a_hold_keeps_the_model_that_a_voiceprint_does_not_need() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(MODEL);
        if !path.exists() {
            eprintln!("SKIP: {} is missing; run `just models`", path.display());
            return;
        }
        let r = Recognizer::new(path.clone());
        let hold = r.hold();
        wait_for(&r, |s| *s != State::Loading);
        assert_eq!(r.state(), State::Ready);
        // A list change that wants nothing doesn't drop it meanwhile.
        r.set_wanted(vec![]);
        assert_eq!(r.state(), State::Ready);
        drop(hold);
        assert_eq!(r.state(), State::NotNeeded);

        // A voiceprint of the model outlives the hold.
        let sha = sha256_of(&path).unwrap();
        r.set_wanted(vec![sha]);
        let hold = r.hold();
        wait_for(&r, |s| *s != State::Loading);
        drop(hold);
        assert_eq!(r.state(), State::Ready);
    }

    #[tokio::test]
    async fn an_enrollment_without_a_model_answers_at_once() {
        let r = Recognizer::new("/nonexistent/speaker.onnx".into());
        let err = r.enroll(vec![0; 16_000]).await.unwrap().err().unwrap();
        assert!(matches!(err, Unenrolled::NotLoaded));
    }

    #[test]
    fn a_full_queue_answers_busy() {
        let r = Recognizer::new("/nonexistent/speaker.onnx".into());
        // No thread reads this queue, so it fills.
        let (jobs, _queue) = mpsc::sync_channel(QUEUE);
        lock(&r.shared).jobs = Some(jobs);
        let waiting: Vec<_> = (0..QUEUE).map(|_| r.identify(vec![])).collect();
        let err = r.identify(vec![]).blocking_recv().unwrap().unwrap_err();
        assert_eq!(err, "busy");
        drop(waiting);
    }
}
