<!-- A 0–100 slider with a large readout. While held, it shows its own value;
     it commits once, on release, and the satellite's state takes over again. -->
<script lang="ts">
  let {
    label,
    value,
    hint,
    disabled = false,
    commit,
    preview,
  }: {
    label: string;
    value: number;
    hint?: string;
    disabled?: boolean;
    commit: (v: number) => Promise<unknown>;
    /** The value while it is held, then null once it is committed. */
    preview?: (v: number | null) => void;
  } = $props();

  let editing = $state<number | null>(null);
  const shown = $derived(editing ?? value);
  // The label holds the <output> too, which is labelable: name the input.
  const id = `sl-${Math.random().toString(36).slice(2, 8)}`;

  $effect(() => preview?.(editing));
  let timer: ReturnType<typeof setTimeout> | undefined;

  // Arrow keys fire a change per press: send the value once they settle.
  function release() {
    clearTimeout(timer);
    timer = setTimeout(async () => {
      if (editing === null) return;
      await commit(editing);
      editing = null;
    }, 350);
  }
</script>

<label class="pn-slider" for={id}>
  <span class="pn-slider-label">{label}</span>
  <output class="pn-slider-value" aria-hidden="true">{shown}</output>
  <input
    {id}
    aria-describedby={hint ? `${id}-hint` : undefined}
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
  {#if hint}<span id="{id}-hint" class="pn-slider-hint">{hint}</span>{/if}
</label>
