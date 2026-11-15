<script lang="ts">
  import Device from "./Device.svelte";
  import { device } from "./device.svelte.ts";
  import Group from "./Group.svelte";
  import Ring from "./Ring.svelte";
  import { clock, timerLeft } from "./view.ts";

  const s = $derived(device.status);
  const elapsed = $derived(device.now - device.receivedAt);
  const live = $derived(device.link === "live");
  // The words are chosen now; the example addresses the first of them.
  const phrase = $derived(
    s?.wakeWord?.available.find((w) => w.id === s?.settings?.wakeWords[0])?.phrase ?? "Alexa",
  );
</script>

<Group title="Timers">
  {#snippet aside()}{#if s?.timers.length}{s.timers.length}{/if}{/snippet}
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
    <div class="pn-empty">
      <Device model={s?.model ?? ""} effect="idle" width={112} label="Ring off" />
      <span class="pn-empty-title">No timers</span>
      <p>Ask the satellite to set one, for example “{phrase}, set a pasta timer for ten minutes.”</p>
    </div>
  {/if}
</Group>

<style>
  .pn-timers {
    padding: 0 var(--space-4);
  }
</style>
