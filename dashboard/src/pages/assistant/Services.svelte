<!-- Assistant › Services: the connections (a service's address and key, from
     a preset) and the three slots of a standalone request. Everything here
     can be set up while Home Assistant still answers. -->
<script lang="ts">
  import { onMount } from "svelte";

  import { assistant } from "../../lib/assistant.svelte.ts";
  import { connectionAddress, keyShown, presetName, SLOTS, slotWords, usedBy } from "../../lib/assistant.ts";
  import { device } from "../../lib/device.svelte.ts";
  import Group from "../../lib/Group.svelte";
  import Icon from "../../lib/Icon.svelte";
  import ConnectionForm from "./ConnectionForm.svelte";
  import SlotCard from "./SlotCard.svelte";

  const live = $derived(device.link === "live");
  const view = $derived(assistant.view);
  /** The connection being changed, "new" while one is added, or null. */
  let editing = $state<string | null>(null);

  onMount(() => void assistant.load());
</script>

{#if view}
  <Group title="Connections">
    {#snippet foot()}
      A connection is a service's address and, where it needs one, its key. The satellite keeps keys to itself: here
      a key shows only as set, with its last four characters.
    {/snippet}
    {#each view.connections as c (c.id)}
      {#if editing === c.id}
        <ConnectionForm connection={c} onclose={() => (editing = null)} />
      {:else}
        {@const used = usedBy(view, c.id)}
        <div class="pn-row">
          <div class="pn-row-text">
            <span class="pn-row-label">{presetName(c.preset)}</span>
            <span class="pn-row-hint">
              <span class="host">{connectionAddress(c, view.presets)}</span> · {keyShown(c)}{used.length
                ? ` · for ${slotWords(used)}`
                : ""}
            </span>
          </div>
          <button
            class="pn-btn pn-btn-sm"
            type="button"
            disabled={!live || editing !== null}
            aria-label="Change the {presetName(c.preset)} connection"
            onclick={() => (editing = c.id)}>Change</button
          >
        </div>
      {/if}
    {/each}
    {#if editing === "new"}
      <ConnectionForm onclose={() => (editing = null)} />
    {:else}
      <div class="pn-row">
        <div class="pn-row-text">
          <span class="pn-row-label">{view.connections.length ? "Add another connection" : "Add a connection"}</span>
          <span class="pn-row-hint">From a preset, such as OpenAI, Anthropic, ElevenLabs or Ollama</span>
        </div>
        <button
          class="pn-btn pn-btn-sm"
          type="button"
          disabled={!live || editing !== null}
          onclick={() => (editing = "new")}>Add…</button
        >
      </div>
    {/if}
  </Group>

  {#each SLOTS as slot (slot.key)}
    <SlotCard {slot} />
  {/each}
{:else if assistant.error}
  <div class="pn-notice" data-tone="danger" role="alert">
    <Icon name="alert" class="pn-icon pn-notice-icon" />
    <div class="pn-notice-body">
      <p class="pn-notice-title">The services couldn't be read</p>
      <p>{assistant.error}</p>
      <div><button class="pn-btn" type="button" onclick={() => assistant.load()}>Try again</button></div>
    </div>
  </div>
{:else}
  <span class="pn-skeleton" style:height="160px"></span>
  <span class="pn-skeleton" style:height="200px"></span>
{/if}

<style>
  .host {
    font-family: var(--font-mono);
  }
</style>
