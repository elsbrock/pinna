use std::net::Ipv4Addr;

use futures::StreamExt;
use pinna_http::trace::Sink as _;

use super::*;

const FROM: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 23));

fn rig() -> (Arc<Timeline>, mpsc::Receiver<Input>) {
    let (tx, rx) = mpsc::channel(64);
    (Arc::new(Timeline::new(tx)), rx)
}

fn minutes(n: u64) -> Duration {
    Duration::from_secs(n * 60)
}

/// Move the paused clock and let the expiry task run.
async fn pass(d: Duration) {
    tokio::time::advance(d).await;
    tokio::task::yield_now().await;
}

fn started(id: u64) -> trace::Event {
    trace::Event::Started {
        id: Id::new(id),
        kind: Kind::Provider,
        method: "POST".into(),
        url: "https://api.example.com/v1/chat".into(),
        at: Instant::now().into_std(),
        bytes_out: Some(12),
        body_out: Some("{\"a\":1}".into()),
    }
}

fn ended(id: u64, outcome: trace::Outcome, body_in: Option<&str>) -> trace::Event {
    trace::Event::Ended {
        id: Id::new(id),
        at: Instant::now().into_std(),
        bytes_in: 5,
        outcome,
        body_in: body_in.map(str::to_owned),
    }
}

/// What the followers get from the backlog: every event and request.
fn kept(t: &Timeline) -> (Vec<TimelineEvent>, Vec<TimelineRequest>) {
    let backlog = t.backlog();
    let mut events = Vec::new();
    let mut requests = Vec::new();
    for batch in backlog.batches {
        events.extend(batch.events);
        requests.extend(batch.requests);
    }
    (events, requests)
}

fn input(rx: &mut mpsc::Receiver<Input>) -> Option<DebuggingSnapshot> {
    let mut last = None;
    while let Ok(Input::Debugging(snapshot)) = rx.try_recv() {
        last = Some(snapshot);
    }
    last.expect("the Brain was told")
}

#[tokio::test(start_paused = true)]
async fn nothing_older_than_ten_minutes_is_kept() {
    let (t, _rx) = rig();
    t.start(FROM, 480, false, false);
    t.event(EventKind::Wake, "old");
    pass(Duration::from_secs(599)).await;
    t.event(EventKind::Wake, "new");
    assert_eq!(kept(&t).0.len(), 2);
    pass(Duration::from_secs(2)).await;
    t.event(EventKind::Wake, "newest");
    let details: Vec<_> = kept(&t).0.into_iter().map(|e| e.detail).collect();
    assert_eq!(details, ["new", "newest"]);
}

#[tokio::test(start_paused = true)]
async fn an_open_request_is_kept_and_ages_once_it_ends() {
    let (t, _rx) = rig();
    t.start(FROM, 480, false, false);
    t.record(started(1));
    pass(minutes(11)).await;
    t.event(EventKind::Wake, "x");
    let (_, requests) = kept(&t);
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].end_unix_ms, 0);
    t.record(ended(1, trace::Outcome::Done, None));
    pass(Duration::from_secs(599)).await;
    t.event(EventKind::Wake, "y");
    assert_eq!(kept(&t).1.len(), 1);
    pass(Duration::from_secs(2)).await;
    t.event(EventKind::Wake, "z");
    assert!(kept(&t).1.is_empty());
}

#[tokio::test(start_paused = true)]
async fn the_budgets_drop_the_oldest_first() {
    let (t, _rx) = rig();
    t.start(FROM, 480, false, false);
    for i in 0..=MAX_EVENTS {
        t.event(EventKind::Wake, &i.to_string());
    }
    let (events, _) = kept(&t);
    assert_eq!(events.len(), MAX_EVENTS);
    assert_eq!(events[0].detail, "1");

    for id in 1..=MAX_REQUESTS as u64 + 1 {
        t.record(started(id));
    }
    let (_, requests) = kept(&t);
    assert_eq!(requests.len(), MAX_REQUESTS);
    assert_eq!(requests[0].id, 2);
}

