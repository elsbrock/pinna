<script lang="ts">
  import { copyText } from "../lib/copy.ts";
  import { device } from "../lib/device.svelte.ts";
  import Group from "../lib/Group.svelte";
  import Icon from "../lib/Icon.svelte";
  import { network } from "../lib/network.svelte.ts";
  import { toasts } from "../lib/toast.svelte.ts";

  const s = $derived(device.status);
  const live = $derived(device.link === "live");

  $effect(() => network.watch());
  // Over USB the page's own host isn't one Home Assistant can reach: offer the Wi-Fi address.
  const host = $derived(location.hostname === "10.42.0.1" ? (network.ipv4 ?? "pinna.local") : location.hostname);
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
      <div class="pn-row-text"><span class="pn-row-label">Home Assistant</span></div>
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
        {#if !live || !s.haConnected}—{:else if s.voiceSubscribed}Ready{:else}
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
  </Group>

  <Group title="Add it to Home Assistant">
    {#snippet foot()}
      Home Assistant usually finds the satellite on its own under Settings › Devices &amp; services. If it doesn't, add
      the ESPHome integration with this host and port.
    {/snippet}
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
