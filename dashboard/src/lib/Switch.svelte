<!-- An on/off setting that takes effect at once (ARIA switch). `checked` is the
     satellite's state: it changes when the reply or the stream says so. -->
<script lang="ts">
  let {
    id,
    label,
    checked,
    hint,
    disabled = false,
    toggle,
  }: {
    id: string;
    label: string;
    checked: boolean;
    hint?: string;
    disabled?: boolean;
    toggle: (on: boolean) => unknown;
  } = $props();
</script>

<div class="pn-switch">
  <button
    {id}
    class="pn-switch-control"
    role="switch"
    aria-checked={checked}
    aria-labelledby="{id}-label"
    aria-describedby={hint ? `${id}-hint` : undefined}
    {disabled}
    onclick={() => toggle(!checked)}
  ></button>
  <span class="pn-switch-text">
    <label id="{id}-label" class="pn-switch-label" for={id}>{label}</label>
    {#if hint}<span id="{id}-hint" class="pn-switch-hint">{hint}</span>{/if}
  </span>
</div>
