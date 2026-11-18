<!-- One slot of a standalone request: its connection and its model, or for
     text-to-speech its voice. Saving checks it with the provider first; a
     check never sends a prompt. The brain has Advanced settings, and
     text-to-speech plays a sample. -->
<script lang="ts">
  import { assistant } from "../../lib/assistant.svelte.ts";
  import { errorSlot, hostOf, presetName, SLOTS, servesSlot, slotLines } from "../../lib/assistant.ts";
  import { device } from "../../lib/device.svelte.ts";
  import Group from "../../lib/Group.svelte";
  import Icon from "../../lib/Icon.svelte";
  import BrainAdvanced from "./BrainAdvanced.svelte";

  let { slot }: { slot: (typeof SLOTS)[number] } = $props();

  const s = $derived(device.status);
  const live = $derived(device.link === "live");
  const view = $derived(assistant.view);
  const choice = $derived(view?.[slot.key]);
  const line = $derived(slotLines(s?.assistant, view, assistant.failed).find((l) => l.key === slot.key)!);
  const lastError = $derived(s?.assistant?.lastError ?? "");
  const presetOf = (id: string) => view?.presets.find((p) => p.id === view?.connections.find((c) => c.id === id)?.preset);
  const options = $derived(
    (view?.connections ?? []).filter((c) => {
      const p = view?.presets.find((x) => x.id === c.preset);
      return !!p && servesSlot(p, slot.key);
    }),
  );
  // A few presets that serve the slot, for the hint while none is added.
  const offering = $derived(
    (view?.presets ?? [])
      .filter((p) => servesSlot(p, slot.key) && p.id !== "custom")
      .slice(0, 3)
      .map((p) => presetName(p.id)),
  );

  // The form follows the satellite until someone changes it.
  let touched = $state(false);
  let connection = $state("");
  let model = $state("");
  $effect(() => {
    if (touched) return;
    connection = choice?.connection ?? options[0]?.id ?? "";
    model = choice?.model ?? "";
  });
  // A preset that names its own model (OpenClaw) takes none.
  const fixedModel = $derived(slot.key === "brain" ? (presetOf(connection)?.model ?? "") : "");
  const unchanged = $derived(!!choice && choice.connection === connection && choice.model === model.trim());
  const id = $derived(`slot-${slot.key}`);

  let checking = $state(false);
  let error = $state<string | null>(null);
  async function check() {
    checking = true;
    error = await assistant.setSlot(slot.key, connection, fixedModel ? "" : model.trim());
    checking = false;
    if (!error) touched = false;
  }

  // Play sample: the reply voice saying one sentence on the satellite.
  const SAMPLE = "This is how replies sound.";
  const everySlotSet = $derived(!!view && SLOTS.every((x) => !!view[x.key]));
  const canSample = $derived(live && line.ready && everySlotSet);
  let sampling = $state(false);
  let sampleError = $state<string | null>(null);
  async function sample() {
    sampling = true;
    sampleError = await assistant.playSample(SAMPLE);
    sampling = false;
  }

  let advanced = $state(false);
</script>

