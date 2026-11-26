//! The debugging timeline: what pinnad keeps for the dashboard while someone
//! has debugging on, and the switch that says so (ADR 0036).
//!
//! The switch is off until `StartDebugging` turns it on for a time. While it
//! is off the taps cost one relaxed atomic load. While it is on, the requests
//! that `pinna_http::trace` reports and the events the Brain and the audio
//! path push are kept in memory for 10 minutes, and every `WatchTimeline`
//! follower hears of them as they happen. Nothing is written to storage.
//!
//! Ageing and order use the monotonic clock. Unix time is stamped only when a
//! message is built, so a clock set late doesn't throw entries away.
// The taps of the later steps use the rest of this module.
#![allow(dead_code)]

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use buffa::Message as _;
use futures::Stream;
use pinna_api::{
    Phase, TimelineBatch, TimelineEvent, TimelineEventKind, TimelineLevels, TimelineOutcome,
    TimelineRequest, TimelineRequestKind, TimelineState, WatchTimelineResponse,
    watch_timeline_response::Item as WatchItem,
};
use pinna_http::NetError;
use pinna_http::trace::{self, Id, Kind, Level};
use tokio::sync::{broadcast, mpsc, watch};
use tokio::time::Instant;

use crate::voice::{DebuggingSnapshot, Input};

/// How long an entry is kept after it ended.
pub const KEEP: Duration = Duration::from_secs(600);
/// The length of a level bucket.
pub const BUCKET_MS: u32 = 100;
/// Metadata runs as long as the window; the levels and the text stop after this.
const LEVELS_AND_TEXT_MAX: Duration = Duration::from_secs(3600);
const MAX_EVENTS: usize = 5_000;
const MAX_REQUESTS: usize = 1_000;
/// Chunks of levels; each holds at least one bucket, and 10 minutes are 6,000.
const MAX_LEVELS: usize = 6_000;
const MAX_TEXT_BYTES: usize = 2 * 1024 * 1024;
/// What a follower that falls behind may miss before it is cut off. The ring
/// keeps its last items until they are overwritten, so it is small: a request
/// goes through it up to four times.
const LIVE_CAPACITY: usize = 512;
/// Live items arriving within this time go out in one batch.
const LIVE_GATHER: Duration = Duration::from_millis(250);
/// A batch stays well under the 4 MiB of a Connect message, and under the
/// 1 MiB the ADR sets.
const BATCH_BYTES: usize = 1024 * 1024 - 64 * 1024;
const BATCH_EVENTS: usize = 1_000;
const BATCH_REQUESTS: usize = 200;
const BATCH_LEVELS: usize = 60;
const BATCH_BODY_BYTES: usize = 512 * 1024;

static TIMELINE: OnceLock<Arc<Timeline>> = OnceLock::new();

#[cfg(test)]
thread_local! {
    static FOR_THIS_TEST: std::cell::Cell<Option<&'static Arc<Timeline>>> =
        const { std::cell::Cell::new(None) };
}

/// The one timeline of this process, from [`init`].
pub fn get() -> Option<&'static Arc<Timeline>> {
    #[cfg(test)]
    if let Some(timeline) = FOR_THIS_TEST.with(std::cell::Cell::get) {
        return Some(timeline);
    }
    TIMELINE.get()
}

/// For a test: the taps on this thread use a timeline of their own, off
/// until it is started, so parallel tests don't hear each other's events.
#[cfg(test)]
pub fn for_this_test() -> Arc<Timeline> {
    let (tx, rx) = mpsc::channel(64);
    std::mem::forget(rx);
    let timeline = Arc::new(Timeline::new(tx));
    FOR_THIS_TEST.with(|t| t.set(Some(Box::leak(Box::new(timeline.clone())))));
    timeline
}

/// Make the process-wide timeline. It tells the Brain when the switch
/// changes and sets the level that `pinna_http::trace` records at.
pub fn init(brain: mpsc::Sender<Input>) -> Arc<Timeline> {
    TIMELINE
        .get_or_init(|| {
            let mut timeline = Timeline::new(brain);
            timeline.drive_http = true;
            Arc::new(timeline)
        })
        .clone()
}

/// Push an event if debugging is on.
pub fn event(kind: EventKind, detail: &str) {
    if let Some(timeline) = get() {
        timeline.event(kind, detail);
    }
}

/// Run `f` on the timeline if debugging is on, so a tap builds nothing
/// while it is off.
pub fn with(f: impl FnOnce(&Timeline)) {
    if let Some(timeline) = get().filter(|t| t.on()) {
        f(timeline);
    }
}

