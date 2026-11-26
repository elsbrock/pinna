<!-- The selected request or event of the debugging timeline, with every field it
     has (ADR 0036). Text bodies are folded: they are what was said. -->
<script lang="ts">
  import { copyText } from "./copy.ts";
  import { toasts } from "./toast.svelte.ts";
  import {
    EV,
    PHASE_WORDS,
    REQ_KIND_WORDS,
    eventWord,
    formatDuration,
    formatSize,
    outcomeWord,
    requestStatus,
    type Selection,
    type TimelineEvent,
    type TimelineRequest,
  } from "./timeline.ts";

  let {
    selection,
    requests,
    now,
    onclose,
  }: { selection: Selection; requests: TimelineRequest[]; now: number; onclose: () => void } = $props();

  const request = $derived(selection.type === "request" ? requests.find((r) => r.id === selection.id) : undefined);
  const event = $derived(selection.type === "event" ? selection.event : undefined);

  const time = (ms: number) =>
    new Date(ms).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" }) +
    `.${String(ms % 1000).padStart(3, "0")}`;
  const after = (r: TimelineRequest, at: number) => (at ? formatDuration(at - r.startMs) : "not yet");

  function requestFields(r: TimelineRequest): [string, string][] {
    return [
      ["Kind", REQ_KIND_WORDS[r.kind] ?? "Unknown"],
      ["Request", `${r.method} ${r.url}`],
      ["Status", requestStatus(r)],
      ["Outcome", outcomeWord(r.outcome)],
      ["Started", time(r.startMs)],
      ["Response after", after(r, r.responseMs)],
      ["First body byte after", after(r, r.firstBodyMs)],
      ["Total", r.endMs ? formatDuration(r.endMs - r.startMs) : `${formatDuration(now - r.startMs)} so far, still open`],
      ["Sent", r.bytesOut === null ? "unknown" : formatSize(r.bytesOut)],
      ["Received", formatSize(r.bytesIn)],
    ];
  }

  function eventFields(e: TimelineEvent): [string, string][] {
    const out: [string, string][] = [
      ["Kind", eventWord(e.kind)],
      ["Time", time(e.unixMs)],
    ];
    if (e.detail) out.push(["Detail", e.detail]);
    if (e.endMs > e.unixMs) out.push(["Lasted", formatDuration(e.endMs - e.unixMs)]);
    if (e.kind === EV.phase) out.push(["Phase", PHASE_WORDS[e.phase] ?? "Unknown"]);
    if (e.kind === EV.wake || e.kind === EV.nearMiss) {
      out.push(["Score", e.score.toFixed(2)], ["Threshold", e.threshold.toFixed(2)]);
      out.push(["Level", `${e.levelDbfs.toFixed(1)} dBFS`], ["Gain", `${e.gainDb.toFixed(1)} dB`]);
    }
    if (e.run) out.push(["Run", String(e.run)]);
    return out;
  }

  async function copy(text: string) {
    await copyText(text);
    toasts.show("Copied");
  }
</script>

<section class="details" aria-label="Details">
  <div class="head">
    <h3>{request ? "Request" : (event ? eventWord(event.kind) : "Request")}</h3>
    <button class="pn-btn pn-btn-sm" type="button" onclick={onclose}>Close</button>
  </div>
  {#if selection.type === "request" && !request}
    <p class="muted">This request is no longer kept.</p>
  {:else}
    <dl>
      {#each request ? requestFields(request) : event ? eventFields(event) : [] as [k, v] (k)}
        <dt>{k}</dt>
        <dd>{v}</dd>
      {/each}
    </dl>
    {#if request}
      {#each [["Request text", request.bodyOut], ["Response text", request.bodyIn]] as [title, body] (title)}
        {#if body}
          <details>
            <summary>{title}</summary>
            <!-- Focusable so the keyboard can scroll it. -->
            <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
            <pre tabindex="0">{body}</pre>
            <button class="pn-btn pn-btn-sm" type="button" onclick={() => copy(body)}>Copy</button>
          </details>
        {/if}
      {/each}
    {/if}
  {/if}
</section>

<style>
  .details {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
    padding: var(--space-3) var(--space-4);
    border: 1px solid var(--line);
    border-radius: var(--radius-md);
    background: var(--surface-raised);
  }
  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-3);
  }
  h3 {
    margin: 0;
    font-size: 15px;
    line-height: 22px;
  }
  .muted {
    margin: 0;
    color: var(--ink-muted);
  }
  dl {
    display: grid;
    grid-template-columns: max-content minmax(0, 1fr);
    gap: 2px var(--space-4);
    margin: 0;
    font-size: 13px;
    line-height: 18px;
  }
  dt {
    color: var(--ink-muted);
  }
  dd {
    margin: 0;
    font-family: var(--font-mono);
    overflow-wrap: anywhere;
  }
  details {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
  }
  summary {
    cursor: pointer;
    font-weight: 650;
  }
  pre {
    margin: var(--space-2) 0 0;
    max-height: 16rem;
    overflow: auto;
    padding: var(--space-2) var(--space-3);
    border-radius: var(--radius-sm);
    background: var(--surface-sunken);
    font-family: var(--font-mono);
    font-size: 13px;
    line-height: 18px;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  pre:focus-visible {
    outline: none;
    box-shadow: var(--focus-ring);
  }
  details > button {
    align-self: flex-start;
  }
</style>
