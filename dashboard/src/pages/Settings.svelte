<script lang="ts">
  import { device } from "../lib/device.svelte.ts";
  import Icon from "../lib/Icon.svelte";
  import Slider from "../lib/Slider.svelte";

  const s = $derived(device.status);
  const set = $derived(s?.settings);
  const live = $derived(device.link === "live");
</script>

<p class="lead">
  These settings are shared with Home Assistant: a change here shows up there, and the other way round.
  The satellite keeps them across restarts.
</p>

{#if set}
  <div class="grid">
    <section class="panel" aria-labelledby="wake-title">
      <h2 id="wake-title" class="pn-type-heading">Wake word</h2>
      <Slider
        label="Sensitivity"
        value={set.wakeSensitivity}
        disabled={!live}
        hint="Higher wakes more easily, and more often by mistake."
        commit={(v) => device.updateSettings({ wakeSensitivity: v })}
      />
      <p class="pn-field-hint">
        The satellite listens for <strong>"{s?.wakeWord?.phrase || "its wake word"}"</strong>. Turn it on or off
        on the <a href="#/">Overview</a>. A different wake word is set with <code class="pn-code">[wake]</code> in
        <code class="pn-code">/var/lib/pinna/pinnad.toml</code>.
      </p>
    </section>

    <section class="panel" aria-labelledby="ring-title">
      <h2 id="ring-title" class="pn-type-heading">Ring</h2>
      <Slider
        label="Brightness"
        value={set.ledBrightness}
        disabled={!live}
        hint="How bright the light ring is in every state."
        commit={(v) => device.updateSettings({ ledBrightness: v })}
      />
    </section>
  </div>
  {#if device.error}
    <p class="pn-field-error" role="alert"><Icon name="alert" />{device.error}</p>
  {/if}
{/if}

<style>
  .lead {
    margin: 0;
    max-width: var(--measure);
    color: var(--ink-muted);
  }
  .grid {
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    gap: var(--space-6);
    align-items: start;
  }
  p {
    margin: 0;
  }
  a {
    color: var(--signal);
  }
  @media (min-width: 900px) {
    .grid {
      grid-template-columns: repeat(2, minmax(0, 1fr));
    }
  }
</style>
