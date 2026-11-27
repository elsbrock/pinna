//! The audio lanes of the timeline, averaged into buckets as they are
//! recorded (ADR 0036, "Audio lanes").
//!
//! The capture loop reports each 20 ms frame and the speaker its output
//! level for each chunk. Both go into buckets of `BUCKET_MS` on a grid that
//! starts when the levels turn on, and a task hands the finished buckets to
//! the timeline once a second. Nothing older is kept here, so the memory is
//! a few seconds of buckets.

use std::collections::VecDeque;
use std::time::Duration;

use pinna_api::{TimelineLevels, TimelineWord};
use tokio::time::Instant;

use super::{BUCKET_MS, Levels};

/// Buckets are emitted this long after they end, so the speaker's samples
/// for them, which are stamped with the time they are heard, have arrived.
pub const LAG: Duration = Duration::from_millis(500);
/// A speaker sample stamped further ahead than this is not believed.
const MAX_AHEAD: usize = 6_000;
/// dBFS of a bucket whose mean square is 0.
const FLOOR_DBFS: f32 = -120.0;

fn bucket_len() -> Duration {
    Duration::from_millis(u64::from(BUCKET_MS))
}

/// A running mean.
#[derive(Clone, Copy, Default)]
struct Mean {
    total: f64,
    count: u32,
}

impl Mean {
    fn add(&mut self, v: f64) {
        self.total += v;
        self.count += 1;
    }

    /// The mean, or `None` when nothing was added.
    fn get(self) -> Option<f64> {
        (self.count > 0).then(|| self.total / f64::from(self.count))
    }

    /// The mean as dBFS, NaN when nothing was added.
    fn dbfs(self) -> f32 {
        self.get().map_or(f32::NAN, |ms| {
            if ms > 0.0 {
                ((10.0 * ms.log10()) as f32).max(FLOOR_DBFS)
            } else {
                FLOOR_DBFS
            }
        })
    }
}

/// The words the detector listened for and the threshold it applied.
#[derive(Clone, PartialEq)]
struct WordSet {
    names: Vec<String>,
    threshold: f32,
}

impl WordSet {
    fn matches(&self, threshold: f32, scores: &[(&str, f32)]) -> bool {
        self.threshold == threshold
            && self.names.len() == scores.len()
            && self.names.iter().zip(scores).all(|(n, (s, _))| n == s)
    }
}

/// What the detector said about one frame.
pub struct Detected<'a> {
    pub threshold: f32,
    /// Each word and the score of the latest block.
    pub scores: &'a [(&'a str, f32)],
    /// Whether any block of the frame had speech in it.
    pub speech: bool,
}

#[derive(Default)]
struct Bucket {
    before: Mean,
    after: Mean,
    gain: Mean,
    speaker: Mean,
    speech: bool,
    words: Option<(WordSet, Vec<f32>)>,
}

/// The buckets from `first` on that are not yet emitted.
#[derive(Default)]
pub struct Accumulator {
    /// Where the grid starts; `None` while the levels are off.
    start: Option<Instant>,
    /// The index of the first bucket in `buckets`.
    first: usize,
    buckets: VecDeque<Bucket>,
}

impl Accumulator {
    /// The levels turned on: a new grid starts at `now`.
    pub fn begin(&mut self, now: Instant) {
        self.start = Some(now);
        self.first = 0;
        self.buckets.clear();
    }

    /// The levels turned off: forget the grid.
    pub fn end(&mut self) {
        self.start = None;
        self.buckets.clear();
    }

    pub fn running(&self) -> bool {
        self.start.is_some()
    }

    /// The bucket for `at`, made if it is ahead; `None` before the grid, in
    /// an emitted bucket, or too far ahead.
    fn bucket(&mut self, at: Instant) -> Option<&mut Bucket> {
        let start = self.start?;
        let since = at.checked_duration_since(start)?;
        let index = (since.as_millis() / u128::from(BUCKET_MS)) as usize;
        let slot = index.checked_sub(self.first)?;
        if slot > MAX_AHEAD {
            return None;
        }
        while self.buckets.len() <= slot {
            self.buckets.push_back(Bucket::default());
        }
        self.buckets.get_mut(slot)
    }

