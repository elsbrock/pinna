<!-- Sets the dashboard password after a press of Action at the satellite (ADR
     0029): the first password, or a new one for an owner who forgot theirs.
     Then signs in with it and loads the page afresh. A grant lasts 5 minutes;
     when it lapses, the form asks for the press again and keeps the password
     that was typed. -->
<script lang="ts">
  import { clock } from "./clock.svelte.ts";
  import ConfirmAtSatellite from "./ConfirmAtSatellite.svelte";
  import Icon from "./Icon.svelte";
  import PasswordField from "./PasswordField.svelte";
  import { security } from "./security.svelte.ts";
  import { grantLeft, passwordNote, passwordProblem, tokenNeeds } from "./view.ts";

  let { oncancel, id = "set-password" }: { oncancel: () => void; id?: string } = $props();

  let grant = $state<string | null>(null);
  // When the page got the grant (Date.now()): the satellite's clock for it started just before.
  let grantedAt = $state(0);
  let password = $state("");
  let error = $state<string | null>(null);
  let saving = $state(false);
  // Only a first password cuts off clients that have none; a new one after a
  // forgotten password leaves the tokens as they are.
  const needs = $derived(security.state?.passwordSet ? null : tokenNeeds(security.state));
  const lapsed = $derived(grant !== null && grantLeft(grantedAt, clock.now) === 0);
  const note = $derived(passwordNote(password));

  $effect(() => clock.use());

  async function save() {
    const problem = passwordProblem(password);
    if (problem) return (error = problem);
    saving = true;
    error = await security.setPassword({ newPassword: password, grant: grant ?? "" });
    if (!error) {
      const outcome = await security.signIn(password, true, security.state?.passwordSet ? "changed" : "set");
      if (!outcome.ok) error = `The password is set, but signing in failed. ${outcome.message}`;
    }
    saving = false;
  }
</script>

{#if grant === null}
  <ConfirmAtSatellite
    ongrant={(g) => {
      grant = g;
      grantedAt = Date.now();
      clock.now = grantedAt;
    }}
    {oncancel}
  />
{:else}
  <form
    onsubmit={(e) => {
      e.preventDefault();
      void save();
    }}
  >
    {#if lapsed}
      <p class="pn-health" data-tone="caution">The confirmation expired.</p>
      <div class="actions">
        <button class="pn-btn pn-btn-primary" type="button" disabled={saving} onclick={() => (grant = null)}>
          Confirm again
        </button>
        <button class="pn-btn" type="button" disabled={saving} onclick={oncancel}>Cancel</button>
      </div>
    {:else}
      <p class="pn-health" data-tone="ok">Confirmed at the satellite.</p>
    {/if}
    {#if needs}
      <div class="pn-notice" data-tone="caution">
        <Icon name="info" class="pn-icon pn-notice-icon" />
        <div class="pn-notice-body"><p>{needs}</p></div>
      </div>
    {/if}
    <PasswordField
      id="{id}-new"
      label="New password"
      bind:value={password}
      autocomplete="new-password"
      hint="8 to 128 characters. Don't use a password you use anywhere else: it crosses your network unencrypted."
      {note}
      {error}
      disabled={saving}
      autofocus
    />
    {#if !lapsed}
      <div class="actions">
        <button class="pn-btn pn-btn-primary" type="submit" disabled={saving || !password}>Set password</button>
        <button class="pn-btn" type="button" disabled={saving} onclick={oncancel}>Cancel</button>
      </div>
    {/if}
  </form>
{/if}

<style>
  form {
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
  }
  p {
    margin: 0;
  }
  .actions {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-2) var(--space-3);
  }
</style>
