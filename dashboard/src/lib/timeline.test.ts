import assert from "node:assert/strict";
import { test } from "node:test";

import {
  applyBatch,
  applyState,
  debuggingHint,
  decimate,
  dropData,
  emptyTimeline,
  EV,
  eventWord,
  exportName,
  filterRequests,
  formatDuration,
  formatSize,
  holdsText,
  levelSeries,
  linePath,
  markerShape,
  muteSpans,
  OUTCOME,
  packRows,
  phaseSections,
  prune,
  requestBars,
  requestFailed,
  requestLabel,
  recordedWords,
  requestRows,
  satelliteNow,
  speechSpans,
  timeLeft,
  timelineExport,
  timeTicks,
  timeScale,
  valueAt,
  wordSeries,
  type TimelineEvent,
  type TimelineLevels,
  type TimelineRequest,
  type TimelineState,
} from "./timeline.ts";

const NOW = 1_000_000_000_000;
const MIN = 60_000;

const state = (over: Partial<TimelineState> = {}): TimelineState => ({
  on: true,
  untilMs: NOW + 15 * MIN,
  levelsUntilMs: 0,
  textUntilMs: 0,
  bucketMs: 100,
  cleared: false,
  nowMs: NOW,
  keepSeconds: 600,
  ...over,
});
const event = (over: Partial<TimelineEvent> = {}): TimelineEvent => ({
  unixMs: NOW,
  kind: EV.phase,
  detail: "",
  phase: 1,
  score: 0,
  threshold: 0,
  levelDbfs: 0,
  gainDb: 0,
  endMs: 0,
  run: 0,
  ...over,
});
const request = (over: Partial<TimelineRequest> = {}): TimelineRequest => ({
  id: 1,
  kind: 1,
  method: "POST",
  url: "https://api.example.com/v1/messages",
  startMs: NOW,
  status: 200,
  responseMs: NOW + 800,
  firstBodyMs: NOW + 812,
  endMs: NOW + 2100,
  bytesOut: 1200,
  bytesIn: 4200,
  outcome: OUTCOME.done,
  bodyOut: "",
  bodyIn: "",
  ...over,
});
const levels = (over: Partial<TimelineLevels> = {}): TimelineLevels => ({
  startMs: NOW,
  bucketMs: 100,
  beforeGain: [-50, -40, -30],
  afterGain: [-40, -30, -20],
  gain: [10, 10, 10],
  speaker: [-90, -90, -90],
  speech: [false, true, true],
  words: [],
  ...over,
});
const batch = (over: Partial<{ events: TimelineEvent[]; requests: TimelineRequest[]; levels: TimelineLevels[] }> = {}) => ({
  events: [],
  requests: [],
  levels: [],
  ...over,
});

test("a state lines the page's clock up with the satellite's", () => {
  const tl = applyState(emptyTimeline(), state({ nowMs: NOW + 5_000 }), NOW);
  assert.equal(tl.offset, 5_000);
  assert.equal(satelliteNow(tl, NOW + 1_000), NOW + 6_000);
});

test("cleared drops everything, and a new connection's backlog replaces what was held", () => {
  let tl = applyState(emptyTimeline(), state(), NOW);
  tl = applyBatch(tl, batch({ events: [event()], requests: [request()], levels: [levels()] }), NOW);
  assert.equal(tl.events.length + tl.requests.length + tl.levels.length, 3);
  const cleared = applyState(tl, state({ on: false, cleared: true }), NOW);
  assert.deepEqual([cleared.events, cleared.requests, cleared.levels], [[], [], []]);
  assert.equal(cleared.state?.on, false);
  const fresh = dropData(tl);
  assert.deepEqual([fresh.events, fresh.requests, fresh.levels], [[], [], []]);
  assert.equal(fresh.state, tl.state, "the state stays until the new one arrives");
});

