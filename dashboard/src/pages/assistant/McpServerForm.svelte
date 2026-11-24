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

  const KIND_HINTS: Record<SignIn, string> = {
    none: "Servers that sign in through a browser (OAuth) don't work yet; if the service gives you an access token, choose Token.",
    bearer: "Sent as Authorization: Bearer; most API keys go here. The satellite shows only the end of a long token.",
    basic: "Sent as HTTP Basic authentication. The satellite shows only the user name.",
    header:
      "For a server that names its own header, such as X-API-Key. The satellite shows only the header's name and the end of a long value.",
  };

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

  // The sign-in. The stored one is sent until another kind is chosen,
  // Replace is pressed or None removes it; a new address drops it, so the
  // owner enters it again or chooses None before saving.
  const stored = start?.credential;
  const storedKind = signIn(stored);
  const EMPTY = { token: "", user: "", password: "", name: "", value: "" };
  let kind = $state<SignIn>(storedKind);
  let replacing = $state(false);
  let typed = $state({ ...EMPTY });
  let first = $state<HTMLInputElement>();
  // The password or a header's value: what is left to type after Replace.
  let secret = $state<HTMLInputElement>();
  let replaceButton = $state<HTMLButtonElement>();
  const typedUrl = $derived(url.kind === "set" ? url.value.trim() : "");
  const untouched = $derived(!!stored && kind === storedKind && !replacing);
  const keeping = $derived(untouched && !typedUrl);
  const dropped = $derived(untouched && !!typedUrl);
  const credential = $derived(untouched ? undefined : typedCredential(kind, typed));
  const removingSignIn = $derived(!!stored && kind === "none");
  // Where the sign-in goes: the address typed now, or else the stored one.
  const plainHttp = $derived(typedUrl ? typedUrl.toLowerCase().startsWith("http://") : !!start?.plainHttp);
  const sending = $derived(kind !== "none" && !dropped);
  // What is left to type, named field by field.
  const missing = $derived.by(() => {
    if (kind === "none" || untouched || credential) return "";
    if (kind === "bearer") return "Enter the token.";
    const [named, secretTyped, what, other, both] =
      kind === "basic"
        ? [typed.user.trim(), typed.password, "user name", "password", "user name and password"]
        : [typed.name.trim(), typed.value.trim(), "header's name", "header's value", "header's name and value"];
    if (!named) return `Enter the ${secretTyped ? what : both}.`;
    return `Enter the ${other}.`;
  });
  const signInHint = $derived(
    dropped
      ? "A new address removes the stored sign-in: select Replace to enter it again, choose None, or select Keep the stored one next to the address."
      : [
          sending && plainHttp
            ? "Over http, the sign-in crosses the network unencrypted: use an https address if the server has one."
            : "",
          KIND_HINTS[kind],
        ]
          .filter(Boolean)
          .join(" "),
  );
  const describedBy = $derived(missing ? `${id}-signin-hint ${id}-missing` : `${id}-signin-hint`);
  const ready = $derived(
    !!saved && (!!server || url.kind === "set") && (kind === "none" || keeping || !!credential),
  );

  // A changed address may answer differently.
  $effect(() => {
    void url;
    untrack(() => (error = null));
  });

  function chooseSignIn(id: string) {
    kind = id as SignIn;
    replacing = false;
    error = null;
  }

  async function replace() {
    // Names aren't secret and are shown already: only the secret is typed again.
    let named = false;
    if (stored && storedKind === "basic" && !typed.user) [typed.user, named] = [stored.label, true];
    if (stored && storedKind === "header" && !typed.name) [typed.name, named] = [stored.label, true];
    replacing = true;
    await tick();
    (named ? secret : first)?.focus();
  }

  async function keepStored() {
    kind = storedKind;
    replacing = false;
    typed = { ...EMPTY };
    error = null;
    await tick();
    replaceButton?.focus();
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
      hint="The whole address, with any token in it. The satellite keeps it and shows only its host.{stored
        ? ' A new address removes the stored sign-in.'
        : ''}"
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
    {#if stored}
      <div class="pn-field">
        <span id="{id}-stored-label" class="pn-field-label">Stored sign-in</span>
        <div class="pn-secret">
          <output
            class="pn-secret-shown"
            class:is-removed={dropped || kind === "none"}
            aria-labelledby="{id}-stored-label"
          >
            {#if dropped}
              Removed with the old address when you save
            {:else if kind === "none"}
              Removed when you save
            {:else}
              {SIGN_INS.find((s) => s.id === storedKind)?.label}: {signInShown(stored)}{keeping
                ? ""
                : ". Replaced when you save."}
            {/if}
          </output>
          {#if untouched}
            <button
              bind:this={replaceButton}
              class="pn-btn pn-btn-sm"
              type="button"
              aria-label="Replace the sign-in"
              disabled={!live}
              onclick={replace}>Replace</button
            >
          {:else}
            <button
              class="pn-btn pn-btn-quiet pn-btn-sm"
              type="button"
              aria-label={typedUrl ? "Undo the sign-in change" : undefined}
              disabled={!live}
              onclick={keepStored}>{typedUrl ? "Undo" : "Keep the stored sign-in"}</button
            >
          {/if}
        </div>
      </div>
    {/if}
    {#if kind !== "none" && !untouched}
      {#if kind === "bearer"}
        <div class="pn-field">
          <label class="pn-field-label" for="{id}-token">Token</label>
          <input
            bind:this={first}
            id="{id}-token"
            class="pn-input is-mono"
            type="password"
            autocomplete="new-password"
            spellcheck="false"
            bind:value={typed.token}
            disabled={!live}
            aria-describedby={describedBy}
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
            aria-describedby={describedBy}
            oninput={() => (error = null)}
          />
        </div>
        <div class="pn-field">
          <label class="pn-field-label" for="{id}-password">Password</label>
          <input
            bind:this={secret}
            id="{id}-password"
            class="pn-input"
            type="password"
            autocomplete="new-password"
            bind:value={typed.password}
            disabled={!live}
            aria-describedby={describedBy}
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
            aria-describedby={describedBy}
            oninput={() => (error = null)}
          />
        </div>
        <div class="pn-field">
          <label class="pn-field-label" for="{id}-value">Header value</label>
          <input
            bind:this={secret}
            id="{id}-value"
            class="pn-input is-mono"
            type="password"
            autocomplete="new-password"
            spellcheck="false"
            bind:value={typed.value}
            disabled={!live}
            aria-describedby={describedBy}
            oninput={() => (error = null)}
          />
        </div>
      {/if}
      {#if missing}<span id="{id}-missing" class="pn-field-hint missing">{missing}</span>{/if}
    {/if}
    {#if server?.problem}
      <span class="pn-field-hint">When last asked: {server.problem}</span>
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
  /* Belongs to the fields above it, not to the form's next row. */
  .missing {
    margin-top: calc(-1 * var(--space-3));
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