#[tokio::test(start_paused = true)]
async fn the_text_budget_takes_the_oldest_bodies_first() {
    let (t, _rx) = rig();
    t.start(FROM, 60, false, true);
    let body = "x".repeat(10 * 1024);
    for id in 1..=300 {
        t.record(started(id));
        t.record(ended(id, trace::Outcome::Done, Some(&body)));
    }
    let (_, requests) = kept(&t);
    assert_eq!(requests.len(), 300, "only the bodies go");
    let text: usize = requests
        .iter()
        .map(|r| r.body_out.len() + r.body_in.len())
        .sum();
    assert!(text <= MAX_TEXT_BYTES, "{text}");
    assert!(requests[0].body_in.is_empty() && requests[0].body_out.is_empty());
    assert_eq!(requests[299].body_in.len(), body.len());
    assert_eq!(t.inner.lock().unwrap().text_bytes, text);
}

#[tokio::test(start_paused = true)]
async fn stop_clears_and_tells_every_follower() {
    let (t, mut rx) = rig();
    let mut follower = t.tx.subscribe();
    t.start(FROM, 15, true, true);
    t.event(EventKind::Wake, "x");
    t.record(started(1));
    assert_eq!(kept(&t).0.len(), 1);
    t.stop(FROM);
    assert!(!t.on() && !t.levels_on());
    let (events, requests) = kept(&t);
    assert!(events.is_empty() && requests.is_empty());
    assert_eq!(input(&mut rx), None);
    let mut cleared = None;
    while let Ok(item) = follower.try_recv() {
        if let Item::State(state) = item {
            cleared = Some(state);
        }
    }
    let state = cleared.unwrap();
    assert!(state.cleared && !state.on);
}

#[tokio::test(start_paused = true)]
async fn time_running_out_turns_off_and_keeps_what_was_kept() {
    let (t, mut rx) = rig();
    t.start(FROM, 15, false, false);
    pass(minutes(14)).await;
    t.event(EventKind::Wake, "x");
    pass(minutes(2)).await;
    assert!(!t.on());
    assert_eq!(input(&mut rx), None);
    assert_eq!(kept(&t).0.len(), 1);
    // It ages out like the rest.
    pass(minutes(9)).await;
    assert!(kept(&t).0.is_empty());
}

#[tokio::test(start_paused = true)]
async fn the_levels_and_the_text_end_at_an_hour_and_the_rest_at_the_end() {
    let (t, mut rx) = rig();
    t.start(FROM, 480, true, true);
    assert_eq!(t.level(), Level::Text);
    let snapshot = input(&mut rx).unwrap();
    assert!(snapshot.levels_until.is_some() && snapshot.text_until.is_some());
    assert_eq!(snapshot.until - snapshot.text_until.unwrap(), minutes(420));

    pass(minutes(61)).await;
    assert!(t.on() && !t.levels_on());
    assert_eq!(t.level(), Level::Metadata);
    let snapshot = input(&mut rx).unwrap();
    assert!(snapshot.levels_until.is_none() && snapshot.text_until.is_none());

    pass(minutes(420)).await;
    assert!(!t.on());
    assert_eq!(t.level(), Level::Off);
}

#[tokio::test(start_paused = true)]
async fn a_short_window_cuts_the_levels_at_its_own_end() {
    let (t, _rx) = rig();
    t.start(FROM, 15, true, false);
    let state = t.state();
    assert_eq!(state.until_unix_ms, state.levels_until_unix_ms);
    assert_eq!(state.text_until_unix_ms, 0);
    assert_eq!(t.level(), Level::Metadata);
}

/// The HTTP trace level is global, so only this timeline sets it.
#[tokio::test(start_paused = true)]
async fn the_http_trace_level_follows_the_switch() {
    let (tx, _rx) = mpsc::channel(64);
    let mut timeline = Timeline::new(tx);
    timeline.drive_http = true;
    let t = Arc::new(timeline);
    t.start(FROM, 480, false, true);
    assert_eq!(trace::level(), Level::Text);
    pass(minutes(61)).await;
    assert_eq!(trace::level(), Level::Metadata);
    t.muted();
    assert_eq!(trace::level(), Level::Metadata);
    pass(minutes(420)).await;
    assert_eq!(trace::level(), Level::Off);
    t.start(FROM, 15, false, false);
    assert_eq!(trace::level(), Level::Metadata);
    t.stop(FROM);
    assert_eq!(trace::level(), Level::Off);
}