test("a batch appends events and levels, and a request replaces the one with its id", () => {
  let tl = applyState(emptyTimeline(), state(), NOW);
  tl = applyBatch(tl, batch({ events: [event()], requests: [request({ endMs: 0, status: 0, outcome: 0 })] }), NOW);
  tl = applyBatch(tl, batch({ events: [event({ unixMs: NOW + 1 })], levels: [levels()] }), NOW);
  tl = applyBatch(tl, batch({ requests: [request(), request({ id: 2, startMs: NOW + 5 })] }), NOW);
  assert.equal(tl.events.length, 2);
  assert.equal(tl.levels.length, 1);
  assert.equal(tl.requests.length, 2);
  assert.equal(tl.requests.find((r) => r.id === 1)?.status, 200, "replaced, not appended");
});

test("nothing older than keep_seconds stays; an open request does; a stage that ends inside does", () => {
  let tl = applyState(emptyTimeline(), state(), NOW);
  const old = NOW - 11 * MIN;
  tl = applyBatch(
    tl,
    batch({
      events: [
        event({ unixMs: old }),
        event({ unixMs: NOW - 9 * MIN }),
        event({ unixMs: old, kind: EV.stage, endMs: NOW - 5 * MIN }),
      ],
      requests: [
        request({ id: 1, startMs: old, endMs: old + 1000 }),
        request({ id: 2, startMs: old, endMs: 0, status: 0, outcome: 0 }),
        request({ id: 3, startMs: NOW - 2 * MIN, endMs: NOW - MIN }),
      ],
      levels: [levels({ startMs: old }), levels({ startMs: NOW - MIN })],
    }),
    NOW,
  );
  assert.equal(tl.events.length, 2);
  assert.deepEqual(tl.requests.map((r) => r.id), [2, 3]);
  assert.equal(tl.levels.length, 1);
  assert.equal(tl.levels[0].startMs, NOW - MIN);
});

test("pruning on a timer follows the satellite's clock and trims a chunk's old buckets", () => {
  const tl = applyState(emptyTimeline(), state({ nowMs: NOW + 10 * MIN }), NOW + 10 * MIN);
  const held = { ...tl, levels: [levels({ startMs: NOW + 10 * MIN - 600_000 - 100 })] };
  const pruned = prune(held, NOW + 10 * MIN);
  assert.equal(pruned.levels[0].beforeGain.length, 2);
  assert.equal(pruned.levels[0].startMs, NOW + 10 * MIN - 600_000);
  assert.equal(pruned.levels[0].words.length, 0);
  assert.equal(prune(pruned, NOW + 10 * MIN), pruned, "nothing to drop returns the same value");
  const gone = prune({ ...tl, levels: [levels({ startMs: NOW - 700_000 })] }, NOW + 10 * MIN);
  assert.equal(gone.levels.length, 0);
});

test("a phase lasts until the next, and the last until now", () => {
  const events = [
    event({ unixMs: NOW + 3000, phase: 3 }),
    event({ unixMs: NOW, phase: 2 }),
    event({ unixMs: NOW + 1000, kind: EV.wake }),
    event({ unixMs: NOW + 5000, phase: 1 }),
  ];
  assert.deepEqual(phaseSections(events, NOW + 9000), [
    { phase: 2, startMs: NOW, endMs: NOW + 3000 },
    { phase: 3, startMs: NOW + 3000, endMs: NOW + 5000 },
    { phase: 1, startMs: NOW + 5000, endMs: NOW + 9000 },
  ]);
  assert.deepEqual(phaseSections([], NOW), []);
});

test("mute spans run from MUTED to UNMUTED, or to now", () => {
  const e = (kind: number, at: number) => event({ kind, unixMs: NOW + at });
  assert.deepEqual(muteSpans([e(EV.muted, 1000), e(EV.unmuted, 4000), e(EV.muted, 6000)], NOW + 9000), [
    { startMs: NOW + 1000, endMs: NOW + 4000 },
    { startMs: NOW + 6000, endMs: NOW + 9000 },
  ]);
  assert.deepEqual(muteSpans([e(EV.muted, 1000), e(EV.muted, 2000), e(EV.unmuted, 3000)], NOW + 9000), [
    { startMs: NOW + 1000, endMs: NOW + 3000 },
  ]);
  assert.deepEqual(muteSpans([e(EV.unmuted, 1000)], NOW + 9000), [], "a mute from before the data");
  assert.deepEqual(muteSpans([e(EV.unmuted, 1000)], NOW + 9000, NOW), [{ startMs: NOW, endMs: NOW + 1000 }]);
});

