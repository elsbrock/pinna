<!-- The brain's Advanced settings: who runs the tools, how much of the
     conversation it gets, and how long it may take to start answering. Each
     preset sets them; these are for a server of your own. Saved together. -->
<script lang="ts">
  import { Delegation, History } from "../../gen/pinna/v1/assistant_pb.ts";
  import { assistant } from "../../lib/assistant.svelte.ts";
  import { brainPreset, delegation as runsTools, presetName } from "../../lib/assistant.ts";
  import { device } from "../../lib/device.svelte.ts";
  import Icon from "../../lib/Icon.svelte";
  import Select from "../../lib/Select.svelte";

  const live = $derived(device.link === "live");
  const set = $derived(assistant.view?.brainOverride);
  // What the brain's preset does without the setting.
  const presetRuns = $derived(assistant.view ? runsTools({ ...assistant.view, brainOverride: undefined }) : null);
  const service = $derived(assistant.view ? presetName(brainPreset(assistant.view)?.id ?? "") : "");

  // The form follows the satellite until someone changes it.
  let touched = $state(false);
  let delegation = $state(String(Delegation.UNSPECIFIED));
  let history = $state(String(History.UNSPECIFIED));
  let limit = $state("");
  let firstText = $state("");
  $effect(() => {
    if (touched) return;
    delegation = String(set?.delegation ?? Delegation.UNSPECIFIED);
    history = String(set?.history ?? History.UNSPECIFIED);
    limit = set?.historyLimit ? String(set.historyLimit) : "";
    firstText = set?.firstTextSeconds === undefined ? "" : String(set.firstTextSeconds);
  });
  const sized = $derived(Number(history) === History.TURNS || Number(history) === History.BYTES);
  const edit = (apply: () => void) => {
    apply();
    touched = true;
    error = null;
  };

  let error = $state<string | null>(null);
  async function save() {
    error = await assistant.update({
      brainOverride: {
        delegation: Number(delegation),
        history: Number(history),
        historyLimit: sized ? Math.max(0, Math.trunc(Number(limit) || 0)) : 0,
        firstTextSeconds: firstText.trim() ? Math.max(0, Math.trunc(Number(firstText) || 0)) : undefined,
      },
    });
    if (!error) touched = false;
  }
</script>

<Select
  id="brain-delegation"
  label="Who runs the tools"
  hint="The satellite offers the brain its tools and runs the ones it asks for. A brain that is an agent answers with tools and memory of its own.{presetRuns &&
  service
    ? ` For ${service}, ${presetRuns === 'hand-off' ? 'the brain' : 'the satellite'} runs them unless you choose here.`
    : ''}"
  value={delegation}
  options={[
    { id: String(Delegation.UNSPECIFIED), label: "The preset's choice" },
    { id: String(Delegation.TOOL_LOOP), label: "The satellite" },
    { id: String(Delegation.HAND_OFF), label: "The brain" },
  ]}
  disabled={!live}
  choose={(v) => edit(() => (delegation = v))}
/>
<Select
  id="brain-history"
  label="Conversation it gets"
  hint="What the brain hears of the requests before this one."
  value={history}
  options={[
    { id: String(History.UNSPECIFIED), label: "The preset's choice" },
    { id: String(History.TURNS), label: "The last requests" },
    { id: String(History.BYTES), label: "Up to a size" },
    { id: String(History.NEW_TURN_ONLY), label: "Only the new request" },
  ]}
  disabled={!live}
  choose={(v) => edit(() => (history = v))}
/>
{#if sized}
  <div class="pn-row">
    <div class="pn-row-text">
      <label class="pn-row-label" for="brain-limit">{Number(history) === History.TURNS ? "Requests" : "Bytes"}</label>
      <span id="brain-limit-hint" class="pn-row-hint">
        {Number(history) === History.TURNS ? "1 to 10, with their replies." : "Up to 65536 bytes of text, oldest first to go."}
      </span>
    </div>
    <input
      id="brain-limit"
      class="pn-input is-mono number"
      type="number"
      inputmode="numeric"
      min="1"
      max={Number(history) === History.TURNS ? 10 : 65536}
      value={limit}
      disabled={!live}
      aria-describedby="brain-limit-hint"
      oninput={(e) => edit(() => (limit = e.currentTarget.value))}
    />
  </div>
{/if}
<div class="pn-row">
  <div class="pn-row-text">
    <label class="pn-row-label" for="brain-first-text">Time to answer</label>
    <span id="brain-first-text-hint" class="pn-row-hint">
      Seconds it may take to start answering, 1 to 300. Empty keeps the preset's: 30 for a model, 120 for an agent.
    </span>
  </div>
  <input
    id="brain-first-text"
    class="pn-input is-mono number"
    type="number"
    inputmode="numeric"
    min="1"
    max="300"
    placeholder="Preset"
    value={firstText}
    disabled={!live}
    aria-describedby="brain-first-text-hint"
    oninput={(e) => edit(() => (firstText = e.currentTarget.value))}
  />
</div>
<div class="pn-row is-actions">
  {#if error}<span class="pn-field-error" role="alert"><Icon name="alert" />{error}</span>{/if}
  <button class="pn-btn pn-btn-primary" type="button" disabled={!live || assistant.busy || !touched} onclick={save}>
    Save
  </button>
</div>

<style>
  .number {
    flex: none;
    width: 7em;
  }
</style>