    /// One capture frame: its mean square before and after the gain, the
    /// gain, and what the detector made of it when it ran.
    pub fn frame(
        &mut self,
        at: Instant,
        before: f64,
        after: f64,
        gain_db: f32,
        detected: Option<Detected<'_>>,
    ) {
        let Some(bucket) = self.bucket(at) else {
            return;
        };
        bucket.before.add(before);
        bucket.after.add(after);
        bucket.gain.add(f64::from(gain_db));
        let Some(d) = detected else { return };
        bucket.speech |= d.speech;
        match &mut bucket.words {
            Some((set, max)) if set.matches(d.threshold, d.scores) => {
                for (m, (_, s)) in max.iter_mut().zip(d.scores) {
                    *m = m.max(*s);
                }
            }
            words => {
                let set = WordSet {
                    names: d.scores.iter().map(|(n, _)| (*n).to_owned()).collect(),
                    threshold: d.threshold,
                };
                *words = Some((set, d.scores.iter().map(|(_, s)| *s).collect()));
            }
        }
    }

    /// The speaker's output, heard at `at`: its mean square.
    pub fn speaker(&mut self, at: Instant, mean_square: f32) {
        if let Some(bucket) = self.bucket(at) {
            bucket.speaker.add(f64::from(mean_square));
        }
    }

    /// Take the buckets that ended `lag` before `now`, as messages: one per
    /// run of buckets with the same words.
    pub fn take_finished(&mut self, now: Instant, lag: Duration) -> Vec<Levels> {
        let Some(start) = self.start else {
            return Vec::new();
        };
        let ready = now
            .checked_duration_since(start + lag)
            .map_or(0, |d| (d.as_millis() / u128::from(BUCKET_MS)) as usize);
        let count = ready.saturating_sub(self.first).min(self.buckets.len());
        let mut done = Vec::new();
        let mut current: Option<Message> = None;
        for (i, bucket) in self.buckets.drain(..count).enumerate() {
            let key = bucket.words.as_ref().map(|(set, _)| set);
            if current
                .as_ref()
                .is_some_and(|m| m.set.is_some() && key.is_some() && m.set.as_ref() != key)
            {
                done.extend(current.take().map(Message::finish));
            }
            let m = current.get_or_insert_with(|| {
                Message::new(start + bucket_len() * (self.first + i) as u32)
            });
            m.push(&bucket);
        }
        done.extend(current.map(Message::finish));
        self.first += count;
        done
    }
}

/// A message being built from consecutive buckets.
struct Message {
    levels: Levels,
    /// `None` until a bucket has words; the buckets before it get NaN.
    set: Option<WordSet>,
    scores: Vec<Vec<f32>>,
}

impl Message {
    fn new(start: Instant) -> Self {
        Self {
            levels: Levels {
                start,
                data: TimelineLevels::default(),
            },
            set: None,
            scores: Vec::new(),
        }
    }

    fn push(&mut self, bucket: &Bucket) {
        let data = &mut self.levels.data;
        let before = data.before_gain_dbfs.len();
        if self.set.is_none()
            && let Some((set, _)) = &bucket.words
        {
            self.scores = vec![vec![f32::NAN; before]; set.names.len()];
            self.set = Some(set.clone());
        }
        data.before_gain_dbfs.push(bucket.before.dbfs());
        data.after_gain_dbfs.push(bucket.after.dbfs());
        data.gain_db
            .push(bucket.gain.get().map_or(f32::NAN, |g| g as f32));
        data.speaker_dbfs.push(bucket.speaker.dbfs());
        data.speech.push(bucket.speech);
        for (i, scores) in self.scores.iter_mut().enumerate() {
            scores.push(bucket.words.as_ref().map_or(f32::NAN, |(_, max)| max[i]));
        }
    }

