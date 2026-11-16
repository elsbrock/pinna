<!-- A colour as a settings row: the label (with a hint) on the left, a swatch
     that opens the browser's colour picker on the right. `value` is the
     device's colour as #rrggbb; `choose` runs once the picker closes. -->
<script lang="ts">
  let {
    id,
    label,
    value,
    hint,
    disabled = false,
    choose,
  }: {
    id: string;
    label: string;
    value: string;
    hint?: string;
    disabled?: boolean;
    choose: (hex: string) => unknown;
  } = $props();
</script>

<div class="pn-row">
  <div class="pn-row-text">
    <label id="{id}-label" class="pn-row-label" for={id}>{label}</label>
    {#if hint}<span id="{id}-hint" class="pn-row-hint">{hint}</span>{/if}
  </div>
  <input
    {id}
    type="color"
    class="pn-swatch"
    {disabled}
    {value}
    aria-describedby={hint ? `${id}-hint` : undefined}
    onchange={(e) => choose(e.currentTarget.value)}
  />
</div>