<Group title={slot.name}>
  <div class="pn-row">
    <div class="pn-row-text">
      <span class="pn-row-label">Last check</span>
      <span class="pn-row-hint">
        {#if line.ready}
          Passed with this connection and {slot.holds}.
        {:else if line.tone === "danger"}
          <!-- The form below says why, while it shows the same refusal. -->
          {error ? "" : assistant.failed[slot.key]}
        {:else if line.word === "Not checked"}
          Its connection changed since. Check it again.
        {:else}
          Choose a connection and a {slot.holds}, then check them.
        {/if}
      </span>
    </div>
    <span class="pn-row-value"><span class="pn-health" data-tone={line.tone}>{line.word}</span></span>
  </div>
  {#if errorSlot(lastError) === slot.key}
    <div class="pn-row">
      <div class="pn-row-text">
        <span class="pn-row-label">Last failed request</span>
        <span class="pn-row-hint is-danger">{lastError}</span>
      </div>
    </div>
  {/if}

  {#if options.length === 0}
    <div class="pn-row">
      <span class="pn-row-hint">
        No connection offers {slot.key === "brain" ? "a brain" : slot.name.toLowerCase()} yet. Add one above, such as
        {offering.slice(0, -1).join(", ")} or {offering.at(-1)}.
      </span>
    </div>
  {:else}
    <form
      class="pn-row is-stack"
      aria-label={slot.name}
      onsubmit={(e) => {
        e.preventDefault();
        void check();
      }}
    >
      <div class="fields">
        <div class="pn-field">
          <label class="pn-field-label" for="{id}-connection">Connection</label>
          <span class="pn-select-wrap">
            <select
              id="{id}-connection"
              class="pn-select"
              bind:value={connection}
              disabled={!live}
              onchange={() => {
                touched = true;
                error = null;
              }}
            >
              {#each options as c (c.id)}
                <option value={c.id}>{presetName(c.preset)} · {hostOf(c.baseUrl)}</option>
              {/each}
            </select>
            <Icon name="chevron-down" class="pn-select-chev" />
          </span>
        </div>
        {#if fixedModel}
          <div class="pn-field">
            <span class="pn-field-label">Model</span>
            <span class="pn-field-hint">{presetName(presetOf(connection)?.id ?? "")} uses its own: {fixedModel}.</span>
          </div>
        {:else}
          <div class="pn-field" data-invalid={error ? "" : undefined}>
            <label class="pn-field-label" for="{id}-model">{slot.holds === "voice" ? "Voice" : "Model"}</label>
            <input
              id="{id}-model"
              class="pn-input is-mono"
              type="text"
              autocomplete="off"
              autocapitalize="off"
              spellcheck="false"
              bind:value={model}
              disabled={!live}
              aria-describedby="{id}-model-hint"
              aria-invalid={error ? "true" : undefined}
              oninput={() => {
                touched = true;
                error = null;
              }}
            />
            <span id="{id}-model-hint" class="pn-field-hint">
              {slot.holds === "voice"
                ? "The voice's name or ID, as the service lists it."
                : "The model's ID, as the service lists it."}
            </span>
          </div>
        {/if}
      </div>
      {#if error}<span class="pn-field-error" role="alert"><Icon name="alert" />{error}</span>{/if}
      <div class="actions">
        <button
          class="pn-btn pn-btn-primary"
          type="submit"
          disabled={!live || assistant.busy || !connection || (!fixedModel && !model.trim())}
        >
          {checking ? "Checking…" : unchanged ? "Check again" : "Save and check"}
        </button>
      </div>
    </form>
  {/if}

  {#if slot.key === "tts"}
    <div class="pn-row">
      <div class="pn-row-text">
        <span class="pn-row-label">Play sample</span>
        <span id="sample-hint" class="pn-row-hint">
          {#if !line.ready}
            Plays once this voice passes its check.
          {:else if !everySlotSet}
            Plays once speech-to-text and the brain are chosen too.
          {:else}
            The satellite says “{SAMPLE}” with this voice.
          {/if}
        </span>
      </div>
      <button
        class="pn-btn"
        type="button"
        disabled={!canSample}
        aria-disabled={assistant.busy || undefined}
        aria-describedby="sample-hint"
        onclick={() => {
          if (!assistant.busy) void sample();
        }}>{sampling ? "Playing…" : "Play sample"}</button
      >
    </div>
    {#if sampleError}
      <div class="pn-row">
        <span class="pn-field-error" role="alert"><Icon name="alert" />{sampleError}</span>
      </div>
    {/if}
  {/if}

  {#if slot.key === "brain"}
    <button
      class="pn-row"
      type="button"
      aria-expanded={advanced}
      aria-controls="brain-advanced"
      onclick={() => (advanced = !advanced)}
    >
      <span class="pn-row-text">
        <span class="pn-row-label">Advanced</span>
        <span class="pn-row-hint">Who runs the tools, how much of the conversation it gets, and how long it may take</span>
      </span>
      <Icon name={advanced ? "chevron-down" : "chevron-right"} class="pn-icon pn-row-chev" />
    </button>
    {#if advanced}
      <div id="brain-advanced"><BrainAdvanced /></div>
    {/if}
  {/if}
</Group>

<style>
  .fields {
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    gap: var(--space-4);
  }
  @media (min-width: 640px) {
    .fields {
      grid-template-columns: minmax(0, 1fr) minmax(0, 1fr);
    }
  }
  .fields .pn-select-wrap {
    align-self: stretch;
    max-width: none;
  }
  .fields .pn-select {
    width: 100%;
  }
  form {
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
  }
  .actions {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-2) var(--space-3);
  }
  .is-danger {
    color: var(--danger);
    font-weight: 650;
  }
</style>
