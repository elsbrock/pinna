<script lang="ts">
  import { device } from "./device.svelte.ts";
  import Ring from "./Ring.svelte";
  import { clock, timerLeft } from "./view.ts";

  const s = $derived(device.status);
  const elapsed = $derived(device.now - device.receivedAt);
  const live = $derived(device.link === "live");
</script>

<section class="panel" aria-labelledby="timers-title">
  <h2 id="timers-title" class="pn-type-heading">Timers</h2>
  {#if s?.alarmRinging || s?.timers.length}
    <div class="pn-timers">
      {#if s.alarmRinging}
        <div class="pn-timer is-ringing">
          <Ring effect="timer-ringing" size={32} label="Ring: timer ringing" />
          <div class="pn-timer-main">
            <span class="pn-timer-name">Timer</span>
            <span class="pn-timer-meta">Ringing</span>
          </div>
          <button class="pn-btn pn-btn-sm" disabled={!live || device.busy} onclick={() => device.stop()}>
            Stop
          </button>
        </div>
      {/if}
      {#each s.timers as t (t.id)}
        {@const left = timerLeft(t, elapsed)}
        <div class="pn-timer" class:is-paused={!t.active}>
          <Ring
            effect="timer"
            value={t.totalSeconds ? left / t.totalSeconds : 1}
            size={32}
            label="Ring: timer, {clock(left)} left"
          />
          <div class="pn-timer-main">
            <span class="pn-timer-name">{t.name || "Timer"}</span>
            <span class="pn-timer-meta">{t.active ? `of ${clock(t.totalSeconds)}` : "Paused"}</span>
          </div>
          <span class="pn-timer-left">{clock(left)}</span>
        </div>
      {/each}
    </div>
  {:else}
    <p class="empty">No timers. Ask the satellite to set one.</p>
  {/if}
</section>

<style>
  .empty {
    margin: 0;
    color: var(--ink-muted);
  }
</style>