test("a time scale maps both ways", () => {
  const s = timeScale(1000, 2000, 100, 300);
  assert.equal(s.x(1500), 200);
  assert.equal(s.t(300), 2000);
  assert.equal(s.t(s.x(1234)), 1234);
});

test("a level series breaks where chunks leave a gap", () => {
  const a = levels({ startMs: NOW });
  const b = levels({ startMs: NOW + 1000 });
  const pts = levelSeries([b, a], (l) => l.beforeGain);
  assert.equal(pts.length, 7);
  assert.ok(Number.isNaN(pts[3].v));
  assert.equal(pts[3].t, NOW + 300);
  assert.equal(pts[4].t, NOW + 1000);
  const joined = levelSeries([a, levels({ startMs: NOW + 300 })], (l) => l.beforeGain);
  assert.equal(joined.length, 6, "consecutive chunks don't gap");
});

test("lines break at NaN, and a series of NaN draws nothing", () => {
  const x = (t: number) => t;
  const y = (v: number) => v;
  assert.equal(
    linePath([{ t: 0, v: 1 }, { t: 1, v: 2 }, { t: 2, v: NaN }, { t: 3, v: 4 }, { t: 4, v: 5 }], x, y),
    "M0.0 1.0L1.0 2.0M3.0 4.0L4.0 5.0",
  );
  assert.equal(linePath([{ t: 0, v: NaN }], x, y), "");
});

test("decimating keeps each pixel column's lowest and highest level, or the highest score", () => {
  const pts = Array.from({ length: 100 }, (_, i) => ({ t: i, v: i % 10 }));
  const mm = decimate(pts, 0, 100, 10, "minmax");
  assert.equal(mm.length, 20);
  assert.deepEqual(mm.slice(0, 2).map((p) => p.v), [0, 9]);
  const mx = decimate(pts, 0, 100, 10, "max");
  assert.equal(mx.length, 10);
  assert.ok(mx.every((p) => p.v === 9));
  assert.ok(mx.every((p, i) => Math.abs(p.t - (i * 10 + 5)) < 1e-9), "at the column's centre");
  // Fewer points than columns: nothing to drop, nothing invented.
  assert.equal(decimate([{ t: 0, v: 1 }, { t: 99, v: 3 }], 0, 100, 10, "max").length, 2);
});

test("decimating keeps a gap as NaN and ignores points outside the window", () => {
  const pts = [
    { t: 0, v: 1 },
    { t: 10, v: NaN },
    { t: 20, v: 3 },
    { t: 500, v: 9 },
  ];
  const out = decimate(pts, 0, 100, 10, "max");
  assert.equal(out.length, 3);
  assert.ok(Number.isNaN(out[1].v));
  assert.equal(linePath(out, (t) => t, (v) => v), "M5.0 1.0M25.0 3.0");
  const mixed = decimate([{ t: 1, v: NaN }, { t: 2, v: 5 }], 0, 100, 10, "max");
  assert.deepEqual(mixed.map((p) => p.v), [5], "a column with a value draws it");
});

test("word scores come by name, with the latest threshold, and speech as spans", () => {
  const a = levels({
    words: [
      { name: "Okay Nabu", threshold: 0.5, scores: [0.1, 0.2, 0.3] },
      { name: "Hey Jarvis", threshold: 0.6, scores: [0, 0, 0.9] },
    ],
  });
  const b = levels({ startMs: NOW + 300, speech: [true, false, false], words: [{ name: "Okay Nabu", threshold: 0.55, scores: [0.4, 0.4, 0.4] }] });
  const words = wordSeries([a, b]);
  assert.deepEqual(words.map((w) => [w.name, w.threshold, w.points.length]), [
    ["Okay Nabu", 0.55, 6],
    ["Hey Jarvis", 0.6, 3],
  ]);
  assert.deepEqual(speechSpans([a, b]), [
    { startMs: NOW + 100, endMs: NOW + 300 },
    { startMs: NOW + 300, endMs: NOW + 400 },
  ]);
});

