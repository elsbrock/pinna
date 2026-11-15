<script lang="ts">
  import { ConnectError } from "@connectrpc/connect";

  import type { GetNetworkResponse } from "../gen/pinna/v1/device_pb.ts";
  import { client, device } from "../lib/device.svelte.ts";
  import External from "../lib/External.svelte";
  import Icon from "../lib/Icon.svelte";
  import { band, interfaceLabel, signalWord, wifiIssue } from "../lib/view.ts";

  let net = $state<GetNetworkResponse | null>(null);
  let error = $state<string | null>(null);

  // Read now, then every 5 s while the page is open and visible.
  $effect(() => {
    let stop = false;
    const read = async () => {
      if (document.visibilityState !== "visible") return;
      try {
        net = await client.getNetwork({}, { timeoutMs: 5000 });
        error = null;
      } catch (e) {
        if (!stop) error = ConnectError.from(e).rawMessage;
      }
    };
    void read();
    const timer = setInterval(read, 5000);
    return () => {
      stop = true;
      clearInterval(timer);
    };
  });

  const wireless = $derived(net?.interfaces.find((i) => i.wireless));
  const issue = $derived(net ? wifiIssue(wireless) : null);
</script>

{#if net}
  <p class="lead">
    Hostname <code class="pn-code">{net.hostname}</code>. Without Wi-Fi, the dashboard also opens over USB: connect
    the satellite to a computer and go to <code class="pn-code">http://10.42.0.1/</code>.
  </p>

  {#if issue}
    <div class="pn-notice" data-tone="caution" role="status">
      <Icon name="alert" class="pn-icon pn-notice-icon" />
      <div class="pn-notice-body">
        <p class="pn-notice-title">{issue.title}</p>
        <p>{issue.body} <External href={issue.href}>Troubleshooting</External></p>
      </div>
    </div>
  {/if}

  <div class="grid">
    {#each net.interfaces as i (i.name)}
      <section class="panel" aria-labelledby="if-{i.name}">
        <div class="head">
          <h2 id="if-{i.name}" class="pn-type-heading">{interfaceLabel(i.name, i.wireless)}</h2>
          <span class="pn-health" data-tone={i.up ? "ok" : "caution"}>{i.up ? "Up" : "Down"}</span>
        </div>
        <dl class="pn-facts">
          {#if i.wifi}
            <dt>Network</dt>
            <dd>{i.wifi.ssid}</dd>
            <dt>Signal</dt>
            <dd>{signalWord(i.wifi.signalDbm)} <span class="muted">({i.wifi.signalDbm} dBm)</span></dd>
            {#if i.wifi.frequencyMhz}
              <dt>Band</dt>
              <dd>{band(i.wifi.frequencyMhz)}</dd>
            {/if}
            {#if i.wifi.txBitrate}
              <dt>Speed</dt>
              <dd class="is-mono">{i.wifi.txBitrate}</dd>
            {/if}
          {:else if i.wireless}
            <dt>Network</dt>
            <dd>{i.addresses.some((a) => !a.includes(":")) ? "Connected (no link details)" : "Not connected"}</dd>
          {/if}
          <dt>Addresses</dt>
          <dd class="is-mono">
            {#each i.addresses as a (a)}<span class="addr">{a}</span>{:else}None{/each}
          </dd>
          <dt>Interface</dt>
          <dd class="is-mono">{i.name}</dd>
          {#if i.mac}
            <dt>MAC</dt>
            <dd class="is-mono">{i.mac.toLowerCase()}</dd>
          {/if}
        </dl>
      </section>
    {/each}
  </div>

  <p class="pn-field-hint">
    Wi-Fi is set in <code class="pn-code">/var/lib/pinna/wifi/wpa_supplicant-wlan0.conf</code> over SSH, so a
    mistake here can't take the satellite off the network.
  </p>
{:else if error && device.link === "live"}
  <p class="pn-field-error" role="alert"><Icon name="alert" />Can't read the network: {error}</p>
{:else}
  <p class="muted" role="status">Reading the network…</p>
{/if}

<style>
  .lead,
  p {
    margin: 0;
    max-width: var(--measure);
  }
  .lead,
  .muted {
    color: var(--ink-muted);
  }
  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(min(100%, 340px), 1fr));
    gap: var(--space-6);
    align-items: start;
  }
  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-3);
  }
  .addr {
    display: block;
  }
</style>
