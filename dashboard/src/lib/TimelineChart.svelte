<!-- The debugging timeline as hand-written SVG (ADR 0036): one time axis, then a row
     each for the phases, the events, the stages, the audio lanes and the requests.
     Colours are tokens only. The pointer is read on the whole plot, not on each
     mark, so a hover shows every row's value at that time and a click picks the
     mark under it. The Requests view is the accessible form of the requests. -->
<script lang="ts">
  import {
    EV,
    decimate,
    eventWord,
    formatDuration,
    formatSize,
    levelSeries,
    linePath,
    markerShape,
    muteSpans,
    packRows,
    phaseSections,
    PHASE_WORDS,
    requestBars,
    requestFailed,
    requestLabel,
    requestRows,
    sectionAt,
    speechSpans,
    stageSpans,
    timeScale,
    timeTicks,
    valueAt,
    wordSeries,
    type Marker,
    type Point,
    type Selection,
    type Timeline,
    type TimelineEvent,
    type TimelineRequest,
  } from "./timeline.ts";

  let {
    tl,
    end,
    windowMs,
    homeAssistant,
    passwordSet,
    selected,
    onselect,
  }: {
    tl: Timeline;
    /** The satellite's time at the right edge. */
    end: number;
    windowMs: number;
    /** Home Assistant makes the requests, not the satellite. */
    homeAssistant: boolean;
    passwordSet: boolean | undefined;
    selected: Selection | null;
    onselect: (s: Selection | null) => void;
  } = $props();

  const AXIS_H = 22;
  const PHASE_H = 28;
  const EVENTS_H = 32;
  const STAGE_ROW = 22;
  const LANE_H = 68;
  const REQ_ROW = 20;
  const MAX_REQUESTS = 40;
  const MARKER = 10;
  /** Every dBFS row shares this scale, so their heights compare. */
  const DBFS: [number, number] = [-90, 0];
  const SERIES = ["s1", "s2", "s3"] as const;

  let cellW = $state(0);
  let rowsEl = $state<HTMLElement>();
  let cellEl = $state<HTMLElement>();
  const pw = $derived(Math.max(120, cellW));

  const t1 = $derived(end);
  const t0 = $derived(end - windowMs);
  const sc = $derived(timeScale(t0, t1, 0, pw));
  const ticks = $derived(timeTicks(t0, t1, pw, 78));
  const bucketMs = $derived(tl.state?.bucketMs || 100);
  const withSeconds = $derived(windowMs <= 5 * 60_000);

  const clock = (ms: number, seconds = true) =>
    new Date(ms).toLocaleTimeString([], seconds ? { hour: "2-digit", minute: "2-digit", second: "2-digit" } : { hour: "2-digit", minute: "2-digit" });

  const clampX = (x: number) => Math.min(pw, Math.max(0, x));
  const inWindow = (a: number, b: number) => b >= t0 && a <= t1;

  // Phases and mutes.
  const sections = $derived(phaseSections(tl.events, end).filter((s) => inWindow(s.startMs, s.endMs)));
  const mutes = $derived(muteSpans(tl.events, end, t0).filter((s) => inWindow(s.startMs, s.endMs)));
  const phaseClass: Record<number, string> = { 1: "idle", 2: "listening", 3: "thinking", 4: "speaking" };

  // Events.
  const markers = $derived(
    tl.events
      .filter((e) => markerShape(e.kind) !== null && e.unixMs >= t0 && e.unixMs <= t1)
      .map((e) => ({ event: e, shape: markerShape(e.kind) as Marker, x: sc.x(e.unixMs) })),
  );
  const markerClass = (kind: number) =>
    kind === EV.wake || kind === EV.nearMiss
      ? "m-signal"
      : kind === EV.error
        ? "m-danger"
        : kind === EV.interrupted
          ? "m-caution"
          : kind === EV.homeAssistant
            ? "m-speaking"
            : kind === EV.run
              ? "m-thinking"
              : "m-muted";

  // Stages.
  const stages = $derived(
    packRows(stageSpans(tl.events).filter((s) => inWindow(s.startMs, s.endMs))),
  );
  const stageRows = $derived(Math.max(1, ...stages.map((s) => s.row + 1)));
  const STAGES_H = $derived(stageRows * STAGE_ROW + 8);

  // Lanes.
  type LaneSeries = { name: string; cls: string; full: Point[]; pts: Point[] };
  type Lane = {
    id: string;
    title: string;
    unit: string;
    lo: number;
    hi: number;
    grid: number[];
    series: LaneSeries[];
    fmt: (v: number) => string;
    thresholds: { name: string; v: number }[];
    speech: { startMs: number; endMs: number }[];
    note?: string;
  };

  const hasLevels = $derived(tl.levels.length > 0);
  const lanes = $derived.by((): Lane[] => {
    if (!hasLevels) return [];
    const make = (name: string, cls: string, pick: (l: (typeof tl.levels)[number]) => readonly number[] | undefined, mode: "minmax" | "max"): LaneSeries => {
      const full = levelSeries(tl.levels, pick);
      return { name, cls, full, pts: decimate(full, t0, t1, pw, mode) };
    };
    const words = wordSeries(tl.levels).slice(0, SERIES.length);
    const more = wordSeries(tl.levels).length - words.length;
    const gain = make("Gain", "s1", (l) => l.gain, "minmax");
    const finite = gain.full.filter((p) => p.t >= t0 && Number.isFinite(p.v)).map((p) => p.v);
    const gLo = finite.length ? Math.floor(Math.min(...finite) - 1) : 0;
    const gHi = finite.length ? Math.max(Math.ceil(Math.max(...finite) + 1), gLo + 6) : 6;
    const db = (v: number) => `${v.toFixed(1)} dBFS`;
    return [
      {
        id: "wake",
        title: "Wake word",
        unit: "score",
        lo: 0,
        hi: 1,
        grid: [0, 0.5, 1],
        series: words.map((w, i) => ({
          name: w.name,
          cls: SERIES[i],
          full: w.points,
          pts: decimate(w.points, t0, t1, pw, "max"),
        })),
        fmt: (v) => v.toFixed(2),
        thresholds: words.filter((w) => Number.isFinite(w.threshold)).map((w) => ({ name: w.name, v: w.threshold })),
        speech: speechSpans(tl.levels).filter((s) => inWindow(s.startMs, s.endMs)),
        note: more > 0 ? `${more} more ${more === 1 ? "word isn't" : "words aren't"} drawn.` : undefined,
      },
      {
        id: "mics",
        title: "Microphones",
        unit: "dBFS",
        lo: DBFS[0],
        hi: DBFS[1],
        grid: [-60, -30],
        series: [make("Before gain", "s1", (l) => l.beforeGain, "minmax"), make("After gain", "s2", (l) => l.afterGain, "minmax")],
        fmt: db,
        thresholds: [],
        speech: [],
      },
      {
        id: "gain",
        title: "Gain",
        unit: "dB",
        lo: gLo,
        hi: gHi,
        grid: [gLo, gHi],
        series: [gain],
        fmt: (v) => `${v.toFixed(1)} dB`,
        thresholds: [],
        speech: [],
      },
      {
        id: "speaker",
        title: "Speaker",
        unit: "dBFS",
        lo: DBFS[0],
        hi: DBFS[1],
        grid: [-60, -30],
        series: [make("Output level", "s1", (l) => l.speaker, "minmax")],
        fmt: db,
        thresholds: [],
        speech: [],
      },
    ];
  });

  const ly = (lane: Lane) => (v: number) => {
    const f = (Math.min(lane.hi, Math.max(lane.lo, v)) - lane.lo) / (lane.hi - lane.lo || 1);
    return 5 + (1 - f) * (LANE_H - 10);
  };

  // Where each line ends, for its direct label: apart when two end close together.
  function endLabels(lane: Lane): { name: string; cls: string; y: number }[] {
    const y = ly(lane);
    const ends = lane.series
      .map((s) => {
        const last = [...s.pts].reverse().find((p) => Number.isFinite(p.v));
        return last ? { name: s.name, cls: s.cls, y: y(last.v) } : null;
      })
      .filter((e): e is { name: string; cls: string; y: number } => e !== null)
      .sort((a, b) => a.y - b.y);
    for (let i = 1; i < ends.length; i++) if (ends[i].y - ends[i - 1].y < 13) ends[i].y = ends[i - 1].y + 13;
    return ends.map((e) => ({ ...e, y: Math.min(LANE_H - 3, Math.max(11, e.y - 5)) }));
  }

  // Requests.
  const visibleRequests = $derived(
    requestRows(tl.requests.filter((r) => inWindow(r.startMs, r.endMs || end))).slice(-MAX_REQUESTS),
  );
  const hiddenRequests = $derived(
    tl.requests.filter((r) => inWindow(r.startMs, r.endMs || end)).length - visibleRequests.length,
  );
  const REQ_H = $derived(Math.max(1, visibleRequests.length) * REQ_ROW + 8);
  const barLabel = (r: TimelineRequest, startX: number, endX: number) => {
    const text = requestLabel(r, end);
    const room = text.length * 6.3;
    if (endX + 6 + room <= pw) return { text, x: endX + 6, anchor: "start" as const, halo: false };
    if (startX - 6 - room >= 0) return { text, x: startX - 6, anchor: "end" as const, halo: false };
    return { text, x: 6, anchor: "start" as const, halo: true };
  };

  // The pointer.
  type Hover = { x: number; left: number; top: number; t: number; hit: Selection | null };
  let hover = $state<Hover | null>(null);

  function hitAt(row: string | undefined, x: number, y: number, t: number): Selection | null {
    if (row === "events") {
      let best: { e: TimelineEvent; d: number } | null = null;
      for (const m of markers) {
        const d = Math.abs(m.x - x);
        if (d <= MARKER && (!best || d < best.d)) best = { e: m.event, d };
      }
      return best ? { type: "event", event: best.e } : null;
    }
    if (row === "stages") {
      const s = stages.find((s) => s.row === Math.floor((y - 4) / STAGE_ROW) && t >= s.span.startMs && t <= s.span.endMs);
      return s ? { type: "event", event: s.span.event } : null;
    }
    if (row === "requests") {
      const r = visibleRequests[Math.floor((y - 4) / REQ_ROW)];
      return r ? { type: "request", id: r.id } : null;
    }
    return null;
  }

  function move(e: PointerEvent) {
    if (!cellEl || !rowsEl) return;
    const cell = cellEl.getBoundingClientRect();
    const rows = rowsEl.getBoundingClientRect();
    const x = e.clientX - cell.left;
    if (x < 0 || x > cell.width) {
      hover = null;
      return;
    }
    const rowEl = (e.target as Element).closest<HTMLElement>("[data-row]");
    const y = rowEl ? e.clientY - rowEl.getBoundingClientRect().top : 0;
    const t = sc.t(x);
    hover = { x, left: cell.left - rows.left + x, top: e.clientY - rows.top, t, hit: hitAt(rowEl?.dataset.row, x, y, t) };
  }

  function click() {
    onselect(hover?.hit ?? null);
  }

  const isSelected = (s: Selection | null, e: TimelineEvent) => s?.type === "event" && s.event === e;

  // What the tooltip says about the time under the pointer.
  const atPhase = $derived(hover ? sectionAt(sections, hover.t) : undefined);
  const atMute = $derived(hover ? mutes.some((m) => hover!.t >= m.startMs && hover!.t <= m.endMs) : false);
  const atValues = $derived(
    hover
      ? lanes.flatMap((l) =>
          l.series.map((s) => ({ lane: l.title, name: s.name, cls: s.cls, v: valueAt(s.full, hover!.t, bucketMs), fmt: l.fmt })),
        )
      : [],
  );

  const fmtTick = (t: number) => clock(t, withSeconds);
