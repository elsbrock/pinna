<script lang="ts">
  import { ConnectError } from "@connectrpc/connect";
  import { tick } from "svelte";

  import { client } from "../lib/device.svelte.ts";
  import Icon from "../lib/Icon.svelte";
  import Switch from "../lib/Switch.svelte";

  type Line = { key: number; time: string; level: string; message: string };
  const MAX = 2000;

  let lines = $state<Line[]>([]);
  let error = $state<string | null>(null);
  let connected = $state(false);
  let onlyProblems = $state(false);
  let follow = $state(true);
  let copied = $state(false);
  let box = $state<HTMLElement>();
  let key = 0;

  const time = (ms: bigint) =>
    new Date(Number(ms)).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" });

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
            const line = { key: key++, time: time(l.unixMs), level: l.level, message: l.message };
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
          error = ConnectError.from(e).rawMessage;
        }
        connected = false;
        await new Promise((r) => setTimeout(r, 3000));
      }
    })();
    return () => abort.abort();
  });

  const shown = $derived(onlyProblems ? lines.filter((l) => l.level === "WARN" || l.level === "ERROR") : lines);

  // Keep the newest line in view while following.
  $effect(() => {
    void shown.length;
    if (follow && box) tick().then(() => box && (box.scrollTop = box.scrollHeight));
  });

  function scrolled() {
    if (!box) return;
    const atEnd = box.scrollTop + box.clientHeight >= box.scrollHeight - 24;
    if (follow !== atEnd) follow = atEnd;
  }

  async function copy() {
    const text = shown.map((l) => `${l.time} ${l.level.padEnd(5)} ${l.message}`).join("\n");
    try {
      await navigator.clipboard.writeText(text);
    } catch {
      // Not a secure context (plain http on the LAN): the old way still works.
      const area = document.createElement("textarea");
      area.value = text;
      document.body.append(area);
      area.select();
      document.execCommand("copy");
      area.remove();
    }
    copied = true;
    setTimeout(() => (copied = false), 1500);
  }
</script>

<p class="lead">
  pinnad's log from the journal, newest at the bottom. Debug lines are left out: they can contain what was said.
</p>

<div class="tools">
  <div class="filter" role="group" aria-label="Show">
    <button class="pn-btn pn-btn-sm" aria-pressed={!onlyProblems} onclick={() => (onlyProblems = false)}>All</button>
    <button class="pn-btn pn-btn-sm" aria-pressed={onlyProblems} onclick={() => (onlyProblems = true)}>
      Warnings and errors
    </button>
  </div>
  <Switch id="follow" label="Follow" checked={follow} toggle={(on) => (follow = on)} />
  <button class="pn-btn pn-btn-sm" disabled={!shown.length} onclick={copy}>{copied ? "Copied" : "Copy"}</button>
</div>

{#if error && !connected}
  <p class="pn-field-error" role="alert"><Icon name="alert" />Can't read the log: {error}. Trying again…</p>
{/if}

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
    <p class="empty">{onlyProblems && lines.length ? "No warnings or errors." : "No log lines yet."}</p>
  {/each}
</div>

<style>
  .lead {
    margin: 0;
    max-width: var(--measure);
    color: var(--ink-muted);
  }
  .tools {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--space-3) var(--space-6);
  }
  .filter {
    display: flex;
    gap: var(--space-1);
  }
  .filter [aria-pressed="true"] {
    background: var(--surface-sunken);
    border-color: var(--ink-muted);
  }
  .log {
    height: min(70vh, 640px);
    overflow: auto;
    padding: var(--space-3) var(--space-4);
    border: 1px solid var(--line);
    border-radius: var(--radius-md);
    background: var(--surface-sunken);
    font-family: var(--font-mono);
    font-size: 13px;
    line-height: 20px;
    font-variant-numeric: tabular-nums;
  }
  .log:focus-visible {
    outline: none;
    box-shadow: var(--focus-ring);
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