/// The phase the API shows changed.
pub fn phase(phase: Phase) {
    with(|t| {
        let word = match phase {
            Phase::PHASE_IDLE => "idle",
            Phase::PHASE_LISTENING => "listening",
            Phase::PHASE_THINKING => "thinking",
            Phase::PHASE_SPEAKING => "speaking",
            _ => "",
        };
        let mut event = Event::new(EventKind::Phase, word);
        event.phase = phase;
        t.push(event);
    });
}

/// A stage that took `took` and ends now, for a standalone run (0 for none).
pub fn stage(detail: impl FnOnce() -> String, took: Duration, run: u64) {
    with(|t| {
        let end = Instant::now();
        let mut event = Event::new(EventKind::Stage, &detail());
        event.at = end.checked_sub(took).unwrap_or(end);
        event.end = Some(end);
        event.run = run;
        t.push(event);
    });
}

/// The microphones were muted or turned on again: a mute ends the levels and
/// the text for the rest of the window, and is an event.
pub fn muted(muted: bool) {
    if let Some(timeline) = get() {
        if muted {
            timeline.event(EventKind::Muted, "");
            timeline.muted();
        } else {
            timeline.event(EventKind::Unmuted, "");
        }
    }
}

/// Whether the audio levels are being recorded.
pub fn levels_on() -> bool {
    get().is_some_and(|t| t.levels_on())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventKind {
    Phase,
    Muted,
    Unmuted,
    Wake,
    NearMiss,
    Run,
    EndOfSpeech,
    Interrupted,
    Error,
    HomeAssistant,
    Stage,
    Mode,
    WakeWord,
    Calibration,
    Microphone,
}

impl EventKind {
    fn proto(self) -> TimelineEventKind {
        match self {
            EventKind::Phase => TimelineEventKind::TIMELINE_EVENT_KIND_PHASE,
            EventKind::Muted => TimelineEventKind::TIMELINE_EVENT_KIND_MUTED,
            EventKind::Unmuted => TimelineEventKind::TIMELINE_EVENT_KIND_UNMUTED,
            EventKind::Wake => TimelineEventKind::TIMELINE_EVENT_KIND_WAKE,
            EventKind::NearMiss => TimelineEventKind::TIMELINE_EVENT_KIND_NEAR_MISS,
            EventKind::Run => TimelineEventKind::TIMELINE_EVENT_KIND_RUN,
            EventKind::EndOfSpeech => TimelineEventKind::TIMELINE_EVENT_KIND_END_OF_SPEECH,
            EventKind::Interrupted => TimelineEventKind::TIMELINE_EVENT_KIND_INTERRUPTED,
            EventKind::Error => TimelineEventKind::TIMELINE_EVENT_KIND_ERROR,
            EventKind::HomeAssistant => TimelineEventKind::TIMELINE_EVENT_KIND_HOME_ASSISTANT,
            EventKind::Stage => TimelineEventKind::TIMELINE_EVENT_KIND_STAGE,
            EventKind::Mode => TimelineEventKind::TIMELINE_EVENT_KIND_MODE,
            EventKind::WakeWord => TimelineEventKind::TIMELINE_EVENT_KIND_WAKE_WORD,
            EventKind::Calibration => TimelineEventKind::TIMELINE_EVENT_KIND_CALIBRATION,
            EventKind::Microphone => TimelineEventKind::TIMELINE_EVENT_KIND_MICROPHONE,
        }
    }
}

/// Something that happened, or a stage that ran.
#[derive(Clone, Debug)]
pub struct Event {
    pub at: Instant,
    pub kind: EventKind,
    pub detail: String,
    pub phase: Phase,
    pub score: f32,
    pub threshold: f32,
    pub level_dbfs: f32,
    pub gain_db: f32,
    /// For a stage: when it ended. `None` for an instant.
    pub end: Option<Instant>,
    /// A standalone run's id; 0 otherwise.
    pub run: u64,
}

impl Event {
    pub fn new(kind: EventKind, detail: &str) -> Self {
        Self {
            at: Instant::now(),
            kind,
            detail: detail.to_owned(),
            phase: Phase::PHASE_UNSPECIFIED,
            score: 0.0,
            threshold: 0.0,
            level_dbfs: 0.0,
            gain_db: 0.0,
            end: None,
            run: 0,
        }
    }

    fn proto(&self, clock: &Clock) -> TimelineEvent {
        TimelineEvent {
            unix_ms: clock.at(self.at),
            kind: self.kind.proto().into(),
            detail: self.detail.clone(),
            phase: self.phase.into(),
            score: self.score,
            threshold: self.threshold,
            level_dbfs: self.level_dbfs,
            gain_db: self.gain_db,
            end_unix_ms: self.end.map_or(0, |e| clock.at(e)),
            run: self.run,
            ..Default::default()
        }
    }

    /// When it ended, for ageing.
    fn over(&self) -> Instant {
        self.end.unwrap_or(self.at)
    }
}

/// How a request ended, or that it has not.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Open,
    Done,
    Dropped,
    Timeout,
    Unreachable,
    Tls,
    Protocol,
}

