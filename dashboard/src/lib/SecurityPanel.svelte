<!-- The dashboard password and the tokens for machines (ADR 0029), on the Password
     and tokens page: set, change and remove the password, sign out, and create the metrics
     and agent tokens. Anything that changes the password ends every session, so
     a new password signs this browser in again. -->
<script lang="ts">
  import { onMount } from "svelte";

  import Confirm from "./Confirm.svelte";
  import ConfirmAtSatellite from "./ConfirmAtSatellite.svelte";
  import { clock } from "./clock.svelte.ts";
  import { copyText } from "./copy.ts";
  import { device } from "./device.svelte.ts";
  import Group from "./Group.svelte";
  import Icon from "./Icon.svelte";
  import PasswordField from "./PasswordField.svelte";
  import { security } from "./security.svelte.ts";
  import SetPassword from "./SetPassword.svelte";
  import { toasts } from "./toast.svelte.ts";
  import { failuresText, passwordNote, passwordProblem, secondsUntil, splitRetry, tokenShown } from "./view.ts";

  const REFRESH_MS = 10_000;

  const sec = $derived(security.state);
  const live = $derived(device.link === "live");

  type Panel = "set" | "change" | "change-grant" | "remove" | "remove-grant";
  let panel = $state<Panel | null>(null);
  let current = $state("");
  let next = $state("");
  let currentError = $state<string | null>(null);
  let nextError = $state<string | null>(null);
  // A wrong current password counts as a failed sign-in, and the satellite slows those down.
  let retryAt = $state(0);
  const retryIn = $derived(secondsUntil(retryAt, clock.now));
  let busy = $state(false);
  let signingOutEverywhere = $state(false);
  const newNote = $derived(passwordNote(next));

  // The failed sign-ins row follows the satellite without a reload.
  onMount(() => {
    void security.refresh();
    const timer = setInterval(() => void security.refresh(), REFRESH_MS);
    return () => clearInterval(timer);
  });
  $effect(() => clock.use());

  function open(p: Panel | null) {
    panel = p;
    current = next = "";
    currentError = nextError = null;
    retryAt = 0;
  }

  /** The satellite's sentence about the current password: its wait counts down here, once. */
  function refuse(sentence: string) {
    const { text, wait } = splitRetry(sentence);
    currentError = text;
    clock.now = Date.now();
    retryAt = clock.now + wait * 1000;
  }

  function closeOnEscape(e: KeyboardEvent) {
    const inline = panel === "change" || panel === "remove";
    if (e.key === "Escape" && inline && !busy && !replacingOpen && !signingOutEverywhere) open(null);
  }

  async function change() {
    nextError = passwordProblem(next);
    if (nextError) return;
    busy = true;
    const failed = await security.setPassword({ newPassword: next, currentPassword: current });
    if (failed) refuse(failed);
    else {
      const outcome = await security.signIn(next, true, "changed");
      if (!outcome.ok) nextError = `The password is changed, but signing in failed. ${outcome.message}`;
    }
    busy = false;
  }

  async function remove(req: { currentPassword?: string; grant?: string }) {
    busy = true;
    const failed = await security.removePassword(req);
    busy = false;
    if (failed) {
      if (panel === "remove-grant") toasts.show(failed, "danger");
      else refuse(failed);
      return;
    }
    open(null);
    toasts.show("Password removed");
  }

  async function signOut() {
    const failed = await security.signOut();
    if (failed) toasts.show(failed, "danger");
  }

  async function signOutEverywhere() {
    const failed = await security.signOutEverywhere();
    if (failed) return toasts.show(failed, "danger");
    location.reload();
  }

  // A token shows in full once, right after it is made.
  const KINDS = [
    {
      kind: "metrics",
      label: "Metrics token",
      hint: "Prometheus sends it as its authorization credentials, to read /metrics.",
    },
    {
      kind: "agent",
      label: "Agent token",
      hint: "An MCP client sends it in an Authorization: Bearer header, to use /mcp.",
    },
  ] as const;
  // Each stays until it is dismissed, so making the second doesn't hide the first.
  type Kind = "metrics" | "agent";
  let fresh = $state<Partial<Record<Kind, { token: string; replaced: boolean }>>>({});
  let tokenError = $state<{ kind: Kind; text: string } | null>(null);
  let replacing = $state<(typeof KINDS)[number] | null>(null);
  let replacingOpen = $state(false);

  async function create(kind: Kind, replaced: boolean) {
    busy = true;
    tokenError = null;
    const r = await security.createToken(kind);
    busy = false;
    if ("error" in r) tokenError = { kind, text: r.error };
    else fresh[kind] = { token: r.token, replaced };
  }

  /** A token that exists is replaced only after the owner says so: whatever uses it stops working. */
  function ask(k: (typeof KINDS)[number], exists: boolean) {
    if (!exists) return void create(k.kind, false);
    replacing = k;
    replacingOpen = true;
  }

  async function copy(token: string) {
    await copyText(token);
    toasts.show("Copied");
  }
