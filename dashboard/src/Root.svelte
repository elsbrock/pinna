<!-- What the page shows first (ADR 0029): the offer of a password, the sign-in
     form, or the dashboard. -->
<script lang="ts">
  import App from "./App.svelte";
  import Offer from "./lib/Offer.svelte";
  import { security } from "./lib/security.svelte.ts";
  import SignIn from "./lib/SignIn.svelte";
  import { gate } from "./lib/view.ts";

  const showing = $derived(gate(security.state));
</script>

{#if showing === "offer"}
  <Offer />
{:else if showing === "signin"}
  <SignIn />
{:else}
  <App />
{/if}