impl Outcome {
    fn proto(self) -> TimelineOutcome {
        match self {
            Outcome::Open => TimelineOutcome::TIMELINE_OUTCOME_OPEN,
            Outcome::Done => TimelineOutcome::TIMELINE_OUTCOME_DONE,
            Outcome::Dropped => TimelineOutcome::TIMELINE_OUTCOME_DROPPED,
            Outcome::Timeout => TimelineOutcome::TIMELINE_OUTCOME_TIMEOUT,
            Outcome::Unreachable => TimelineOutcome::TIMELINE_OUTCOME_UNREACHABLE,
            Outcome::Tls => TimelineOutcome::TIMELINE_OUTCOME_TLS,
            Outcome::Protocol => TimelineOutcome::TIMELINE_OUTCOME_PROTOCOL,
        }
    }
}

impl From<trace::Outcome> for Outcome {
    fn from(outcome: trace::Outcome) -> Self {
        match outcome {
            trace::Outcome::Done => Outcome::Done,
            trace::Outcome::Dropped => Outcome::Dropped,
            trace::Outcome::Failed(NetError::Timeout) => Outcome::Timeout,
            trace::Outcome::Failed(NetError::Unreachable) => Outcome::Unreachable,
            trace::Outcome::Failed(NetError::Tls) => Outcome::Tls,
            trace::Outcome::Failed(NetError::Protocol) => Outcome::Protocol,
        }
    }
}

/// One request, as `pinna_http::trace` reported it so far.
#[derive(Clone, Debug)]
pub struct Record {
    pub id: u64,
    pub kind: Kind,
    pub method: String,
    pub url: String,
    pub start: Instant,
    pub status: u16,
    pub response: Option<Instant>,
    pub first_body: Option<Instant>,
    /// `None` while open.
    pub end: Option<Instant>,
    pub bytes_out: Option<u64>,
    pub bytes_in: u64,
    pub outcome: Outcome,
    /// Shared with the copies the live followers get, so the ring doesn't
    /// hold a body of its own for each change of a request.
    pub body_out: Option<Arc<str>>,
    pub body_in: Option<Arc<str>>,
}

impl Record {
    fn text_bytes(&self) -> usize {
        self.body_out.as_ref().map_or(0, |b| b.len()) + self.body_in.as_ref().map_or(0, |b| b.len())
    }

    fn proto(&self, clock: &Clock) -> TimelineRequest {
        let kind = match self.kind {
            Kind::Provider => TimelineRequestKind::TIMELINE_REQUEST_KIND_PROVIDER,
            Kind::Mcp => TimelineRequestKind::TIMELINE_REQUEST_KIND_MCP,
            Kind::Tool => TimelineRequestKind::TIMELINE_REQUEST_KIND_TOOL,
            Kind::Media => TimelineRequestKind::TIMELINE_REQUEST_KIND_MEDIA,
            Kind::Peer => TimelineRequestKind::TIMELINE_REQUEST_KIND_GROUP,
        };
        let at = |t: Option<Instant>| t.map_or(0, |t| clock.at(t));
        TimelineRequest {
            id: self.id,
            kind: kind.into(),
            method: self.method.clone(),
            url: self.url.clone(),
            start_unix_ms: clock.at(self.start),
            status: u32::from(self.status),
            response_unix_ms: at(self.response),
            first_body_unix_ms: at(self.first_body),
            end_unix_ms: at(self.end),
            bytes_out: self.bytes_out,
            bytes_in: self.bytes_in,
            outcome: self.outcome.proto().into(),
            body_out: self.body_out.as_deref().unwrap_or_default().to_owned(),
            body_in: self.body_in.as_deref().unwrap_or_default().to_owned(),
            ..Default::default()
        }
    }
}