#[tokio::test(start_paused = true)]
async fn mute_ends_the_levels_and_the_text_only() {
    let (t, mut rx) = rig();
    t.start(FROM, 60, true, true);
    t.event(EventKind::Wake, "x");
    t.muted();
    assert!(t.on() && !t.levels_on());
    assert_eq!(t.level(), Level::Metadata);
    assert_eq!(kept(&t).0.len(), 1);
    let snapshot = input(&mut rx).unwrap();
    assert!(snapshot.levels_until.is_none() && snapshot.text_until.is_none());
    // A body that arrives now isn't kept.
    t.record(started(1));
    t.record(ended(1, trace::Outcome::Done, Some("said")));
    let (_, requests) = kept(&t);
    assert_eq!(requests[0].body_in, "");
    assert_eq!(requests[0].body_out, "");
}

#[tokio::test(start_paused = true)]
async fn ended_sessions_clear_everything() {
    let (t, mut rx) = rig();
    t.start(FROM, 60, true, true);
    t.event(EventKind::Wake, "x");
    t.sessions_ended();
    assert!(!t.on());
    assert!(kept(&t).0.is_empty());
    assert_eq!(input(&mut rx), None);
}

#[tokio::test(start_paused = true)]
async fn starting_again_replaces_the_times_and_keeps_what_was_kept() {
    let (t, _rx) = rig();
    t.start(FROM, 15, true, false);
    t.event(EventKind::Wake, "x");
    pass(minutes(5)).await;
    t.start(FROM, 60, false, true);
    assert!(!t.levels_on());
    assert_eq!(t.level(), Level::Text);
    assert_eq!(kept(&t).0.len(), 1);
    // The first window's end passes without turning anything off.
    pass(minutes(11)).await;
    assert!(t.on());
    assert_eq!(t.level(), Level::Text);
}

#[tokio::test(start_paused = true)]
async fn the_sink_keeps_one_record_for_a_request() {
    let (t, _rx) = rig();
    t.start(FROM, 60, false, true);
    t.record(started(7));
    pass(Duration::from_millis(30)).await;
    t.record(trace::Event::Response {
        id: Id::new(7),
        status: 200,
        at: Instant::now().into_std(),
    });
    pass(Duration::from_millis(30)).await;
    t.record(trace::Event::FirstBody {
        id: Id::new(7),
        at: Instant::now().into_std(),
    });
    pass(Duration::from_millis(30)).await;
    t.record(ended(7, trace::Outcome::Done, Some("{\"ok\":true}")));
    let (_, requests) = kept(&t);
    assert_eq!(requests.len(), 1);
    let r = &requests[0];
    assert_eq!(r.id, 7);
    assert_eq!(r.method, "POST");
    assert_eq!(r.status, 200);
    assert_eq!(r.bytes_out, Some(12));
    assert_eq!(r.bytes_in, 5);
    assert_eq!(
        r.outcome.as_known(),
        Some(TimelineOutcome::TIMELINE_OUTCOME_DONE)
    );
    assert_eq!(
        r.kind.as_known(),
        Some(TimelineRequestKind::TIMELINE_REQUEST_KIND_PROVIDER)
    );
    assert_eq!(r.response_unix_ms - r.start_unix_ms, 30);
    assert_eq!(r.first_body_unix_ms - r.response_unix_ms, 30);
    assert_eq!(r.end_unix_ms - r.first_body_unix_ms, 30);
    assert_eq!(r.body_out, "{\"a\":1}");
    assert_eq!(r.body_in, "{\"ok\":true}");
}

