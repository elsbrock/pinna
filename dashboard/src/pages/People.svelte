<!-- Voice & sound › People: who the satellite recognizes by voice, and the extra
     instructions each person and Everyone else get (ADR 0038). Enrolling a
     voice needs a press of Action at the satellite; the audio never leaves it. -->
<script lang="ts">
  import { onMount, tick } from "svelte";

  import { chars, isStandalone } from "../lib/assistant.ts";
  import { device } from "../lib/device.svelte.ts";
  import Group from "../lib/Group.svelte";
  import Icon from "../lib/Icon.svelte";
  import InstructionsField from "../lib/InstructionsField.svelte";
  import { addReason, closeWarning, enrollBlocked, recognitionLine, voiceLine } from "../lib/people.ts";
  import { CHANGED_ELSEWHERE, people } from "../lib/people.svelte.ts";
  import { recorder } from "../lib/recorder.svelte.ts";
  import { toasts } from "../lib/toast.svelte.ts";
  import PersonForm from "./people/PersonForm.svelte";

  const s = $derived(device.status);
  const live = $derived(device.link === "live");
  const view = $derived(people.view);
  const standalone = $derived(isStandalone(s));
  const list = $derived(view?.people ?? []);
  const recognition = $derived(recognitionLine(view?.recognition, list.length > 0));
  const full = $derived(view?.limits ? addReason(list.length, view.limits.maxPeople) : null);
  const blocked = $derived(enrollBlocked(s, recorder.here));

  onMount(() => void people.load());

  // "new", a person's id, or nothing. A person removed meanwhile closes their form.
  let editing = $state<string | null>(null);
  // The person just added, whose form says what to do next.
  let addedId = $state<string | null>(null);
  // Back to where the owner was: the person's Change button, or Add after a removal or a cancel.
  async function closeForm(afterId: string | null) {
    editing = null;
    addedId = null;
    await tick();
    document.getElementById(afterId ? `change-${afterId}` : "add-person")?.focus();
  }
  const editingPerson = $derived(list.find((p) => p.id === editing));
  $effect(() => {
    if (view && editing && editing !== "new" && !editingPerson) editing = null;
  });

  // Everyone else's instructions follow the satellite until someone types in them.
  let everyone = $state("");
  let touched = $state(false);
  let base = 0n;
  let everyoneError = $state<string | null>(null);
  $effect(() => {
    if (touched || !view?.everyoneElse) return;
    everyone = view.everyoneElse.instructions;
    base = view.everyoneElse.revision;
  });
  const maxInstructions = $derived(view?.limits?.maxInstructions ?? 0);
  const everyoneTooLong = $derived(maxInstructions > 0 && chars(everyone) > maxInstructions);
  async function saveEveryone() {
    everyoneError = await people.setEveryoneElse(everyone, base);
    if (!everyoneError) toasts.show("Saved");
    // Refused on an old version: the list was read again, and shows the current.
    if (!everyoneError || everyoneError === CHANGED_ELSEWHERE) touched = false;
  }
</script>

