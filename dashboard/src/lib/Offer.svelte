<!-- The first-run offer (ADR 0029), before anything else while the satellite has
     no password and nobody chose to leave it open. Setting one signs in with it
     and loads the page; Leave it open goes on to the dashboard. -->
<script lang="ts">
  import { isStandalone } from "./assistant.ts";
  import AuthScreen from "./AuthScreen.svelte";
  import { device } from "./device.svelte.ts";
  import { security } from "./security.svelte.ts";
  import SetPassword from "./SetPassword.svelte";

  let setting = $state(false);
  let busy = $state(false);
  let error = $state<string | null>(null);

  async function leaveOpen() {
    busy = true;
    error = await security.leaveOpen();
    busy = false;
  }
</script>

<AuthScreen title="Set a password for this dashboard?">
  {#if setting}
    <SetPassword id="offer" oncancel={() => (setting = false)} />
  {:else}
    <p>Anyone on your network can control this satellite: restart it, change its settings and reset it.</p>
    {#if isStandalone(device.status)}
      <p>
        In standalone mode that includes where your voice goes: someone could point a speech-to-text service at their own
        server and receive what the microphones hear after the wake word.
      </p>
    {/if}
    <p>
      A password asks for a sign-in first. To set it, you press Action on the satellite, which proves that you are
      there. You can change it later on the System page.
    </p>
    <div class="actions">
      <button class="pn-btn pn-btn-primary" type="button" disabled={busy} onclick={() => (setting = true)}>
        Set a password
      </button>
      <button class="pn-btn" type="button" disabled={busy} onclick={leaveOpen}>Leave it open</button>
    </div>
    {#if error}<p class="error" role="alert">{error}</p>{/if}
  {/if}
</AuthScreen>

<style>
  .actions {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-2) var(--space-3);
  }
  .error {
    color: var(--danger);
    font-weight: 650;
  }
</style>
