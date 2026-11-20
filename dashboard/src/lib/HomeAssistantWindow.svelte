<!-- The window in which Home Assistant may add a satellite that has a
     dashboard password and no key yet (ADR 0029), in either voice mode. -->
<script lang="ts">
  import { device } from "./device.svelte.ts";
  import Group from "./Group.svelte";
  import Icon from "./Icon.svelte";
  import { windowLeft } from "./view.ts";
  import { windows } from "./windows.svelte.ts";

  const s = $derived(device.status);
  const live = $derived(device.link === "live");

  $effect(() => windows.watch());
  // With a password and no key, the ESPHome API refuses Home Assistant until the
  // owner opens a window. A key makes the window pointless.
  const closed = $derived(live && windows.passwordSet && !s?.apiEncrypted);
  const left = $derived(windows.homeAssistantLeft);
</script>

{#if closed}
  <Group title="Let Home Assistant add this satellite">
    {#snippet foot()}
      Once Home Assistant has added the satellite, only its key works, and the window no longer matters.
    {/snippet}
    <div class="pn-row">
      <div class="pn-row-text">
        <span class="pn-row-label">{left ? "Home Assistant can add this satellite" : "Home Assistant can't connect"}</span>
        <span class="pn-row-hint" role="status">
          {#if left}
            Add the satellite in Home Assistant now. {windowLeft(left)}.
          {:else}
            This satellite has a dashboard password and Home Assistant hasn't set a key yet, so it refuses Home Assistant's
            connections. Choose the button, then add the satellite in Home Assistant within 15 minutes.
          {/if}
        </span>
        {#if windows.error}
          <span class="pn-field-error" role="alert"><Icon name="alert" />{windows.error}</span>
        {/if}
      </div>
      <button class="pn-btn" disabled={windows.opening} onclick={() => windows.openHomeAssistant()}>
        Let Home Assistant add this satellite
      </button>
    </div>
  </Group>
{/if}
