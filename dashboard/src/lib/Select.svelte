<!-- A named choice from a fixed list, as a settings row: the label (with a
     hint, when there is one) on the left, the choice on the right. The page
     owns the options and reacts to a change; `value` is the device's state,
     so the row follows the satellite even when the choice is made elsewhere. -->
<script lang="ts">
  import Icon from "./Icon.svelte";

  let {
    id,
    label,
    value,
    options,
    hint,
    disabled = false,
    choose,
  }: {
    id: string;
    label: string;
    value: string;
    options: { id: string; label: string }[];
    hint?: string;
    disabled?: boolean;
    choose: (id: string) => unknown;
  } = $props();
</script>

<div class="pn-row">
  <div class="pn-row-text">
    <label id="{id}-label" class="pn-row-label" for={id}>{label}</label>
    {#if hint}<span id="{id}-hint" class="pn-row-hint">{hint}</span>{/if}
  </div>
  <span class="pn-select-wrap">
    <select
      {id}
      class="pn-select"
      {disabled}
      value={value}
      aria-labelledby="{id}-label"
      aria-describedby={hint ? `${id}-hint` : undefined}
      onchange={(e) => choose(e.currentTarget.value)}
    >
      {#each options as o (o.id)}
        <option value={o.id}>{o.label}</option>
      {/each}
    </select>
    <Icon name="chevron-down" class="pn-select-chev" />
  </span>
</div>
