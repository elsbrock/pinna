<!-- The sign-in form (ADR 0029), shown instead of the page while a password is
     set and this browser has no session. "Forgot the password" asks for a press
     of Action at the satellite, then sets a new one. -->
<script lang="ts">
  import AuthScreen from "./AuthScreen.svelte";
  import { device, savedName } from "./device.svelte.ts";
  import PasswordField from "./PasswordField.svelte";
  import { security } from "./security.svelte.ts";
  import SetPassword from "./SetPassword.svelte";
  import { displayName, secondsUntil } from "./view.ts";

  // Without a session there is no Status: the name this browser last saw, or the one in the address.
  const name = $derived(device.status ? displayName(device.status) : savedName() || location.hostname);

  let forgot = $state(false);
  let password = $state("");
  let error = $state<string | null>(null);
  let busy = $state(false);
  let now = $state(Date.now());
  let waitUntil = $state(Date.now() + (security.state?.loginRetryAfterS ?? 0) * 1000);
  const wait = $derived(secondsUntil(waitUntil, now));

  $effect(() => {
    const timer = setInterval(() => (now = Date.now()), 500);
    return () => clearInterval(timer);
  });

  async function submit() {
    busy = true;
    error = null;
    const outcome = await security.signIn(password);
    if (!outcome.ok) {
      error = outcome.message;
      now = Date.now();
      waitUntil = now + outcome.wait * 1000;
      password = "";
    }
    busy = false;
  }
</script>

<AuthScreen title={forgot ? "Set a new password" : name}>
  {#if forgot}
    <p>
      To set a new password for {name}, press Action on the satellite. Nothing else is needed: no cable and no reset.
    </p>
    <SetPassword id="forgot" oncancel={() => (forgot = false)} />
  {:else}
    <p>Sign in to control this satellite.</p>
    <form
      onsubmit={(e) => {
        e.preventDefault();
        void submit();
      }}
    >
      <PasswordField
        id="sign-in-password"
        label="Password"
        bind:value={password}
        autocomplete="current-password"
        {error}
        disabled={busy}
        autofocus
      />
      {#if wait > 0}<p class="wait" role="status">Try again in {wait} s.</p>{/if}
      <button class="pn-btn pn-btn-primary" type="submit" disabled={busy || wait > 0 || !password}>Sign in</button>
    </form>
    <button class="pn-btn pn-btn-quiet pn-btn-sm forgot" type="button" onclick={() => (forgot = true)}>
      Forgot the password?
    </button>
  {/if}
</AuthScreen>

<style>
  form {
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
  }
  .wait {
    color: var(--ink-muted);
  }
  .forgot {
    align-self: flex-start;
  }
</style>
