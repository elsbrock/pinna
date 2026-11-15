<script lang="ts">
  import Controls from "./lib/Controls.svelte";
  import Details from "./lib/Details.svelte";
  import { device } from "./lib/device.svelte.ts";
  import Logo from "./lib/Logo.svelte";
  import PhaseBadge from "./lib/PhaseBadge.svelte";
  import Ring from "./lib/Ring.svelte";
  import Timers from "./lib/Timers.svelte";
  import { badge, health, ringEffect } from "./lib/view.ts";

  const s = $derived(device.status);
  const ring = $derived(ringEffect(s, device.link));
  const line = $derived(health(s, device.link, device.offlineSince));

  $effect(() => {
    document.title = s ? `${s.name} · Pinna` : "Pinna";
  });
</script>

<header class="bar">
  <Logo />
  {#if s}
    <span class="served">Served by <span class="pn-type-data">{s.name}</span></span>
  {/if}
</header>

<main>
  {#if !s}
    <p class="connecting" role="status">
      {device.link === "offline" ? "Can't reach the satellite. Trying again…" : "Connecting…"}
    </p>
  {:else}
    <section class="hero">
      <Ring effect={ring} size={176} label="Ring: {ring.replace('-', ' ')}" />
      <div class="who">
        <h1 class="pn-type-display-l">{s.name}</h1>
        <div class="state">
          {#if device.link === "live"}<PhaseBadge badge={badge(s)} />{/if}
          <span class="pn-health" data-tone={line.tone} role="status">{line.text}</span>
        </div>
        {#if device.link === "live" && !s.haConnected}
          <p class="next">
            Home Assistant usually finds the satellite on its own under Settings › Devices &amp; services.
            If it doesn't, add the ESPHome integration with host
            <code class="pn-code">{location.hostname}</code> and port <code class="pn-code">6053</code>.
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
</main>

<style>
  .bar,
  main {
    max-width: var(--page-max);
    margin: 0 auto;
    padding: 0 var(--space-4);
  }
  .bar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-4);
    padding-top: var(--space-4);
    padding-bottom: var(--space-4);
  }
  .served {
    font-size: 13px;
    color: var(--ink-muted);
  }
  main {
    display: flex;
    flex-direction: column;
    gap: var(--space-8);
    padding-bottom: var(--space-8);
  }
  .connecting {
    margin: var(--space-8) 0;
    color: var(--ink-muted);
  }
  .hero {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--space-6);
    padding-top: var(--space-4);
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
    .bar,
    main {
      padding-left: var(--space-8);
      padding-right: var(--space-8);
    }
    .panels {
      grid-template-columns: repeat(2, minmax(0, 1fr));
    }
    .panels > :global(:first-child) {
      grid-row: span 2;
    }
  }
</style>
