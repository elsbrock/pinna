<!-- A 0–100 slider with a large readout. While held, it shows its own value;
     it commits once, on release, and the satellite's state takes over again. -->
<script lang="ts">
  let {
    label,
    value,
    hint,
    disabled = false,
    commit,
  }: {
    label: string;
    value: number;
    hint?: string;
    disabled?: boolean;
    commit: (v: number) => Promise<unknown>;
  } = $props();

  let editing = $state<number | null>(null);
  const shown = $derived(editing ?? value);

  async function release() {
    if (editing === null) return;
    await commit(editing);
    editing = null;
  }
</script>

<label class="pn-slider">
  <span class="pn-slider-label">{label}</span>
  <output class="pn-slider-value" aria-hidden="true">{shown}</output>
  <input
    type="range"
    min="0"
    max="100"
    step="1"
    value={shown}
    style:--pct="{shown}%"
    {disabled}
    oninput={(e) => (editing = Number(e.currentTarget.value))}
    onchange={release}
  />
  {#if hint}<span class="pn-slider-hint">{hint}</span>{/if}
</label>
