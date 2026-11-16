<script lang="ts">
  import { create } from "@bufbuild/protobuf";

  import { HostNamesSchema } from "../gen/pinna/v1/device_pb.ts";
  import { device } from "../lib/device.svelte.ts";
  import External from "../lib/External.svelte";
  import Group from "../lib/Group.svelte";
  import Icon from "../lib/Icon.svelte";
  import { network } from "../lib/network.svelte.ts";
  import SignalBars from "../lib/SignalBars.svelte";
  import WifiPanel from "../lib/WifiPanel.svelte";
  import { band, bitrate, interfaceLabel, signalWord, wifiConnected, wifiIssue } from "../lib/view.ts";

  $effect(() => network.watch());
  const net = $derived(network.current);
  const wireless = $derived(network.wireless);
  const issue = $derived(net ? wifiIssue(wireless) : null);
  // Wi-Fi first; every interface gets its own group.
  const interfaces = $derived(net ? [...net.interfaces].sort((a, b) => Number(b.wireless) - Number(a.wireless)) : []);

  // Names added here (ADR 0007). The one this page is open by can't be
  // removed from here: the page would stop answering.
  const live = $derived(device.link === "live");
  const hosts = $derived(device.status?.settings?.allowedHosts ?? []);
  const here = location.hostname.toLowerCase().replace(/\.$/, "");
  let newHost = $state("");
  let hostError = $state<string | null>(null);

  async function saveHosts(names: string[], done: string) {
    return device.saveSettings({ allowedHosts: create(HostNamesSchema, { names }) }, done);
  }
  const FULL = 16;
  async function addHost() {
    const name = newHost.trim().toLowerCase().replace(/\.$/, "");
    hostError = hosts.includes(name)
      ? `${name} is already on the list.`
      : await saveHosts([...hosts, newHost], `Added ${name}`);
    if (!hostError) newHost = "";
  }
  async function removeHost(name: string) {
    hostError = await saveHosts(hosts.filter((h) => h !== name), `Removed ${name}`);
  }
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

  {#each interfaces as i (i.name)}
    <Group title={interfaceLabel(i.name, i.wireless)}>
      {#snippet aside()}
        <span class="pn-health" data-tone={i.up ? "ok" : "caution"}>{i.up ? "Up" : "Down"}</span>
      {/snippet}
      {#if i.wireless}
        <div class="pn-row">
          {#if i.wifi}<SignalBars dbm={i.wifi.signalDbm} />{/if}
          <div class="pn-row-text">
            <span class="pn-row-label">{i.wifi?.ssid || (wifiConnected(i) ? "Connected" : "Not connected")}</span>
            {#if i.wifi}
              <span class="pn-row-hint">
                {signalWord(i.wifi.signalDbm)} signal ({i.wifi.signalDbm} dBm){i.wifi.frequencyMhz
                  ? ` · ${band(i.wifi.frequencyMhz)}`
                  : ""}{i.wifi.txBitrate ? ` · ${bitrate(i.wifi.txBitrate)}` : ""}
              </span>
            {:else if wifiConnected(i)}
              <span class="pn-row-hint">The satellite can't read the network's name and signal.</span>
            {/if}
          </div>
        </div>
      {/if}
      <div class="pn-row is-stack">
        <span class="pn-row-label">Addresses</span>
        <span class="addrs">
          {#each i.addresses as a (a)}<span>{a}</span>{:else}<span class="none">None</span>{/each}
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

  <WifiPanel />

  <Group title="Name and USB">
    <div class="pn-row">
      <div class="pn-row-text">
        <span class="pn-row-label">Hostname</span>
        <span class="pn-row-hint">Its name on the network; mDNS answers for it with .local.</span>
      </div>
      <span class="pn-row-value is-mono">{net.hostname}</span>
    </div>
    <div class="pn-row">
      <div class="pn-row-text">
        <span class="pn-row-label">Other names</span>
        <span class="pn-row-hint">
          Names to open this page by besides its IP address and .local names, for example one your router gives the
          satellite.
        </span>
      </div>
    </div>
    {#each hosts as h (h)}
      <div class="pn-row">
        <div class="pn-row-text">
          <span class="pn-row-value is-mono">{h}</span>
          {#if h === here}<span class="pn-row-hint">This page is open by this name.</span>{/if}
        </div>
        <button
          class="pn-btn pn-btn-quiet pn-btn-sm"
          disabled={!live || device.busy || h === here}
          onclick={() => removeHost(h)}
          aria-label="Remove {h}"
        >
          Remove
        </button>
      </div>
    {/each}
    <form
      class="pn-row is-stack"
      onsubmit={(e) => {
        e.preventDefault();
        void addHost();
      }}
    >
      <div class="pn-field" data-invalid={hostError ? "" : undefined}>
        <label class="pn-field-label" for="new-host">Add a name</label>
        <div class="inline">
          <input
            id="new-host"
            class="pn-input is-mono"
            autocomplete="off"
            autocapitalize="off"
            spellcheck="false"
            placeholder="e.g. kitchen.home.arpa"
            bind:value={newHost}
            disabled={!live || hosts.length >= FULL}
            aria-describedby={hosts.length >= FULL ? "new-host-hint" : undefined}
            aria-invalid={hostError ? "true" : undefined}
            oninput={() => (hostError = null)}
          />
          <button class="pn-btn" type="submit" disabled={!live || device.busy || !newHost.trim() || hosts.length >= FULL}>
            Add
          </button>
        </div>
        {#if hostError}<span class="pn-field-error" role="alert"><Icon name="alert" />{hostError}</span>{/if}
        {#if hosts.length >= FULL}
          <span id="new-host-hint" class="pn-field-hint">The list is full: remove a name to add another.</span>
        {/if}
      </div>
    </form>
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
    display: flex;
    flex-direction: column;
    margin-top: 2px;
    font-family: var(--font-mono);
    font-size: 14px;
    line-height: 20px;
    font-weight: 500;
    color: var(--ink-muted);
    font-variant-numeric: tabular-nums;
    overflow-wrap: anywhere;
  }
  .none {
    font-family: var(--font-text);
  }
  form {
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
  }
  .inline {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-2) var(--space-3);
  }
  .inline input {
    flex: 1 1 220px;
    width: auto;
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
