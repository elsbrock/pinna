// The debugging timeline's data (ADR 0036): what WatchTimeline sends, kept for
// at most ten minutes, and the arithmetic the Timeline page draws from. Times
// are Unix milliseconds as plain numbers. No Svelte and no runtime imports, so
// `node --test` runs it; the generated enums can't be loaded there, so the
// kinds are plain numbers, as in view.ts.

/** pinna.v1.TimelineEventKind values. */
export const EV = {
  phase: 1,
  muted: 2,
  unmuted: 3,
  wake: 4,
  nearMiss: 5,
  run: 6,
  endOfSpeech: 7,
  interrupted: 8,
  error: 9,
  homeAssistant: 10,
  stage: 11,
  mode: 12,
  wakeWord: 13,
  calibration: 14,
  microphone: 15,
} as const;

/** pinna.v1.TimelineRequestKind values. */
export const REQ_KIND_WORDS: Record<number, string> = {
  1: "Assistant service (provider)",
  2: "MCP server",
  3: "Tool",
  4: "Media",
  5: "Satellite group",
};

/** pinna.v1.TimelineOutcome values. */
export const OUTCOME = { open: 0, done: 1, dropped: 2, timeout: 3, unreachable: 4, tls: 5, protocol: 6 } as const;
const OUTCOME_WORDS: Record<number, string> = {
  0: "Open",
  1: "Completed",
  2: "Stopped reading",
  3: "Timed out",
  4: "Unreachable",
  5: "Secure connection failed",
  6: "Protocol error",
};
export const outcomeWord = (o: number) => OUTCOME_WORDS[o] ?? "Unknown";

/** pinna.v1.Phase values, as the phase words say them. */
export const PHASE_WORDS: Record<number, string> = { 1: "Idle", 2: "Listening", 3: "Thinking", 4: "Speaking" };

/** What entries are kept for when the state doesn't say. */
export const KEEP_SECONDS = 600;

export type TimelineState = {
  on: boolean;
  untilMs: number;
  levelsUntilMs: number;
  textUntilMs: number;
  bucketMs: number;
  cleared: boolean;
  nowMs: number;
  keepSeconds: number;
};

export type TimelineEvent = {
  unixMs: number;
  kind: number;
  detail: string;
  phase: number;
  score: number;
  threshold: number;
  levelDbfs: number;
  gainDb: number;
  /** For a stage: when it ended; 0 for an instant. */
  endMs: number;
  run: number;
};

export type TimelineRequest = {
  id: number;
  kind: number;
  method: string;
  url: string;
  startMs: number;
  status: number;
  responseMs: number;
  firstBodyMs: number;
  /** 0 while open. */
  endMs: number;
  bytesOut: number | null;
  bytesIn: number;
  outcome: number;
  bodyOut: string;
  bodyIn: string;
};

export type TimelineWord = { name: string; threshold: number; scores: number[] };

/** Consecutive buckets from `startMs`, `bucketMs` apart. NaN = nothing was computed then. */
export type TimelineLevels = {
  startMs: number;
  bucketMs: number;
  beforeGain: number[];
  afterGain: number[];
  gain: number[];
  speaker: number[];
  speech: boolean[];
  words: TimelineWord[];
};

export type TimelineBatch = { events: TimelineEvent[]; requests: TimelineRequest[]; levels: TimelineLevels[] };

export type Timeline = {
  state: TimelineState | null;
  /** The satellite's clock minus this page's, in ms. */
  offset: number;
  events: TimelineEvent[];
  requests: TimelineRequest[];
  levels: TimelineLevels[];
};

/** What the page shows details for. */
export type Selection = { type: "request"; id: number } | { type: "event"; event: TimelineEvent };

export const emptyTimeline =(): Timeline => ({ state: null, offset: 0, events: [], requests: [], levels: [] });

/** The satellite's time, for this page's `localNow`. */
export const satelliteNow = (tl: Pick<Timeline, "offset">, localNow: number) => localNow + tl.offset;

