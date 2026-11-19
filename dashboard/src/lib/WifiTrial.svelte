<!-- A Wi-Fi network on trial (ADR 0008), on every page: the satellite keeps it
     once the dashboard is opened at its new address, or on Keep here; it goes
     back when the time runs out, or on Go back. -->
<script lang="ts">
  import { device } from "./device.svelte.ts";
  import Icon from "./Icon.svelte";
  import { mmss } from "./view.ts";
  import { wifi } from "./wifi.svelte.ts";

  const t = $derived(wifi.trial);
  const left = $derived(wifi.keepLeft(device.now));
  const overUsb = location.hostname === "10.42.0.1";
</script>

{#if t}
  <div class="pn-notice wifi-trial" data-tone="caution" role="status">
    <Icon name="wifi" class="pn-icon pn-notice-icon" />
    <div class="pn-notice-body">
      <p class="pn-notice-title">Keep {t.ssid}?</p>
      <p>
        The satellite is on {t.ssid} at <code class="pn-code">{t.address}</code>.
        {#if overUsb}
          To check that it is reachable there, open <code class="pn-code">http://{t.address}/</code> from a phone on
          {t.ssid}: that keeps it too.
        {:else}
          Opening <code class="pn-code">http://{t.address}/</code> on {t.ssid} keeps it too.
        {/if}
        {#if t.fallback}
          Otherwise it goes back to {t.fallback} in {mmss(left)}.
        {:else if wifi.hasHotspot}
          Otherwise it leaves {t.ssid} in {mmss(left)}, and runs its setup hotspot again.
        {:else}
          Otherwise it leaves {t.ssid} in {mmss(left)}, and is reachable over USB only.
        {/if}
      </p>
      <div class="actions">
        <button class="pn-btn pn-btn-primary" disabled={wifi.busy} onclick={() => wifi.keep(true)}>Keep it</button>
        <button class="pn-btn pn-btn-quiet" disabled={wifi.busy} onclick={() => wifi.keep(false)}>Go back</button>
      </div>
    </div>
  </div>
{/if}

<style>
  .wifi-trial .pn-notice-body {
    min-width: 0;
    overflow-wrap: anywhere;
  }
  .actions {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-2) var(--space-3);
    margin-top: var(--space-3);
  }
</style>
