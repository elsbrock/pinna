//! Enrolling a voice (ADR 0038).
//!
//! The person stands at the satellite and has pressed Action; the dashboard
//! presents that grant to start. The satellite then listens for 30 s as a
//! recording does ([`crate::recording`]): the frames requests get, after the
//! gain. They stay in memory, at most 30 s, and are never written or sent.
//! The wake word is paused meanwhile, and muting, a press of Action, an
//! announcement, an alarm, the client leaving or pinnad stopping ends it with
//! nothing stored.
//!
//! After 30 s the model's thread computes the voiceprint, and only a
//! voiceprint that passes the checks replaces an earlier one.

use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use connectrpc::ConnectError;
use pinna_api::{AudioSource, RecordingEnd};
use pinna_speaker::EnrollError;
use pinna_vad::Vad;

use super::recognizer::{Hold, State, Unenrolled};
use super::{People, PersonView};
use crate::auth::Auth;
use crate::recording::{Recorder, Recording, SAMPLE_RATE, Step};
use crate::timeline;
use crate::voice::RecordingKind;

/// How long the satellite listens.
pub const SECONDS: u32 = 30;
/// A progress event follows every 200 ms: two chunks of 100 ms.
const PROGRESS_CHUNKS: usize = 2;
const PROGRESS_SAMPLES: usize = SAMPLE_RATE as usize / 5;
/// The VAD's frame, 10 ms.
const FRAME: usize = 160;
const FLOOR_DBFS: f32 = -96.0;
/// How long to wait for a model that is still loading when the recording is
/// over: about 9 s on the Echo Dot (2nd gen), which the 30 s should cover.
const LOAD_WAIT: Duration = Duration::from_secs(30);

const NOT_FOUND: &str = "This person isn't on the list. Reload the page.";
const REMOVED: &str = "The person was removed meanwhile.";
const BUSY: &str = "The speaker model is busy with other voiceprints. Try again in a moment.";
const LOADING: &str = "The speaker model is still loading. Try again in a moment.";
const NOT_LOADED: &str = "The speaker model isn't loaded. Try again.";
const STOPPED: &str = "The speaker model stopped. Try again.";

/// Why nothing was saved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    TooShort,
    TooQuiet,
    Clipped,
    Inconsistent,
    Ended,
    Busy,
    Unavailable,
}

impl Failure {
    fn label(self) -> &'static str {
        match self {
            Failure::TooShort => "too short",
            Failure::TooQuiet => "too quiet",
            Failure::Clipped => "clipped",
            Failure::Inconsistent => "inconsistent",
            Failure::Ended => "ended",
            Failure::Busy => "busy",
            Failure::Unavailable => "unavailable",
        }
    }
}

/// What the stream of an enrollment says.
#[derive(Debug, Clone)]
pub enum Event {
    Started {
        seconds: u32,
    },
    Progress {
        seconds_heard: f32,
        seconds_of_speech: f32,
        level_dbfs: f32,
    },
    Done {
        person: PersonView,
        seconds_of_speech: f32,
        close_to: Vec<String>,
    },
    Failed {
        reason: Failure,
        sentence: String,
    },
}

/// Starts enrollments. The recorder allows one at a time.
pub struct Enroller {
    people: Arc<People>,
    recorder: Arc<Recorder>,
    auth: Arc<Auth>,
}

impl Enroller {
    pub fn new(people: Arc<People>, recorder: Arc<Recorder>, auth: Arc<Auth>) -> Self {
        Self {
            people,
            recorder,
            auth,
        }
    }

    /// Start enrolling person `id` for the browser at `from`, which presents
    /// `grant` from `session`. Refused with a sentence, and nothing captured,
    /// for an unknown person, a grant that isn't good, and whatever a
    /// recording is refused for (ADR 0030), and while the satellite isn't
    /// idle.
    pub async fn start(
        &self,
        from: IpAddr,
        id: &str,
        grant: &str,
        session: Option<&str>,
    ) -> Result<Enrollment, ConnectError> {
        let Some(person) = self.people.person(id) else {
            return Err(ConnectError::not_found(NOT_FOUND));
        };
        // Before the grant is used up: a refusal doesn't cost a press.
        if let State::Unavailable(reason) = self.people.recognizer().state() {
            return Err(ConnectError::unavailable(reason));
        }
        if let Some(refusal) = self.recorder.refusal_for(RecordingKind::Enrollment) {
            return Err(refusal);
        }
        self.auth.take_enroll_grant(grant, session)?;
        // The load takes about 9 s and overlaps the recording.
        let hold = self.people.recognizer().hold();
        let recording = self
            .recorder
            .start_kind(from, SECONDS, RecordingKind::Enrollment)
            .await?;
        let name = person.person.name;
        tracing::info!("enrollment: {name}: started by {from}");
        Ok(Enrollment {
            people: self.people.clone(),
            id: id.to_owned(),
            name,
            from,
            recording: Some(recording),
            hold: Some(hold),
            pcm: Vec::with_capacity(SECONDS as usize * SAMPLE_RATE as usize),
            vad: Vad::new(),
            unvetted: Vec::new(),
            speech_samples: 0,
            chunks: 0,
            began: Instant::now(),
            started_sent: false,
            done: false,
        })
    }
}