test("a value is looked up at a time within its bucket", () => {
  const pts = levelSeries([levels()], (l) => l.beforeGain);
  assert.equal(valueAt(pts, NOW + 150, 100), -40);
  assert.ok(Number.isNaN(valueAt(pts, NOW + 900, 100)));
  assert.ok(Number.isNaN(valueAt(pts, NOW - 5, 100)));
});

test("stages that overlap go on separate rows", () => {
  const rows = packRows([
    { startMs: 0, endMs: 100 },
    { startMs: 50, endMs: 150 },
    { startMs: 100, endMs: 200 },
  ]);
  assert.deepEqual(rows.map((r) => r.row), [0, 1, 0]);
});

test("requests sort by start, and bars show waiting then receiving, open ones to now", () => {
  assert.deepEqual(requestRows([request({ id: 2, startMs: NOW + 10 }), request({ id: 1 })]).map((r) => r.id), [1, 2]);
  const done = requestBars(request(), NOW + 9999);
  assert.deepEqual(done.waiting, { startMs: NOW, endMs: NOW + 800 });
  assert.deepEqual(done.receiving, { startMs: NOW + 800, endMs: NOW + 2100 });
  assert.equal(done.open, false);
  const open = requestBars(request({ endMs: 0 }), NOW + 5000);
  assert.deepEqual(open.receiving, { startMs: NOW + 800, endMs: NOW + 5000 });
  assert.equal(open.open, true);
  const waiting = requestBars(request({ endMs: 0, responseMs: 0, status: 0 }), NOW + 300);
  assert.deepEqual(waiting.waiting, { startMs: NOW, endMs: NOW + 300 });
  assert.equal(waiting.receiving, null);
});

test("a request fails by its outcome or its status, not by being stopped by the caller", () => {
  assert.equal(requestFailed(request()), false);
  assert.equal(requestFailed(request({ status: 503 })), true);
  assert.equal(requestFailed(request({ status: 0, outcome: OUTCOME.timeout })), true);
  assert.equal(requestFailed(request({ outcome: OUTCOME.dropped })), false);
  assert.equal(requestFailed(request({ endMs: 0, outcome: OUTCOME.open })), false);
});

test("durations and sizes read short", () => {
  assert.equal(formatDuration(812), "812 ms");
  assert.equal(formatDuration(2100), "2.1 s");
  assert.equal(formatDuration(59_960), "60.0 s");
  assert.equal(formatDuration(65_000), "1 min 5 s");
  assert.equal(formatDuration(-1), "");
  assert.equal(formatSize(512), "512 B");
  assert.equal(formatSize(4200), "4.2 KB");
  assert.equal(formatSize(1_500_000), "1.5 MB");
});

test("a request's label names its method, address, status and duration", () => {
  assert.equal(requestLabel(request(), NOW + 9999), "POST api.example.com/v1/messages · 200 · 2.1 s");
  assert.equal(requestLabel(request({ endMs: 0, status: 0, outcome: 0 }), NOW + 5000), "POST api.example.com/v1/messages · open · open");
  assert.equal(requestLabel(request({ status: 0, outcome: OUTCOME.timeout }), NOW), "POST api.example.com/v1/messages · timed out · 2.1 s");
});

test("the filter matches every word against the kind, method, address and status", () => {
  const rs = [request(), request({ id: 2, kind: 2, method: "GET", url: "https://mcp.local/", status: 500 })];
  assert.equal(filterRequests(rs, "").length, 2);
  assert.deepEqual(filterRequests(rs, "mcp 500").map((r) => r.id), [2]);
  assert.deepEqual(filterRequests(rs, "POST example").map((r) => r.id), [1]);
  assert.equal(filterRequests(rs, "nothing").length, 0);
});

