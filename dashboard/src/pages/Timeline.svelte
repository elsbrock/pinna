<!-- Requests and the audio pipeline on one time axis (ADR 0036). Debugging is off
     until someone starts it, for a time. The page follows WatchTimeline, holds at
     most ten minutes, and scrolls as a page: it follows time sideways, and the
     chart's time axis stays in view. -->
<script lang="ts">
  import { onMount, untrack } from "svelte";

  import { ConfirmationPurpose } from "../gen/pinna/v1/device_pb.ts";
  import { isStandalone } from "../lib/assistant.ts";
  import { clock } from "../lib/clock.svelte.ts";
  import Confirm from "../lib/Confirm.svelte";
  import ConfirmAtSatellite from "../lib/ConfirmAtSatellite.svelte";
  import { client, device, errorSentence } from "../lib/device.svelte.ts";
  import Group from "../lib/Group.svelte";
  import Icon from "../lib/Icon.svelte";
  import RequestTable from "../lib/RequestTable.svelte";
  import { security } from "../lib/security.svelte.ts";
  import SwitchRow from "../lib/SwitchRow.svelte";
  import TimelineChart from "../lib/TimelineChart.svelte";
  import TimelineDetails from "../lib/TimelineDetails.svelte";
  import { timelineStore } from "../lib/timeline.svelte.ts";
  import {
    debuggingHint,
    exportName,
    filterRequests,
    holdsText,
    lastRecorded,
    requestsNewestFirst,
    timeLeft,
    timelineExport,
    type Timeline,
  } from "../lib/timeline.ts";
  import { toasts } from "../lib/toast.svelte.ts";
  import { secondsUntil } from "../lib/view.ts";

  const store = timelineStore;
  $effect(() => store.watch());
  $effect(() => clock.use());
  onMount(() => {
    void security.refresh();
  });

  const MINUTES = [
    { minutes: 15, label: "15 min" },
    { minutes: 60, label: "1 h" },
    { minutes: 480, label: "8 h" },
  ];
  const WINDOWS = [1, 5, 10];

  // What is drawn: a snapshot, at most four times a second while live, and
  // frozen while paused. Data keeps arriving into the store meanwhile.
  let snap = $state.raw<{ tl: Timeline; end: number }>(
    store.paused && store.frozen ? store.frozen : { tl: store.data, end: store.now() },
  );
  // Pause freezes the chart; the Requests view has no Pause, so it stays live.
  $effect(() => {
    const id = setInterval(() => {
      if (!store.paused || store.view !== "timeline") snap = { tl: store.data, end: store.now() };
    }, 250);
    return () => clearInterval(id);
  });
  function pause() {
    store.frozen = snap;
    store.paused = true;
  }
  function resume() {
    store.paused = false;
    store.frozen = null;
    snap = { tl: store.data, end: store.now() };
  }

  let query = $state("");
  let confirmingStop = $state(false);

  const st = $derived(store.data.state);
  const satNow = $derived(clock.now + store.data.offset);
  const left = $derived(st ? secondsUntil(st.untilMs, satNow) : 0);
  const on = $derived(!!st?.on && left > 0);
  const levelsLeft = $derived(st && st.levelsUntilMs ? secondsUntil(st.levelsUntilMs, satNow) : 0);
  const textLeft = $derived(st && st.textUntilMs ? secondsUntil(st.textUntilMs, satNow) : 0);
  const hasData = $derived(store.data.events.length + store.data.requests.length + store.data.levels.length > 0);
  const passwordSet = $derived(security.state?.passwordSet);
  const homeAssistant = $derived(!isStandalone(device.status));
  const live = $derived(device.link === "live");
  const shown = $derived(filterRequests(requestsNewestFirst(snap.tl.requests), query));
  // Debugging has ended: the chart's right edge stays at the newest thing kept, not at the clock.
  const recordedUntil = $derived(lastRecorded(store.data));
  const chartEnd = $derived(on ? snap.end : lastRecorded(snap.tl) || snap.end);
  const clockTime = (ms: number) =>
    new Date(ms).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" });

  // A request the filter hides has no details.
  $effect(() => {
    query;
    untrack(() => {
      const sel = store.selected;
      if (sel?.type === "request" && !filterRequests(requestsNewestFirst(store.data.requests), query).some((r) => r.id === sel.id)) {
        store.selected = null;
      }
    });
  });

  // Starting.
  let minutes = $state(15);
  let levels = $state(false);
  let text = $state(false);
  let stage = $state<"idle" | "confirming" | "starting">("idle");
  let startError = $state<string | null>(null);

  async function start(grant: string) {
    stage = "starting";
    startError = null;
    try {
      await client.startDebugging({ minutes, levels: levels && passwordSet === true, textGrant: grant });
    } catch (e) {
      startError = errorSentence(e);
    }
    stage = "idle";
  }
  function begin() {
    startError = null;
    if (text && passwordSet) stage = "confirming";
    else void start("");
  }

  let stopping = $state(false);
  async function stop() {
    stopping = true;
    try {
      await client.stopDebugging({});
      store.selected = null;
    } catch (e) {
      toasts.show(errorSentence(e), "danger");
    }
    stopping = false;
  }

  function download() {
    const now = new Date();
    const name = device.status?.name || "pinna";
    const blob = new Blob([timelineExport(name, store.data, now)], { type: "application/json" });
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url;
    a.download = exportName(name, now);
    a.click();
    URL.revokeObjectURL(url);
  }

  const lower = (t: string) => `${t.charAt(0).toLowerCase()}${t.slice(1)}`;

  const emptyText = "Nothing recorded yet. Say the wake word, or ask something from the Assistant page.";
