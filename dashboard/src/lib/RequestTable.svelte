<!-- The debugging timeline's requests as a table (ADR 0036): the same data as the
     waterfall, in a form the keyboard and screen readers reach. The button in the
     first cell selects the row, and covers it for the pointer. -->
<script lang="ts">
  import {
    REQ_KIND_WORDS,
    formatDuration,
    formatSize,
    outcomeWord,
    requestFailed,
    requestStatus,
    type TimelineRequest,
  } from "./timeline.ts";

  let {
    requests,
    now,
    selectedId,
    onselect,
  }: { requests: TimelineRequest[]; now: number; selectedId: number | null; onselect: (id: number) => void } = $props();

  const time = (ms: number) =>
    new Date(ms).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" });
  const firstByte = (r: TimelineRequest) => {
    const at = r.firstBodyMs || r.responseMs;
    return at ? formatDuration(at - r.startMs) : "";
  };
  const total = (r: TimelineRequest) => (r.endMs ? formatDuration(r.endMs - r.startMs) : `${formatDuration(now - r.startMs)} so far`);
</script>

<div class="wrap">
  <table>
    <caption class="sr">Requests, oldest first. Select a row for its details.</caption>
    <thead>
      <tr>
        <th scope="col">Time</th>
        <th scope="col">Kind</th>
        <th scope="col">Method</th>
        <th scope="col">Address</th>
        <th scope="col">Status</th>
        <th scope="col" class="num">First byte</th>
        <th scope="col" class="num">Total</th>
        <th scope="col" class="num">Sent</th>
        <th scope="col" class="num">Received</th>
        <th scope="col">Outcome</th>
      </tr>
    </thead>
    <tbody>
      {#each requests as r (r.id)}
        <tr class:selected={selectedId === r.id} class:failed={requestFailed(r)}>
          <td class="mono">
            <button type="button" aria-pressed={selectedId === r.id} onclick={() => onselect(r.id)}>{time(r.startMs)}</button>
          </td>
          <td>{REQ_KIND_WORDS[r.kind] ?? ""}</td>
          <td class="mono">{r.method}</td>
          <td class="mono addr">{r.url}</td>
          <td class="mono">{requestStatus(r)}</td>
          <td class="mono num">{firstByte(r)}</td>
          <td class="mono num">{total(r)}</td>
          <td class="mono num">{r.bytesOut === null ? "" : formatSize(r.bytesOut)}</td>
          <td class="mono num">{formatSize(r.bytesIn)}</td>
          <td>{outcomeWord(r.outcome)}</td>
        </tr>
      {:else}
        <tr class="none"><td colspan="10">No requests match.</td></tr>
      {/each}
    </tbody>
  </table>
</div>

<style>
  .wrap {
    overflow-x: auto;
    max-width: 100%;
  }
  table {
    width: 100%;
    border-collapse: collapse;
    font-size: 13px;
    line-height: 18px;
  }
  th,
  td {
    padding: 6px var(--space-3);
    text-align: left;
    white-space: nowrap;
    border-top: 1px solid var(--line);
  }
  thead th {
    border-top: 0;
    color: var(--ink-muted);
    font-weight: 650;
  }
  .num {
    text-align: right;
  }
  .mono {
    font-family: var(--font-mono);
    font-variant-numeric: tabular-nums;
  }
  .addr {
    max-width: 28rem;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  tbody tr {
    position: relative;
  }
  tbody tr:hover {
    background: var(--surface-sunken);
  }
  tr.selected {
    background: var(--signal-soft);
  }
  tr.failed td:nth-child(5),
  tr.failed td:nth-child(10) {
    color: var(--danger);
    font-weight: 650;
  }
  td button {
    all: unset;
    cursor: pointer;
    font: inherit;
  }
  /* The button covers its row, so a press anywhere on it selects it. */
  td button::after {
    content: "";
    position: absolute;
    inset: 0;
  }
  td button:focus-visible::after {
    box-shadow: inset 0 0 0 2px var(--focus);
  }
  .none td {
    color: var(--ink-muted);
    white-space: normal;
  }
  .sr {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip-path: inset(50%);
    white-space: nowrap;
  }
</style>
