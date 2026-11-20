<!-- A password field with Show and Hide. The value is bindable; what the
     satellite makes of it shows in `error`. -->
<script lang="ts">
  import Icon from "./Icon.svelte";

  let {
    id,
    label,
    value = $bindable(""),
    autocomplete,
    hint,
    error,
    disabled = false,
    autofocus = false,
  }: {
    id: string;
    label: string;
    value?: string;
    /** "current-password" or "new-password": password managers follow it. */
    autocomplete: "current-password" | "new-password";
    hint?: string;
    error?: string | null;
    disabled?: boolean;
    autofocus?: boolean;
  } = $props();

  let shown = $state(false);
  const describedBy = $derived([hint && `${id}-hint`, error && `${id}-error`].filter(Boolean).join(" ") || undefined);

  function focus(node: HTMLInputElement) {
    if (autofocus) node.focus();
  }
</script>

<div class="pn-field" data-invalid={error ? "" : undefined}>
  <label class="pn-field-label" for={id}>{label}</label>
  <div class="pn-secret">
    <input
      {id}
      class="pn-input"
      type={shown ? "text" : "password"}
      {autocomplete}
      autocapitalize="off"
      spellcheck="false"
      {disabled}
      bind:value
      use:focus
      aria-describedby={describedBy}
      aria-invalid={error ? "true" : undefined}
    />
    <button
      class="pn-btn pn-btn-quiet pn-btn-sm"
      type="button"
      {disabled}
      aria-pressed={shown}
      aria-controls={id}
      onclick={() => (shown = !shown)}
    >
      {shown ? "Hide" : "Show"}
    </button>
  </div>
  {#if error}<span id="{id}-error" class="pn-field-error" role="alert"><Icon name="alert" />{error}</span>{/if}
  {#if hint}<span id="{id}-hint" class="pn-field-hint">{hint}</span>{/if}
</div>
