<!-- Add a person, or change one: the name, the extra instructions, their voice
     and removing them (ADR 0038). An edit names the revision it started from;
     one made on an older version is refused, and the form shows the current. -->
<script lang="ts">
  import { onMount, tick, untrack } from "svelte";

  import type { Person } from "../../gen/pinna/v1/people_pb.ts";
  import Confirm from "../../lib/Confirm.svelte";
  import { device } from "../../lib/device.svelte.ts";
  import Icon from "../../lib/Icon.svelte";
  import InstructionsField from "../../lib/InstructionsField.svelte";
  import { chars } from "../../lib/assistant.ts";
  import { closeWarning, isNameRefusal, nameProblem, removeNote, voiceLine, VOICE } from "../../lib/people.ts";
  import { CHANGED_ELSEWHERE, people } from "../../lib/people.svelte.ts";
  import { toasts } from "../../lib/toast.svelte.ts";
  import Enroll from "./Enroll.svelte";

  let {
    person,
    blocked = null,
    onclose,
    onadded,
    added = false,
  }: {
    person?: Person;
    /** Why a voice can't be enrolled now, from what the satellite is doing. */
    blocked?: string | null;
    onclose: (removed?: boolean) => void;
    /** This person was just added: say what to do next, and lead to Enroll voice. */
    added?: boolean;
    /** An added person's id: the page goes on to their form. */
    onadded: (id: string) => void;
  } = $props();

  const live = $derived(device.link === "live");
  const id = $derived(person ? `person-${person.id}` : "person-new");
  const limits = $derived(people.view?.limits);
  const maxName = $derived(limits?.maxName ?? 0);
  const maxInstructions = $derived(limits?.maxInstructions ?? 0);

  // The form starts from the person as they were when it opened.
  const start = untrack(() => person);
  let name = $state(start?.name ?? "");
  let instructions = $state(start?.instructions ?? "");
  let revision = start?.revision ?? 0n;
  let error = $state<string | null>(null);
  // The satellite's refusal of the name, shown under it.
  let nameError = $state<string | null>(null);
  let touched = $state(false);
  let nameInput = $state<HTMLInputElement>();
  let enrollButton = $state<HTMLButtonElement>();
  let removing = $state(false);
  let enrolling = $state(false);

  const problem = $derived(nameProblem(name, maxName));
  const shownName = $derived(nameError ?? (touched ? problem : null));
  // Nothing else is touched while an enrollment waits or listens.
  const locked = $derived(people.enrolling);

  onMount(() => {
    if (!added) nameInput?.focus();
    else
      void tick().then(() => {
        enrollButton?.focus();
        enrollButton?.scrollIntoView({ block: "nearest" });
      });
  });
  const tooLong = $derived(maxInstructions > 0 && chars(instructions) > maxInstructions);
  const unchanged = $derived(!!person && name.trim() === person.name && instructions === person.instructions);
  const renamed = $derived(!!person && !!name.trim() && name.trim() !== person.name);
  const close = $derived(person ? closeWarning(person.closeTo, people.view?.people ?? []) : null);

  async function save() {
    const saved = await people.setPerson(person?.id ?? "", revision, name.trim(), instructions);
    error = null;
    nameError = null;
    if (saved.error && isNameRefusal(saved.error)) nameError = saved.error;
    else error = saved.error;
    if (saved.error === CHANGED_ELSEWHERE && person) {
      // The list was read again: show the current version.
      name = person.name;
      instructions = person.instructions;
      revision = person.revision;
    } else if (!saved.error) {
      if (person) toasts.show("Saved");
      else onadded(saved.id);
    }
  }

  async function remove() {
    if (!person) return;
    error = await people.deletePerson(person.id);
    if (!error) onclose(true);
  }
</script>

<!-- Escape closes the form, but not in the middle of an enrollment or over a dialog. -->
<svelte:window
  onkeydown={(e) => {
    if (e.key === "Escape" && !locked && !removing && !document.querySelector("dialog[open]")) onclose();
  }}