test("events get a marker, and what other rows draw gets none", () => {
  assert.equal(markerShape(EV.wake), "triangle");
  assert.equal(markerShape(EV.nearMiss), "triangle-open");
  assert.equal(markerShape(EV.error), "circle");
  assert.equal(markerShape(EV.calibration), "diamond");
  assert.equal(markerShape(EV.phase), null);
  assert.equal(markerShape(EV.stage), null);
  assert.equal(markerShape(EV.muted), null);
  assert.equal(eventWord(EV.nearMiss), "Near miss");
  assert.equal(eventWord(99), "Event");
});

test("axis ticks fall on round times, far enough apart", () => {
  const t0 = NOW;
  const ticks = timeTicks(t0, t0 + MIN, 600, 70);
  assert.ok(ticks.length >= 4 && ticks.length <= 9);
  assert.ok(ticks.every((t) => t % 1000 === 0 && t >= t0 && t <= t0 + MIN));
  const gaps = ticks.slice(1).map((t, i) => t - ticks[i]);
  assert.ok(gaps.every((g) => g === gaps[0]));
  assert.ok(gaps[0] / ((MIN / 600)) >= 70);
  assert.equal(timeTicks(t0, t0 + 10 * MIN, 300, 70).length > 0, true);
});

test("the time left reads in minutes and hours", () => {
  assert.equal(timeLeft(0), "No time left");
  assert.equal(timeLeft(30), "Less than a minute left");
  assert.equal(timeLeft(42 * 60), "42 min left");
  assert.equal(timeLeft(42 * 60 - 20), "42 min left");
  assert.equal(timeLeft(3600), "1 h left");
  assert.equal(timeLeft(5400), "1 h 30 min left");
  assert.equal(debuggingHint(undefined), "Off");
  assert.equal(debuggingHint({ secondsLeft: 42 * 60 }), "On, 42 min left");
});

test("what debugging records is named from the time left of each part", () => {
  assert.equal(recordedWords({ levelsSecondsLeft: 0, textSecondsLeft: 0 }), "requests");
  assert.equal(recordedWords({ levelsSecondsLeft: 9, textSecondsLeft: 0 }), "requests and the audio levels");
  assert.equal(recordedWords({ levelsSecondsLeft: 9, textSecondsLeft: 9 }), "requests, the audio levels and what was said");
  assert.equal(recordedWords({ levelsSecondsLeft: 0, textSecondsLeft: 9 }), "requests and what was said");
});

test("the export holds what the page holds, with gaps as null, and says whether text is in it", () => {
  let tl = applyState(emptyTimeline(), state(), NOW);
  tl = applyBatch(
    tl,
    batch({
      events: [event()],
      requests: [request({ bodyIn: "hello" })],
      levels: [levels({ beforeGain: [NaN, -3, -3] })],
    }),
    NOW,
  );
  const out = JSON.parse(timelineExport("Kitchen", tl, new Date(Date.UTC(2026, 10, 26, 21, 44))));
  assert.equal(out.satellite, "Kitchen");
  assert.equal(out.exportedAt, "2026-11-26T21:44:00.000Z");
  assert.equal(out.state.on, true);
  assert.equal(out.events.length, 1);
  assert.equal(out.requests[0].bodyIn, "hello");
  assert.deepEqual(out.levels[0].beforeGain, [null, -3, -3]);
  assert.equal(holdsText(tl.requests), true);
  assert.equal(holdsText([request()]), false);
  assert.equal(exportName("Kitchen", new Date(2026, 10, 26, 21, 4, 7)), "Kitchen-timeline-2026-11-26-210407.json");
  assert.equal(exportName("", new Date(2026, 0, 2, 3, 4, 5)), "pinna-timeline-2026-01-02-030405.json");
});