#[tokio::test(start_paused = true)]
async fn the_sink_maps_how_a_request_ended() {
    let (t, _rx) = rig();
    t.start(FROM, 15, false, false);
    let ends = [
        (
            trace::Outcome::Failed(NetError::Timeout),
            TimelineOutcome::TIMELINE_OUTCOME_TIMEOUT,
        ),
        (
            trace::Outcome::Failed(NetError::Unreachable),
            TimelineOutcome::TIMELINE_OUTCOME_UNREACHABLE,
        ),
        (
            trace::Outcome::Failed(NetError::Tls),
            TimelineOutcome::TIMELINE_OUTCOME_TLS,
        ),
        (
            trace::Outcome::Failed(NetError::Protocol),
            TimelineOutcome::TIMELINE_OUTCOME_PROTOCOL,
        ),
        (
            trace::Outcome::Dropped,
            TimelineOutcome::TIMELINE_OUTCOME_DROPPED,
        ),
    ];
    for (i, (outcome, _)) in ends.iter().enumerate() {
        t.record(started(i as u64 + 1));
        t.record(ended(i as u64 + 1, *outcome, None));
    }
    t.record(started(99));
    let (_, requests) = kept(&t);
    for (r, (_, want)) in requests.iter().zip(ends) {
        assert_eq!(r.outcome.as_known(), Some(want));
    }
    let open = requests.last().unwrap();
    assert_eq!(
        open.outcome.as_known(),
        Some(TimelineOutcome::TIMELINE_OUTCOME_OPEN)
    );
    // No text was asked for, so none is kept.
    assert!(requests.iter().all(|r| r.body_out.is_empty()));
}

#[tokio::test(start_paused = true)]
async fn a_peer_call_is_a_group_request() {
    let (t, _rx) = rig();
    t.start(FROM, 15, false, false);
    let trace::Event::Started {
        id,
        method,
        url,
        at,
        bytes_out,
        body_out,
        ..
    } = started(1)
    else {
        unreachable!()
    };
    t.record(trace::Event::Started {
        id,
        kind: Kind::Peer,
        method,
        url,
        at,
        bytes_out,
        body_out,
    });
    let (_, requests) = kept(&t);
    assert_eq!(
        requests[0].kind.as_known(),
        Some(TimelineRequestKind::TIMELINE_REQUEST_KIND_GROUP)
    );
}

#[tokio::test(start_paused = true)]
async fn nothing_is_recorded_while_off() {
    let (t, _rx) = rig();
    t.record(started(1));
    t.event(EventKind::Wake, "x");
    assert!(kept(&t).0.is_empty() && kept(&t).1.is_empty());
    // A request that started while off and ends while on leaves no record.
    t.start(FROM, 15, false, false);
    t.record(ended(1, trace::Outcome::Done, None));
    assert!(kept(&t).1.is_empty());
}

#[tokio::test(start_paused = true)]
async fn the_backlog_goes_out_in_batches_under_a_megabyte() {
    let (t, _rx) = rig();
    t.start(FROM, 60, false, true);
    // 10,000 bytes of text each: 200 of them fit the 2 MiB budget.
    let body = "y".repeat(5_000);
    for id in 1..=200 {
        t.record(trace::Event::Started {
            id: Id::new(id),
            kind: Kind::Provider,
            method: "POST".into(),
            url: "https://api.example.com/v1/chat".into(),
            at: Instant::now().into_std(),
            bytes_out: None,
            body_out: Some(body.clone()),
        });
        t.record(ended(id, trace::Outcome::Done, Some(&body)));
    }
    for i in 0..3_000 {
        t.event(EventKind::Wake, &format!("event {i}"));
    }
    let backlog = t.backlog();
    let mut requests = 0;
    let mut events = 0;
    for batch in &backlog.batches {
        assert!(batch.encoded_len() < 1024 * 1024, "{}", batch.encoded_len());
        assert!(batch.requests.len() <= BATCH_REQUESTS);
        assert!(batch.events.len() <= BATCH_EVENTS);
        let bodies: usize = batch
            .requests
            .iter()
            .map(|r| r.body_out.len() + r.body_in.len())
            .sum();
        assert!(bodies <= BATCH_BODY_BYTES);
        requests += batch.requests.len();
        events += batch.events.len();
    }
    assert_eq!((requests, events), (200, 3_000));
    assert!(backlog.batches.len() >= 4);
}