const keepMs = (tl: Timeline) => (tl.state?.keepSeconds || KEEP_SECONDS) * 1000;

/** A new connection's backlog replaces what the page holds. */
export const dropData = (tl: Timeline): Timeline => ({ ...tl, events: [], requests: [], levels: [] });

/** A state: line the clock up, and drop everything when it says `cleared`. */
export function applyState(tl: Timeline, state: TimelineState, localNow: number): Timeline {
  const next: Timeline = { ...tl, state, offset: state.nowMs - localNow };
  if (state.cleared) return dropData(next);
  return prune(next, localNow);
}

/** A batch: append events and levels; a request replaces the one with its id. */
export function applyBatch(tl: Timeline, batch: TimelineBatch, localNow: number): Timeline {
  const requests = new Map(tl.requests.map((r) => [r.id, r]));
  for (const r of batch.requests) requests.set(r.id, r);
  return prune(
    {
      ...tl,
      events: [...tl.events, ...batch.events],
      requests: [...requests.values()],
      levels: [...tl.levels, ...batch.levels],
    },
    localNow,
  );
}

/** The last bucket's end. */
const levelsEnd = (l: TimelineLevels) => l.startMs + l.beforeGain.length * l.bucketMs;

/** The buckets from `cutoff` on, or null when none are left. */
function trimLevels(l: TimelineLevels, cutoff: number): TimelineLevels | null {
  const n = Math.max(l.beforeGain.length, l.afterGain.length, l.gain.length, l.speaker.length);
  const drop = Math.max(0, Math.ceil((cutoff - l.startMs) / l.bucketMs));
  if (drop === 0) return l;
  if (drop >= n) return null;
  return {
    ...l,
    startMs: l.startMs + drop * l.bucketMs,
    beforeGain: l.beforeGain.slice(drop),
    afterGain: l.afterGain.slice(drop),
    gain: l.gain.slice(drop),
    speaker: l.speaker.slice(drop),
    speech: l.speech.slice(drop),
    words: l.words.map((w) => ({ ...w, scores: w.scores.slice(drop) })),
  };
}

/**
 * Nothing older than `keepSeconds` stays, as on the satellite. A request that is
 * still open is kept; so is a stage that ends inside the window.
 */
export function prune(tl: Timeline, localNow: number): Timeline {
  const cutoff = satelliteNow(tl, localNow) - keepMs(tl);
  const events = tl.events.filter((e) => Math.max(e.unixMs, e.endMs) >= cutoff);
  const requests = tl.requests.filter((r) => r.endMs === 0 || r.endMs >= cutoff);
  const levels: TimelineLevels[] = [];
  for (const l of tl.levels) {
    const t = levelsEnd(l) < cutoff ? null : trimLevels(l, cutoff);
    if (t) levels.push(t);
  }
  const same = events.length === tl.events.length && requests.length === tl.requests.length;
  if (same && levels.length === tl.levels.length && levels.every((l, i) => l === tl.levels[i])) return tl;
  return { ...tl, events, requests, levels };
}

export type Section = { phase: number; startMs: number; endMs: number };

/** Phase sections: each lasts until the next change, the last until `now`. */
export function phaseSections(events: readonly TimelineEvent[], now: number): Section[] {
  const changes = events.filter((e) => e.kind === EV.phase).sort((a, b) => a.unixMs - b.unixMs);
  return changes.map((e, i) => ({
    phase: e.phase,
    startMs: e.unixMs,
    endMs: Math.max(e.unixMs, changes[i + 1]?.unixMs ?? now),
  }));
}

export type Span = { startMs: number; endMs: number };

/**
 * When the microphones were muted: from a MUTED to the next UNMUTED, or to
 * `now`. An UNMUTED with no MUTED before it ends a mute that began before the
 * page's data, which starts at `from` when given.
 */
