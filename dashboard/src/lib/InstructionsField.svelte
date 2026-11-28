<!-- A multi-line field for a profile's extra instructions (ADR 0038), with a
     counter against the satellite's limit and the refusal's sentence. -->
<script lang="ts">
  import { chars } from "./assistant.ts";
  import Icon from "./Icon.svelte";
  import { countOf } from "./people.ts";

  let {
    id,
    label,
    value = $bindable(""),
    max,
    hint,
    error = null,
    disabled = false,
    oninput,
  }: {
    id: string;
    label: string;
    value?: string;
    max: number;
    hint: string;
    error?: string | null;
    disabled?: boolean;
    oninput?: () => void;
  } = $props();

  const tooLong = $derived(max > 0 && chars(value) > max);
</script>

<div class="pn-field" data-invalid={error || tooLong ? "" : undefined}>
  <div class="label-line">
    <label class="pn-field-label" for={id}>{label}</label>
    <span id="{id}-count" class="count" class:is-over={tooLong}>{countOf(value, max)}</span>
  </div>
  <textarea
    {id}
    class="pn-input text"
    rows="4"
    spellcheck="true"
    bind:value
    {disabled}
    aria-describedby="{id}-count {id}-hint"
    aria-invalid={error || tooLong ? "true" : undefined}
    {oninput}
  ></textarea>
  {#if error}<span class="pn-field-error" role="alert"><Icon name="alert" />{error}</span>{/if}
  <span id="{id}-hint" class="pn-field-hint">
    {#if tooLong}Too long to save: at most {max.toLocaleString("en")} characters.{:else}{hint}{/if}
  </span>
</div>

<style>
  .label-line {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    justify-content: space-between;
    gap: var(--space-1) var(--space-3);
  }
  .count {
    font-family: var(--font-mono);
    font-size: 13px;
    line-height: 18px;
    color: var(--ink-muted);
    font-variant-numeric: tabular-nums;
  }
  .count.is-over {
    color: var(--danger);
    font-weight: 650;
  }
  .text {
    height: auto;
    min-height: 6em;
    padding: var(--space-2) var(--space-3);
    line-height: 22px;
    resize: vertical;
  }
</style>
