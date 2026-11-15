<!-- A settings row with a switch on the right (ARIA switch). `checked` is the
     satellite's state: it changes when the reply or the stream says so. -->
<script lang="ts">
  let {
    id,
    label,
    checked,
    hint,
    disabled = false,
    busy = false,
    toggle,
  }: {
    id: string;
    label: string;
    checked: boolean;
    hint?: string;
    disabled?: boolean;
    /** A command is in flight: ignore presses but keep focus (disabled would drop it). */
    busy?: boolean;
    toggle: (on: boolean) => unknown;
  } = $props();
</script>

<div class="pn-row">
  <div class="pn-row-text">
    <label id="{id}-label" class="pn-row-label" for={id}>{label}</label>
    {#if hint}<span id="{id}-hint" class="pn-row-hint">{hint}</span>{/if}
  </div>
  <button
    {id}
    class="pn-switch-control"
    role="switch"
    aria-checked={checked}
    aria-labelledby="{id}-label"
    aria-describedby={hint ? `${id}-hint` : undefined}
    {disabled}
    aria-disabled={busy || undefined}
    onclick={() => {
      if (!busy) toggle(!checked);
    }}
  ></button>
</div>
