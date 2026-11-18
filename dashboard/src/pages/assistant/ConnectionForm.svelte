<!-- Add a connection from a preset, or change one: its address and its key.
     The key is write-only (SecretField). A change re-checks the slots that use
     the connection; in standalone mode each must pass, or nothing is saved. -->
<script lang="ts">
  import { untrack } from "svelte";

  import type { ProviderConnection } from "../../gen/pinna/v1/assistant_pb.ts";
  import { assistant } from "../../lib/assistant.svelte.ts";
  import {
    addressKind,
    connectionAddress,
    hostOf,
    isStandalone,
    keyFields,
    presetName,
    type SecretEdit,
    SLOTS,
    servesSlot,
    slotWords,
    usedBy,
  } from "../../lib/assistant.ts";
  import Confirm from "../../lib/Confirm.svelte";
  import { device } from "../../lib/device.svelte.ts";
  import Icon from "../../lib/Icon.svelte";
  import SecretField from "../../lib/SecretField.svelte";

  let { connection, onclose }: { connection?: ProviderConnection; onclose: () => void } = $props();

  const view = $derived(assistant.view);
  const standalone = $derived(isStandalone(device.status));
  const live = $derived(device.link === "live");
  const id = $derived(connection ? `conn-${connection.id}` : "conn-new");

  // The form starts from the connection as it was when it opened.
  const start = untrack(() => connection);
  let presetId = $state(start?.preset ?? "");
  const preset = $derived(view?.presets.find((p) => p.id === presetId));
  const kind = $derived(addressKind(preset?.baseUrl ?? ""));
  // A changed connection keeps its address in the field; a hosted service's is fixed.
  const startKind = untrack(() => addressKind(view?.presets.find((p) => p.id === start?.preset)?.baseUrl ?? ""));
  let address = $state(start && startKind !== "fixed" ? start.baseUrl : "");
  let key = $state<SecretEdit>({ kind: "keep" });
  let error = $state<string | null>(null);
  let removing = $state(false);
  let pending = $state(false);

  const used = $derived(view && connection ? usedBy(view, connection.id) : []);
  const forSlots = $derived(preset ? slotWords(SLOTS.filter((s) => servesSlot(preset, s.key)).map((s) => s.key)) : "");
  const addressHint = $derived(
    kind === "host"
      ? `The computer ${presetName(presetId)} runs on: its name or address on your network, such as 192.168.1.20. Pinna adds the port and path.`
      : kind === "host-port"
        ? `Where ${presetName(presetId)} listens: its name or address on your network and its port, such as 192.168.1.20:8080.`
        : "The address of an OpenAI-compatible API, up to and including /v1.",
  );
  const keyHint = $derived(
    kind === "fixed"
      ? `From your ${presetName(presetId)} account. The satellite keeps it and never shows it again.`
      : "Only if the server asks for one. The satellite keeps it and never shows it again.",
  );
  const ready = $derived(!!preset && (kind === "fixed" || !!address.trim()));
  // A key is bound to the address it was entered for: a new address drops it.
  const addressChanged = $derived(!!connection && kind !== "fixed" && address.trim() !== connection.baseUrl);
  const keyDropped = $derived(addressChanged && !!connection?.keySet);

  async function save() {
    pending = true;
    error = await assistant.setConnection(
      {
        id: connection?.id ?? "",
        revision: connection?.revision ?? 0n,
        preset: presetId,
        baseUrl: kind === "fixed" ? "" : address.trim(),
        ...keyFields(key),
      },
      connection ? "Saved" : "Connection added",
    );
    pending = false;
    if (!error) onclose();
  }

  async function remove() {
    if (!connection) return;
    error = await assistant.deleteConnection(connection.id, connection.revision);
    if (!error) onclose();
  }
</script>

<form
  class="pn-row is-stack"
  aria-label={connection ? `Change the ${presetName(connection.preset)} connection` : "Add a connection"}
  onsubmit={(e) => {
    e.preventDefault();
    if (ready && !assistant.busy) void save();
  }}