/>

<form
  class="pn-row is-stack"
  aria-label={person ? `Change ${person.name}` : "Add a person"}
  onsubmit={(e) => {
    e.preventDefault();
    touched = true;
    if (!problem && !tooLong && !unchanged && live && !people.busy && !locked) void save();
  }}
>
  <div class="form">
    {#if added}
      <p class="pn-field-hint" role="status">Added {person?.name}. Now enroll their voice.</p>
    {/if}
    <div class="pn-field" data-invalid={shownName ? "" : undefined}>
      <label class="pn-field-label" for="{id}-name">Name</label>
      <input
        id="{id}-name"
        class="pn-input"
        autocomplete="off"
        spellcheck="false"
        maxlength={maxName || undefined}
        bind:this={nameInput}
        bind:value={name}
        disabled={!live}
        aria-invalid={shownName ? "true" : undefined}
        aria-describedby={shownName ? `${id}-name-hint ${id}-name-error` : `${id}-name-hint`}
        oninput={() => {
          error = null;
          nameError = null;
        }}
        onblur={() => (touched = true)}
      />
      <span id="{id}-name-hint" class="pn-field-hint">
        {#if renamed}
          Home Assistant shows the name as Last speaker: update automations that compare it.
        {:else}
          At most {maxName} characters. It must differ from the other names, and from Everyone else.
        {/if}
      </span>
      {#if shownName}<span id="{id}-name-error" class="pn-field-error" role="alert"><Icon name="alert" />{shownName}</span>{/if}
    </div>
    <InstructionsField
      id="{id}-instructions"
      label="Extra instructions"
      bind:value={instructions}
      max={maxInstructions}
      hint="Added to the prompt when the satellite recognizes this person. Say how to address them, and their pronouns, here."
      disabled={!live}
      oninput={() => (error = null)}
    />
    {#if error}<span class="pn-field-error" role="alert"><Icon name="alert" />{error}</span>{/if}
    <div class="actions">
      <button class="pn-btn pn-btn-primary" type="submit" disabled={!live || people.busy || !!problem || tooLong || unchanged || locked}>
        {person ? "Save" : "Add"}
      </button>
      <button class="pn-btn" type="button" disabled={locked} onclick={() => onclose()}>{person ? "Close" : "Cancel"}</button>
      {#if person}
        <button class="pn-btn pn-btn-quiet remove" type="button" disabled={!live || people.busy || locked} onclick={() => (removing = true)}
          >Remove…</button
        >
      {/if}
    </div>
    {#if locked}<p class="pn-field-hint">Finish or cancel the enrollment first.</p>{/if}
  </div>
</form>

{#if person}
  <div class="pn-row is-stack">
    <div class="form">
      <div class="voice">
        <div class="pn-row-text">
          <span class="pn-row-label">Voice</span>
          <span class="pn-row-hint">{voiceLine(person.voice)}</span>
          {#if close}<span class="pn-row-hint is-caution">{close}</span>{/if}
        </div>
        {#if !enrolling}
          <button
            bind:this={enrollButton}
            class="pn-btn"
            type="button"
            disabled={!live}
            onclick={() => (enrolling = true)}
          >
            {person.voice === VOICE.none ? "Enroll voice" : "Enroll again"}
          </button>
        {/if}
      </div>
      {#if enrolling}
        <Enroll {person} again={person.voice !== VOICE.none} {blocked} onclose={() => (enrolling = false)} />
      {/if}
    </div>
  </div>

  <Confirm bind:open={removing} title="Remove {person.name}?" action="Remove" danger confirm={remove}>
    <p>{removeNote(person.voice)}</p>
  </Confirm>
{/if}

<style>
  .form {
    display: flex;
    flex-direction: column;
    gap: var(--space-4);
  }
  .voice {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-2) var(--space-4);
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