</script>

<svelte:window onkeydown={closeOnEscape} />

{#if sec}
  <Group title="Dashboard password">
    {#snippet foot()}
      The password crosses your network unencrypted, so don't use one you use anywhere else. Home Assistant has its
      own key, and the password doesn't cover it.
    {/snippet}
    <div class="pn-row is-actions">
      <div class="pn-row-text">
        <span class="pn-row-label">Password</span>
        <span class="pn-row-hint">
          {#if sec.passwordSet}
            Set. This dashboard asks for it.
          {:else}
            Not set. Anyone on your network can control this satellite.
          {/if}
        </span>
      </div>
      {#if !sec.passwordSet}
        <button class="pn-btn pn-btn-primary" disabled={!live || panel === "set"} onclick={() => open("set")}>
          Set a password…
        </button>
      {:else}
        <div class="actions">
          <button class="pn-btn" disabled={!live || !!panel} onclick={() => open("change")}>Change…</button>
          <button class="pn-btn" disabled={!live || !!panel} onclick={() => open("remove")}>Remove…</button>
        </div>
      {/if}
    </div>

    {#if panel === "set" || panel === "change-grant"}
      <div class="pn-row is-stack">
        <SetPassword id="system" oncancel={() => open(null)} />
      </div>
    {:else if panel === "change"}
      <form
        class="pn-row is-stack"
        onsubmit={(e) => {
          e.preventDefault();
          void change();
        }}
      >
        <PasswordField
          id="current-password"
          label="Current password"
          bind:value={current}
          autocomplete="current-password"
          error={currentError}
          disabled={busy}
          autofocus
        />
        <PasswordField
          id="new-password"
          label="New password"
          bind:value={next}
          autocomplete="new-password"
          hint="8 to 128 characters. Changing it signs every browser out, this one in again."
          note={newNote}
          error={nextError}
          disabled={busy}
        />
        {#if retryIn > 0}<p class="muted" role="status">Try again in {retryIn} s.</p>{/if}
        <div class="actions">
          <button class="pn-btn pn-btn-primary" type="submit" disabled={busy || !current || !next || retryIn > 0}>
            Change password
          </button>
          <button class="pn-btn" type="button" disabled={busy} onclick={() => open("change-grant")}>
            Confirm at the satellite instead
          </button>
          <button class="pn-btn pn-btn-quiet" type="button" disabled={busy} onclick={() => open(null)}>Cancel</button>
        </div>
      </form>
    {:else if panel === "remove"}
      <form
        class="pn-row is-stack"
        onsubmit={(e) => {
          e.preventDefault();
          void remove({ currentPassword: current });
        }}
      >
        <p>
          Without a password, anyone on your network can control this satellite, and <code class="pn-code">/mcp</code>
          and <code class="pn-code">/metrics</code> need no token. Sign-ins stop, and every browser signs out.
        </p>
        <PasswordField
          id="remove-password"
          label="Current password"
          bind:value={current}
          autocomplete="current-password"
          error={currentError}
          disabled={busy}
          autofocus
        />
        {#if retryIn > 0}<p class="muted" role="status">Try again in {retryIn} s.</p>{/if}
        <div class="actions">
          <button class="pn-btn pn-btn-danger" type="submit" disabled={busy || !current || retryIn > 0}>
            Remove password
          </button>
          <button class="pn-btn" type="button" disabled={busy} onclick={() => open("remove-grant")}>
            Confirm at the satellite instead
          </button>
          <button class="pn-btn pn-btn-quiet" type="button" disabled={busy} onclick={() => open(null)}>Cancel</button>
        </div>
      </form>
    {:else if panel === "remove-grant"}
      <div class="pn-row is-stack">
        <p>Removing the password opens this dashboard to anyone on your network.</p>
        <ConfirmAtSatellite ongrant={(grant) => remove({ grant })} oncancel={() => open(null)} />
      </div>
    {/if}

    {#if sec.passwordSet}
      <div class="pn-row">
        <div class="pn-row-text">
          <span class="pn-row-label">Failed sign-ins</span>
          <span class="pn-row-hint">{failuresText(sec, device.now)}</span>
        </div>
      </div>
      <div class="pn-row">
        <div class="pn-row-text">
          <span class="pn-row-label">Sign out</span>
          <span class="pn-row-hint">This browser only. Your session lasts 30 days otherwise.</span>
        </div>
        <button class="pn-btn" onclick={signOut}>Sign out</button>
      </div>
      <div class="pn-row is-actions">
        <div class="pn-row-text">
          <span class="pn-row-label">Sign out everywhere</span>
          <span class="pn-row-hint">Ends every session, in every browser. Tokens keep working.</span>
        </div>
        <button class="pn-btn" disabled={!live} onclick={() => (signingOutEverywhere = true)}>
          Sign out everywhere…
        </button>
      </div>
    {/if}
  </Group>

  <Group title="Tokens">
    {#snippet foot()}
      With a password, <code class="pn-code">/metrics</code> and <code class="pn-code">/mcp</code> need a token in an
      <code class="pn-code">Authorization: Bearer</code> header. A new token replaces the old one, which stops working
      at once.
    {/snippet}
    {#each KINDS as k (k.kind)}
      {@const last4 = k.kind === "metrics" ? sec.metricsTokenLast4 : sec.agentTokenLast4}
      <div class="pn-row is-actions">
        <div class="pn-row-text">
          <span class="pn-row-label">{k.label}</span>
          <span class="pn-row-hint">{k.hint}</span>
        </div>
        <span class="pn-row-value is-mono">{tokenShown(last4)}</span>
        <button class="pn-btn" disabled={!live || busy} onclick={() => ask(k, !!last4)}>
          {last4 ? "Create a new token" : "Create a token"}
        </button>
      </div>
      {#if fresh[k.kind]}
        {@const made = fresh[k.kind]!}
        <div class="pn-row is-stack">
          <div class="pn-field">
            <span class="pn-field-label">New {k.label.toLowerCase()}</span>
            <div class="pn-secret">
              <output class="pn-secret-shown">{made.token}</output>
              <button class="pn-btn pn-btn-sm" onclick={() => copy(made.token)}>
                <Icon name="copy" />Copy
              </button>
              <button class="pn-btn pn-btn-quiet pn-btn-sm" onclick={() => (fresh[k.kind] = undefined)}>
                Dismiss
              </button>
            </div>
            <span class="pn-field-hint">
              This is the only time it shows. Copy it now.{made.replaced
                ? " It replaces the old token, which no longer works."
                : ""}
            </span>
          </div>
        </div>
      {/if}
      {#if tokenError?.kind === k.kind}
        <div class="pn-row">
          <span class="pn-field-error" role="alert"><Icon name="alert" />{tokenError.text}</span>
        </div>
      {/if}
    {/each}
  </Group>

  <Confirm
    bind:open={replacingOpen}
    title="Replace the {replacing?.label.toLowerCase() ?? 'token'}?"
    action="Replace token"
    danger
    confirm={() => replacing && create(replacing.kind, true)}
  >
    <p>The old token stops working at once. Whatever uses it needs the new one.</p>
  </Confirm>

  <Confirm
    bind:open={signingOutEverywhere}
    title="Sign out everywhere?"
    action="Sign out everywhere"
    confirm={signOutEverywhere}
  >
    <p>
      Every browser signs out, this one too, and has to sign in again with the password. Tokens for agents and
      Prometheus keep working.
    </p>
  </Confirm>
{/if}

<style>
  .actions {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-2) var(--space-3);
  }
  form.pn-row,
  .pn-row.is-stack {
    display: flex;
    flex-direction: column;
    align-items: stretch;
    gap: var(--space-3);
  }
  p {
    margin: 0;
  }
  .muted {
    color: var(--ink-muted);
  }
</style>