>
  <div class="form">
    {#if connection}
      <div class="head">
        <span class="pn-row-label">{presetName(connection.preset)}</span>
        <span class="pn-row-hint">{connectionAddress(connection, view?.presets ?? [])}</span>
      </div>
    {:else}
      <div class="pn-field">
        <label class="pn-field-label" for="{id}-preset">Service</label>
        <span class="pn-select-wrap">
          <select
            id="{id}-preset"
            class="pn-select"
            bind:value={presetId}
            aria-describedby="{id}-preset-hint"
            onchange={() => (error = null)}
          >
            <option value="" disabled>Choose a service</option>
            {#each view?.presets ?? [] as p (p.id)}
              <option value={p.id}>{presetName(p.id)}</option>
            {/each}
          </select>
          <Icon name="chevron-down" class="pn-select-chev" />
        </span>
        <span id="{id}-preset-hint" class="pn-field-hint">
          {#if preset}
            For {forSlots}.{kind === "fixed" ? ` It connects to ${hostOf(preset.baseUrl)}.` : ""}
          {:else}
            Hosted services such as OpenAI need an account and its key. Ollama, LM Studio and agents run on a computer
            of yours.
          {/if}
        </span>
      </div>
    {/if}

    {#if preset && kind !== "fixed"}
      <div class="pn-field">
        <label class="pn-field-label" for="{id}-address">{kind === "host" ? "Computer" : "Address"}</label>
        <input
          id="{id}-address"
          class="pn-input is-mono"
          type="text"
          inputmode="url"
          autocomplete="off"
          autocapitalize="off"
          spellcheck="false"
          placeholder={kind === "host" ? "192.168.1.20" : kind === "host-port" ? "192.168.1.20:8080" : "https://example.com/v1"}
          bind:value={address}
          disabled={!live}
          aria-describedby="{id}-address-hint"
          oninput={() => (error = null)}
        />
        <span id="{id}-address-hint" class="pn-field-hint">{addressHint}</span>
      </div>
    {/if}

    {#if preset}
      <SecretField
        id="{id}-key"
        label="API key"
        stored={!!connection?.keySet && !addressChanged}
        shown="set ••••{connection?.keyLast4 ?? ''}"
        bind:edit={key}
        hint={keyDropped
          ? "A new address removes the stored key. Enter it again if the server needs one."
          : keyHint}
        disabled={!live}
      />
    {/if}

    {#if error}<span class="pn-field-error" role="alert"><Icon name="alert" />{error}</span>{/if}

    <div class="actions">
      <button class="pn-btn pn-btn-primary" type="submit" disabled={!live || assistant.busy || !ready}>
        {pending ? (used.length ? "Checking…" : "Saving…") : connection ? "Save" : "Add"}
      </button>
      <button class="pn-btn" type="button" onclick={onclose}>Cancel</button>
      {#if connection}
        <button
          class="pn-btn pn-btn-quiet remove"
          type="button"
          disabled={!live || assistant.busy}
          onclick={() => (removing = true)}>Remove connection…</button
        >
      {/if}
    </div>
  </div>
</form>

{#if connection}
  <Confirm
    bind:open={removing}
    title="Remove the {presetName(connection.preset)} connection?"
    action="Remove"
    danger
    confirm={remove}
  >
    <p>Its key is removed from the satellite too.</p>
    {#if used.length && standalone}
      <p>It is chosen for {slotWords(used)}. Choose another connection there first: the satellite needs them.</p>
    {:else if used.length}
      <p>It is chosen for {slotWords(used)}, which are cleared with it.</p>
    {/if}
  </Confirm>
{/if}

<style>
  .form {
    display: flex;
    flex-direction: column;
    gap: var(--space-4);
  }
  .head {
    display: flex;
    flex-direction: column;
    gap: 2px;
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
