// Follows WatchTimeline (ADR 0036) for the Timeline page and holds what it sends.
// The first message of every connection is a state; the backlog that follows
// replaces what the page held, as the Logs page does for the journal.
import type { TimelineBatch as WireBatch, TimelineState as WireState } from "../gen/pinna/v1/device_pb.ts";
import { client, errorSentence } from "./device.svelte.ts";
import {
  applyBatch,
  applyState,
  dropData,
  emptyTimeline,
  prune,
  satelliteNow,
  type Selection,
  type Timeline,
  type TimelineBatch,
  type TimelineState,
} from "./timeline.ts";

const state = (s: WireState): TimelineState => ({
  on: s.on,
  untilMs: Number(s.untilUnixMs),
  levelsUntilMs: Number(s.levelsUntilUnixMs),
  textUntilMs: Number(s.textUntilUnixMs),
  bucketMs: s.bucketMs || 100,
  cleared: s.cleared,
  nowMs: Number(s.nowUnixMs),
  keepSeconds: s.keepSeconds,
});

const batch = (b: WireBatch, bucketMs: number): TimelineBatch => ({
  events: b.events.map((e) => ({
    unixMs: Number(e.unixMs),
    kind: e.kind,
    detail: e.detail,
    phase: e.phase,
    score: e.score,
    threshold: e.threshold,
    levelDbfs: e.levelDbfs,
    gainDb: e.gainDb,
    endMs: Number(e.endUnixMs),
    run: Number(e.run),
  })),
  requests: b.requests.map((r) => ({
    id: Number(r.id),
    kind: r.kind,
    method: r.method,
    url: r.url,
    startMs: Number(r.startUnixMs),
    status: r.status,
    responseMs: Number(r.responseUnixMs),
    firstBodyMs: Number(r.firstBodyUnixMs),
    endMs: Number(r.endUnixMs),
    bytesOut: r.bytesOut === undefined ? null : Number(r.bytesOut),
    bytesIn: Number(r.bytesIn),
    outcome: r.outcome,
    bodyOut: r.bodyOut,
    bodyIn: r.bodyIn,
  })),
  levels: b.levels.map((l) => ({
    startMs: Number(l.startUnixMs),
    bucketMs,
    beforeGain: l.beforeGainDbfs,
    afterGain: l.afterGainDbfs,
    gain: l.gainDb,
    speaker: l.speakerDbfs,
    speech: l.speech,
    words: l.words.map((w) => ({ name: w.name, threshold: w.threshold, scores: w.scores })),
  })),
});

export class TimelineStore {
  data = $state.raw<Timeline>(emptyTimeline());
  /** A state has arrived on this connection. */
  connected = $state(false);
  error = $state<string | null>(null);

  // The page's view, kept here so it survives leaving the page and coming back.
  view = $state<"timeline" | "requests">("timeline");
  windowMin = $state(typeof matchMedia === "function" && matchMedia("(max-width: 600px)").matches ? 1 : 5);
  paused = $state(false);
  /** What the chart showed when it was paused. */
  frozen = $state.raw<{ tl: Timeline; end: number } | null>(null);
  selected = $state.raw<Selection | null>(null);

  /** The satellite's time, now. */
  now = () => satelliteNow(this.data, Date.now());

  /** Follow the stream until the returned stop is called; for an $effect. */
  watch(): () => void {
    const abort = new AbortController();
    // Entries age out of the page even while nothing arrives.
    const timer = setInterval(() => (this.data = prune(this.data, Date.now())), 5_000);
    (async () => {
      while (!abort.signal.aborted) {
        try {
          let first = true;
          for await (const res of client.watchTimeline({}, { signal: abort.signal })) {
            const item = res.item;
            if (item.case === "state") {
              if (first) {
                this.data = dropData(this.data);
                first = false;
                this.connected = true;
                this.error = null;
              }
              this.data = applyState(this.data, state(item.value), Date.now());
            } else if (item.case === "batch" && !first) {
              this.data = applyBatch(this.data, batch(item.value, this.data.state?.bucketMs || 100), Date.now());
            }
          }
        } catch (e) {
          if (abort.signal.aborted) return;
          this.error = errorSentence(e);
        }
        this.connected = false;
        await new Promise((r) => setTimeout(r, 3000));
      }
    })();
    return () => {
      abort.abort();
      clearInterval(timer);
    };
  }
}

/** The one store: the page holds on to what it follows while another page is open. */
export const timelineStore = new TimelineStore();
