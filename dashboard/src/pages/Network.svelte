<script lang="ts">
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

  const current = $derived(device.status?.name || net?.hostname || "");

  // The dashboard address (ADR 0021). The page can't change or remove it while
  // it is open by that address's host: it would stop answering.
  const live = $derived(device.link === "live");
  const url = $derived(device.status?.settings?.url ?? "");
  const hereHost = location.hostname.toLowerCase().replace(/\.$/, "");
  const hostOf = (u: string) => {
    try {
      return new URL(u).hostname.toLowerCase();
    } catch {
      return "";
    }
  };
  let urlDraft = $state<string | null>(null);
  const urlValue = $derived(urlDraft ?? url);
  let urlError = $state<string | null>(null);
  // Hosts that answer anyway: IP addresses, .local names, localhost, the hostname.
  const answersAnyway = $derived(
    hereHost === "localhost" ||
      hereHost === current ||
      hereHost.endsWith(".local") ||
      hereHost.startsWith("[") ||
      /^\d+\.\d+\.\d+\.\d+$/.test(hereHost),
  );
  const openByAddress = $derived(!!url && hostOf(url) === hereHost && !answersAnyway);
  const saveBlocked = $derived(openByAddress && hostOf(urlValue.trim()) !== hereHost);
  async function saveUrl(next: string, done: string) {
    urlError = await device.saveSettings({ url: next }, done);
    if (!urlError) urlDraft = null;
  }
  // The hostname, also the node name (ADR 0010). An edit stays a draft until saved.
  let draft = $state<string | null>(null);
  const nameValue = $derived(draft ?? current);
  let nameError = $state<string | null>(null);
  async function saveName() {
    const old = current;
    const next = nameValue.trim().toLowerCase().replace(/\.$/, "");
    nameError = await device.saveSettings({ hostname: nameValue }, `Renamed to ${next}`);
    if (nameError) return;
    draft = null;
    // Opened by the old .local name: it stops answering, so follow the new one
    // once mDNS has probed it.
    if (location.hostname.toLowerCase().replace(/\.$/, "") === `${old}.local`.toLowerCase()) {
      const { protocol, port, hash } = location;
      setTimeout(() => location.replace(`${protocol}//${next}.local${port ? `:${port}` : ""}/${hash}`), 1500);
    }
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
    <form
      class="pn-row is-stack"
      onsubmit={(e) => {
        e.preventDefault();
        void saveName();
      }}
    >
      <div class="pn-field" data-invalid={nameError ? "" : undefined}>
        <label class="pn-field-label" for="hostname">Hostname</label>
        <div class="inline">
          <input
            id="hostname"
            class="pn-input is-mono"
            autocomplete="off"
            autocapitalize="off"
            spellcheck="false"
            bind:value={() => nameValue, (v) => (draft = v)}
            disabled={!live}
            aria-describedby="hostname-hint"
            aria-invalid={nameError ? "true" : undefined}
            oninput={() => (nameError = null)}
          />
          <button
            class="pn-btn"
            type="submit"
            disabled={!live || device.busy || !nameValue.trim() || nameValue.trim() === current}
          >
            Save
          </button>
        </div>
        {#if nameError}<span class="pn-field-error" role="alert"><Icon name="alert" />{nameError}</span>{/if}
        <span id="hostname-hint" class="pn-field-hint">
          Its name on the network and in ESPHome: http://{current}.local/
        </span>
      </div>
    </form>
    <form
      class="pn-row is-stack"
      onsubmit={(e) => {
        e.preventDefault();
        void saveUrl(urlValue, urlValue.trim() ? "Saved the dashboard address" : "Removed the dashboard address");
      }}
    >
      <div class="pn-field" data-invalid={urlError ? "" : undefined}>
        <label class="pn-field-label" for="dashboard-url">Dashboard address</label>
        <div class="inline">
          <input
            id="dashboard-url"
            class="pn-input is-mono"
            inputmode="url"
            autocomplete="off"
            autocapitalize="off"
            spellcheck="false"
            placeholder="https://dot.example.com/"
            bind:value={() => urlValue, (v) => (urlDraft = v)}
            disabled={!live}
            aria-describedby="dashboard-url-hint"
            aria-invalid={urlError ? "true" : undefined}
            oninput={() => (urlError = null)}
          />
          <button
            class="pn-btn"
            type="submit"
            disabled={!live || device.busy || !urlValue.trim() || urlValue.trim() === url || saveBlocked}
          >
            Save
          </button>
          {#if url}
            <button
              class="pn-btn"
              type="button"
              disabled={!live || device.busy || openByAddress}
              onclick={() => saveUrl("", "Removed the dashboard address")}
            >
              Remove
            </button>
          {/if}
        </div>
        {#if urlError}<span class="pn-field-error" role="alert"><Icon name="alert" />{urlError}</span>{/if}
        <span id="dashboard-url-hint" class="pn-field-hint">
          Where else this dashboard opens, for example behind a reverse proxy. Other satellites link here. Without
          one, they link to {current}.local or its IP address.
          {#if openByAddress}
            This page is open by this address, so it can't be removed or changed to another host from here: the page
            would stop answering. Open it by its IP address or .local name first.
          {/if}
        </span>
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
