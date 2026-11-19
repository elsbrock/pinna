<!-- Voice & sound › Wake words: the words on offer, and adding or removing a
     custom one (ADR 0013). The satellite tests an uploaded model before it
     keeps it, which can take up to a minute, so the form has its own pending
     state and the rest of the page keeps working. -->
<script lang="ts">
  import { tick } from "svelte";

  import Confirm from "../lib/Confirm.svelte";
  import { device } from "../lib/device.svelte.ts";
  import FileField from "../lib/FileField.svelte";
  import Group from "../lib/Group.svelte";
  import Icon from "../lib/Icon.svelte";
  import {
    addedNextStep,
    customWordCount,
    MAX_CUSTOM_WORDS,
    modelFileProblem,
    phraseProblem,
    PHRASE_MAX,
    PHRASE_MIN,
    removeWordEffects,
    wordHint,
  } from "../lib/view.ts";

  const s = $derived(device.status);
  const live = $derived(device.link === "live");
  const words = $derived(s?.wakeWord?.available ?? []);
  const customs = $derived(s ? customWordCount(s) : 0);
  const full = $derived(customs >= MAX_CUSTOM_WORDS);

  let phrase = $state("");
  let file = $state<File | null>(null);
  // What the satellite or the checks here refused, and what to do after an add.
  let refused = $state<string | null>(null);
  let added = $state<string | null>(null);
  const fileProblem = $derived(file ? modelFileProblem(file.name, file.size) : null);
  const canAdd = $derived(live && !full && !device.addingWakeWord && !!phrase.trim() && !!file && !fileProblem);

  async function add() {
    if (!file || device.addingWakeWord) return;
    const p = phrase.trim();
    refused = phraseProblem(p);
    if (refused) return;
    added = null;
    try {
      await device.addWakeWord(p, file);
      phrase = "";
      file = null;
      added = addedNextStep(device.status);
      // The Add button is disabled now that the form is empty: keep the focus
      // in the form, once the phrase field is enabled again.
      await tick();
      document.getElementById("ww-phrase")?.focus();
    } catch (e) {
      refused = e instanceof Error ? e.message : "The satellite refused.";
    }
  }

  let removing = $state<{ id: string; phrase: string } | null>(null);
  let removeOpen = $state(false);
  async function remove() {
    if (!removing) return;
    // The next step after an add no longer applies.
    added = null;
    await device.removeWakeWord(removing.id, removing.phrase);
    // Its row is gone: keep the focus on the page.
    document.getElementById("ww-phrase")?.focus();
  }
</script>

{#if s}
  <Group title="Wake words">
    {#snippet aside()}{#if words.length}{words.length}{/if}{/snippet}
    {#each words as w (w.id)}
      <div class="pn-row">
        <div class="pn-row-text">
          <span class="pn-row-label">{w.phrase}</span>
          <span class="pn-row-hint" class:is-danger={!!w.error}>{wordHint(s, w)}</span>
        </div>
        {#if w.custom}
          <button
            class="pn-btn pn-btn-quiet pn-btn-sm"
            disabled={!live}
            aria-disabled={device.busy || undefined}
            aria-label="Remove {w.phrase}"
            onclick={() => {
              if (device.busy) return;
              removing = { id: w.id, phrase: w.phrase };
              removeOpen = true;
            }}>Remove</button
          >
        {/if}
      </div>
    {:else}
      <div class="pn-row">
        <div class="pn-row-text">
          <span class="pn-row-label">No wake words</span>
          <span class="pn-row-hint">No wake word models are installed on this satellite. Add one below.</span>
        </div>
      </div>
    {/each}
  </Group>

  <Group title="Add a wake word">
    {#snippet foot()}
      Use an openWakeWord model, <code class="pn-code">.onnx</code> or <code class="pn-code">.tflite</code>, of at most
      2 MiB. You can add up to {MAX_CUSTOM_WORDS}. Pinna can't run microWakeWord models, which ESPHome devices such as
      Voice PE use. The satellite tests each model before it keeps it.
    {/snippet}
    <form
      class="pn-row is-stack"
      onsubmit={(e) => {
        e.preventDefault();
        void add();
      }}
    >
      {#if full}
        <p class="pn-field-hint">
          This satellite has {MAX_CUSTOM_WORDS} added wake words, the most it keeps. Remove one first.
        </p>
      {/if}
      <div class="pn-field">
        <label class="pn-field-label" for="ww-phrase">Phrase</label>
        <input
          id="ww-phrase"
          class="pn-input"
          type="text"
          autocomplete="off"
          spellcheck="false"
          minlength={PHRASE_MIN}
          maxlength={PHRASE_MAX}
          bind:value={phrase}
          disabled={!live || full || device.addingWakeWord}
          aria-describedby="ww-phrase-hint"
          oninput={() => {
            refused = null;
            added = null;
          }}
        />
        <span id="ww-phrase-hint" class="pn-field-hint">The phrase the model was trained on, {PHRASE_MIN} to {PHRASE_MAX} characters.</span>
      </div>
      <FileField
        id="ww-file"
        label="Model file"
        accept=".onnx,.tflite"
        bind:file
        error={fileProblem}
        disabled={!live || full || device.addingWakeWord}
      />
      <div class="pn-field" data-invalid={refused ? "" : undefined}>
        <div class="actions">
          <!-- aria-disabled while it tests: disabling the pressed button would drop the focus. -->
          <button
            class="pn-btn pn-btn-primary"
            type="submit"
            disabled={!canAdd && !device.addingWakeWord}
            aria-disabled={device.addingWakeWord || undefined}
          >
            {device.addingWakeWord ? "Testing on the satellite…" : "Add"}
          </button>
          {#if device.addingWakeWord}
            <span class="pn-field-hint" role="status">
              The satellite tests the model on silence, noise and time. It can take up to a minute.
            </span>
          {/if}
        </div>
        {#if refused}<span class="pn-field-error" role="alert"><Icon name="alert" />{refused}</span>{/if}
        {#if added}<span class="pn-field-hint" role="status">Added. {added}</span>{/if}
      </div>
    </form>
  </Group>

  <Confirm bind:open={removeOpen} title="Remove “{removing?.phrase}”?" action="Remove" danger confirm={remove}>
    {#each removing ? removeWordEffects(s, removing.id) : [] as line (line)}
      <p>{line}</p>
    {/each}
  </Confirm>
{:else}
  <span class="pn-skeleton" style:height="120px"></span>
{/if}

<style>
  .actions {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--space-2) var(--space-3);
  }
  .is-danger {
    color: var(--danger);
    font-weight: 650;
  }
</style>
