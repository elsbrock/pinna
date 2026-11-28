<!-- Asks for someone at the satellite (ADR 0029): the ring circles for 60
     seconds, white for a password and in the Listening color for a recording
     (ADR 0030, ADR 0036, ADR 0038), and a press of Action hands the caller a one-time grant. Starts
     when it appears. Cancel, and leaving while it waits, stop the ring at once. -->
<script lang="ts">
  import { onMount } from "svelte";

  import { ConfirmationPurpose, ConfirmationState } from "../gen/pinna/v1/device_pb.ts";
  import { clock } from "./clock.svelte.ts";
  import { client, errorSentence } from "./device.svelte.ts";
  import Icon from "./Icon.svelte";
  import { security } from "./security.svelte.ts";
  import { secondsUntil, secondsWord, splitRetry } from "./view.ts";

  let {
    ongrant,
    oncancel,
    purpose = ConfirmationPurpose.PASSWORD,
    skip,
    who,
  }: {
    ongrant: (grant: string) => void;
    oncancel: () => void;
    purpose?: ConfirmationPurpose;
    /** Another way on when the wait ends without a press. */
    skip?: { label: string; onskip: () => void };
    /** Whose voice is enrolled, for ENROLL: the one who presses Action. */
    who?: string;
  } = $props();
  const debugging = $derived(purpose === ConfirmationPurpose.DEBUGGING_TEXT);
  const enrolling = $derived(purpose === ConfirmationPurpose.ENROLL);
  // The ring shows the recording pattern for all three: the microphones record.
  const recording = $derived(purpose === ConfirmationPurpose.RECORDING || debugging || enrolling);

  let stage = $state<"starting" | "waiting" | "expired" | "stopped" | "refused" | "done">("starting");
  // The satellite sends the seconds left once, when the wait starts: count down here.
  let deadline = $state(0);
  const left = $derived(secondsUntil(deadline, clock.now));
  let refusal = $state("");
  // When a new confirmation may start; the button that would be refused waits for it.
  let retryAt = $state(0);
  const retryIn = $derived(secondsUntil(retryAt, clock.now));
  let id = "";
  let stream: AbortController | null = null;

  $effect(() => clock.use());
  $effect(() => {
    security.confirmingHere = stage === "waiting";
    return () => (security.confirmingHere = false);
  });

  /** Stop the ring, if this asked for it and it still waits. */
  function release() {
    stream?.abort();
    const mine = id;
    id = "";
    if (mine) client.cancelConfirmation({ id: mine }).catch(() => {});
  }

  function waitFor(seconds: number) {
    clock.now = Date.now();
    retryAt = clock.now + seconds * 1000;
  }

  async function start() {
    release();
    const mine = (stream = new AbortController());
    stage = "starting";
    try {
      const started = await client.startConfirmation({ purpose });
      id = started.id;
      clock.now = Date.now();
      deadline = clock.now + started.seconds * 1000;
      stage = "waiting";
      for await (const m of client.watchConfirmation({ id }, { signal: mine.signal })) {
        if (m.state === ConfirmationState.CONFIRMED) {
          id = "";
          stage = "done";
          return ongrant(m.grant);
        }
        if (m.state === ConfirmationState.EXPIRED || m.state === ConfirmationState.CANCELED) {
          id = "";
          // The button that would be refused stays disabled for the satellite's cooldown.
          waitFor(await security.cooldown());
          stage = m.state === ConfirmationState.EXPIRED ? "expired" : "stopped";
          return;
        }
        deadline = Date.now() + m.secondsLeft * 1000;
      }
    } catch (e) {
      if (mine.signal.aborted) return;
      const { text, wait } = splitRetry(errorSentence(e));
      refusal = text;
      waitFor(wait);
      stage = "refused";
    }
  }

  async function cancel() {
    release();
    oncancel();
  }

  onMount(() => {
    void start();
    return release;
  });
</script>

<div class="confirm" role="status">
  {#if stage === "starting"}
    <p>Asking the satellite…</p>
  {:else if stage === "waiting"}
    {#if enrolling && who}
      <p class="pn-notice-title">{who} presses Action on the satellite to start.</p>
      <p>The ring circles in the Listening color. {secondsWord(left)} left. The audio stays on the satellite.</p>
    {:else if recording}
      <p class="pn-notice-title">
        Press Action on the satellite to {debugging
          ? "record what was said"
          : enrolling
            ? "start enrolling a voice"
            : "start recording"}.
      </p>
      <p>
        The ring circles in the Listening color. {secondsWord(left)} left.{enrolling
          ? " The audio stays on the satellite."
          : ""}
      </p>
    {:else}
      <p class="pn-notice-title">Press the Action button (the one with the dot) on the satellite.</p>
      <p>Its ring circles white. {secondsWord(left)} left.</p>
    {/if}
    <p class="muted">
      {enrolling && who ? `Only ${who} should press it.` : "Press it only if you started this yourself."}
    </p>
  {:else if stage === "expired"}
    <p class="pn-notice-title">Nobody pressed Action in time.</p>
    <p>Try again, and press Action on the satellite while its ring circles {recording ? "in the Listening color" : "white"}.</p>
  {:else if stage === "stopped"}
    <p class="pn-notice-title">The confirmation was stopped.</p>
    <p>
      Someone stopped it from the dashboard. Try again, and press Action on the satellite while its ring circles
      {recording ? "in the Listening color" : "white"}.
    </p>
  {:else if stage === "refused"}
    <p class="refused"><Icon name="alert" />{refusal}</p>
  {/if}
  {#if retryIn > 0 && (stage === "expired" || stage === "stopped" || stage === "refused")}
    <p class="muted">Try again in {retryIn} s.</p>
  {/if}
  <div class="actions">
    {#if stage === "expired" || stage === "stopped" || stage === "refused"}
      <button class="pn-btn pn-btn-primary" type="button" disabled={retryIn > 0} onclick={start}>Try again</button>
      {#if skip}<button class="pn-btn" type="button" onclick={() => { release(); skip.onskip(); }}>{skip.label}</button>{/if}
    {/if}
    <button class="pn-btn" type="button" onclick={cancel}>Cancel</button>
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
