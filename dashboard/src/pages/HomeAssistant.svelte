<script lang="ts">
  import { copyText } from "../lib/copy.ts";
  import { device } from "../lib/device.svelte.ts";
  import Group from "../lib/Group.svelte";
  import Icon from "../lib/Icon.svelte";
  import { network } from "../lib/network.svelte.ts";
  import { toasts } from "../lib/toast.svelte.ts";
  import HomeAssistantWindow from "../lib/HomeAssistantWindow.svelte";
  import { homeAssistantBlocked } from "../lib/view.ts";
  import { windows } from "../lib/windows.svelte.ts";

  const s = $derived(device.status);
  const live = $derived(device.link === "live");
  const blocked = $derived(live && homeAssistantBlocked(windows.security, s ?? undefined));

  $effect(() => network.watch());
  // The satellite's own address on the network. The page's host may be one Home
  // Assistant can't reach: USB (10.42.0.1), a port forward, a proxy.
  const host = $derived(
    network.ipv4 ?? (location.hostname === "10.42.0.1" && s ? `${s.name}.local` : location.hostname),
  );
  // The ESPHome API port; the image doesn't change it.
  const PORT = "6053";

  async function copy(text: string) {
    await copyText(text);
    toasts.show(`Copied ${text}`);
  }
</script>

{#if s}
  <Group title="Connection">
    <div class="pn-row">
      <div class="pn-row-text">
        <span class="pn-row-label">Home Assistant</span>
        {#if blocked}
          <span class="pn-row-hint">
            {windows.homeAssistantLeft > 0
              ? "The window is open: Home Assistant can add this satellite now."
              : "This satellite refuses Home Assistant until the window below is open."}
          </span>
        {/if}
      </div>
      <span class="pn-row-value">
        {#if !live}
          <span class="pn-health" data-tone="danger">Not reachable</span>
        {:else if s.haConnected}
          <span class="pn-health" data-tone="ok">Connected</span>
        {:else}
          <span class="pn-health" data-tone="caution">Not connected</span>
        {/if}
      </span>
    </div>
    <div class="pn-row">
      <div class="pn-row-text">
        <span class="pn-row-label">Voice pipeline</span>
        <span class="pn-row-hint">Home Assistant runs speech-to-text, the conversation and the reply.</span>
      </div>
      <span class="pn-row-value">
        {#if !live}Not reachable{:else if !s.haConnected}Needs Home Assistant{:else if s.voiceSubscribed}Ready{:else}
          <span class="pn-health" data-tone="caution">Not set up</span>
        {/if}
      </span>
    </div>
    {#if live && s.haConnected && !s.voiceSubscribed}
      <div class="pn-row">
        <span class="pn-row-hint">
          Choose an assistant for this satellite on its device page in Home Assistant.
        </span>
      </div>
    {/if}
    <div class="pn-row">
      <div class="pn-row-text">
        <span class="pn-row-label">Encryption</span>
        <span class="pn-row-hint">
          {#if s.apiEncrypted && s.apiKeyFromImage}
            The image carries this satellite's key. A factory reset keeps it.
          {:else if s.apiEncrypted}
            Home Assistant set the key. Only a factory reset removes it.
          {:else if s.haConnected}
            Home Assistant connected without turning it on, so it may be older than the versions that do. Anyone on your
            network can read what the satellite and Home Assistant send each other.
          {:else}
            Home Assistant turns it on when it adds the satellite. Until then, anyone on your network can read what the
            satellite sends.
          {/if}
        </span>
      </div>
      <span class="pn-row-value">{s.apiEncrypted ? "On" : "Off"}</span>
    </div>
  </Group>

  <HomeAssistantWindow />

  <Group title="Add it to Home Assistant">
    {#snippet foot()}
      Home Assistant usually finds the satellite on its own under Settings › Devices &amp; services. If it doesn't, add
      the ESPHome integration with this host and port.
    {/snippet}
    <a class="pn-row" href="#/system">
      <span class="pn-row-text">
        <span class="pn-row-label">Name</span>
        <span class="pn-row-hint">
          {s.settings?.defaultName
            ? "Name it before you add it: Home Assistant's entity IDs keep the first name."
            : "The name Home Assistant shows"}
        </span>
      </span>
      <span class="pn-row-value">{s.settings?.friendlyName ?? s.name}</span>
      <Icon name="chevron-right" class="pn-icon pn-row-chev" />
    </a>
    <div class="pn-row">
      <div class="pn-row-text"><span class="pn-row-label">Host</span></div>
      <span class="pn-row-value is-mono">{host}</span>
      <button class="pn-btn pn-btn-quiet pn-btn-sm" onclick={() => copy(host)} aria-label="Copy the host">
        <Icon name="copy" />Copy
      </button>
    </div>
    <div class="pn-row">
      <div class="pn-row-text"><span class="pn-row-label">Port</span></div>
      <span class="pn-row-value is-mono">{PORT}</span>
      <button class="pn-btn pn-btn-quiet pn-btn-sm" onclick={() => copy(PORT)} aria-label="Copy the port">
        <Icon name="copy" />Copy
      </button>
    </div>
  </Group>
{/if}
