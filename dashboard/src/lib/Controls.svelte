<script lang="ts">
  import { device } from "./device.svelte.ts";
  import Icon from "./Icon.svelte";
  import Slider from "./Slider.svelte";
  import Switch from "./Switch.svelte";
  import { canStop, startBlocked, wakeHint } from "./view.ts";

  const s = $derived(device.status);
  const live = $derived(device.link === "live");
  const startReason = $derived(startBlocked(s, device.link));
  const stoppable = $derived(canStop(s, device.link));
</script>

<section class="panel" aria-labelledby="controls-title">
  <h2 id="controls-title" class="pn-type-heading">Controls</h2>

  <Slider label="Volume" value={s?.volume ?? 0} disabled={!live} commit={(v) => device.setVolume(v)} />

  <Switch
    id="mics"
    label="Microphones"
    checked={!!s && !s.muted}
    disabled={!live || device.busy || !!s?.hardwareMuted}
    hint={s?.hardwareMuted
      ? s.softwareMuted
        ? "Off with the microphone button on the satellite, and in software. Press the button, then turn them on here."
        : "Off with the microphone button on the satellite. Press it there to turn them on."
      : undefined}
    toggle={(on) => device.setMicrophones(on)}
  />

  {#if s}
    <Switch
      id="wake"
      label="Wake word"
      checked={!!s.settings?.wakeWord}
      disabled={!live || device.busy}
      hint={wakeHint(s)}
      toggle={(on) => device.updateSettings({ wakeWord: on })}
    />
  {/if}

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
