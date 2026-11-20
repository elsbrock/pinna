<!-- Asks for someone at the satellite (ADR 0029): the ring circles white for 60
     seconds, and a press of Action hands the caller a one-time grant. Starts
     when it appears. Leaving the page doesn't cancel it: the ring stops by
     itself. -->
<script lang="ts">
  import { onMount } from "svelte";

  import { ConfirmationState } from "../gen/pinna/v1/device_pb.ts";
  import { client, errorSentence } from "./device.svelte.ts";
  import Icon from "./Icon.svelte";
  import { secondsUntil, secondsWord } from "./view.ts";

  let { ongrant, oncancel }: { ongrant: (grant: string) => void; oncancel: () => void } = $props();

  let stage = $state<"starting" | "waiting" | "expired" | "refused">("starting");
  // The satellite sends the seconds left once, when the wait starts: count down here.
  let deadline = $state(0);
  let now = $state(Date.now());
  const left = $derived(secondsUntil(deadline, now));
  let refusal = $state("");
  let stream: AbortController | null = null;

  async function start() {
    stream?.abort();
    const mine = (stream = new AbortController());
    stage = "starting";
    try {
      const { id, seconds } = await client.startConfirmation({});
      now = Date.now();
      deadline = now + seconds * 1000;
      stage = "waiting";
      for await (const m of client.watchConfirmation({ id }, { signal: mine.signal })) {
        if (m.state === ConfirmationState.CONFIRMED) return ongrant(m.grant);
        if (m.state === ConfirmationState.EXPIRED) break;
        deadline = Date.now() + m.secondsLeft * 1000;
      }
      stage = "expired";
    } catch (e) {
      if (mine.signal.aborted) return;
      refusal = errorSentence(e);
      stage = "refused";
    }
  }

  onMount(() => {
    void start();
    const timer = setInterval(() => (now = Date.now()), 500);
    return () => {
      clearInterval(timer);
      stream?.abort();
    };
  });
</script>

<div class="confirm" role="status">
  {#if stage === "starting"}
    <p>Asking the satellite…</p>
  {:else if stage === "waiting"}
    <p class="pn-notice-title">Press Action on the satellite.</p>
    <p>Its ring circles white. {secondsWord(left)} left.</p>
    <p class="muted">Press it only if you started this yourself.</p>
  {:else if stage === "expired"}
    <p class="pn-notice-title">Nobody pressed Action in time.</p>
    <p>Try again, and press Action on the satellite while its ring circles white.</p>
  {:else}
    <p class="refused"><Icon name="alert" />{refusal}</p>
  {/if}
  <div class="actions">
    {#if stage === "expired" || stage === "refused"}
      <button class="pn-btn pn-btn-primary" type="button" onclick={start}>Try again</button>
    {/if}
    <button class="pn-btn" type="button" onclick={oncancel}>Cancel</button>
  </div>
</div>

<style>
  .confirm {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
  }
  p {
    margin: 0;
  }
  .muted {
    color: var(--ink-muted);
  }
  .refused {
    display: flex;
    gap: var(--space-2);
    color: var(--danger);
    font-weight: 650;
  }
  .actions {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-2) var(--space-3);
    margin-top: var(--space-2);
  }
</style>
