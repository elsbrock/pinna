<script lang="ts">
  import { device } from "./device.svelte.ts";
  import Icon from "./Icon.svelte";
  import { canStop, startBlocked, wakeLine } from "./view.ts";

  const s = $derived(device.status);
  const live = $derived(device.link === "live");

  // While the slider is held, the page shows its own value; the stream takes over after.
  let editing = $state<number | null>(null);
  const volume = $derived(editing ?? s?.volume ?? 0);

  async function commitVolume() {
    if (editing === null) return;
    await device.setVolume(editing);
    editing = null;
  }

  const startReason = $derived(startBlocked(s, device.link));
  const stoppable = $derived(canStop(s, device.link));
  const micsOn = $derived(!!s && !s.muted);
</script>

<section class="panel" aria-labelledby="controls-title">
  <h2 id="controls-title" class="pn-type-heading">Controls</h2>

  <label class="pn-slider">
    <span class="pn-slider-label">Volume</span>
    <output class="pn-slider-value" aria-hidden="true">{volume}</output>
    <input
      type="range"
      min="0"
      max="100"
      step="1"
      value={volume}
      style:--pct="{volume}%"
      disabled={!live}
      oninput={(e) => (editing = Number(e.currentTarget.value))}
      onchange={commitVolume}
    />
  </label>

  <div class="pn-switch">
    <button
      id="mics"
      class="pn-switch-control"
      role="switch"
      aria-checked={micsOn}
      aria-labelledby="mics-label"
      aria-describedby="mics-hint"
      disabled={!live || device.busy || !!s?.hardwareMuted}
      onclick={() => device.setMicrophones(!micsOn)}
    ></button>
    <span class="pn-switch-text">
      <label id="mics-label" class="pn-switch-label" for="mics">Microphones</label>
      <span id="mics-hint" class="pn-switch-hint">
        {#if s?.hardwareMuted}
          Off with the microphone button on the satellite. Press it there to turn them on.
        {:else if s}
          {wakeLine(s)}
        {/if}
      </span>
    </span>
  </div>

  <div class="actions">
    <button
      class="pn-btn pn-btn-primary pn-btn-lg"
      disabled={!!startReason || device.busy}
      aria-describedby="start-reason"
      onclick={() => device.startConversation()}
    >
      Start conversation
    </button>
    <button class="pn-btn pn-btn-lg" disabled={!stoppable || device.busy} onclick={() => device.stop()}>
      Stop
    </button>
  </div>
  {#if startReason && !stoppable}
    <p id="start-reason" class="pn-field-hint">{startReason}</p>
  {/if}
  {#if device.error}
    <p class="pn-field-error" role="alert"><Icon name="alert" />{device.error}</p>
  {/if}
</section>

<style>
  .actions {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-3);
  }
  p {
    margin: 0;
  }
</style>