/// Consecutive buckets of the audio lanes, from `start`, `BUCKET_MS` apart.
/// Filled by the audio taps.
#[derive(Clone, Debug)]
pub struct Levels {
    pub start: Instant,
    /// `start_unix_ms` is stamped when the message is built.
    pub data: TimelineLevels,
}

impl Levels {
    fn over(&self) -> Instant {
        let buckets = self
            .data
            .after_gain_dbfs
            .len()
            .max(self.data.speaker_dbfs.len());
        self.start + Duration::from_millis(u64::from(BUCKET_MS) * buckets as u64)
    }

    fn proto(&self, clock: &Clock) -> TimelineLevels {
        TimelineLevels {
            start_unix_ms: clock.at(self.start),
            ..self.data.clone()
        }
    }
}

/// What a follower hears after its backlog. Every buffered item carries the
/// sequence number it got when it was kept, so a follower can skip what its
/// backlog holds already.
#[derive(Clone, Debug)]
pub enum Item {
    State(TimelineState),
    Event(u64, Event),
    Request(u64, Record),
    Levels(u64, Levels),
}

impl Item {
    fn seq(&self) -> Option<u64> {
        match self {
            Item::State(_) => None,
            Item::Event(s, _) | Item::Request(s, _) | Item::Levels(s, _) => Some(*s),
        }
    }
}

/// The satellite's clock read once, to turn instants into Unix milliseconds
/// that agree with each other.
struct Clock {
    now: Instant,
    unix_ms: u64,
}

impl Clock {
    fn now() -> Self {
        let unix_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64);
        Self {
            now: Instant::now(),
            unix_ms,
        }
    }

    fn at(&self, at: Instant) -> u64 {
        if at >= self.now {
            self.unix_ms + (at - self.now).as_millis() as u64
        } else {
            self.unix_ms
                .saturating_sub((self.now - at).as_millis() as u64)
        }
    }
}

/// The three times the switch runs to.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Switch {
    until: Option<Instant>,
    levels_until: Option<Instant>,
    text_until: Option<Instant>,
}

impl Switch {
    fn next_deadline(&self) -> Option<Instant> {
        [self.until, self.levels_until, self.text_until]
            .into_iter()
            .flatten()
            .min()
    }
}

struct Stored<T> {
    seq: u64,
    value: T,
}

struct Inner {
    switch: Switch,
    /// Counts the starts, so an older expiry task ends.
    generation: u64,
    seq: u64,
    events: VecDeque<Stored<Event>>,
    /// Ids grow, so the first key is the oldest.
    requests: BTreeMap<u64, Stored<Record>>,
    levels: VecDeque<Stored<Levels>>,
    text_bytes: usize,
}

impl Inner {
    fn next_seq(&mut self) -> u64 {
        self.seq += 1;
        self.seq
    }

    fn clear(&mut self) {
        self.events.clear();
        self.requests.clear();
        self.levels.clear();
        self.text_bytes = 0;
    }

    /// Drop what ended more than [`KEEP`] ago. A request that is still open
    /// stays, whatever its start.
    fn age(&mut self, now: Instant) {
        let old = |at: Instant| now.saturating_duration_since(at) > KEEP;
        while self.events.front().is_some_and(|e| old(e.value.over())) {
            self.events.pop_front();
        }
        while self.levels.front().is_some_and(|l| old(l.value.over())) {
            self.levels.pop_front();
        }
        let gone: Vec<u64> = self
            .requests
            .iter()
            .filter(|(_, r)| r.value.end.is_some_and(old))
            .map(|(id, _)| *id)
            .collect();
        for id in gone {
            self.remove_request(id);
        }
    }

    fn remove_request(&mut self, id: u64) {
        if let Some(r) = self.requests.remove(&id) {
            self.text_bytes -= r.value.text_bytes();
        }
    }

    /// Enforce the budgets, oldest first.
    fn trim(&mut self) {
        while self.events.len() > MAX_EVENTS {
            self.events.pop_front();
        }
        while self.levels.len() > MAX_LEVELS {
            self.levels.pop_front();
        }
        while self.requests.len() > MAX_REQUESTS {
            let Some(id) = self.requests.keys().next().copied() else {
                break;
            };
            self.remove_request(id);
        }
        // Over the text budget: the oldest requests lose their bodies first.
        let mut freed = 0;
        for r in self.requests.values_mut() {
            if self.text_bytes - freed <= MAX_TEXT_BYTES {
                break;
            }
            freed += r.value.text_bytes();
            r.value.body_out = None;
            r.value.body_in = None;
        }
        self.text_bytes -= freed;
    }

