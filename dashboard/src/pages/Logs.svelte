<!-- The journal, newest at the bottom. The log fills the window (App.svelte) and
     is the only thing that scrolls; it follows the newest line while it is
     scrolled to the bottom. -->
<script lang="ts">
  import { tick } from "svelte";

  import type { GetUpdatesResponse } from "../gen/pinna/v1/device_pb.ts";
  import { copyText } from "../lib/copy.ts";
  import { client, device, errorSentence } from "../lib/device.svelte.ts";
  import Icon from "../lib/Icon.svelte";
  import { issueReport } from "../lib/report.ts";
  import { toasts } from "../lib/toast.svelte.ts";
  import { wifi } from "../lib/wifi.svelte.ts";

  type Line = { key: number; time: string; level: string; message: string };
  const MAX = 2000;
  /** How close to the bottom still counts as at the bottom, in pixels. */
  const SLACK = 24;

  let lines = $state<Line[]>([]);
  let error = $state<string | null>(null);
  let connected = $state(false);
  let onlyProblems = $state(false);
  let query = $state("");
  let follow = $state(true);
  let box = $state<HTMLElement>();
  let updates = $state<GetUpdatesResponse | null>(null);
  let key = 0;

  const time = (ms: number) =>
    new Date(ms).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" });
  const text = (l: Line) => `${l.time} ${l.level.padEnd(5)} ${l.message}`;

  // Follow the journal while the page is open; start again (with the backlog) after a break.
  $effect(() => {
    const abort = new AbortController();
    (async () => {
      while (!abort.signal.aborted) {
        try {
          let first = true;
          for await (const res of client.watchLogs({}, { signal: abort.signal })) {
            const l = res.line;
            if (!l) continue;
            const line = { key: key++, time: time(Number(l.unixMs)), level: l.level, message: l.message };
            if (first) {
              // A new connection sends the backlog again: replace, don't append.
              lines = [];
              first = false;
              connected = true;
              error = null;
            }
            lines.push(line);
            if (lines.length > MAX) lines.splice(0, lines.length - MAX);
          }
        } catch (e) {
          if (abort.signal.aborted) return;
          error = errorSentence(e);
        }
        connected = false;
        await new Promise((r) => setTimeout(r, 3000));
      }
    })();
    return () => abort.abort();
  });

  // The running slot, for Copy for an issue; it changes only with a restart.
  // Without it the report says the slot is unknown.
  $effect(() => {
    client.getUpdates({}, { timeoutMs: 10_000 }).then(
      (u) => (updates = u),
      () => {},
    );
  });

  const needle = $derived(query.trim().toLowerCase());
  const shown = $derived(
    lines.filter(
      (l) =>
        (!onlyProblems || l.level === "WARN" || l.level === "ERROR") &&
        (!needle || text(l).toLowerCase().includes(needle)),
    ),
  );
  const empty = $derived.by(() => {
    if (!lines.length) return "No log lines yet.";
    const what = onlyProblems ? "No warnings or errors" : "No lines";
    return needle ? `${what} match “${query.trim()}”.` : `${what}.`;
  });

  function atEnd(b: HTMLElement) {
    return b.scrollTop + b.clientHeight >= b.scrollHeight - SLACK;
  }

  function toEnd() {
    if (box) box.scrollTop = box.scrollHeight;
  }

  // Keep the newest line in view while following. When a filter shortens the
  // log so that it no longer scrolls, it is at the bottom again.
  $effect(() => {
    void shown;
    tick().then(() => {
      if (!box) return;
      if (follow) toEnd();
      else if (atEnd(box)) follow = true;
    });
  });

  // A taller or narrower window moves the bottom: stay on it while following.
  $effect(() => {
    if (!box) return;
    const watch = new ResizeObserver(() => follow && toEnd());
    watch.observe(box);
    return () => watch.disconnect();
  });

  function scrolled() {
    if (!box) return;
    const end = atEnd(box);
    if (follow !== end) follow = end;
  }

  function newest() {
    follow = true;
    toEnd();
    // The button goes away: keep the keyboard on the log, not on the page.
    box?.focus();
  }

  const count = (n: number) => `${n} ${n === 1 ? "line" : "lines"}`;

  async function copy() {
    await copyText(shown.map(text).join("\n"));
    toasts.show(`Copied ${count(shown.length)}`);
  }

  // Every line the page holds, whatever the filter: the reader of the issue
  // needs the context.
  async function copyForIssue() {
    if (!device.status) return;
    const names = [...(wifi.saved ?? []).map((w) => w.ssid), wifi.trial?.ssid, wifi.last?.ssid, wifi.lost];
    const known = names.filter((n): n is string => !!n);
    await copyText(issueReport(device.status, updates, lines.map(text), known));
    toasts.show("Copied. Read it before you post it: issues are public.");
  }

  function download() {
    const blob = new Blob([shown.map(text).join("\n") + "\n"], { type: "text/plain" });
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    // Local time, as the lines show it.
    const d = new Date();
    const p = (n: number) => String(n).padStart(2, "0");
    const stamp = `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}-${p(d.getHours())}${p(d.getMinutes())}`;
    a.href = url;
    a.download = `${device.status?.name || "pinna"}-log-${stamp}.txt`;
    a.click();
    URL.revokeObjectURL(url);
  }
