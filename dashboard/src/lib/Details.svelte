<script lang="ts">
  import { device } from "./device.svelte.ts";
  import { uptime } from "./view.ts";

  const s = $derived(device.status);
  // Uptime only arrives with a change; count on between messages.
  const up = $derived(
    s ? Number(s.uptimeSeconds) + Math.floor((device.now - device.receivedAt) / 1000) : 0,
  );
</script>

<section class="panel" aria-labelledby="details-title">
  <h2 id="details-title" class="pn-type-heading">Details</h2>
  {#if s}
    <dl class="pn-facts">
      <dt>Version</dt>
      <dd class="is-mono">{s.version}</dd>
      <dt>Node name</dt>
      <dd class="is-mono">{s.name}</dd>
      <dt>MAC</dt>
      <dd class="is-mono">{s.mac.toLowerCase()}</dd>
      <dt>Uptime</dt>
      <dd class="is-mono">{device.link === "live" ? uptime(up) : "—"}</dd>
      <dt>Audio backend</dt>
      <dd class="is-mono">{s.audioBackend}</dd>
      <dt>LED backend</dt>
      <dd class="is-mono">{s.ledBackend}</dd>
    </dl>
  {/if}
</section>