/// A running enrollment, as the stream of `Enroll` reads it. Dropping it ends
/// it and stores nothing.
pub struct Enrollment {
    people: Arc<People>,
    id: String,
    name: String,
    from: IpAddr,
    /// Gone once the time is up, so the ring and the taps are free while the
    /// voiceprint is computed.
    recording: Option<Recording>,
    /// The model stays loaded until the enrollment is over.
    hold: Option<Hold>,
    /// What was heard, at most `SECONDS`.
    pcm: Vec<i16>,
    vad: Vad,
    /// Samples that don't fill a VAD frame yet.
    unvetted: Vec<i16>,
    speech_samples: usize,
    chunks: usize,
    began: Instant,
    started_sent: bool,
    done: bool,
}

fn dbfs(samples: &[i16]) -> f32 {
    if samples.is_empty() {
        return FLOOR_DBFS;
    }
    let mean_square = samples
        .iter()
        .map(|&s| f64::from(s) * f64::from(s))
        .sum::<f64>()
        / samples.len() as f64;
    ((10.0 * (mean_square / (32768.0 * 32768.0)).log10()) as f32).max(FLOOR_DBFS)
}

fn seconds(samples: usize) -> f32 {
    samples as f32 / SAMPLE_RATE as f32
}

impl Enrollment {
    /// The next event: `started`, `progress` every 200 ms, then `done` or
    /// `failed`, after which there is none.
    pub async fn next(&mut self) -> Option<Event> {
        if self.done {
            return None;
        }
        if !self.started_sent {
            self.started_sent = true;
            return Some(Event::Started { seconds: SECONDS });
        }
        loop {
            let step = self.recording.as_mut()?.step().await;
            match step {
                Step::Chunk((AudioSource::AUDIO_SOURCE_HEARD, data)) => {
                    self.hear(&data);
                    self.chunks += 1;
                    if self.chunks.is_multiple_of(PROGRESS_CHUNKS) {
                        let tail = self.pcm.len().saturating_sub(PROGRESS_SAMPLES);
                        return Some(Event::Progress {
                            seconds_heard: seconds(self.pcm.len()),
                            seconds_of_speech: seconds(self.speech_samples),
                            level_dbfs: dbfs(&self.pcm[tail..]),
                        });
                    }
                }
                Step::Chunk(_) => {}
                Step::Ended(finish) => {
                    self.done = true;
                    let event = match finish.reason {
                        RecordingEnd::RECORDING_END_TIME_UP => self.compute().await,
                        reason => self.ended(reason, &finish.detail),
                    };
                    // A saved voiceprint keeps the model loaded, if nobody else
                    // needs it the model unloads now.
                    self.hold = None;
                    self.report(&event);
                    return Some(event);
                }
            }
        }
    }

    /// Keep what was heard, up to the time, and count the speech in it the
    /// way `pinna_speaker::speech` does, frame by frame.
    fn hear(&mut self, data: &[u8]) {
        let room = SECONDS as usize * SAMPLE_RATE as usize - self.pcm.len();
        let samples = data
            .chunks_exact(2)
            .take(room)
            .map(|b| i16::from_le_bytes([b[0], b[1]]));
        let before = self.pcm.len();
        self.pcm.extend(samples);
        self.unvetted.extend_from_slice(&self.pcm[before..]);
        let frames = self.unvetted.len() / FRAME;
        for frame in self.unvetted[..frames * FRAME].chunks_exact(FRAME) {
            if self.vad.process(frame) {
                self.speech_samples += FRAME;
            }
        }
        self.unvetted.drain(..frames * FRAME);
    }

