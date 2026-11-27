<!-- The debugging timeline's requests as a table (ADR 0036): the same data as the
     waterfall, in a form the keyboard and screen readers reach. The button in the
     first cell selects the row, and covers it for the pointer. -->
<script lang="ts">
  import {
    formatDuration,
    outcomeWord,
    requestAddress,
    requestFailed,
    requestStatus,
    type TimelineRequest,
  } from "./timeline.ts";

  let {
    requests,
    filtered,
    now,
    selectedId,
    onselect,
  }: {
    requests: TimelineRequest[];
    /** A filter is typed: an empty list means nothing matches, not that nothing happened. */
    filtered: boolean;
    now: number;
    selectedId: number | null;
    onselect: (id: number) => void;
  } = $props();

  const time = (ms: number) =>
    new Date(ms).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" });
  const total = (r: TimelineRequest) => (r.endMs ? formatDuration(r.endMs - r.startMs) : `${formatDuration(now - r.startMs)} so far`);
</script>

<div class="wrap">
  <table>
    <caption class="sr">Requests, newest first. Select a row for its details.</caption>
    <thead>
      <tr>
        <th scope="col">Time</th>
        <th scope="col">Status</th>
        <th scope="col" class="num">Total</th>
        <th scope="col">Address</th>
        <th scope="col">Result</th>
      </tr>
    </thead>
    <tbody>
      {#each requests as r (r.id)}
        <tr class:selected={selectedId === r.id} class:failed={requestFailed(r)}>
          <td class="mono">
            <button
              type="button"
              aria-label="{requestAddress(r)}, {requestStatus(r)}"
              aria-pressed={selectedId === r.id}
              onclick={() => onselect(r.id)}>{time(r.startMs)}</button
            >
          </td>
          <td class="mono">{requestStatus(r)}</td>
          <td class="mono num">{total(r)}</td>
          <td class="mono addr">{requestAddress(r)}</td>
          <td>{outcomeWord(r.outcome)}</td>
        </tr>
      {:else}
        <tr class="none"><td colspan="5">{filtered ? "No requests match." : "No requests yet."}</td></tr>
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
  tr.failed td:nth-child(2),
  tr.failed td:nth-child(5) {
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