#[tokio::test(start_paused = true)]
async fn a_follower_gets_the_state_then_the_backlog_then_what_follows() {
    let (t, _rx) = rig();
    t.start(FROM, 15, false, false);
    t.event(EventKind::Wake, "before");
    let mut stream = Box::pin(t.watch());

    let first = stream.next().await.unwrap();
    let Some(WatchItem::State(state)) = first.item else {
        panic!("the state comes first");
    };
    assert!(state.on);
    assert_eq!((state.bucket_ms, state.keep_seconds), (100, 600));
    let second = stream.next().await.unwrap();
    let Some(WatchItem::Batch(batch)) = second.item else {
        panic!("then what was kept");
    };
    assert_eq!(batch.events.len(), 1);

    // Two events within the gathering time go out as one batch.
    t.event(EventKind::Wake, "a");
    t.event(EventKind::NearMiss, "b");
    let third = stream.next().await.unwrap();
    let Some(WatchItem::Batch(batch)) = third.item else {
        panic!("then what happens");
    };
    let details: Vec<_> = batch.events.iter().map(|e| e.detail.as_str()).collect();
    assert_eq!(details, ["a", "b"]);

    // A state change comes on its own.
    t.stop(FROM);
    let fourth = stream.next().await.unwrap();
    let Some(WatchItem::State(state)) = fourth.item else {
        panic!("the state changed");
    };
    assert!(state.cleared && !state.on);
}

#[tokio::test(start_paused = true)]
async fn a_request_that_changed_twice_goes_out_once_and_in_its_last_form() {
    let (t, _rx) = rig();
    t.start(FROM, 15, false, false);
    let mut stream = Box::pin(t.watch());
    stream.next().await.unwrap();
    t.record(started(1));
    t.record(ended(1, trace::Outcome::Done, None));
    let message = stream.next().await.unwrap();
    let Some(WatchItem::Batch(batch)) = message.item else {
        panic!("a batch");
    };
    assert_eq!(batch.requests.len(), 1);
    assert_eq!(
        batch.requests[0].outcome.as_known(),
        Some(TimelineOutcome::TIMELINE_OUTCOME_DONE)
    );
}

#[tokio::test(start_paused = true)]
async fn live_items_the_backlog_holds_are_skipped() {
    let (t, _rx) = rig();
    t.start(FROM, 15, false, false);
    t.event(EventKind::Wake, "one");
    t.event(EventKind::Wake, "two");
    let after = t.backlog().last;
    let item = |seq, detail: &str| Item::Event(seq, Event::new(EventKind::Wake, detail));
    let batches = live_batches(
        vec![
            item(after - 1, "one"),
            item(after, "two"),
            item(after + 1, "three"),
        ],
        after,
    );
    assert_eq!(batches.len(), 1);
    assert_eq!(batches[0].events.len(), 1);
    assert_eq!(batches[0].events[0].detail, "three");
}

#[tokio::test(start_paused = true)]
async fn a_follower_that_falls_behind_is_cut_off() {
    let (t, _rx) = rig();
    t.start(FROM, 15, false, false);
    let mut stream = Box::pin(t.watch());
    for i in 0..LIVE_CAPACITY + 10 {
        t.event(EventKind::Wake, &i.to_string());
    }
    // The state, then the end of the stream.
    let mut messages = 0;
    while stream.next().await.is_some() {
        messages += 1;
    }
    assert_eq!(messages, 1);
}

#[tokio::test(start_paused = true)]
async fn while_off_a_follower_gets_the_state_and_waits() {
    let (t, _rx) = rig();
    let mut stream = Box::pin(t.watch());
    let Some(WatchItem::State(state)) = stream.next().await.unwrap().item else {
        panic!("the state");
    };
    assert!(!state.on);
    let waited = tokio::time::timeout(Duration::from_secs(5), stream.next()).await;
    assert!(waited.is_err(), "nothing else comes");
}