export function muteSpans(events: readonly TimelineEvent[], now: number, from?: number): Span[] {
  const marks = events
    .filter((e) => e.kind === EV.muted || e.kind === EV.unmuted)
    .sort((a, b) => a.unixMs - b.unixMs);
  const out: Span[] = [];
  let open: number | null = null;
  for (const m of marks) {
    if (m.kind === EV.muted) open ??= m.unixMs;
    else if (open !== null) {
      out.push({ startMs: open, endMs: m.unixMs });
      open = null;
    } else if (from !== undefined && m.unixMs > from) out.push({ startMs: from, endMs: m.unixMs });
  }
  if (open !== null) out.push({ startMs: open, endMs: Math.max(open, now) });
  return out;
}

/** The section that holds `t`, if any. */
export function sectionAt(sections: readonly Section[], t: number): Section | undefined {
  return sections.find((s) => t >= s.startMs && t < s.endMs) ?? sections.find((s) => t === s.endMs);
}

/** A time to an x position, and back. */
export function timeScale(t0: number, t1: number, x0: number, x1: number) {
  const span = t1 - t0 || 1;
  return {
    x: (t: number) => x0 + ((t - t0) / span) * (x1 - x0),
    t: (x: number) => t0 + ((x - x0) / (x1 - x0 || 1)) * span,
  };
}

export type Point = { t: number; v: number };

/** One field of the level chunks as a series, with NaN where chunks leave a gap. */
export function levelSeries(
  chunks: readonly TimelineLevels[],
  pick: (l: TimelineLevels) => readonly number[] | undefined,
): Point[] {
  const out: Point[] = [];
  let end = NaN;
  for (const c of [...chunks].sort((a, b) => a.startMs - b.startMs)) {
    const vs = pick(c);
    if (!vs?.length) continue;
    if (Number.isFinite(end) && c.startMs - end > c.bucketMs / 2) out.push({ t: end, v: NaN });
    vs.forEach((v, i) => out.push({ t: c.startMs + i * c.bucketMs, v }));
    end = c.startMs + vs.length * c.bucketMs;
  }
  return out;
}

/** A wake word's scores across the chunks, by name, with its threshold. */
export function wordSeries(chunks: readonly TimelineLevels[]): { name: string; threshold: number; points: Point[] }[] {
  const names: string[] = [];
  for (const c of chunks) for (const w of c.words) if (!names.includes(w.name)) names.push(w.name);
  return names.map((name) => {
    const threshold = chunks.flatMap((c) => c.words).filter((w) => w.name === name).at(-1)?.threshold ?? NaN;
    const points = levelSeries(chunks, (l) => l.words.find((w) => w.name === name)?.scores);
    return { name, threshold, points };
  });
}

/** Where the detector heard speech, as spans. */
export function speechSpans(chunks: readonly TimelineLevels[]): Span[] {
  const out: Span[] = [];
  for (const c of [...chunks].sort((a, b) => a.startMs - b.startMs)) {
    let from: number | null = null;
    c.speech.forEach((s, i) => {
      const t = c.startMs + i * c.bucketMs;
      if (s && from === null) from = t;
      if (!s && from !== null) {
        out.push({ startMs: from, endMs: t });
        from = null;
      }
    });
    if (from !== null) out.push({ startMs: from, endMs: c.startMs + c.speech.length * c.bucketMs });
  }
  return out;
}

/**
 * Fewer points for a lane of `width` pixels over [t0, t1]: for each pixel
 * column the lowest and highest value ("minmax", for levels) or the highest
 * ("max", for scores). A column with only NaN stays NaN, so the line breaks
 * there; a column with no points is skipped.
 */
