<!-- A key or a secret address, write-only (ADR 0017). Once stored it shows as
     set, with what tells it apart (a key's last four characters, a server's
     host), never itself: Replace types a new one, Remove drops it. What is
     typed stays in this field until the form is saved or closed. A key is
     hidden while it is typed; an address shows, so it can be checked. -->
<script lang="ts">
  import { tick } from "svelte";

  import type { SecretEdit } from "./assistant.ts";
  import Icon from "./Icon.svelte";

  let {
    id,
    label,
    stored,
    shown,
    edit = $bindable({ kind: "keep" }),
    removable = true,
    plain = false,
    placeholder,
    hint,
    error,
    disabled = false,
  }: {
    id: string;
    label: string;
    /** The satellite holds one. */
    stored: boolean;
    /** How a stored one shows, such as "set ••••abcd". */
    shown: string;
    edit?: SecretEdit;
    /** Offer Remove: a key may go, an MCP server's address may not. */
    removable?: boolean;
    /** Show what is typed (an address); a key stays hidden. */
    plain?: boolean;
    placeholder?: string;
    hint?: string;
    error?: string | null;
    disabled?: boolean;
  } = $props();

  let replacing = $state(false);
  let input = $state<HTMLInputElement>();
  const typing = $derived(!stored || replacing);
  const describedBy = $derived([hint && `${id}-hint`, error && `${id}-error`].filter(Boolean).join(" ") || undefined);

  async function replace() {
    replacing = true;
    await tick();
    input?.focus();
  }

  function keep() {
    replacing = false;
    edit = { kind: "keep" };
  }
</script>

<div class="pn-field" data-invalid={error ? "" : undefined}>
  <label class="pn-field-label" for={typing ? id : `${id}-shown`}>{label}</label>
  {#if typing}
    <div class="pn-secret">
      <input
        bind:this={input}
        {id}
        class="pn-input is-mono"
        type={plain ? "text" : "password"}
        inputmode={plain ? "url" : undefined}
        autocomplete="off"
        autocapitalize="off"
        spellcheck="false"
        {placeholder}
        {disabled}
        value={edit.kind === "set" ? edit.value : ""}
        aria-describedby={describedBy}
        aria-invalid={error ? "true" : undefined}
        oninput={(e) => {
          const value = e.currentTarget.value;
          edit = value.trim() ? { kind: "set", value } : { kind: "keep" };
        }}
      />
      {#if stored}
        <button class="pn-btn pn-btn-quiet pn-btn-sm" type="button" {disabled} onclick={keep}>Keep the stored one</button>
      {/if}
    </div>
  {:else}
    <div class="pn-secret">
      <output id="{id}-shown" class="pn-secret-shown" class:is-removed={edit.kind === "clear"} aria-describedby={describedBy}>
        {edit.kind === "clear" ? "Removed when you save" : shown}
      </output>
      {#if edit.kind === "clear"}
        <button class="pn-btn pn-btn-quiet pn-btn-sm" type="button" {disabled} onclick={keep}>Undo</button>
      {:else}
        <button class="pn-btn pn-btn-sm" type="button" {disabled} onclick={replace}>Replace</button>
        {#if removable}
          <button class="pn-btn pn-btn-quiet pn-btn-sm" type="button" {disabled} onclick={() => (edit = { kind: "clear" })}>
            Remove
          </button>
        {/if}
      {/if}
    </div>
  {/if}
  {#if error}<span id="{id}-error" class="pn-field-error" role="alert"><Icon name="alert" />{error}</span>{/if}
  {#if hint}<span id="{id}-hint" class="pn-field-hint">{hint}</span>{/if}
</div>