#[tokio::test(start_paused = true)]
async fn a_busy_brain_still_learns_the_newest_state() {
    let (tx, mut rx) = mpsc::channel(1);
    let t = Arc::new(Timeline::new(tx.clone()));
    // The Brain's queue is full when debugging starts, and still when the
    // levels are muted away.
    tx.try_send(Input::Debugging(None)).unwrap();
    t.start(FROM, 15, true, false);
    t.muted();
    assert!(matches!(rx.recv().await, Some(Input::Debugging(None))));
    // Told late, but with the state as it is by then: on, without levels.
    let told = tokio::time::timeout(Duration::from_secs(1), rx.recv()).await;
    let Ok(Some(Input::Debugging(late))) = told else {
        panic!("the Brain was never told");
    };
    assert!(late.is_some_and(|d| d.levels_until.is_none()));
}

fn kept_levels(t: &Timeline) -> Vec<TimelineLevels> {
    t.backlog()
        .batches
        .into_iter()
        .flat_map(|b| b.levels)
        .collect()
}

fn lane_frame(at: Instant) {
    let detected = Detected {
        threshold: 0.55,
        scores: &[("alexa", 0.3)],
        speech: true,
    };
    levels_frame(at, 0.01, 0.1, 12.0, Some(detected));
}

#[tokio::test(start_paused = true)]
async fn the_lanes_are_kept_once_a_second_while_the_levels_are_on() {
    let t = for_this_test();
    t.start(FROM, 15, true, false);
    // Let the task start its first second.
    tokio::task::yield_now().await;
    let t0 = Instant::now();
    for i in 0..10 {
        lane_frame(t0 + Duration::from_millis(i * 20));
    }
    levels_speaker((t0 + Duration::from_millis(1_250)).into_std(), 0.25);
    // Nothing is finished yet.
    pass(Duration::from_millis(900)).await;
    assert!(kept_levels(&t).is_empty());
    // At 1 s the buckets that ended 0.5 s ago go out, the first five. The
    // speaker's sample for 1.2 s is in a bucket that is still ahead.
    pass(Duration::from_millis(100)).await;
    let kept = kept_levels(&t);
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].after_gain_dbfs.len(), 5);
    assert!((kept[0].after_gain_dbfs[0] + 10.0).abs() < 1e-3);
    assert!(kept[0].speaker_dbfs.iter().all(|s| s.is_nan()));
    assert_eq!(kept[0].words[0].scores[0], 0.3);
    assert_eq!(kept[0].speech, [true, true, false, false, false]);
    // At 2 s the next eight, with that sample in its own bucket, once.
    pass(Duration::from_secs(1)).await;
    let kept = kept_levels(&t);
    assert_eq!(kept.len(), 2);
    assert_eq!(kept[1].speaker_dbfs.len(), 8);
    assert!((kept[1].speaker_dbfs[7] + 6.0206).abs() < 1e-3);
    assert!(kept[1].words.is_empty());
}

#[tokio::test(start_paused = true)]
async fn nothing_is_computed_or_kept_while_the_levels_are_off() {
    let t = for_this_test();
    t.start(FROM, 15, false, true);
    assert!(!levels_on());
    lane_frame(Instant::now());
    levels_speaker(Instant::now().into_std(), 0.25);
    assert!(!t.levels.lock().unwrap().running());
    pass(Duration::from_secs(5)).await;
    assert!(kept_levels(&t).is_empty());
}

#[tokio::test(start_paused = true)]
async fn turning_the_levels_off_keeps_what_is_finished_and_stops() {
    let t = for_this_test();
    t.start(FROM, 15, true, false);
    let t0 = Instant::now();
    lane_frame(t0);
    lane_frame(t0 + Duration::from_millis(120));
    pass(Duration::from_millis(350)).await;
    assert!(kept_levels(&t).is_empty());
    t.muted();
    assert!(!t.levels_on() && !t.levels.lock().unwrap().running());
    // The two buckets that hold frames are kept at once, without the tick.
    let kept = kept_levels(&t);
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].after_gain_dbfs.len(), 2);
    // Nothing follows.
    lane_frame(Instant::now());
    pass(Duration::from_secs(5)).await;
    assert_eq!(kept_levels(&t).len(), 1);
}