export function decimate(points: readonly Point[], t0: number, t1: number, width: number, mode: "minmax" | "max"): Point[] {
  const cols = Math.max(1, Math.floor(width));
  const span = t1 - t0 || 1;
  type Col = { min: number; max: number; nan: boolean };
  const seen = new Map<number, Col>();
  for (const p of points) {
    if (p.t < t0 || p.t > t1) continue;
    const c = Math.min(cols - 1, Math.floor(((p.t - t0) / span) * cols));
    const col = seen.get(c) ?? { min: Infinity, max: -Infinity, nan: false };
    if (Number.isNaN(p.v)) col.nan = true;
    else {
      col.min = Math.min(col.min, p.v);
      col.max = Math.max(col.max, p.v);
    }
    seen.set(c, col);
  }
  const out: Point[] = [];
  for (const c of [...seen.keys()].sort((a, b) => a - b)) {
    const col = seen.get(c)!;
    const t = t0 + ((c + 0.5) / cols) * span;
    if (col.min === Infinity) out.push({ t, v: NaN });
    else if (mode === "max" || col.min === col.max) out.push({ t, v: col.max });
    else out.push({ t, v: col.min }, { t, v: col.max });
  }
  return out;
}

/** An SVG path through the points, which starts a new piece after every NaN. */
export function linePath(points: readonly Point[], x: (t: number) => number, y: (v: number) => number): string {
  let d = "";
  let pen = false;
  for (const p of points) {
    if (Number.isNaN(p.v)) {
      pen = false;
      continue;
    }
    d += `${pen ? "L" : "M"}${x(p.t).toFixed(1)} ${y(p.v).toFixed(1)}`;
    pen = true;
  }
  return d;
}

/** The value of a series at `t`: the last point at or before it, within a bucket. */
export function valueAt(points: readonly Point[], t: number, bucketMs: number): number {
  let lo = 0;
  let hi = points.length - 1;
  let best = -1;
  while (lo <= hi) {
    const mid = (lo + hi) >> 1;
    if (points[mid].t <= t) {
      best = mid;
      lo = mid + 1;
    } else hi = mid - 1;
  }
  return best >= 0 && t - points[best].t < bucketMs ? points[best].v : NaN;
}

/** Spans on as few rows as keep them from overlapping, in order of start. */
export function packRows<T extends Span>(spans: readonly T[]): { row: number; span: T }[] {
  const ends: number[] = [];
  return [...spans]
    .sort((a, b) => a.startMs - b.startMs)
    .map((span) => {
      let row = ends.findIndex((e) => e <= span.startMs);
      if (row < 0) row = ends.length;
      ends[row] = Math.max(span.endMs, span.startMs + 1);
      return { row, span };
    });
}

/** Step events that have a span, for the steps row. */
export const stageSpans = (events: readonly TimelineEvent[]) =>
  events
    .filter((e) => e.kind === EV.stage && e.endMs > e.unixMs)
    .map((e) => ({ startMs: e.unixMs, endMs: e.endMs, label: e.detail, event: e }));

/** Requests by start, then id. */
export const requestRows = (requests: readonly TimelineRequest[]): TimelineRequest[] =>
  [...requests].sort((a, b) => a.startMs - b.startMs || a.id - b.id);

/** Requests, newest first, as the Requests view lists them. */
export const requestsNewestFirst = (requests: readonly TimelineRequest[]): TimelineRequest[] =>
  requestRows(requests).reverse();

