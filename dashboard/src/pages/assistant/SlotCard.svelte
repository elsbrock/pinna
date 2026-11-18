<!-- One slot of a standalone request: its connection and its model, or for
     text-to-speech its voice. Saving checks it with the provider first; a
     check never sends a prompt. The brain has Advanced settings, and
     text-to-speech plays a sample. -->
<script lang="ts">
  import { assistant } from "../../lib/assistant.svelte.ts";
  import {
    connectionAddress,
    errorSlot,
    failureText,
    keyShown,
    presetName,
    SLOTS,
    servesSlot,
    slotLines,
    slotName,
  } from "../../lib/assistant.ts";
  import { device } from "../../lib/device.svelte.ts";
  import Group from "../../lib/Group.svelte";
  import Dropdown from "../../lib/Dropdown.svelte";
  import Icon from "../../lib/Icon.svelte";
  import SuggestInput from "../../lib/SuggestInput.svelte";
  import BrainAdvanced from "./BrainAdvanced.svelte";

  let { slot }: { slot: (typeof SLOTS)[number] } = $props();

  const s = $derived(device.status);
  const live = $derived(device.link === "live");
  const view = $derived(assistant.view);
  const choice = $derived(view?.[slot.key]);
  const line = $derived(slotLines(s?.assistant, view, assistant.failed).find((l) => l.key === slot.key)!);
  const failure = $derived(errorSlot(s?.assistant) === slot.key ? failureText(s?.assistant) : "");
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
  let speechModel = $state("");
  $effect(() => {
    if (touched) return;
    connection = choice?.connection ?? options[0]?.id ?? "";
    model = choice?.model ?? "";
    speechModel = choice?.speechModel ?? "";
  });
  const edited = () => {
    touched = true;
    error = null;
  };
  const preset = $derived(presetOf(connection));
  // ElevenLabs takes a voice's ID, which nobody can read: the list shows the
  // names. Its speech-to-text models are Scribe's, which it doesn't list.
  const modelHint = $derived.by(() => {
    const elevenlabs = preset?.id === "elevenlabs";
    if (slot.holds === "voice")
      return elevenlabs
        ? "The voice's ID. Choose a voice from your library by name, or paste its ID."
        : "The voice's name or ID, as the service lists it, such as alloy or nova for OpenAI.";
    if (slot.key === "stt" && elevenlabs) return "A Scribe model, such as scribe_v2.";
    return "The model's ID, as the service lists it. Choose from its list, or type it.";
  });
  // A preset that names its own model (OpenClaw) takes none.
  const fixedModel = $derived(slot.key === "brain" ? (presetOf(connection)?.model ?? "") : "");
  const unchanged = $derived(
    !!choice &&
      choice.connection === connection &&
      choice.model === model.trim() &&
      choice.speechModel === speechModel.trim(),
  );
  const id = $derived(`slot-${slot.key}`);

  let checking = $state(false);
  let error = $state<string | null>(null);
  // A change to the chosen connection (its address or key) makes an old
  // refusal moot: check again.
  const revision = $derived(view?.connections.find((c) => c.id === connection)?.revision);
  $effect(() => {
    void revision;
    error = null;
  });
  // What stays in use when a change is refused.
  const saved = $derived(
    choice ? slotName(choice.model, view?.connections.find((c) => c.id === choice.connection)?.preset ?? "") : "",
  );
  async function check() {
    checking = true;
    error = await assistant.setSlot(
      slot.key,
      connection,
      fixedModel ? "" : model.trim(),
      slot.key === "tts" ? speechModel.trim() : "",
    );
    checking = false;
    if (!error) touched = false;
  }

  // Play sample: the reply voice saying one sentence on the satellite.
  const SAMPLE = "This is how replies sound.";
  const canSample = $derived(live && line.ready);
  let sampling = $state(false);
  let sampled = $state(false);
  let sampleError = $state<string | null>(null);
  async function sample() {
    sampling = true;
    sampled = false;
    sampleError = await assistant.playSample(SAMPLE);
    sampling = false;
    sampled = !sampleError;
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
  {#if failure}
    <div class="pn-row">
      <div class="pn-row-text">
        <span class="pn-row-label">Last failed request</span>
        <span class="pn-row-hint is-danger">{failure}</span>
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
          <Dropdown
            id="{id}-connection"
            value={connection}
            options={options.map((c) => ({
              id: c.id,
              label: `${presetName(c.preset)} · ${connectionAddress(c, view?.presets ?? [])} · ${keyShown(c)}`,
            }))}
            disabled={!live}
            choose={(v) => {
              connection = v;
              touched = true;
              error = null;
            }}
          />
        </div>
        {#if fixedModel}
          <div class="pn-field">
            <span class="pn-field-label">Model</span>
            <span class="pn-field-hint">{presetName(presetOf(connection)?.id ?? "")} uses its own: {fixedModel}.</span>
          </div>
        {:else}
          <!-- A refusal shows under the form: it may be about the
               connection as much as the model. -->
          <div class="pn-field">
            <label class="pn-field-label" for="{id}-model">{slot.holds === "voice" ? "Voice" : "Model"}</label>
            <SuggestInput
              id="{id}-model"
              bind:value={model}
              source="{connection}|{slot.holds}"
              load={() => assistant.choices(slot.key, connection, slot.holds === "voice" ? "voice" : "model")}
              disabled={!live || !connection}
              describedby="{id}-model-hint"
              oninput={edited}
            />
            <span id="{id}-model-hint" class="pn-field-hint">{modelHint}</span>
          </div>
          {#if slot.key === "tts"}
            <div class="pn-field">
              <label class="pn-field-label" for="{id}-speech-model">Model</label>
              <SuggestInput
                id="{id}-speech-model"
                bind:value={speechModel}
                source="{connection}|model"
                load={() => assistant.choices("tts", connection, "model")}
                placeholder={preset?.ttsModel}
                disabled={!live || !connection}
                describedby="{id}-speech-model-hint"
                oninput={edited}
              />
              <span id="{id}-speech-model-hint" class="pn-field-hint">
                The model that speaks the voice. Empty uses {preset?.ttsModel || "the service's"}.
              </span>
            </div>
          {/if}
        {/if}
      </div>
      {#if error}<span class="pn-field-error" role="alert"
          ><Icon name="alert" />{saved && !unchanged ? `Not saved: it still uses ${saved}. ` : ""}{error}</span
        >{/if}
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
          {:else if sampled}
            It played on the satellite.
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
  .fields :global(.pn-select-wrap) {
    align-self: stretch;
    max-width: none;
  }
  .fields :global(.pn-select) {
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