    fn state(&self, clock: &Clock, cleared: bool) -> TimelineState {
        let at = |t: Option<Instant>| t.map_or(0, |t| clock.at(t));
        TimelineState {
            on: self.switch.until.is_some(),
            until_unix_ms: at(self.switch.until),
            levels_until_unix_ms: at(self.switch.levels_until),
            text_until_unix_ms: at(self.switch.text_until),
            bucket_ms: BUCKET_MS,
            cleared,
            now_unix_ms: clock.unix_ms,
            keep_seconds: KEEP.as_secs() as u32,
            ..Default::default()
        }
    }
}

pub struct Timeline {
    /// Debugging runs. Read by the taps without the lock.
    on: AtomicBool,
    levels_on: AtomicBool,
    inner: Mutex<Inner>,
    tx: broadcast::Sender<Item>,
    /// Told when the switch changes.
    brain: mpsc::Sender<Input>,
    /// The switch as the Brain should know it, for a late telling.
    latest: watch::Sender<Option<DebuggingSnapshot>>,
    /// Set the level `pinna_http::trace` records at. Only the process-wide
    /// timeline does, because that level is global.
    drive_http: bool,
}

impl Timeline {
    pub fn new(brain: mpsc::Sender<Input>) -> Self {
        Self {
            on: AtomicBool::new(false),
            levels_on: AtomicBool::new(false),
            inner: Mutex::new(Inner {
                switch: Switch::default(),
                generation: 0,
                seq: 0,
                events: VecDeque::new(),
                requests: BTreeMap::new(),
                levels: VecDeque::new(),
                text_bytes: 0,
            }),
            tx: broadcast::channel(LIVE_CAPACITY).0,
            brain,
            latest: watch::channel(None).0,
            drive_http: false,
        }
    }

    pub fn on(&self) -> bool {
        self.on.load(Ordering::Relaxed)
    }

    pub fn levels_on(&self) -> bool {
        self.levels_on.load(Ordering::Relaxed)
    }

    /// Turn debugging on for `minutes`, or replace the time and what it
    /// records while it runs; what was kept stays. The levels and the text
    /// stop after an hour at most.
    pub fn start(self: &Arc<Self>, from: IpAddr, minutes: u32, levels: bool, text: bool) {
        let now = Instant::now();
        let total = Duration::from_secs(u64::from(minutes) * 60);
        let short = total.min(LEVELS_AND_TEXT_MAX);
        let mut inner = self.inner.lock().unwrap();
        inner.generation += 1;
        let generation = inner.generation;
        inner.switch = Switch {
            until: Some(now + total),
            levels_until: levels.then_some(now + short),
            text_until: text.then_some(now + short),
        };
        self.changed(&mut inner, false);
        drop(inner);
        let parts: Vec<&str> = [(levels, "levels"), (text, "text")]
            .into_iter()
            .filter_map(|(on, name)| on.then_some(name))
            .collect();
        if parts.is_empty() {
            tracing::info!("debugging on for {minutes} min from {from}");
        } else {
            tracing::info!(
                "debugging on for {minutes} min from {from}: {}",
                parts.join(", ")
            );
        }
        self.spawn_expiry(generation);
    }

    /// Turn debugging off and clear what it kept, here and in every page.
    pub fn stop(&self, from: IpAddr) {
        self.stop_with(&format!("debugging stopped from {from}"));
    }

    /// The dashboard sessions ended (a new password, a reset, Sign out
    /// everywhere): the next password holder doesn't inherit what was kept.
    pub fn sessions_ended(&self) {
        self.stop_with("debugging stopped: the dashboard sessions ended");
    }

    fn stop_with(&self, message: &str) {
        let mut inner = self.inner.lock().unwrap();
        let was_on = inner.switch.until.is_some();
        inner.switch = Switch::default();
        inner.generation += 1;
        inner.clear();
        self.changed(&mut inner, true);
        drop(inner);
        if was_on {
            tracing::info!("{message}");
        }
    }

    /// The microphones were muted: the levels and the text end now, the rest
    /// goes on.
    pub fn muted(&self) {
        let mut inner = self.inner.lock().unwrap();
        let levels = inner.switch.levels_until.take().is_some();
        let text = inner.switch.text_until.take().is_some();
        if levels || text {
            self.changed(&mut inner, false);
            drop(inner);
            tracing::info!("debugging: the levels and the text end, the microphones are muted");
        }
    }