/** `POST /v1/audio/transcriptions`: the method and the path, without scheme, host or query. */
export function requestAddress(r: Pick<TimelineRequest, "method" | "url">): string {
  const path = r.url.replace(/^[a-z][a-z0-9+.-]*:\/\/[^/?#]*/i, "").replace(/[?#].*$/, "") || "/";
  return `${r.method} ${path}`;
}

/** The newest end of anything kept (an event, a request or a level bucket); 0 when nothing is. */
export function lastRecorded(tl: Pick<Timeline, "events" | "requests" | "levels">): number {
  let last = 0;
  for (const e of tl.events) last = Math.max(last, e.unixMs, e.endMs);
  for (const r of tl.requests) last = Math.max(last, r.endMs || r.responseMs || r.startMs);
  for (const l of tl.levels) last = Math.max(last, levelsEnd(l));
  return last;
}

/** A request's two bars: waiting until the response, then receiving until the end or `now`. */
export function requestBars(r: TimelineRequest, now: number): { waiting: Span; receiving: Span | null; open: boolean } {
  const open = r.endMs === 0;
  const end = open ? Math.max(now, r.startMs) : r.endMs;
  const response = r.responseMs > 0 ? Math.min(Math.max(r.responseMs, r.startMs), end) : 0;
  if (!response) return { waiting: { startMs: r.startMs, endMs: end }, receiving: null, open };
  return {
    waiting: { startMs: r.startMs, endMs: response },
    receiving: { startMs: response, endMs: end },
    open,
  };
}

/** The request failed: it didn't end well, or the server said so. */
export const requestFailed = (r: TimelineRequest) =>
  (r.outcome !== OUTCOME.open && r.outcome !== OUTCOME.done && r.outcome !== OUTCOME.dropped) || r.status >= 400;

/** "812 ms", "2.1 s", "1 min 5 s". */
export function formatDuration(ms: number): string {
  if (!Number.isFinite(ms) || ms < 0) return "";
  if (ms < 1000) return `${Math.round(ms)} ms`;
  if (ms < 60_000) return `${(Math.round(ms / 100) / 10).toFixed(1)} s`;
  const s = Math.round(ms / 1000);
  return `${Math.floor(s / 60)} min ${s % 60} s`;
}

/** "512 B", "4.2 KB", "1.5 MB". */
export function formatSize(bytes: number): string {
  if (bytes < 1000) return `${bytes} B`;
  if (bytes < 1_000_000) return `${(bytes / 1000).toFixed(1)} KB`;
  return `${(bytes / 1_000_000).toFixed(1)} MB`;
}

/** How a request ended, for a label: its status, or why it has none. */
export const requestStatus = (r: TimelineRequest) =>
  r.status > 0 ? String(r.status) : r.outcome === OUTCOME.open ? "open" : outcomeWord(r.outcome).toLowerCase();

/** `POST api.example.com/v1/messages · 200 · 2.1 s`. */
export function requestLabel(r: TimelineRequest, now: number): string {
  const end = r.endMs || now;
  const duration = r.endMs ? formatDuration(end - r.startMs) : "open";
  return `${r.method} ${r.url.replace(/^[a-z][a-z0-9+.-]*:\/\//i, "")} · ${requestStatus(r)} · ${duration}`;
}

/** The requests that contain every word of `query` in the line the table shows. */
export function filterRequests(requests: readonly TimelineRequest[], query: string): TimelineRequest[] {
  const words = query.toLowerCase().split(/\s+/).filter(Boolean);
  if (!words.length) return [...requests];
  return requests.filter((r) => {
    const hay = `${REQ_KIND_WORDS[r.kind] ?? ""} ${r.method} ${r.url} ${requestStatus(r)} ${outcomeWord(r.outcome)}`.toLowerCase();
    return words.every((w) => hay.includes(w));
  });
}

const EVENT_WORDS: Record<number, string> = {
  [EV.phase]: "Phase",
  [EV.muted]: "Muted",
  [EV.unmuted]: "Unmuted",
  [EV.wake]: "Wake word",
  [EV.nearMiss]: "Near miss",
  [EV.run]: "Request",
  [EV.endOfSpeech]: "End of speech",
  [EV.interrupted]: "Interrupted",
  [EV.error]: "Error",
  [EV.homeAssistant]: "Home Assistant",
  [EV.stage]: "Step",
  [EV.mode]: "Mode changed",
  [EV.wakeWord]: "Wake word setting changed",
  [EV.calibration]: "Calibration",
  [EV.microphone]: "Microphone stream changed",
};
export const eventWord = (kind: number) => EVENT_WORDS[kind] ?? "Event";

export type Marker = "triangle" | "triangle-open" | "bar" | "square" | "circle" | "circle-open" | "diamond";

/** The shape the events row draws for an event; null for what other rows show (phases, mute, steps, run milestones). */
export function markerShape(kind: number): Marker | null {
  switch (kind) {
    case EV.wake:
      return "triangle";
    case EV.nearMiss:
      return "triangle-open";
    case EV.endOfSpeech:
      return "bar";
    case EV.interrupted:
      return "square";
    case EV.error:
      return "circle";
    case EV.homeAssistant:
      return "circle-open";
    case EV.mode:
    case EV.wakeWord:
    case EV.calibration:
    case EV.microphone:
      return "diamond";
    default:
      return null;
  }
}

/** Times for the axis, at multiples of a round step that keeps labels `minGap` pixels apart. */
export function timeTicks(t0: number, t1: number, width: number, minGap: number): number[] {
  const steps = [1, 2, 5, 10, 15, 30, 60, 120, 300, 600].map((s) => s * 1000);
  const perPx = (t1 - t0) / Math.max(1, width);
  const step = steps.find((s) => s / perPx >= minGap) ?? steps[steps.length - 1];
  const out: number[] = [];
  for (let t = Math.ceil(t0 / step) * step; t <= t1; t += step) out.push(t);
  return out;
}

/** "42 min left", "1 h 30 min left", "Less than a minute left". */
export function timeLeft(seconds: number): string {
  if (seconds <= 0) return "No time left";
  if (seconds < 60) return "Less than a minute left";
  const min = Math.floor(seconds / 60);
  if (min < 60) return `${min} min left`;
  const h = Math.floor(min / 60);
  return `${h} h${min % 60 ? ` ${min % 60} min` : ""} left`;
}

/** The System row's hint: "Off", or "On, 42 min left". */
export function debuggingHint(d: { secondsLeft: number } | undefined | null): string {
  if (!d) return "Off";
  const left = timeLeft(d.secondsLeft);
  return `On, ${left.charAt(0).toLowerCase()}${left.slice(1)}`;
}

/** What debugging records: "requests", "requests and the audio levels". */
export function recordedWords(d: { levelsSecondsLeft: number; textSecondsLeft: number }): string {
  const parts = ["requests"];
  if (d.levelsSecondsLeft > 0) parts.push("the audio levels");
  if (d.textSecondsLeft > 0) parts.push("what was said");
  return parts.length === 1 ? parts[0] : `${parts.slice(0, -1).join(", ")} and ${parts[parts.length - 1]}`;
}

/** Some request holds what was said. */
export const holdsText = (requests: readonly TimelineRequest[]) => requests.some((r) => r.bodyOut || r.bodyIn);

/** `<name>-timeline-2026-11-26-2144-07.json`, in local time like the log's. */
export function exportName(name: string, d: Date): string {
  const p = (n: number) => String(n).padStart(2, "0");
  const day = `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}`;
  return `${name || "pinna"}-timeline-${day}-${p(d.getHours())}${p(d.getMinutes())}${p(d.getSeconds())}.json`;
}

/** NaN is not JSON: a gap is null. */
const gaps = (vs: readonly number[]) => vs.map((v) => (Number.isNaN(v) ? null : v));

/** What the page holds, as the JSON file Download saves. */
export function timelineExport(name: string, tl: Timeline, now: Date): string {
  return JSON.stringify(
    {
      satellite: name,
      exportedAt: now.toISOString(),
      state: tl.state,
      events: tl.events,
      requests: tl.requests,
      levels: tl.levels.map((l) => ({
        ...l,
        beforeGain: gaps(l.beforeGain),
        afterGain: gaps(l.afterGain),
        gain: gaps(l.gain),
        speaker: gaps(l.speaker),
        words: l.words.map((w) => ({ ...w, scores: gaps(w.scores) })),
      })),
    },
    null,
    2,
  );
}
