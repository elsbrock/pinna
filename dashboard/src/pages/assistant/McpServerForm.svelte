<!-- Add one of the owner's MCP servers, or change one: its name, its
     address and how it signs in. The address may carry a token, so it is
     write-only: once stored, only its host shows. A sign-in is write-only
     too: once stored, only what tells it apart shows (ADR 0017). -->
<script lang="ts">
  import { tick, untrack } from "svelte";

  import type { McpServer } from "../../gen/pinna/v1/assistant_pb.ts";
  import { assistant } from "../../lib/assistant.svelte.ts";
  import {
    mcpName,
    SIGN_INS,
    type SecretEdit,
    type SignIn,
    signIn,
    signInShown,
    typedCredential,
  } from "../../lib/assistant.ts";
  import Confirm from "../../lib/Confirm.svelte";
  import { device } from "../../lib/device.svelte.ts";
  import Dropdown from "../../lib/Dropdown.svelte";
  import Icon from "../../lib/Icon.svelte";
  import SecretField from "../../lib/SecretField.svelte";

  let { server, onclose }: { server?: McpServer; onclose: () => void } = $props();

  const live = $derived(device.link === "live");
  const id = $derived(server ? `mcp-${server.id}` : "mcp-new");

  // The form starts from the server as it was when it opened.
  const start = untrack(() => server);
  let name = $state(start?.name ?? "");
  let url = $state<SecretEdit>({ kind: "keep" });
  let error = $state<string | null>(null);
  let removing = $state(false);
  // What the typed name is saved as: "Home Assistant" becomes home-assistant.
  const saved = $derived(mcpName(name));

  // The sign-in: the stored one is kept until another is chosen, Replace is
  // pressed, or None removes it.
  const stored = start?.credential;
  let kind = $state<SignIn>(signIn(stored));
  let replacing = $state(false);
  let typed = $state({ token: "", user: "", password: "", name: "", value: "" });
  let first = $state<HTMLInputElement>();
  const keeping = $derived(!!stored && kind === signIn(stored) && !replacing);
  const credential = $derived(keeping ? undefined : typedCredential(kind, typed));
  const removingSignIn = $derived(!!stored && kind === "none");
  const typedUrl = $derived(url.kind === "set" ? url.value.trim() : "");
  const signInHint = $derived(
    [
      removingSignIn
        ? "The stored sign-in is removed when you save."
        : "Sent with every request to the server. The satellite keeps it and never shows it again.",
      keeping && typedUrl ? "A new address removes the stored sign-in: choose Replace to enter it again." : "",
      kind !== "none" && typedUrl.toLowerCase().startsWith("http://")
        ? "Over http, it crosses the network unencrypted."
        : "",
      "Servers that sign in through a browser (OAuth) can't be added yet.",
    ]
      .filter(Boolean)
      .join(" "),
  );
  const ready = $derived(
    !!saved && (!!server || url.kind === "set") && (kind === "none" || keeping || !!credential),
  );

  function chooseSignIn(id: string) {
    kind = id as SignIn;
    replacing = false;
    typed = { token: "", user: "", password: "", name: "", value: "" };
    error = null;
  }

  async function replace() {
    replacing = true;
    await tick();
    first?.focus();
  }

  async function save() {
    error = await assistant.setMcpServer(
      {
        id: server?.id ?? "",
        revision: server?.revision ?? 0n,
        name: saved,
        url: url.kind === "set" ? url.value.trim() : undefined,
        enabled: server?.enabled ?? true,
        credential,
        clearCredential: removingSignIn,
      },
      server ? "Saved" : "MCP server added",
    );
    if (!error) onclose();
  }

  async function remove() {
    if (!server) return;
    error = await assistant.deleteMcpServer(server.id, server.revision);
    if (!error) onclose();
  }
</script>

<form
  class="pn-row is-stack"
  aria-label={server ? `Change ${server.name}` : "Add an MCP server"}
  onsubmit={(e) => {
    e.preventDefault();
    if (ready && !assistant.busy) void save();
  }}