    /// End what has run out. Nothing is cleared: what was kept ages out.
    /// False when this is not the current run any more, or nothing runs.
    fn tick(&self, generation: u64) -> bool {
        let now = Instant::now();
        let mut inner = self.inner.lock().unwrap();
        if inner.generation != generation || inner.switch.until.is_none() {
            return false;
        }
        let was = inner.switch;
        let due = |t: Option<Instant>| t.filter(|t| *t > now);
        inner.switch.levels_until = due(was.levels_until);
        inner.switch.text_until = due(was.text_until);
        let ended = inner.switch.until.is_some_and(|t| t <= now);
        if ended {
            inner.switch = Switch::default();
        }
        if inner.switch != was {
            self.changed(&mut inner, false);
        }
        let running = inner.switch.until.is_some();
        drop(inner);
        if ended {
            tracing::info!("debugging ended");
        }
        running
    }

    /// A task that turns each part off at its time. A later start makes it end.
    fn spawn_expiry(self: &Arc<Self>, generation: u64) {
        let timeline = self.clone();
        tokio::spawn(async move {
            loop {
                let next = {
                    let inner = timeline.inner.lock().unwrap();
                    if inner.generation != generation {
                        return;
                    }
                    inner.switch.next_deadline()
                };
                let Some(next) = next else { return };
                tokio::time::sleep_until(next).await;
                if !timeline.tick(generation) {
                    return;
                }
            }
        });
    }

    /// After every change of the switch: the flags, the level of the HTTP
    /// trace, the followers, the Brain.
    fn changed(&self, inner: &mut Inner, cleared: bool) {
        let switch = inner.switch;
        self.on.store(switch.until.is_some(), Ordering::Relaxed);
        self.levels_on
            .store(switch.levels_until.is_some(), Ordering::Relaxed);
        if self.drive_http {
            trace::set_level(level(&switch));
        }
        let state = inner.state(&Clock::now(), cleared);
        let _ = self.tx.send(Item::State(state));
        let snapshot = switch.until.map(|until| DebuggingSnapshot {
            until: until.into_std(),
            levels_until: switch.levels_until.map(Instant::into_std),
            text_until: switch.text_until.map(Instant::into_std),
        });
        self.latest.send_replace(snapshot);
        if let Err(mpsc::error::TrySendError::Full(_)) =
            self.brain.try_send(Input::Debugging(snapshot))
        {
            // Status and Home Assistant's sensor must not keep a stale state:
            // wait for room, then send the state as it is by then, so a
            // later change that got through first isn't undone.
            let brain = self.brain.clone();
            let latest = self.latest.subscribe();
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                runtime.spawn(async move {
                    if let Ok(permit) = brain.reserve().await {
                        permit.send(Input::Debugging(*latest.borrow()));
                    }
                });
            }
        }
    }

    /// The level the HTTP trace records at.
    pub fn level(&self) -> Level {
        level(&self.inner.lock().unwrap().switch)
    }

    /// Push an event if debugging is on.
    pub fn event(&self, kind: EventKind, detail: &str) {
        if self.on() {
            self.push(Event::new(kind, detail));
        }
    }

    /// Keep an event, which carries its own time.
    pub fn push(&self, event: Event) {
        if !self.on() {
            return;
        }
        let mut inner = self.inner.lock().unwrap();
        let seq = inner.next_seq();
        let _ = self.tx.send(Item::Event(seq, event.clone()));
        inner.events.push_back(Stored { seq, value: event });
        inner.age(Instant::now());
        inner.trim();
    }

    /// Keep a chunk of the audio lanes while they are recorded.
    pub fn push_levels(&self, levels: Levels) {
        if !self.levels_on() {
            return;
        }
        let mut inner = self.inner.lock().unwrap();
        let seq = inner.next_seq();
        let _ = self.tx.send(Item::Levels(seq, levels.clone()));
        inner.levels.push_back(Stored { seq, value: levels });
        inner.age(Instant::now());
        inner.trim();
    }

    /// What a new follower needs: the live items from now on, and everything
    /// kept until now. Both are read under one lock, so none is missed.
    pub fn watch(self: &Arc<Self>) -> impl Stream<Item = WatchTimelineResponse> + Send + use<> {
        let (rx, backlog) = {
            let mut inner = self.inner.lock().unwrap();
            let rx = self.tx.subscribe();
            let now = Instant::now();
            inner.age(now);
            (rx, backlog(&inner, now))
        };
        let live = Live {
            rx,
            after: backlog.last,
            queue: std::iter::once(state_item(backlog.state))
                .chain(backlog.batches.into_iter().map(batch_item))
                .collect(),
            pending: Vec::new(),
            deadline: None,
        };
        futures::stream::unfold(live, Live::next)
    }

    /// The state and what was kept, for the tests and for `watch`.
    pub fn backlog(&self) -> Backlog {
        let mut inner = self.inner.lock().unwrap();
        let now = Instant::now();
        inner.age(now);
        backlog(&inner, now)
    }

    /// The most recent state, built now.
    pub fn state(&self) -> TimelineState {
        self.inner.lock().unwrap().state(&Clock::now(), false)
    }
}

