<!-- Sets the dashboard password after a press of Action at the satellite (ADR
     0029): the first password, or a new one for an owner who forgot theirs.
     Then signs in with it and loads the page afresh. -->
<script lang="ts">
  import ConfirmAtSatellite from "./ConfirmAtSatellite.svelte";
  import Icon from "./Icon.svelte";
  import PasswordField from "./PasswordField.svelte";
  import { security } from "./security.svelte.ts";
  import { passwordProblem, tokenNeeds } from "./view.ts";

  let { oncancel, id = "set-password" }: { oncancel: () => void; id?: string } = $props();

  let grant = $state<string | null>(null);
  let password = $state("");
  let error = $state<string | null>(null);
  let saving = $state(false);
  // Only a first password cuts off clients that have none; a new one after a
  // forgotten password leaves the tokens as they are.
  const needs = $derived(security.state?.passwordSet ? null : tokenNeeds(security.state));

  async function save() {
    const problem = passwordProblem(password);
    if (problem) return (error = problem);
    saving = true;
    error = await security.setPassword({ newPassword: password, grant: grant ?? "" });
    if (!error) {
      const outcome = await security.signIn(password, true);
      if (!outcome.ok) error = `The password is set, but signing in failed. ${outcome.message}`;
    }
    saving = false;
  }
</script>

{#if grant === null}
  <ConfirmAtSatellite ongrant={(g) => (grant = g)} {oncancel} />
{:else}
  <form
    onsubmit={(e) => {
      e.preventDefault();
      void save();
    }}
  >
    <p class="pn-health" data-tone="ok"><Icon name="check" />Confirmed at the satellite.</p>
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
      {error}
      disabled={saving}
      autofocus
    />
    <div class="actions">
      <button class="pn-btn pn-btn-primary" type="submit" disabled={saving || !password}>Set password</button>
      <button class="pn-btn" type="button" disabled={saving} onclick={oncancel}>Cancel</button>
    </div>
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
