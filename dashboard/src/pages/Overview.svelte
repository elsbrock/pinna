<script lang="ts">
  import Controls from "../lib/Controls.svelte";
  import Details from "../lib/Details.svelte";
  import { device } from "../lib/device.svelte.ts";
  import PhaseBadge from "../lib/PhaseBadge.svelte";
  import Ring from "../lib/Ring.svelte";
  import Timers from "../lib/Timers.svelte";
  import { badge, health, ringEffect } from "../lib/view.ts";

  const s = $derived(device.status);
  // Phones get the small ring, so Volume and Microphones stay near the top.
  const phone = matchMedia("(max-width: 599px)");
  let small = $state(phone.matches);
  $effect(() => {
    const update = () => (small = phone.matches);
    phone.addEventListener("change", update);
    return () => phone.removeEventListener("change", update);
  });
  // Over USB the page's own host isn't one Home Assistant can reach.
  const haHost = location.hostname === "10.42.0.1" ? "pinna.local" : location.hostname;
  const ring = $derived(ringEffect(s, device.link));
  const line = $derived(health(s, device.link, device.offlineSince, device.restarting));
</script>

{#if s}
  <section class="hero">
    <Ring effect={ring} size={small ? 64 : 176} label="Ring: {ring.replace('-', ' ')}" />
    <div class="who">
      <h1 class="pn-type-display-l">{s.name}</h1>
      <div class="state">
        {#if device.link === "live"}<PhaseBadge badge={badge(s)} />{/if}
        <span class="pn-health" data-tone={line.tone}>{line.text}</span>
      </div>
      {#if device.link === "live" && !s.haConnected}
        <p class="next">
          Home Assistant usually finds the satellite on its own under Settings › Devices &amp; services.
          If it doesn't, add the ESPHome integration with host
          <code class="pn-code">{haHost}</code> and port <code class="pn-code">6053</code>.
        </p>
      {/if}
    </div>
  </section>

  <div class="panels">
    <Controls />
    <Timers />
    <Details />
  </div>
{/if}

<style>
  .hero {
    display: flex;
    flex-wrap: wrap;
    align-items: flex-start;
    gap: var(--space-4) var(--space-6);
  }
  @media (min-width: 600px) {
    .hero {
      align-items: center;
    }
  }
  .who {
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
    min-width: 0;
    flex: 1 1 260px;
  }
  h1 {
    margin: 0;
    overflow-wrap: anywhere;
  }
  .state {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--space-2) var(--space-4);
  }
  .next {
    margin: 0;
    max-width: var(--measure);
    color: var(--ink-muted);
  }
  .panels {
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    gap: var(--space-6);
    align-items: start;
  }
  @media (min-width: 900px) {
    .panels {
      grid-template-columns: repeat(2, minmax(0, 1fr));
    }
    .panels > :global(:first-child) {
      grid-row: span 2;
    }
  }
</style>
