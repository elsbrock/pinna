<!-- Add one of the owner's MCP servers, or change one: its name and its
     address. The address may carry a token, so it is write-only: once
     stored, only its host shows. -->
<script lang="ts">
  import { untrack } from "svelte";

  import type { McpServer } from "../../gen/pinna/v1/assistant_pb.ts";
  import { assistant } from "../../lib/assistant.svelte.ts";
  import type { SecretEdit } from "../../lib/assistant.ts";
  import Confirm from "../../lib/Confirm.svelte";
  import { device } from "../../lib/device.svelte.ts";
  import Icon from "../../lib/Icon.svelte";
  import SecretField from "../../lib/SecretField.svelte";

  let { server, onclose }: { server?: McpServer; onclose: () => void } = $props();

  const live = $derived(device.link === "live");
  const id = $derived(server ? `mcp-${server.id}` : "mcp-new");

  // The form starts from the server as it was when it opened.
  const start = untrack(() => server);
  let name = $state(start?.name ?? "");
  let url = $state<SecretEdit>({ kind: "keep" });
  let error = $state<string | null>(null);
  let removing = $state(false);
  const ready = $derived(!!name.trim() && (!!server || url.kind === "set"));

  async function save() {
    error = await assistant.setMcpServer(
      {
        id: server?.id ?? "",
        revision: server?.revision ?? 0n,
        name: name.trim(),
        url: url.kind === "set" ? url.value.trim() : undefined,
        enabled: server?.enabled ?? true,
      },
      server ? "Saved" : "MCP server added",
    );
    if (!error) onclose();
  }

  async function remove() {
    if (!server) return;
    error = await assistant.deleteMcpServer(server.id, server.revision);
    if (!error) onclose();
  }
</script>

<form
  class="pn-row is-stack"
  aria-label={server ? `Change ${server.name}` : "Add an MCP server"}
  onsubmit={(e) => {
    e.preventDefault();
    if (ready && !assistant.busy) void save();
  }}
>
  <div class="form">
    <div class="pn-field">
      <label class="pn-field-label" for="{id}-name">Name</label>
      <input
        id="{id}-name"
        class="pn-input is-mono"
        autocomplete="off"
        autocapitalize="off"
        spellcheck="false"
        maxlength="24"
        placeholder="home"
        bind:value={name}
        disabled={!live}
        aria-describedby="{id}-name-hint"
        oninput={() => (error = null)}
      />
      <span id="{id}-name-hint" class="pn-field-hint">
        Lowercase letters, digits and hyphens, up to 24. The brain sees each of its tools under this name.
      </span>
    </div>
    <SecretField
      id="{id}-url"
      label="Address"
      stored={!!server}
      shown="set, on {server?.host ?? ''}"
      bind:edit={url}
      removable={false}
      placeholder="https://example.com/mcp"
      hint="The whole address, with any token in it. The satellite keeps it and shows only its host."
      disabled={!live}
    />
    {#if error}<span class="pn-field-error" role="alert"><Icon name="alert" />{error}</span>{/if}
    <div class="actions">
      <button class="pn-btn pn-btn-primary" type="submit" disabled={!live || assistant.busy || !ready}>
        {server ? "Save" : "Add"}
      </button>
      <button class="pn-btn" type="button" onclick={onclose}>Cancel</button>
      {#if server}
        <button
          class="pn-btn pn-btn-quiet remove"
          type="button"
          disabled={!live || assistant.busy}
          onclick={() => (removing = true)}>Remove server…</button
        >
      {/if}
    </div>
  </div>
</form>

{#if server}
  <Confirm bind:open={removing} title="Remove {server.name}?" action="Remove" danger confirm={remove}>
    <p>The brain loses its tools, and the satellite forgets its address.</p>
  </Confirm>
{/if}

<style>
  .form {
    display: flex;
    flex-direction: column;
    gap: var(--space-4);
  }
  .actions {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-2) var(--space-3);
  }
  .remove {
    margin-left: auto;
    color: var(--danger);
  }
</style>