</script>

{#snippet shape(kind: Marker, x: number, y: number, cls: string, big: boolean)}
  {@const r = (big ? MARKER + 2 : MARKER) / 2}
  {#if kind === "triangle"}
    <path class="mark {cls} fill" d="M{x} {y - r}L{x + r} {y + r}L{x - r} {y + r}Z" />
  {:else if kind === "triangle-open"}
    <path class="mark {cls} open" d="M{x} {y - r}L{x + r} {y + r}L{x - r} {y + r}Z" />
  {:else if kind === "bar"}
    <rect class="mark {cls} fill" x={x - 1.5} y={y - r} width="3" height={r * 2} />
  {:else if kind === "square"}
    <rect class="mark {cls} fill" x={x - r + 1} y={y - r + 1} width={r * 2 - 2} height={r * 2 - 2} />
  {:else if kind === "circle"}
    <circle class="mark {cls} fill" cx={x} cy={y} r={r} />
  {:else if kind === "circle-open"}
    <circle class="mark {cls} open" cx={x} cy={y} r={r - 1} />
  {:else}
    <path class="mark {cls} fill" d="M{x} {y - r}L{x + r} {y}L{x} {y + r}L{x - r} {y}Z" />
  {/if}
{/snippet}

{#snippet grid(h: number)}
  {#each ticks as t (t)}
    <line class="grid" x1={sc.x(t)} x2={sc.x(t)} y1="0" y2={h} />
  {/each}
{/snippet}

<div class="chart" role="group" aria-label="Timeline chart. The Requests view lists the same requests as a table.">
  <!-- The pointer is read here for the whole plot; the Requests view is the keyboard's. -->
  <div class="rows" bind:this={rowsEl} role="presentation" onpointermove={move} onpointerleave={() => (hover = null)} onclick={click}>
    <div class="row" data-row="axis">
      <div class="label"></div>
      <div class="cell" bind:clientWidth={cellW} bind:this={cellEl}>
        <svg width={pw} height={AXIS_H} aria-hidden="true">
          <line class="axis" x1="0" x2={pw} y1={AXIS_H - 0.5} y2={AXIS_H - 0.5} />
          {#each ticks as t (t)}
            {@const x = sc.x(t)}
            <line class="axis" x1={x} x2={x} y1={AXIS_H - 5} y2={AXIS_H} />
            <text class="tick" x={Math.min(pw - 2, Math.max(2, x))} y="12" text-anchor={x < 30 ? "start" : x > pw - 30 ? "end" : "middle"}>{fmtTick(t)}</text>
          {/each}
        </svg>
      </div>
    </div>

    <div class="row" data-row="phase">
      <div class="label">Phase</div>
      <div class="cell">
        <svg width={pw} height={PHASE_H} aria-hidden="true">
          {@render grid(PHASE_H)}
          {#each sections as s (s.startMs)}
            {@const x0 = clampX(sc.x(s.startMs))}
            {@const x1 = clampX(sc.x(s.endMs))}
            {#if x1 - x0 > 0}
              <rect class="sec {phaseClass[s.phase] ?? 'idle'}" x={x0} y="3" width={x1 - x0} height={PHASE_H - 6} />
              {#if x1 - x0 > 76}
                <text class="sec-word" x={x0 + 6} y={PHASE_H / 2 + 4}>{PHASE_WORDS[s.phase] ?? ""}</text>
              {/if}
            {/if}
          {/each}
          {#each mutes as m (m.startMs)}
            {@const x0 = clampX(sc.x(m.startMs))}
            {@const x1 = clampX(sc.x(m.endMs))}
            {#if x1 - x0 > 0}
              <rect class="sec muted" x={x0} y="3" width={x1 - x0} height={PHASE_H - 6} />
              {#if x1 - x0 > 60}
                <text class="sec-word" x={x0 + 6} y={PHASE_H / 2 + 4}>Muted</text>
              {/if}
            {/if}
          {/each}
        </svg>
      </div>
    </div>

    <div class="row" data-row="events">
      <div class="label">Events</div>
      <div class="cell">
        <svg width={pw} height={EVENTS_H} aria-hidden="true">
          {@render grid(EVENTS_H)}
          {#each markers as m, i (i)}
            {@render shape(m.shape, m.x, EVENTS_H / 2, markerClass(m.event.kind), isSelected(selected, m.event) || (hover?.hit?.type === "event" && hover.hit.event === m.event))}
            {#if isSelected(selected, m.event)}
              <circle class="ring" cx={m.x} cy={EVENTS_H / 2} r={MARKER} />
            {/if}
          {/each}
        </svg>
      </div>
    </div>

    <div class="row" data-row="stages">
      <div class="label">Stages</div>
      <div class="cell">
        <svg width={pw} height={STAGES_H} aria-hidden="true">
          {@render grid(STAGES_H)}
          {#each stages as s, i (i)}
            {@const x0 = clampX(sc.x(s.span.startMs))}
            {@const x1 = clampX(sc.x(s.span.endMs))}
            <rect class="stage" class:selected={isSelected(selected, s.span.event)} x={x0} y={4 + s.row * STAGE_ROW} width={Math.max(2, x1 - x0)} height={STAGE_ROW - 4} rx="3" />
            {#if x1 - x0 > 40}
              <text class="stage-word" x={x0 + 5} y={4 + s.row * STAGE_ROW + 12}>{s.span.label}</text>
            {/if}
          {/each}
          {#if !stages.length}
            <text class="none" x="2" y="18">No stages in this window.</text>
          {/if}
        </svg>
      </div>
    </div>

    {#if hasLevels}
      {#each lanes as lane (lane.id)}
        {@const y = ly(lane)}
        <div class="row" data-row={lane.id}>
          <div class="label">
            <span>{lane.title}</span>
            <span class="unit">{lane.unit}</span>
            {#if lane.series.length > 1 || lane.id === "wake"}
              <span class="legend">
                {#each lane.series as s (s.name)}
                  <span class="key"><i class="sw {s.cls}"></i>{s.name}</span>
                {/each}
                {#if lane.thresholds.length}<span class="key"><i class="sw dash"></i>Threshold</span>{/if}
                {#if lane.id === "wake"}<span class="key"><i class="sw band"></i>Speech heard</span>{/if}
              </span>
            {/if}
          </div>
          <div class="cell">
            <svg width={pw} height={LANE_H} aria-hidden="true">
              {@render grid(LANE_H)}
              {#each lane.grid as g (g)}
                <line class="grid" x1="0" x2={pw} y1={y(g)} y2={y(g)} />
                <text class="ylab" x="3" y={y(g) - 2}>{g}</text>
              {/each}
              {#each lane.speech as s (s.startMs)}
                <rect class="speech" x={clampX(sc.x(s.startMs))} y={LANE_H - 7} width={Math.max(1, clampX(sc.x(s.endMs)) - clampX(sc.x(s.startMs)))} height="4" />
              {/each}
              {#each lane.thresholds as th (th.name)}
                <line class="threshold" x1="0" x2={pw} y1={y(th.v)} y2={y(th.v)} />
              {/each}
              {#each lane.series as s (s.name)}
                <path class="line {s.cls}" d={linePath(s.pts, sc.x, y)} />
              {/each}
              {#each endLabels(lane) as e (e.name)}
                <text class="direct" x={pw - 4} y={e.y} text-anchor="end">{e.name}</text>
              {/each}
              {#if lane.thresholds.length}
                <text class="direct thr" x="3" y={y(lane.thresholds[0].v) - 3}>threshold</text>
              {/if}
            </svg>
            {#if lane.note}<p class="lane-note">{lane.note}</p>{/if}
          </div>
        </div>
      {/each}
      <div class="row">
        <div class="label"></div>
        <p class="caption">The microphone lanes trail the sound by a few tens of milliseconds.</p>
      </div>
    {:else}
      <div class="row" data-row="levels-off">
        <div class="label">Audio levels</div>
        <p class="caption">
          {#if passwordSet === false}
            Set a dashboard password to record the audio levels or what was said.
            <a href="#/system/password">Set a password under System › Password and tokens</a>
          {:else}
            Audio levels are off.
          {/if}
        </p>
      </div>
    {/if}

    <div class="row" data-row="requests">
      <div class="label">Requests</div>
      <div class="cell">
        <svg width={pw} height={REQ_H} aria-hidden="true">
          <defs>
            <linearGradient id="tl-fade" x1="0" x2="1" y1="0" y2="0">
              <stop offset="0" class="fade-from" />
              <stop offset="1" class="fade-to" />
            </linearGradient>
            <linearGradient id="tl-fade-bad" x1="0" x2="1" y1="0" y2="0">
              <stop offset="0" class="fade-bad-from" />
              <stop offset="1" class="fade-bad-to" />
            </linearGradient>
          </defs>
          {@render grid(REQ_H)}
          {#each visibleRequests as r, i (r.id)}
            {@const b = requestBars(r, end)}
            {@const x0 = clampX(sc.x(b.waiting.startMs))}
            {@const xr = clampX(sc.x(b.receiving ? b.receiving.startMs : b.waiting.endMs))}
            {@const x1 = clampX(sc.x(b.receiving ? b.receiving.endMs : b.waiting.endMs))}
            {@const yy = 4 + i * REQ_ROW}
            {@const label = barLabel(r, x0, x1)}
            <g class="req" class:failed={requestFailed(r)} class:selected={selected?.type === "request" && selected.id === r.id}>
              {#if b.receiving}
                <rect class="wait" x={x0} y={yy + 2} width={Math.max(1, xr - x0)} height="12" rx="2" />
                <rect class="recv" class:open={b.open} x={xr} y={yy + 2} width={Math.max(2, x1 - xr)} height="12" rx="2" fill={b.open ? (requestFailed(r) ? "url(#tl-fade-bad)" : "url(#tl-fade)") : undefined} />
              {:else}
                <rect class="wait" x={x0} y={yy + 2} width={Math.max(2, x1 - x0)} height="12" rx="2" />
              {/if}
              <text class="req-label" x={label.x} y={yy + 12} text-anchor={label.anchor}>{label.text}</text>
            </g>
          {/each}
          {#if !visibleRequests.length}
            <text class="none" x="2" y="18">
              {homeAssistant ? "In Home Assistant mode, Home Assistant makes the requests itself. The satellite only fetches the reply's audio." : "No requests in this window."}
            </text>
          {/if}
        </svg>
        {#if hiddenRequests > 0}
          <p class="lane-note">{hiddenRequests} older {hiddenRequests === 1 ? "request is" : "requests are"} in the Requests view.</p>
        {/if}
      </div>
    </div>

    {#if hover}
      <div class="cross" style:left="{hover.left}px"></div>
      <div class="tip" class:flip={hover.x > pw / 2} style:left="{hover.left}px" style:top="{Math.max(0, hover.top - 8)}px" role="presentation">
        <p class="tip-time">{clock(hover.t)}</p>
        {#if hover.hit?.type === "event"}
          <p class="tip-hit">{eventWord(hover.hit.event.kind)}{hover.hit.event.detail ? `: ${hover.hit.event.detail}` : ""}</p>
          {#if hover.hit.event.endMs > hover.hit.event.unixMs}
            <p class="tip-line">{formatDuration(hover.hit.event.endMs - hover.hit.event.unixMs)}</p>
          {/if}
        {:else if hover.hit?.type === "request"}
          {@const id = hover.hit.id}
          {@const r = visibleRequests.find((q) => q.id === id)}
          {#if r}
            <p class="tip-hit">{requestLabel(r, end)}</p>
            <p class="tip-line">{formatSize(r.bytesOut ?? 0)} out · {formatSize(r.bytesIn)} in</p>
          {/if}
        {/if}
        <p class="tip-line">Phase: {atMute ? "Muted" : atPhase ? (PHASE_WORDS[atPhase.phase] ?? "Unknown") : "Unknown"}</p>
        {#each atValues as v (v.lane + v.name)}
          <p class="tip-line"><i class="sw {v.cls}"></i>{v.name}: {Number.isNaN(v.v) ? "no value" : v.fmt(v.v)}</p>
        {/each}
      </div>
    {/if}
  </div>

  <p class="legend-line">
    <span class="key"><svg width="12" height="12" aria-hidden="true">{@render shape("triangle", 6, 6, "m-signal", false)}</svg>Wake word</span>
    <span class="key"><svg width="12" height="12" aria-hidden="true">{@render shape("triangle-open", 6, 6, "m-signal", false)}</svg>Near miss</span>
    <span class="key"><svg width="12" height="12" aria-hidden="true">{@render shape("bar", 6, 6, "m-muted", false)}</svg>End of speech</span>
    <span class="key"><svg width="12" height="12" aria-hidden="true">{@render shape("square", 6, 6, "m-caution", false)}</svg>Interrupted</span>
    <span class="key"><svg width="12" height="12" aria-hidden="true">{@render shape("circle", 6, 6, "m-danger", false)}</svg>Error</span>
    <span class="key"><svg width="12" height="12" aria-hidden="true">{@render shape("circle-open", 6, 6, "m-speaking", false)}</svg>Home Assistant, request</span>
    <span class="key"><svg width="12" height="12" aria-hidden="true">{@render shape("diamond", 6, 6, "m-muted", false)}</svg>Mode, wake word, calibration or microphone changed</span>
  </p>
</div>

<style>
  .chart {
    container-type: inline-size;
    min-width: 0;
  }
  .rows {
    position: relative;
    display: flex;
    flex-direction: column;
    cursor: default;
  }
  .row {
    display: grid;
    grid-template-columns: 7.5rem minmax(0, 1fr);
    gap: var(--space-3);
    border-top: 1px solid var(--chart-grid);
  }
  .row:first-child {
    border-top: 0;
  }
  .label {
    padding-top: 6px;
    font-size: 13px;
    line-height: 18px;
    font-weight: 650;
    display: flex;
    flex-direction: column;
    min-width: 0;
  }
  .unit {
    font-weight: 400;
    color: var(--ink-muted);
    font-family: var(--font-mono);
    font-size: 12px;
  }
  .legend {
    display: flex;
    flex-direction: column;
    gap: 2px;
    margin-top: 4px;
    font-weight: 400;
    color: var(--ink-muted);
  }
  .key {
    display: inline-flex;
    align-items: center;
    gap: 6px;
  }
  .sw {
    display: inline-block;
    width: 14px;
    height: 0;
    border-top: 2px solid var(--ink-muted);
    flex: none;
  }
  .sw.s1 {
    border-color: var(--chart-1);
  }
  .sw.s2 {
    border-color: var(--chart-2);
  }
  .sw.s3 {
    border-color: var(--chart-3);
  }
  .sw.dash {
    border-top-style: dashed;
    border-color: var(--chart-threshold);
  }
  .sw.band {
    border-top-width: 4px;
    border-color: var(--line-strong);
  }
  .cell {
    min-width: 0;
  }
  svg {
    display: block;
    max-width: 100%;
    overflow: hidden;
  }
  .grid {
    stroke: var(--chart-grid);
    stroke-width: 1;
  }
  .axis {
    stroke: var(--chart-grid);
    stroke-width: 1;
  }
  .tick,
  .ylab,
  .stage-word,
  .sec-word,
  .none,
  .direct {
    font-family: var(--font-mono);
    font-size: 12px;
    fill: var(--ink-muted);
  }
  .sec-word,
  .stage-word {
    fill: var(--ink);
  }
  .ylab,
  .direct,
  .req-label {
    paint-order: stroke;
    stroke: var(--surface-raised);
    stroke-width: 3px;
    stroke-linejoin: round;
  }
  .direct {
    fill: var(--ink);
  }
  .direct.thr {
    fill: var(--ink-muted);
  }
  .sec {
    fill: var(--surface-sunken);
    stroke: var(--line);
    stroke-width: 1;
  }
  .sec.listening {
    fill: var(--listening-soft);
  }
  .sec.thinking {
    fill: var(--thinking-soft);
  }
  .sec.speaking {
    fill: var(--speaking-soft);
  }
  .sec.muted {
    fill: var(--danger-soft);
  }
  .mark.fill {
    stroke: none;
  }
  .mark.open {
    fill: var(--surface-raised);
    stroke-width: 2;
  }
  .m-signal.fill {
    fill: var(--signal);
  }
  .m-signal.open {
    stroke: var(--signal);
  }
  .m-danger.fill {
    fill: var(--danger);
  }
  .m-caution.fill {
    fill: var(--caution);
  }
  .m-muted.fill {
    fill: var(--ink-muted);
  }
  .m-speaking.open {
    stroke: var(--speaking);
  }
  .m-thinking.open {
    stroke: var(--thinking);
  }
  .ring {
    fill: none;
    stroke: var(--ink);
    stroke-width: 1.5;
  }
  .stage {
    fill: var(--thinking-soft);
    stroke: var(--thinking);
    stroke-width: 1;
  }
  .stage.selected {
    stroke: var(--ink);
    stroke-width: 2;
  }
  .line {
    fill: none;
    stroke-width: 2;
    stroke-linejoin: round;
    stroke-linecap: round;
  }
  .line.s1 {
    stroke: var(--chart-1);
  }
  .line.s2 {
    stroke: var(--chart-2);
  }
  .line.s3 {
    stroke: var(--chart-3);
  }
  .threshold {
    stroke: var(--chart-threshold);
    stroke-width: 1.5;
    stroke-dasharray: 5 4;
  }
  .speech {
    fill: var(--line-strong);
  }
  .req .wait {
    fill: var(--signal);
    opacity: 0.35;
  }
  .req .recv {
    fill: var(--signal);
  }
  .req.failed .wait,
  .req.failed .recv {
    fill: var(--danger);
  }
  .req.selected .wait,
  .req.selected .recv {
    stroke: var(--ink);
    stroke-width: 1.5;
  }
  .fade-from {
    stop-color: var(--signal);
  }
  .fade-to {
    stop-color: var(--signal);
    stop-opacity: 0.1;
  }
  .fade-bad-from {
    stop-color: var(--danger);
  }
  .fade-bad-to {
    stop-color: var(--danger);
    stop-opacity: 0.1;
  }
  .req-label {
    font-size: 12px;
    fill: var(--ink);
    font-family: var(--font-text);
  }
  .caption,
  .legend-line,
  .lane-note {
    margin: 0;
    padding: 6px 0;
    font-size: 13px;
    line-height: 18px;
    color: var(--ink-muted);
  }
  .lane-note {
    padding-top: 0;
  }
  .legend-line {
    display: flex;
    flex-wrap: wrap;
    gap: 4px var(--space-4);
    border-top: 1px solid var(--chart-grid);
    margin-top: var(--space-2);
    padding-top: var(--space-2);
  }
  .cross {
    position: absolute;
    top: 0;
    bottom: 0;
    width: 0;
    border-left: 1px solid var(--ink-muted);
    pointer-events: none;
  }
  .tip {
    position: absolute;
    z-index: 2;
    margin-left: 12px;
    min-width: 11rem;
    max-width: 22rem;
    padding: var(--space-2) var(--space-3);
    border: 1px solid var(--line);
    border-radius: var(--radius-md);
    background: var(--surface-raised);
    box-shadow: var(--shadow-pop);
    pointer-events: none;
    font-size: 13px;
    line-height: 18px;
  }
  .tip.flip {
    margin-left: -12px;
    transform: translateX(-100%);
  }
  .tip p {
    margin: 0;
    overflow-wrap: anywhere;
  }
  .tip-time {
    font-family: var(--font-mono);
    color: var(--ink-muted);
  }
  .tip-hit {
    font-weight: 650;
  }
  .tip-line {
    display: flex;
    align-items: center;
    gap: 6px;
    font-family: var(--font-mono);
  }
  .tip .sw {
    width: 10px;
  }
  @container (max-width: 600px) {
    .row {
      grid-template-columns: minmax(0, 1fr);
      gap: 0;
    }
    .label {
      padding: 6px 0 0;
      flex-direction: row;
      flex-wrap: wrap;
      align-items: baseline;
      gap: 0 var(--space-2);
    }
    .legend {
      flex-direction: row;
      flex-wrap: wrap;
      gap: 0 var(--space-3);
      margin-top: 0;
    }
    .caption {
      padding-top: 0;
    }
  }
</style>