>
  <div class="form">
    <div class="pn-field">
      <label class="pn-field-label" for="{id}-name">Name</label>
      <input
        id="{id}-name"
        class="pn-input is-mono"
        autocomplete="off"
        autocapitalize="off"
        spellcheck="false"
        maxlength="24"
        placeholder="home"
        bind:value={name}
        disabled={!live}
        aria-describedby="{id}-name-hint"
        oninput={() => (error = null)}
      />
      <span id="{id}-name-hint" class="pn-field-hint">
        {#if saved && saved !== name.trim()}
          Saved as {saved}.
        {/if}
        Lowercase letters, digits and hyphens, up to 24. The brain sees each of its tools under this name.
      </span>
    </div>
    <SecretField
      id="{id}-url"
      label="Address"
      stored={!!server}
      shown="set, on {server?.host ?? ''}"
      bind:edit={url}
      removable={false}
      plain
      placeholder="https://example.com/mcp"
      hint="The whole address, with any token in it. The satellite keeps it and shows only its host."
      disabled={!live}
    />
    <div class="pn-field">
      <span id="{id}-signin-label" class="pn-field-label">Sign-in</span>
      <Dropdown
        id="{id}-signin"
        value={kind}
        options={SIGN_INS}
        choose={chooseSignIn}
        disabled={!live}
        labelledby="{id}-signin-label"
        describedby="{id}-signin-hint"
      />
      <span id="{id}-signin-hint" class="pn-field-hint">{signInHint}</span>
    </div>
    {#if keeping && stored}
      <div class="pn-field">
        <span id="{id}-stored-label" class="pn-field-label">
          {SIGN_INS.find((s) => s.id === kind)?.label}
        </span>
        <div class="pn-secret">
          <output class="pn-secret-shown" aria-labelledby="{id}-stored-label">{signInShown(stored)}</output>
          <button class="pn-btn pn-btn-sm" type="button" disabled={!live} onclick={replace}>Replace</button>
        </div>
      </div>
    {:else if kind !== "none"}
      {#if kind === "bearer"}
        <div class="pn-field">
          <label class="pn-field-label" for="{id}-token">Token</label>
          <input
            bind:this={first}
            id="{id}-token"
            class="pn-input is-mono"
            type="password"
            autocomplete="off"
            spellcheck="false"
            bind:value={typed.token}
            disabled={!live}
            oninput={() => (error = null)}
          />
        </div>
      {:else if kind === "basic"}
        <div class="pn-field">
          <label class="pn-field-label" for="{id}-user">User name</label>
          <input
            bind:this={first}
            id="{id}-user"
            class="pn-input"
            autocomplete="off"
            autocapitalize="off"
            spellcheck="false"
            bind:value={typed.user}
            disabled={!live}
            oninput={() => (error = null)}
          />
        </div>
        <div class="pn-field">
          <label class="pn-field-label" for="{id}-password">Password</label>
          <input
            id="{id}-password"
            class="pn-input"
            type="password"
            autocomplete="off"
            bind:value={typed.password}
            disabled={!live}
            oninput={() => (error = null)}
          />
        </div>
      {:else}
        <div class="pn-field">
          <label class="pn-field-label" for="{id}-header">Header name</label>
          <input
            bind:this={first}
            id="{id}-header"
            class="pn-input is-mono"
            autocomplete="off"
            autocapitalize="off"
            spellcheck="false"
            placeholder="X-API-Key"
            bind:value={typed.name}
            disabled={!live}
            oninput={() => (error = null)}
          />
        </div>
        <div class="pn-field">
          <label class="pn-field-label" for="{id}-value">Header value</label>
          <input
            id="{id}-value"
            class="pn-input is-mono"
            type="password"
            autocomplete="off"
            spellcheck="false"
            bind:value={typed.value}
            disabled={!live}
            oninput={() => (error = null)}
          />
        </div>
      {/if}
      {#if replacing && stored && kind === signIn(stored)}
        <div>
          <button class="pn-btn pn-btn-quiet pn-btn-sm" type="button" disabled={!live} onclick={() => (replacing = false)}
            >Keep the stored one</button
          >
        </div>
      {/if}
    {/if}
    {#if error}<span class="pn-field-error" role="alert"><Icon name="alert" />{error}</span>{/if}
    <div class="actions">
      <button class="pn-btn pn-btn-primary" type="submit" disabled={!live || assistant.busy || !ready}>
        {server ? "Save" : "Add"}
      </button>
      <button class="pn-btn" type="button" onclick={onclose}>Cancel</button>
      {#if server}
        <button
          class="pn-btn pn-btn-quiet remove"
          type="button"
          disabled={!live || assistant.busy}
          onclick={() => (removing = true)}>Remove server…</button
        >
      {/if}
    </div>
  </div>
</form>

{#if server}
  <Confirm bind:open={removing} title="Remove {server.name}?" action="Remove" danger confirm={remove}>
    <p>The brain loses its tools, and the satellite forgets its address.</p>
  </Confirm>
{/if}

<style>
  .form {
    display: flex;
    flex-direction: column;
    gap: var(--space-4);
  }
  .actions {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-2) var(--space-3);
  }
  .remove {
    margin-left: auto;
    color: var(--danger);
  }
</style>