</script>

{#snippet form()}
  <Group title="Start debugging">
    <div class="pn-row">
      <div class="pn-row-text">
        <span class="pn-row-label" id="tl-how-long">How long</span>
        <span class="pn-row-hint">
          The satellite keeps only the last 10 minutes. Open this page or download within 10 minutes of a problem. A
          restart clears it.
        </span>
      </div>
      <div class="pn-segmented" role="group" aria-labelledby="tl-how-long">
        {#each MINUTES as m (m.minutes)}
          <button type="button" disabled={stage === "confirming"} aria-pressed={minutes === m.minutes} onclick={() => (minutes = m.minutes)}>{m.label}</button>
        {/each}
      </div>
    </div>
    <SwitchRow
      id="tl-levels"
      label="Audio levels"
      hint="The microphone and speaker levels, the gain and the wake word scores, every 100 ms. Never audio. Up to an hour."
      checked={levels && passwordSet === true}
      disabled={passwordSet !== true || stage === "confirming"}
      toggle={(v) => (levels = v)}
    />
    <SwitchRow
      id="tl-text"
      label="What was said and the replies"
      hint="The text of each request: the transcript, the prompt and the reply. It waits for a press of Action. Up to an hour."
      checked={text && passwordSet === true}
      disabled={passwordSet !== true || stage === "confirming"}
      toggle={(v) => (text = v)}
    />
    {#if passwordSet === false}
      <div class="pn-row is-stack">
        <div class="pn-notice" data-tone="caution">
          <Icon name="alert" class="pn-icon pn-notice-icon" />
          <div class="pn-notice-body">
            <p class="pn-notice-title">Set a dashboard password to record the audio levels or what was said.</p>
            <p><a href="#/system/password">Set a password under System › Password and tokens</a></p>
          </div>
        </div>
      </div>
    {/if}
    {#if stage === "confirming"}
      <div class="pn-row is-stack">
        <ConfirmAtSatellite
          purpose={ConfirmationPurpose.DEBUGGING_TEXT}
          ongrant={(grant) => void start(grant)}
          oncancel={() => (stage = "idle")}
          skip={{ label: "Start without what was said", onskip: () => void start("") }}
        />
      </div>
    {:else}
      <div class="pn-row">
        <div class="pn-row-text"></div>
        <button class="pn-btn pn-btn-primary" type="button" disabled={!live || stage === "starting"} onclick={begin}>
          Start
        </button>
      </div>
    {/if}
    {#if startError}
      <div class="pn-row">
        <span class="pn-field-error" role="alert"><Icon name="alert" />{startError}</span>
      </div>
    {/if}
  </Group>
{/snippet}

{#snippet windows_()}
  <div class="pn-segmented" role="group" aria-label="Window">
    {#each WINDOWS as w (w)}
      <button type="button" aria-pressed={store.windowMin === w} onclick={() => (store.windowMin = w)}>{w} min</button>
    {/each}
  </div>
{/snippet}

{#snippet views_()}
  <div class="pn-segmented" role="group" aria-label="View">
    <button type="button" aria-pressed={store.view === "timeline"} onclick={() => (store.view = "timeline")}>Timeline</button>
    <button type="button" aria-pressed={store.view === "requests"} onclick={() => (store.view = "requests")}>Requests</button>
  </div>
{/snippet}

{#snippet download_()}
  <button class="pn-btn" type="button" disabled={!hasData} onclick={download}>
    <Icon name="download" />Download
  </button>
{/snippet}

{#if store.error && !store.connected}
  <p class="pn-field-error" role="alert"><Icon name="alert" />{store.error} Trying again…</p>
{/if}

{#if !st}
  {#if !store.error}
    <span class="pn-skeleton" style:height="72px"></span>
    <span class="pn-skeleton frame-skeleton"></span>
  {/if}
{:else}
  {#if on}
    <p class="status">
      {[
        debuggingHint({ secondsLeft: left }),
        levelsLeft > 0 && `levels: ${lower(timeLeft(levelsLeft))}`,
        textLeft > 0 && `text: ${lower(timeLeft(textLeft))}`,
      ]
        .filter(Boolean)
        .join(" · ")}
    </p>
    <div class="tools">
      {#if store.view === "timeline"}
        <div class="pn-segmented" role="group" aria-label="Live or paused">
          <button type="button" aria-pressed={!store.paused} onclick={resume}>Live</button>
          <button type="button" aria-pressed={store.paused} onclick={pause}>Pause</button>
        </div>
        {@render windows_()}
      {/if}
      {@render views_()}
      <div class="push">
        {@render download_()}
        <button class="pn-btn" type="button" disabled={stopping || !live} onclick={() => (confirmingStop = true)}>
          Stop debugging
        </button>
      </div>
    </div>
    {#if holdsText(store.data.requests)}
      <p class="hint">The file contains what was said. Send it privately, not in a public issue.</p>
    {/if}
  {:else}
    {#if hasData}
      <p class="status">
        Off. Recorded until {clockTime(recordedUntil)}. What was kept is cleared within 10 minutes.
      </p>
      <div class="tools">
        {#if store.view === "timeline"}{@render windows_()}{/if}
        {@render views_()}
        <div class="push">{@render download_()}</div>
      </div>
      {#if holdsText(store.data.requests)}
        <p class="hint">The file contains what was said. Send it privately, not in a public issue.</p>
      {/if}
    {/if}
    {@render form()}
  {/if}

  {#if on || hasData}
    {#if store.view === "requests"}
      <input
        class="pn-input filter"
        type="search"
        placeholder="Filter"
        aria-label="Filter requests"
        autocomplete="off"
        spellcheck="false"
        bind:value={query}
      />
    {/if}
    <div class="frame">
      {#if !hasData}
        <p class="empty">{emptyText}</p>
      {:else}
        <div class="scroll">
          {#if store.view === "timeline"}
            <TimelineChart
              tl={snap.tl}
              end={chartEnd}
              windowMs={store.windowMin * 60_000}
              {homeAssistant}
              {passwordSet}
              selected={store.selected}
              onselect={(s) => (store.selected = s)}
            />
          {:else if snap.tl.requests.length || !homeAssistant}
            <RequestTable
              requests={shown}
              filtered={query.trim() !== ""}
              now={snap.end}
              selectedId={store.selected?.type === "request" ? store.selected.id : null}
              onselect={(id) => (store.selected = { type: "request", id })}
            />
          {:else}
            <p class="empty">
              In Home Assistant mode, Home Assistant makes the requests itself. The satellite only fetches the reply's
              audio.
            </p>
          {/if}
          {#if store.selected}
            <TimelineDetails selection={store.selected} requests={snap.tl.requests} now={snap.end} onclose={() => (store.selected = null)} />
          {/if}
          <p class="hint">Not recorded: update checks, Sendspin and AirPlay.</p>
        </div>
      {/if}
    </div>
  {/if}
{/if}

<Confirm bind:open={confirmingStop} title="Stop debugging?" action="Stop and clear" danger confirm={stop}>
  <p>This clears what the satellite kept, here and in every open dashboard. Download it first to keep it.</p>
</Confirm>

<style>
  .status {
    margin: 0;
    font-weight: 650;
  }
  .hint {
    margin: 0;
    font-size: 13px;
    line-height: 18px;
    color: var(--ink-muted);
  }
  .tools {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--space-2) var(--space-3);
  }
  .push {
    margin-left: auto;
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-2) var(--space-3);
  }
  .filter {
    width: auto;
    max-width: 24rem;
  }
  .frame-skeleton {
    height: 12rem;
  }
  /* The page scrolls, not the frame: the chart's time axis stays at the top
     of the window while the bands go by. */
  .frame {
    min-height: 16rem;
    display: flex;
    min-width: 0;
  }
  .scroll {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
    padding: var(--space-3) var(--space-4);
    border: 1px solid var(--line);
    border-radius: var(--radius-md);
    background: var(--surface-raised);
    box-shadow: var(--shadow-raised);
  }
  .empty {
    margin: 0;
    padding: var(--space-4);
    color: var(--ink-muted);
  }
  .frame > .empty {
    flex: 1;
    border: 1px solid var(--line);
    border-radius: var(--radius-md);
    background: var(--surface-raised);
  }
  .scroll > .empty {
    padding: 0;
  }
</style>