    fn ended(&self, reason: RecordingEnd, detail: &str) -> Event {
        let cause = match reason {
            RecordingEnd::RECORDING_END_MUTED => {
                "The microphones were muted. Turn them on to enroll again.".to_owned()
            }
            RecordingEnd::RECORDING_END_FELL_BEHIND => {
                "The dashboard fell behind, so enrollment stopped. Try again.".to_owned()
            }
            RecordingEnd::RECORDING_END_STOPPED if !detail.is_empty() => detail.to_owned(),
            RecordingEnd::RECORDING_END_STOPPED => "The recording was stopped.".to_owned(),
            _ => format!("Enrollment ended: {detail}."),
        };
        Event::Failed {
            reason: Failure::Ended,
            sentence: format!("{cause} Nothing was saved."),
        }
    }

    fn failed(reason: Failure, sentence: impl Into<String>) -> Event {
        Event::Failed {
            reason,
            sentence: sentence.into(),
        }
    }

    /// The time is up: compute the voiceprint on the model's thread, and save
    /// it when it passes the checks.
    async fn compute(&mut self) -> Event {
        // Free the taps and the ring: the computing takes a while.
        self.recording = None;
        let people = self.people.clone();
        // The model may still be loading; the answer below says what is.
        let _ =
            tokio::task::spawn_blocking(move || people.recognizer().wait_loaded(LOAD_WAIT)).await;
        let pcm = std::mem::take(&mut self.pcm);
        let computed = match self.people.recognizer().enroll(pcm).await {
            Ok(Ok(computed)) => computed,
            Ok(Err(Unenrolled::Failed(e))) => {
                let reason = match e {
                    EnrollError::TooShort { .. } => Failure::TooShort,
                    EnrollError::TooQuiet { .. } => Failure::TooQuiet,
                    EnrollError::Clipped { .. } => Failure::Clipped,
                    EnrollError::Inconsistent { .. } => Failure::Inconsistent,
                    EnrollError::Model(_) => Failure::Unavailable,
                };
                return Self::failed(reason, e.to_string());
            }
            Ok(Err(Unenrolled::Busy)) => return Self::failed(Failure::Busy, BUSY),
            Ok(Err(Unenrolled::NotLoaded)) => {
                let sentence = match self.people.recognizer().state() {
                    State::Unavailable(reason) => reason,
                    State::Loading => LOADING.to_owned(),
                    _ => NOT_LOADED.to_owned(),
                };
                return Self::failed(Failure::Unavailable, sentence);
            }
            Err(_) => return Self::failed(Failure::Unavailable, STOPPED),
        };
        if self.people.person(&self.id).is_none() {
            return Self::failed(Failure::Ended, format!("{REMOVED} Nothing was saved."));
        }
        if let Err(e) = self.people.save_voiceprint(
            &self.id,
            &computed.enrolled.voiceprint,
            &computed.model_sha,
        ) {
            return Self::failed(
                Failure::Unavailable,
                format!(
                    "Pinna couldn't save the voiceprint: {e}. Check the satellite's storage and try again."
                ),
            );
        }
        let Some(person) = self.people.person(&self.id) else {
            return Self::failed(Failure::Ended, format!("{REMOVED} Nothing was saved."));
        };
        Event::Done {
            seconds_of_speech: computed.enrolled.speech_seconds,
            close_to: person.close_to.clone(),
            person,
        }
    }

    /// The log and the timeline say how it ended, naming the person.
    fn report(&self, event: &Event) {
        let (what, line) = match event {
            Event::Done {
                seconds_of_speech, ..
            } => (
                "saved".to_owned(),
                format!("saved from {seconds_of_speech:.1} s of speech"),
            ),
            Event::Failed { reason, sentence } => (
                reason.label().to_owned(),
                format!("nothing saved, {}: {sentence}", reason.label()),
            ),
            _ => return,
        };
        let name = &self.name;
        tracing::info!(
            "enrollment: {name}: ended after {:.1} s, {line} (asked by {})",
            self.began.elapsed().as_secs_f32(),
            self.from
        );
        timeline::stage(
            || format!("enrollment: {name}: {what}"),
            self.began.elapsed(),
            0,
        );
    }

    pub fn into_stream(self) -> impl futures::Stream<Item = Event> {
        futures::stream::unfold(self, |mut enrollment| async move {
            let next = enrollment.next().await?;
            Some((next, enrollment))
        })
    }
}

impl Drop for Enrollment {
    fn drop(&mut self) {
        if !self.done {
            let name = &self.name;
            tracing::info!(
                "enrollment: {name}: ended after {:.1} s, nothing saved: the browser went away (asked by {})",
                self.began.elapsed().as_secs_f32(),
                self.from
            );
            timeline::stage(
                || format!("enrollment: {name}: browser went away"),
                self.began.elapsed(),
                0,
            );
        }
    }
}

#[cfg(test)]
mod tests;