</script>

<div class="tools">
  <div class="pn-segmented" role="group" aria-label="Show">
    <button type="button" aria-pressed={!onlyProblems} onclick={() => (onlyProblems = false)}>All</button>
    <button type="button" aria-pressed={onlyProblems} onclick={() => (onlyProblems = true)}>Warnings and errors</button>
  </div>
  <input
    class="pn-input filter"
    type="search"
    placeholder="Filter"
    aria-label="Filter lines"
    autocomplete="off"
    spellcheck="false"
    bind:value={query}
  />
  <div class="actions">
    <button class="pn-btn" type="button" disabled={!shown.length} onclick={copy}>
      Copy
    </button>
    <button class="pn-btn" type="button" disabled={!shown.length} onclick={download}>
      Download
    </button>
    <button class="pn-btn" type="button" onclick={copyForIssue}>Copy for an issue</button>
  </div>
</div>

{#if error && !connected}
  <p class="pn-field-error" role="alert"><Icon name="alert" />{error} Trying again…</p>
{/if}

<div class="frame">
  <!-- Focusable so the keyboard can scroll it (a scrollable region must be reachable). -->
  <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
  <div class="log" bind:this={box} onscroll={scrolled} tabindex="0" role="log" aria-label="Log lines">
    {#each shown as l (l.key)}
      <div class="line" data-level={l.level}>
        <span class="time">{l.time}</span>
        <span class="level">{l.level}</span>
        <span class="msg">{l.message}</span>
      </div>
    {:else}
      <p class="empty">{empty}</p>
    {/each}
  </div>
  {#if !follow}
    <button class="pn-btn pn-btn-sm newest" type="button" onclick={newest}>
      <Icon name="chevron-down" />Newest
    </button>
  {/if}
</div>

<style>
  .tools {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--space-3);
  }
  .filter {
    flex: 1 1 12rem;
    width: auto;
  }
  .actions {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-2);
  }
  /* The rest of the window, down to its bottom edge (or the tab bar). */
  .frame {
    position: relative;
    flex: 1 1 0;
    min-height: 12rem;
    display: flex;
  }
  .log {
    flex: 1;
    min-width: 0;
    overflow: auto;
    overscroll-behavior: contain;
    padding: var(--space-3) var(--space-4);
    border: 1px solid var(--line);
    border-radius: var(--radius-md);
    background: var(--surface-raised);
    box-shadow: var(--shadow-raised);
    font-family: var(--font-mono);
    font-size: 13px;
    line-height: 20px;
    font-variant-numeric: tabular-nums;
  }
  .log:focus-visible {
    outline: none;
    box-shadow: var(--focus-ring);
  }
  .newest {
    position: absolute;
    left: 50%;
    bottom: var(--space-4);
    transform: translateX(-50%);
    box-shadow: var(--shadow-pop);
  }
  .line {
    display: grid;
    grid-template-columns: max-content 5ch minmax(0, 1fr);
    gap: var(--space-3);
  }
  .time {
    color: var(--ink-muted);
  }
  .level {
    font-weight: 650;
    color: var(--ink-muted);
  }
  .line[data-level="WARN"] .level {
    color: var(--caution);
  }
  .line[data-level="ERROR"] .level,
  .line[data-level="ERROR"] .msg {
    color: var(--danger);
  }
  .msg {
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  .empty {
    margin: 0;
    color: var(--ink-muted);
    font-family: var(--font-text);
  }
  @media (max-width: 600px) {
    .line {
      grid-template-columns: max-content minmax(0, 1fr);
    }
    .time {
      display: none;
    }
  }
</style>