fn level(switch: &Switch) -> Level {
    if switch.text_until.is_some() {
        Level::Text
    } else if switch.until.is_some() {
        Level::Metadata
    } else {
        Level::Off
    }
}

impl trace::Sink for Timeline {
    fn record(&self, event: trace::Event) {
        if !self.on() {
            return;
        }
        let mut inner = self.inner.lock().unwrap();
        let text = inner.switch.text_until.is_some();
        let id = match &event {
            trace::Event::Started { id, .. }
            | trace::Event::Response { id, .. }
            | trace::Event::FirstBody { id, .. }
            | trace::Event::Ended { id, .. } => Id::get(*id),
        };
        let seq = inner.next_seq();
        if let trace::Event::Started {
            kind,
            method,
            url,
            at,
            bytes_out,
            body_out,
            ..
        } = event
        {
            let record = Record {
                id,
                kind,
                method,
                url,
                start: Instant::from_std(at),
                status: 0,
                response: None,
                first_body: None,
                end: None,
                bytes_out,
                bytes_in: 0,
                outcome: Outcome::Open,
                body_out: body_out.filter(|_| text).map(Arc::from),
                body_in: None,
            };
            inner.text_bytes += record.text_bytes();
            inner.requests.insert(id, Stored { seq, value: record });
        } else {
            let Some(stored) = inner.requests.get_mut(&id) else {
                // It started while debugging was off.
                return;
            };
            stored.seq = seq;
            let before = stored.value.text_bytes();
            match event {
                trace::Event::Response { status, at, .. } => {
                    stored.value.status = status;
                    stored.value.response = Some(Instant::from_std(at));
                }
                trace::Event::FirstBody { at, .. } => {
                    stored.value.first_body = Some(Instant::from_std(at));
                }
                trace::Event::Ended {
                    at,
                    bytes_in,
                    outcome,
                    body_in,
                    ..
                } => {
                    stored.value.end = Some(Instant::from_std(at));
                    stored.value.bytes_in = bytes_in;
                    stored.value.outcome = outcome.into();
                    stored.value.body_in = body_in.filter(|_| text).map(Arc::from);
                }
                trace::Event::Started { .. } => {}
            }
            let after = stored.value.text_bytes();
            inner.text_bytes = inner.text_bytes + after - before;
        }
        if let Some(stored) = inner.requests.get(&id) {
            let _ = self.tx.send(Item::Request(seq, stored.value.clone()));
        }
        inner.age(Instant::now());
        inner.trim();
    }
}

/// The state and what was kept at one moment, ready to send.
pub struct Backlog {
    pub state: TimelineState,
    pub batches: Vec<TimelineBatch>,
    /// The newest sequence number the batches hold.
    pub last: u64,
}

fn backlog(inner: &Inner, now: Instant) -> Backlog {
    let clock = Clock {
        now,
        ..Clock::now()
    };
    let mut batches = Batcher::default();
    // Entries are in the order they were kept, oldest first.
    for e in &inner.events {
        batches.event(e.value.proto(&clock));
    }
    for r in inner.requests.values() {
        batches.request(r.value.proto(&clock));
    }
    for l in &inner.levels {
        batches.levels(l.value.proto(&clock));
    }
    Backlog {
        state: inner.state(&clock, false),
        batches: batches.finish(),
        last: inner.seq,
    }
}

/// Splits what is sent into batches that stay under the limits.
#[derive(Default)]
struct Batcher {
    done: Vec<TimelineBatch>,
    batch: TimelineBatch,
    size: usize,
    bodies: usize,
}

