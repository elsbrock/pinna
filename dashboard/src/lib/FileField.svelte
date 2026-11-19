<!-- A file to upload (pn-file): a button that opens the picker, and the chosen
     file's name and size beside it. The input is hidden but still takes
     focus, so the button shows the focus ring and Enter or Space opens the picker. -->
<script lang="ts">
  import Icon from "./Icon.svelte";
  import { fileSize } from "./view.ts";

  let {
    id,
    label,
    file = $bindable(null),
    accept,
    hint,
    error,
    disabled = false,
  }: {
    id: string;
    label: string;
    file?: File | null;
    /** The picker's filter, such as ".onnx,.tflite". */
    accept?: string;
    hint?: string;
    error?: string | null;
    disabled?: boolean;
  } = $props();

  let input = $state<HTMLInputElement>();
  // The form was cleared: the input keeps its own file until told otherwise.
  $effect(() => {
    if (!file && input) input.value = "";
  });

  const describedBy = $derived([`${id}-name`, hint && `${id}-hint`, error && `${id}-error`].filter(Boolean).join(" "));
</script>

<div class="pn-field" data-invalid={error ? "" : undefined}>
  <span id="{id}-label" class="pn-field-label">{label}</span>
  <div class="pn-file">
    <input
      bind:this={input}
      {id}
      class="pn-file-input"
      type="file"
      {accept}
      {disabled}
      aria-labelledby="{id}-label"
      aria-describedby={describedBy}
      aria-invalid={error ? "true" : undefined}
      onchange={(e) => (file = e.currentTarget.files?.[0] ?? null)}
    />
    <label class="pn-btn" for={id}>{file ? "Choose another file" : "Choose file"}</label>
    <span id="{id}-name" class="pn-file-name" class:is-empty={!file}>
      {#if file}{file.name} <span class="pn-file-size">{fileSize(file.size)}</span>{:else}No file chosen{/if}
    </span>
  </div>
  {#if error}<span id="{id}-error" class="pn-field-error" role="alert"><Icon name="alert" />{error}</span>{/if}
  {#if hint}<span id="{id}-hint" class="pn-field-hint">{hint}</span>{/if}
</div>