{#if view}
  <div class="pn-notice" role="status">
    <Icon name="info" class="pn-icon pn-notice-icon" />
    <div class="pn-notice-body">
      <p class="pn-notice-title">
        Voice recognition is a convenience, not a lock: a recording or a similar voice can pass as someone else.
      </p>
      <p>Anyone the satellite doesn't recognize gets Everyone else, so set limits there.</p>
      <p>Voiceprints stay on this satellite. Each person enrolls on each satellite.</p>
      <p>In standalone mode, the brain's service gets a person's name and extra instructions, in the prompt.</p>
      <p>If you use Home Assistant, it keeps a history of who spoke, as Last speaker.</p>
    </div>
  </div>

  {#if !standalone}
    <div class="pn-notice" role="status">
      <Icon name="info" class="pn-icon pn-notice-icon" />
      <div class="pn-notice-body">
        <p>Extra instructions apply in standalone mode. In Home Assistant mode, the satellite only reports who spoke.</p>
      </div>
    </div>
  {/if}

  {#if recognition}
    <p class="state" class:is-error={recognition.error} role="status">
      {#if recognition.error}<Icon name="alert" />{/if}{recognition.text}
    </p>
  {/if}

  <Group title="People">
    {#snippet aside()}{#if list.length}{list.length}{/if}{/snippet}
    {#each list as p (p.id)}
      {#if editing === p.id}
        <PersonForm
          person={p}
          {blocked}
          added={addedId === p.id}
          onclose={(removed) => void closeForm(removed ? null : p.id)}
          onadded={() => {}}
        />
      {:else}
        {@const close = closeWarning(p.closeTo, list)}
        <div class="pn-row">
          <div class="pn-row-text">
            <span class="pn-row-label">{p.name}</span>
            <span class="pn-row-hint">{voiceLine(p.voice)}</span>
            {#if close}<span class="pn-row-hint is-caution">{close}</span>{/if}
          </div>
          <button
            id="change-{p.id}"
            class="pn-btn pn-btn-sm"
            type="button"
            disabled={!live || people.enrolling}
            aria-label="Change {p.name}"
            onclick={() => (editing = p.id)}
          >
            Change
          </button>
        </div>
      {/if}
    {:else}
      {#if editing !== "new"}
        <div class="pn-row">
          <div class="pn-row-text">
            <span class="pn-row-label">Nobody yet</span>
            <span class="pn-row-hint">
              Add a person, then enroll their voice: they stand at the satellite and speak for 30 seconds. After that
              the satellite knows who speaks{standalone
                ? ", and gives each person their own instructions"
                : ", and reports it as Last speaker"}.
            </span>
          </div>
        </div>
      {/if}
    {/each}
    {#if editing === "new"}
      <PersonForm
        {blocked}
        onclose={() => void closeForm(null)}
        onadded={(id) => {
          addedId = id;
          editing = id;
        }}
      />
    {:else}
      <div class="pn-row">
        <div class="pn-row-text">
          <span class="pn-row-label">Add a person</span>
          <span class="pn-row-hint" class:is-caution={!!full}>
            {full ?? (people.enrolling ? "Finish or cancel the enrollment first." : standalone ? "A name, extra instructions, and their voice" : "A name and their voice")}
          </span>
        </div>
        <button
          id="add-person"
          class="pn-btn"
          type="button"
          disabled={!live || !!full || people.enrolling}
          onclick={() => (editing = "new")}>Add</button
        >
      </div>
    {/if}
  </Group>

  <Group title="Everyone else">
    <form
      class="pn-row is-stack"
      onsubmit={(e) => {
        e.preventDefault();
        if (touched && !everyoneTooLong && !people.busy && live) void saveEveryone();
      }}
    >
      <div class="form">
        <InstructionsField
          id="everyone-instructions"
          label="Extra instructions"
          bind:value={everyone}
          max={maxInstructions}
          hint="Used for anyone the satellite doesn't recognize, and whenever recognition isn't sure."
          error={everyoneError}
          disabled={!live}
          oninput={() => {
            touched = true;
            everyoneError = null;
          }}
        />
        <p class="pn-field-hint">Everyone else can't be enrolled, removed or renamed.</p>
        <div>
          <button class="pn-btn pn-btn-primary" type="submit" disabled={!live || people.busy || !touched || everyoneTooLong}>
            Save
          </button>
        </div>
      </div>
    </form>
  </Group>
{:else if people.error}
  <div class="pn-notice" data-tone="danger" role="alert">
    <Icon name="alert" class="pn-icon pn-notice-icon" />
    <div class="pn-notice-body">
      <p class="pn-notice-title">The people couldn't be read</p>
      <p>{people.error}</p>
      <div><button class="pn-btn" type="button" onclick={() => people.load()}>Try again</button></div>
    </div>
  </div>
{:else}
  <span class="pn-skeleton" style:height="120px"></span>
  <span class="pn-skeleton" style:height="200px"></span>
{/if}

<style>
  .form {
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
  }
  .form p {
    margin: 0;
  }
  .state {
    display: flex;
    gap: var(--space-2);
    margin: 0;
    color: var(--ink-muted);
  }
  .state.is-error {
    color: var(--danger);
    font-weight: 650;
  }
</style>