impl Batcher {
    fn flush(&mut self) {
        let batch = std::mem::take(&mut self.batch);
        if !(batch.events.is_empty() && batch.requests.is_empty() && batch.levels.is_empty()) {
            self.done.push(batch);
        }
        self.size = 0;
        self.bodies = 0;
    }

    /// Room for one more of `len` bytes, in a list of `count` of at most `max`.
    fn room(&mut self, count: usize, max: usize, len: usize, bodies: usize) {
        let full = count >= max
            || (self.size > 0 && self.size + len > BATCH_BYTES)
            || (self.bodies > 0 && self.bodies + bodies > BATCH_BODY_BYTES);
        if full {
            self.flush();
        }
        self.size += len;
        self.bodies += bodies;
    }

    fn event(&mut self, event: TimelineEvent) {
        self.room(
            self.batch.events.len(),
            BATCH_EVENTS,
            event.encoded_len() as usize,
            0,
        );
        self.batch.events.push(event);
    }

    fn request(&mut self, request: TimelineRequest) {
        let bodies = request.body_out.len() + request.body_in.len();
        let len = request.encoded_len() as usize;
        self.room(self.batch.requests.len(), BATCH_REQUESTS, len, bodies);
        self.batch.requests.push(request);
    }

    fn levels(&mut self, levels: TimelineLevels) {
        self.room(
            self.batch.levels.len(),
            BATCH_LEVELS,
            levels.encoded_len() as usize,
            0,
        );
        self.batch.levels.push(levels);
    }

    fn finish(mut self) -> Vec<TimelineBatch> {
        self.flush();
        self.done
    }
}

fn state_item(state: TimelineState) -> WatchTimelineResponse {
    WatchTimelineResponse {
        item: Some(WatchItem::State(Box::new(state))),
        ..Default::default()
    }
}

fn batch_item(batch: TimelineBatch) -> WatchTimelineResponse {
    WatchTimelineResponse {
        item: Some(WatchItem::Batch(Box::new(batch))),
        ..Default::default()
    }
}

/// The batches of live items: what arrives within [`LIVE_GATHER`] goes out as
/// one, a request that changed more than once only in its last form.
fn live_batches(items: Vec<Item>, after: u64) -> Vec<TimelineBatch> {
    let clock = Clock::now();
    let mut batches = Batcher::default();
    let mut latest: HashMap<u64, usize> = HashMap::new();
    let items: Vec<Item> = items
        .into_iter()
        .filter(|i| i.seq().is_some_and(|s| s > after))
        .collect();
    for (i, item) in items.iter().enumerate() {
        if let Item::Request(_, r) = item {
            latest.insert(r.id, i);
        }
    }
    for (i, item) in items.iter().enumerate() {
        match item {
            Item::Event(_, e) => batches.event(e.proto(&clock)),
            Item::Request(_, r) if latest[&r.id] == i => batches.request(r.proto(&clock)),
            Item::Request(..) | Item::State(_) => {}
            Item::Levels(_, l) => batches.levels(l.proto(&clock)),
        }
    }
    batches.finish()
}

/// A follower after its backlog: it ends when it falls behind.
struct Live {
    rx: broadcast::Receiver<Item>,
    /// Items up to this sequence number are in the backlog.
    after: u64,
    queue: VecDeque<WatchTimelineResponse>,
    pending: Vec<Item>,
    deadline: Option<Instant>,
}

impl Live {
    fn flush(&mut self) {
        let items = std::mem::take(&mut self.pending);
        self.deadline = None;
        self.queue
            .extend(live_batches(items, self.after).into_iter().map(batch_item));
    }

    async fn next(mut self) -> Option<(WatchTimelineResponse, Self)> {
        loop {
            if let Some(message) = self.queue.pop_front() {
                return Some((message, self));
            }
            let received = match self.deadline {
                Some(deadline) => tokio::select! {
                    received = self.rx.recv() => Some(received),
                    () = tokio::time::sleep_until(deadline) => None,
                },
                None => Some(self.rx.recv().await),
            };
            match received {
                None => self.flush(),
                Some(Ok(Item::State(state))) => {
                    // What came before the state goes out before it.
                    self.flush();
                    self.queue.push_back(state_item(state));
                }
                Some(Ok(item)) => {
                    if self.pending.is_empty() {
                        self.deadline = Some(Instant::now() + LIVE_GATHER);
                    }
                    self.pending.push(item);
                }
                // Behind or gone: the page connects again and gets the backlog.
                Some(Err(_)) => return None,
            }
        }
    }
}

#[cfg(test)]
mod tests;