    fn finish(mut self) -> Levels {
        if let Some(set) = self.set {
            self.levels.data.words = set
                .names
                .into_iter()
                .zip(self.scores)
                .map(|(name, scores)| TimelineWord {
                    name,
                    threshold: set.threshold,
                    scores,
                    ..Default::default()
                })
                .collect();
        }
        self.levels
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    fn rig() -> (Accumulator, Instant) {
        let t0 = Instant::now();
        let mut acc = Accumulator::default();
        acc.begin(t0);
        (acc, t0)
    }

    fn words<'a>(threshold: f32, scores: &'a [(&'a str, f32)]) -> Option<Detected<'a>> {
        Some(Detected {
            threshold,
            scores,
            speech: false,
        })
    }

    #[test]
    fn frames_average_as_power_into_dbfs() {
        let (mut acc, t0) = rig();
        // Mean squares 0.1 and 0.01 average to 0.055, not to -15 dBFS.
        acc.frame(t0 + ms(10), 0.1, 0.1, 6.0, None);
        acc.frame(t0 + ms(30), 0.01, 0.01, 8.0, None);
        let out = acc.take_finished(t0 + ms(1000), LAG);
        assert_eq!(out.len(), 1);
        let d = &out[0].data;
        let want = 10.0 * 0.055f32.log10();
        assert!((d.before_gain_dbfs[0] - want).abs() < 1e-3);
        assert!((d.after_gain_dbfs[0] - want).abs() < 1e-3);
        assert_eq!(d.gain_db[0], 7.0);
        assert_eq!(d.after_gain_dbfs.len(), 1);
        assert_eq!(out[0].start, t0);
    }

    #[test]
    fn silence_is_the_floor_and_a_lane_without_data_is_nan() {
        let (mut acc, t0) = rig();
        acc.frame(t0, 0.0, 0.0, 0.0, None);
        acc.speaker(t0 + ms(150), 0.01);
        let d = acc.take_finished(t0 + ms(700), LAG).remove(0).data;
        assert_eq!(d.before_gain_dbfs[0], FLOOR_DBFS);
        // The second bucket has only the speaker: nothing from the capture.
        assert!(d.before_gain_dbfs[1].is_nan());
        assert!(d.after_gain_dbfs[1].is_nan());
        assert!(d.gain_db[1].is_nan());
        assert!(d.speaker_dbfs[0].is_nan());
        assert!((d.speaker_dbfs[1] + 20.0).abs() < 1e-3);
        assert_eq!(d.speech, [false, false]);
        assert!(d.words.is_empty());
    }

    #[test]
    fn a_word_keeps_its_highest_score_and_speech_is_any() {
        let (mut acc, t0) = rig();
        for (i, s) in [0.1, 0.7, 0.3].into_iter().enumerate() {
            acc.frame(
                t0 + ms(i as u64 * 20),
                0.1,
                0.1,
                0.0,
                Some(Detected {
                    threshold: 0.55,
                    scores: &[("alexa", s), ("jarvis", 0.2)],
                    speech: i == 1,
                }),
            );
        }
        // The next bucket has frames, but no detector.
        acc.frame(t0 + ms(110), 0.1, 0.1, 0.0, None);
        let d = acc.take_finished(t0 + ms(800), LAG).remove(0).data;
        assert_eq!(d.words.len(), 2);
        assert_eq!(d.words[0].name, "alexa");
        assert_eq!(d.words[0].threshold, 0.55);
        assert_eq!(d.words[0].scores[0], 0.7);
        assert_eq!(d.words[1].scores[0], 0.2);
        assert!(d.words[0].scores[1].is_nan());
        assert_eq!(d.speech[..2], [true, false]);
    }

    #[test]
    fn a_change_of_words_starts_a_new_message() {
        let (mut acc, t0) = rig();
        // No detector yet, then alexa, then alexa with a new threshold, then
        // another word.
        acc.frame(t0, 0.1, 0.1, 0.0, None);
        acc.frame(t0 + ms(100), 0.1, 0.1, 0.0, words(0.55, &[("alexa", 0.1)]));
        acc.frame(t0 + ms(200), 0.1, 0.1, 0.0, words(0.55, &[("alexa", 0.2)]));
        acc.frame(t0 + ms(300), 0.1, 0.1, 0.0, words(0.5, &[("alexa", 0.3)]));
        acc.frame(t0 + ms(400), 0.1, 0.1, 0.0, words(0.5, &[("jarvis", 0.4)]));
        let out = acc.take_finished(t0 + ms(1500), LAG);
        let shape: Vec<_> = out
            .iter()
            .map(|l| {
                (
                    l.data.after_gain_dbfs.len(),
                    l.data
                        .words
                        .iter()
                        .map(|w| w.name.clone())
                        .collect::<Vec<_>>(),
                )
            })
            .collect();
        assert_eq!(
            shape,
            [
                (3, vec!["alexa".to_owned()]),
                (1, vec!["alexa".to_owned()]),
                (1, vec!["jarvis".to_owned()]),
            ]
        );
        // The first bucket had no detector: NaN in the word it adopted.
        assert!(out[0].data.words[0].scores[0].is_nan());
        assert_eq!(out[0].data.words[0].scores[1..], [0.1, 0.2]);
        // Each message starts where the previous one ended.
        assert_eq!(out[1].start, t0 + ms(300));
        assert_eq!(out[2].start, t0 + ms(400));
    }

    #[test]
    fn a_speaker_sample_ahead_lands_in_its_own_bucket() {
        let (mut acc, t0) = rig();
        acc.frame(t0 + ms(10), 0.1, 0.1, 0.0, None);
        // Heard 2 s from now.
        acc.speaker(t0 + ms(2050), 0.25);
        acc.speaker(t0 + ms(2070), 0.25);
        let early = acc.take_finished(t0 + ms(1000), LAG);
        assert!(early[0].data.speaker_dbfs.iter().all(|s| s.is_nan()));
        // Bucket 20 (2.0 to 2.1 s) is not finished at 2.4 s, the lag included.
        let none = acc.take_finished(t0 + ms(2400), LAG);
        assert_eq!(none[0].data.speaker_dbfs.len(), 14);
        assert!(none[0].data.speaker_dbfs.iter().all(|s| s.is_nan()));
        // At 2.7 s it is, and bucket 19 before it has no sample.
        let late = acc.take_finished(t0 + ms(2700), LAG);
        assert_eq!(late.len(), 1);
        let d = &late[0].data;
        assert_eq!(d.speaker_dbfs.len(), 2);
        assert!(d.speaker_dbfs[0].is_nan());
        assert!((d.speaker_dbfs[1] - 10.0 * 0.25f32.log10()).abs() < 1e-3);
        // Emitted once.
        assert!(acc.take_finished(t0 + ms(2700), LAG).is_empty());
    }

    #[test]
    fn nothing_is_kept_while_off_or_before_the_grid() {
        let mut acc = Accumulator::default();
        let t0 = Instant::now();
        acc.frame(t0, 0.1, 0.1, 0.0, None);
        acc.speaker(t0, 0.1);
        assert!(!acc.running());
        assert!(acc.take_finished(t0 + ms(5000), LAG).is_empty());
        acc.begin(t0 + ms(1000));
        acc.frame(t0, 0.1, 0.1, 0.0, None);
        assert!(acc.take_finished(t0 + ms(5000), LAG).is_empty());
        // Far ahead is not believed either.
        acc.speaker(t0 + Duration::from_secs(3600), 0.1);
        assert!(acc.buckets.is_empty());
    }
}
