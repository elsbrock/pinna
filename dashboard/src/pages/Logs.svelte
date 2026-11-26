<!-- The journal, newest at the bottom. The log fills the window (App.svelte) and
     is the only thing that scrolls; it follows the newest line while it is
     scrolled to the bottom. -->
<script lang="ts">
  import { tick, untrack } from "svelte";

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
  /** The log is longer than its box. */
  let scrollable = $state(false);
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
  // What the filter searches: the line as shown, without the level's padding.
  const haystack = (l: Line) => `${l.time} ${l.level} ${l.message}`.toLowerCase();
  const shown = $derived(
    lines.filter(
      (l) => (!onlyProblems || l.level === "WARN" || l.level === "ERROR") && (!needle || haystack(l).includes(needle)),
    ),
  );
  const empty = $derived.by(() => {
    if (!lines.length) return connected ? "No log lines yet." : "Loading the log…";
    const what = onlyProblems ? "No warnings or errors" : "No lines";
    return needle ? `${what} match “${query.trim()}”.` : `${what}.`;
  });

  function atEnd(b: HTMLElement) {
    return b.scrollTop + b.clientHeight >= b.scrollHeight - SLACK;
  }

  // Where this page last put the log. A scroll event at that position isn't the
  // reader's: it reports the page's own move, or the browser keeping the log
  // in range as a filter shortens it.
  let placed = -1;
  function place(top: number) {
    if (!box) return;
    box.scrollTop = top;
    placed = box.scrollTop;
  }
  const toEnd = () => box && place(box.scrollHeight);

  // A filter changes the lines, not whether the log follows. While it doesn't,
  // the line the reader last scrolled to the top stays there, or the next one
  // the filter shows. It is kept until they scroll again, so a filter whose few
  // lines can't be scrolled there doesn't lose it.
  let anchor: { key: number; offset: number } | null = null;
  let restore = false;
  $effect.pre(() => {
    void onlyProblems;
    void needle;
    untrack(() => {
      if (!box || follow) return;
      restore = true;
      if (anchor && placed !== -1) return;
      const top = box.getBoundingClientRect().top;
      for (const el of box.querySelectorAll<HTMLElement>(".line")) {
        const r = el.getBoundingClientRect();
        if (r.bottom > top) return void (anchor = { key: Number(el.dataset.key), offset: r.top - top });
      }
    });
  });

  // Keep the newest line in view while following.
  $effect(() => {
    void shown;
    tick().then(() => {
      if (!box) return;
      if (follow) toEnd();
      else if (restore && anchor) {
        const { key, offset } = anchor;
        restore = false;
        const el = [...box.querySelectorAll<HTMLElement>(".line")].find((e) => Number(e.dataset.key) >= key);
        if (el) place(box.scrollTop + el.getBoundingClientRect().top - box.getBoundingClientRect().top - offset);
        else toEnd();
      }
      scrollable = box.scrollHeight > box.clientHeight;
    });
  });

  // A taller or narrower window moves the bottom: stay on it while following.
  $effect(() => {
    if (!box) return;
    const watch = new ResizeObserver(() => {
      if (follow) toEnd();
      if (box) scrollable = box.scrollHeight > box.clientHeight;
    });
    watch.observe(box);
    return () => watch.disconnect();
  });

  function scrolled() {
    if (!box || box.scrollTop === placed) return;
    placed = -1;
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
    const wifiNames = names.filter((n): n is string => !!n);
    const { markdown, kept } = issueReport(device.status, updates, lines.map(text), { wifi: wifiNames });
    await copyText(markdown);
    const what = kept === lines.length ? `all ${count(kept)}` : `the newest ${kept} of ${count(lines.length)}`;
    toasts.show(`Copied the details and ${what} of the log. Read it before you post it: issues are public.`);
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
  <div class="pn-segmented show" role="group" aria-label="Show">
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
  <div class="files">
    <button
      class="pn-btn pn-btn-icon"
      type="button"
      aria-label="Copy"
      title="Copy the lines shown"
      disabled={!shown.length}
      onclick={copy}
    >
      <Icon name="copy" />
    </button>
    <button
      class="pn-btn pn-btn-icon"
      type="button"
      aria-label="Download"
      title="Download the lines shown as a text file"
      disabled={!shown.length}
      onclick={download}
    >
      <Icon name="download" />
    </button>
  </div>
  <button class="pn-btn issue" type="button" onclick={copyForIssue}>Copy for an issue</button>
</div>

{#if error && !connected}
  <p class="pn-field-error" role="alert"><Icon name="alert" />{error} Trying again…</p>
{/if}

<div class="frame">
  <!-- Focusable so the keyboard can scroll it (a scrollable region must be reachable). -->
  <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
  <div class="log" bind:this={box} onscroll={scrolled} tabindex="0" role="log" aria-label="Log lines">
    {#each shown as l (l.key)}
      <!-- Most lines are INFO: only the others name their level. -->
      <div class="line" data-level={l.level} data-key={l.key}>
        <span class="time">{l.time}</span>
        <span class="msg"
          >{#if l.level !== "INFO"}<span class="level">{l.level}</span>{" "}{/if}{l.message}</span
        >
      </div>
    {:else}
      <p class="empty">{empty}</p>
    {/each}
  </div>
  {#if !follow && scrollable}
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
  .files {
    display: flex;
    gap: var(--space-2);
  }
  /* On a phone, two rows: what to show and the files, then the filter and the issue. */
  @media (max-width: 600px) {
    .files {
      order: 1;
      margin-left: auto;
    }
    .filter {
      order: 2;
      flex-basis: 8rem;
    }
    .issue {
      order: 3;
    }
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
    padding: var(--space-2) 0;
    border: 1px solid var(--line);
    border-radius: var(--radius-md);
    background: var(--surface-raised);
    font-family: var(--font-mono);
    font-size: 13px;
    line-height: 20px;
    font-variant-numeric: tabular-nums;
  }
  .log:focus-visible {
    outline: none;
    box-shadow: var(--focus-ring);
  }
  /* In the corner, clear of the toasts at the bottom centre. */
  .newest {
    position: absolute;
    right: var(--space-4);
    bottom: var(--space-4);
    box-shadow: var(--shadow-pop);
  }
  .line {
    display: grid;
    grid-template-columns: max-content minmax(0, 1fr);
    gap: var(--space-4);
    padding: 1px var(--space-4);
  }
  .line:hover {
    background: var(--surface-sunken);
  }
  .time {
    color: var(--ink-muted);
  }
  .line[data-level="WARN"] {
    background: color-mix(in srgb, var(--caution-soft) 70%, transparent);
    box-shadow: inset 3px 0 0 var(--caution);
  }
  .line[data-level="ERROR"] {
    background: color-mix(in srgb, var(--danger-soft) 70%, transparent);
    box-shadow: inset 3px 0 0 var(--danger);
  }
  .level {
    margin-right: 2px;
    font-size: 11px;
    font-weight: 700;
    letter-spacing: 0.04em;
  }
  .line[data-level="WARN"] .level {
    color: var(--caution);
  }
  .line[data-level="ERROR"] .level {
    color: var(--danger);
  }
  .msg {
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  .empty {
    margin: 0;
    padding: 0 var(--space-4);
    overflow-wrap: anywhere;
    color: var(--ink-muted);
    font-family: var(--font-text);
  }
  @media (max-width: 600px) {
    .line {
      grid-template-columns: minmax(0, 1fr);
    }
    .time {
      display: none;
    }
  }
</style>
