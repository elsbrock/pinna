<script lang="ts">
  import { drawingFor } from "../../../design/devices/index.ts";
  import type { GetUpdatesResponse } from "../gen/pinna/v1/device_pb.ts";
  import Controls from "../lib/Controls.svelte";
  import Device from "../lib/Device.svelte";
  import { client, device } from "../lib/device.svelte.ts";
  import Icon from "../lib/Icon.svelte";
  import { network } from "../lib/network.svelte.ts";
  import PhaseBadge from "../lib/PhaseBadge.svelte";
  import SignalBars from "../lib/SignalBars.svelte";
  import StatTile from "../lib/StatTile.svelte";
  import Timers from "../lib/Timers.svelte";
  import { badge, band, health, ringEffect, signalWord, since, slotWord, uptime } from "../lib/view.ts";

  const s = $derived(device.status);
  const live = $derived(device.link === "live");
  const ring = $derived(ringEffect(s, device.link));
  const line = $derived(health(s, device.link, device.offlineSince, device.restarting));
  const modelName = $derived(drawingFor(s?.model ?? "")?.name);
  // Uptime only arrives with a change; count on between messages.
  const up = $derived(s ? Number(s.uptimeSeconds) + Math.floor((device.now - device.receivedAt) / 1000) : 0);

  // Phones get a smaller drawing, so Volume and Microphones stay near the top.
  const phone = matchMedia("(max-width: 759px)");
  let small = $state(phone.matches);
  $effect(() => {
    const update = () => (small = phone.matches);
    phone.addEventListener("change", update);
    return () => phone.removeEventListener("change", update);
  });

  $effect(() => network.watch());
  const wifi = $derived(network.wireless);
  const address = $derived(network.ipv4 ?? location.hostname);

  // The running slot, for the Software tile; read once.
  let updates = $state<GetUpdatesResponse | null>(null);
  $effect(() => {
    client.getUpdates({}, { timeoutMs: 10_000 }).then(
      (u) => (updates = u),
      () => {},
    );
  });
  const booted = $derived(updates?.slots.find((x) => x.booted));
</script>

{#if s}
  <section class="hero">
    <Device model={s.model} effect={ring} width={small ? 200 : 250} label="Ring: {ring.replace('-', ' ')}" halo />
    <div class="who">
      {#if modelName}<span class="pn-eyebrow">{modelName}</span>{/if}
      <h1 class="pn-type-display-l">{s.name}</h1>
      <div class="state">
        {#if live}<PhaseBadge badge={badge(s)} />{/if}
        <span class="pn-health" data-tone={line.tone}>{line.text}</span>
      </div>
      <span class="meta">{address} · Pinna {s.version}</span>
      <div>
        <button
          class="pn-btn pn-btn-sm"
          disabled={!live}
          title="Run the ring's test pattern, to find this satellite"
          onclick={() => device.identify()}
        >
          <Icon name="sun" />Identify
        </button>
      </div>
    </div>
  </section>

  {#if live && !s.haConnected}
    <div class="pn-notice" data-tone="caution" role="status">
      <Icon name="info" class="pn-icon pn-notice-icon" />
      <div class="pn-notice-body">
        <p class="pn-notice-title">Not connected to Home Assistant yet</p>
        <p>
          Home Assistant usually finds the satellite on its own under Settings › Devices &amp; services.
          <a href="#/home-assistant">How to add it by hand</a>
        </p>
      </div>
    </div>
  {/if}

  <div class="stats">
    <StatTile
      href="#/home-assistant"
      icon="house"
      label="Home Assistant"
      value={!live ? "—" : s.haConnected ? "Connected" : "Not connected"}
    >
      {#if !live}
        Not reachable
      {:else if !s.haConnected}
        <span class="pn-health" data-tone="caution">Waiting for it</span>
      {:else if s.voiceSubscribed}
        <span class="pn-health" data-tone="ok">Voice pipeline ready</span>
      {:else}
        <span class="pn-health" data-tone="caution">No voice pipeline</span>
      {/if}
    </StatTile>
    <StatTile href="#/network" icon="wifi" label="Wi-Fi" value={wifi?.wifi?.ssid ?? (wifi ? "Not connected" : "—")}>
      {#if wifi?.wifi}
        <SignalBars dbm={wifi.wifi.signalDbm} />{signalWord(wifi.wifi.signalDbm)}{wifi.wifi.frequencyMhz
          ? ` · ${band(wifi.wifi.frequencyMhz)}`
          : ""}
      {:else if network.current && !wifi}
        No Wi-Fi interface
      {:else}
        &nbsp;
      {/if}
    </StatTile>
    <StatTile href="#/system" icon="download" label="Software" value={s.version} mono>
      {#if booted}
        Slot {booted.name.toUpperCase()} · {slotWord(booted).toLowerCase()}
      {:else if updates && !updates.available}
        Updates not available here
      {:else}
        &nbsp;
      {/if}
    </StatTile>
    <StatTile href="#/system" icon="clock" label="Uptime" value={live ? uptime(up) : "—"} mono>
      {#if live}Since {since(new Date(device.now), up)}{:else}Not reachable{/if}
    </StatTile>
  </div>

  <div class="cols">
    <Controls />
    <Timers />
  </div>
{/if}

<style>
  .hero {
    position: relative;
    overflow: hidden;
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    justify-items: center;
    gap: var(--space-3);
    padding: 20px var(--space-4) 22px;
    text-align: center;
    background: var(--surface-raised);
    border: 1px solid var(--line);
    border-radius: var(--radius-md);
    box-shadow: var(--shadow-raised);
  }
  .who {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 10px;
    min-width: 0;
  }
  h1 {
    margin: 0;
    overflow-wrap: anywhere;
  }
  .state {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: center;
    gap: var(--space-2) var(--space-4);
  }
  .meta {
    font-family: var(--font-mono);
    font-size: 13px;
    line-height: 18px;
    font-weight: 500;
    color: var(--ink-muted);
    font-variant-numeric: tabular-nums;
    overflow-wrap: anywhere;
  }
  .stats {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: var(--space-3);
  }
  .cols {
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    gap: var(--space-6);
    align-items: start;
  }
  @media (min-width: 760px) {
    .hero {
      grid-template-columns: auto minmax(0, 1fr);
      justify-items: start;
      align-items: center;
      gap: 36px;
      padding: 28px 36px;
      text-align: left;
    }
    .who {
      align-items: flex-start;
    }
    .state {
      justify-content: flex-start;
    }
  }
  @media (min-width: 1000px) {
    .stats {
      grid-template-columns: repeat(4, minmax(0, 1fr));
      gap: var(--space-4);
    }
  }
  @media (min-width: 1240px) {
    .cols {
      grid-template-columns: repeat(2, minmax(0, 1fr));
    }
  }
</style>
