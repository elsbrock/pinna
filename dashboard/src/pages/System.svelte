<script lang="ts">
  import Confirm from "../lib/Confirm.svelte";
  import { device } from "../lib/device.svelte.ts";
  import Group from "../lib/Group.svelte";
  import Icon from "../lib/Icon.svelte";
  import { toasts } from "../lib/toast.svelte.ts";
  import UpdatePanel from "../lib/UpdatePanel.svelte";
  import { uptime } from "../lib/view.ts";

  const s = $derived(device.status);
  const live = $derived(device.link === "live");
  const up = $derived(s ? Number(s.uptimeSeconds) + Math.floor((device.now - device.receivedAt) / 1000) : 0);
  let confirming = $state(false);

  async function restart() {
    const failed = await device.restart();
    if (failed) toasts.show(failed, "danger");
  }
</script>

<UpdatePanel />

{#if s}
  <Group title="Power">
    <div class="pn-row">
      <div class="pn-row-text">
        <span class="pn-row-label">Restart</span>
        <span class="pn-row-hint">The satellite stops listening for a minute or two.</span>
      </div>
      <button
        class="pn-btn"
        disabled={!live || device.busy || device.restarting}
        onclick={() => (confirming = true)}
      >
        <Icon name="restart" />Restart…
      </button>
    </div>
  </Group>

  <Group title="Troubleshooting">
    {#if s.lastBoot}
      <div class="pn-row">
        <div class="pn-row-text">
          <span class="pn-row-label">Last start</span>
          <span class="pn-row-hint">{s.lastBoot.summary}</span>
        </div>
      </div>
    {/if}
    <a class="pn-row" href="#/system/logs">
      <span class="pn-row-icon"><Icon name="logs" /></span>
      <span class="pn-row-text">
        <span class="pn-row-label">Logs</span>
        <span class="pn-row-hint">What the satellite is doing, as it happens</span>
      </span>
      <Icon name="chevron-right" class="pn-icon pn-row-chev" />
    </a>
  </Group>

  <Group title="Details">
    {#each [["Node name", s.name], ["MAC", s.mac.toLowerCase()], ["Uptime", live ? uptime(up) : "—"], ["Audio backend", s.audioBackend], ["LED backend", s.ledBackend]] as [label, value] (label)}
      <div class="pn-row">
        <div class="pn-row-text"><span class="pn-row-label">{label}</span></div>
        <span class="pn-row-value is-mono">{value}</span>
      </div>
    {/each}
  </Group>

  <Confirm bind:open={confirming} title="Restart {s.name}?" action="Restart" confirm={restart}>
    <p>It stops listening and goes dark for a minute or two. This page reconnects on its own.</p>
  </Confirm>
{/if}
