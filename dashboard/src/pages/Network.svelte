<script lang="ts">
  import { device } from "../lib/device.svelte.ts";
  import External from "../lib/External.svelte";
  import Group from "../lib/Group.svelte";
  import Icon from "../lib/Icon.svelte";
  import { network } from "../lib/network.svelte.ts";
  import SignalBars from "../lib/SignalBars.svelte";
  import { band, interfaceLabel, signalWord, wifiIssue } from "../lib/view.ts";

  $effect(() => network.watch());
  const net = $derived(network.current);
  const wireless = $derived(network.wireless);
  const issue = $derived(net ? wifiIssue(wireless) : null);
</script>

{#if net}
  {#if issue}
    <div class="pn-notice" data-tone="caution" role="status">
      <Icon name="alert" class="pn-icon pn-notice-icon" />
      <div class="pn-notice-body">
        <p class="pn-notice-title">{issue.title}</p>
        <p>{issue.body} <External href={issue.href}>Troubleshooting</External></p>
      </div>
    </div>
  {/if}

  {#if wireless}
    <Group title="Wi-Fi">
      {#snippet foot()}
        For now the Wi-Fi network is set in <code class="pn-code">/var/lib/pinna/wifi/wpa_supplicant-wlan0.conf</code>
        over SSH. Choosing a network here is planned.
      {/snippet}
      <div class="pn-row">
        {#if wireless.wifi}<SignalBars dbm={wireless.wifi.signalDbm} />{/if}
        <div class="pn-row-text">
          <span class="pn-row-label">{wireless.wifi?.ssid || "Not connected"}</span>
          {#if wireless.wifi}
            <span class="pn-row-hint">
              {signalWord(wireless.wifi.signalDbm)} signal ({wireless.wifi.signalDbm} dBm){wireless.wifi.frequencyMhz
                ? ` · ${band(wireless.wifi.frequencyMhz)}`
                : ""}{wireless.wifi.txBitrate ? ` · ${wireless.wifi.txBitrate}` : ""}
            </span>
          {/if}
        </div>
        {#if wireless.wifi && wireless.up}
          <span class="pn-row-value"><Icon name="check" />Connected</span>
        {/if}
      </div>
    </Group>
  {/if}

  {#each net.interfaces as i (i.name)}
    <Group title={interfaceLabel(i.name, i.wireless)}>
      {#snippet aside()}
        <span class="pn-health" data-tone={i.up ? "ok" : "caution"}>{i.up ? "Up" : "Down"}</span>
      {/snippet}
      <div class="pn-row">
        <div class="pn-row-text"><span class="pn-row-label">Addresses</span></div>
        <span class="pn-row-value is-mono addrs">
          {#each i.addresses as a (a)}<span>{a}</span>{:else}None{/each}
        </span>
      </div>
      <div class="pn-row">
        <div class="pn-row-text"><span class="pn-row-label">Interface</span></div>
        <span class="pn-row-value is-mono">{i.name}</span>
      </div>
      {#if i.mac}
        <div class="pn-row">
          <div class="pn-row-text"><span class="pn-row-label">MAC</span></div>
          <span class="pn-row-value is-mono">{i.mac.toLowerCase()}</span>
        </div>
      {/if}
    </Group>
  {/each}

  <Group title="Name and USB">
    <div class="pn-row">
      <div class="pn-row-text">
        <span class="pn-row-label">Hostname</span>
        <span class="pn-row-hint">Its name on the network; mDNS answers for it with .local.</span>
      </div>
      <span class="pn-row-value is-mono">{net.hostname}</span>
    </div>
    <div class="pn-row">
      <span class="pn-row-icon"><Icon name="usb" /></span>
      <div class="pn-row-text">
        <span class="pn-row-label">Reachable over USB</span>
        <span class="pn-row-hint">
          Without Wi-Fi, connect the satellite to a computer and open <code class="pn-code">http://10.42.0.1/</code>.
        </span>
      </div>
    </div>
  </Group>
{:else if network.error && device.link === "live"}
  <p class="pn-field-error" role="alert"><Icon name="alert" />Can't read the network: {network.error}</p>
{:else}
  <p class="sr" role="status">Reading the network…</p>
  {#each [0, 1] as k (k)}<span class="pn-skeleton" style:height="140px" aria-hidden="true"></span>{/each}
{/if}

<style>
  .addrs {
    flex-direction: column;
    align-items: flex-end;
    gap: 0;
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
