<!-- Enrolls one person's voice (ADR 0038): what will happen, a press of Action
     at the satellite, then 30 seconds of listening. The audio stays on the
     satellite; only a voiceprint is kept. Cancel, and leaving the page, abort
     the stream, which stores nothing. -->
<script lang="ts">
  import { onMount, tick } from "svelte";

  import { ConfirmationPurpose } from "../../gen/pinna/v1/device_pb.ts";
  import type { Person } from "../../gen/pinna/v1/people_pb.ts";
  import { clock } from "../../lib/clock.svelte.ts";
  import ConfirmAtSatellite from "../../lib/ConfirmAtSatellite.svelte";
  import { errorSentence } from "../../lib/device.svelte.ts";
  import Icon from "../../lib/Icon.svelte";
  import { ENROLL_PASSAGE, ENROLL_SECONDS, ENROLL_SPEECH_NEEDED, closeWarning, enrolledLine, enrollHeading, levelPercent } from "../../lib/people.ts";
  import { people } from "../../lib/people.svelte.ts";
  import { recorder } from "../../lib/recorder.svelte.ts";
  import { secondsUntil, secondsWord } from "../../lib/view.ts";

  let { person, again, blocked = null, onclose }: { person: Person; again: boolean; blocked?: string | null; onclose: () => void } =
    $props();

  let stage = $state<"before" | "confirming" | "starting" | "listening" | "done" | "failed">("before");
  // The person's form and the list keep still while it waits or listens.
  $effect(() => {
    people.enrolling = stage === "confirming" || stage === "starting" || stage === "listening";
    return () => (people.enrolling = false);
  });
  let panel = $state<HTMLElement>();
  $effect(() => {
    if (stage === "before") announce = "";
  });
  let error = $state<string | null>(null);
  let deadline = $state(0);
  const left = $derived(secondsUntil(deadline, clock.now));
  let level = $state(-Infinity);
  let speech = $state(0);
  let total = $state(ENROLL_SECONDS);
  // How it ended: the saved seconds and the close pair's warning, or the satellite's sentence.
  let saved = $state<{ seconds: number; close: string | null } | null>(null);
  let failure = $state("");
  let canceled = $state(false);
  /** For screen readers: the listening started or ended, once each. */
  let announce = $state("");
  let abort: AbortController | null = null;

  const name = $derived(person.name);

  onMount(() => () => {
    abort?.abort();
    recorder.here = false;
  });
  $effect(() => clock.use());

  async function listen(grant: string) {
    const mine = (abort = new AbortController());
    stage = "starting";
    error = null;
    canceled = false;
    level = -Infinity;
    speech = 0;
    recorder.here = true;
    let began = false;
    let finished = false;
    try {
      for await (const m of people.enroll(person.id, grant, mine.signal)) {
        const e = m.event;
        if (e.case === "started") {
          began = true;
          total = e.value.seconds || ENROLL_SECONDS;
          clock.now = Date.now();
          deadline = clock.now + total * 1000;
          stage = "listening";
          announce = `Listening. ${name} can start speaking.`;
          // Cancel stays above the phone's tab bar.
          void tick().then(() => panel?.scrollIntoView({ block: "nearest" }));
        } else if (e.case === "progress") {
          level = e.value.levelDbfs;
          speech = e.value.secondsOfSpeech;
        } else if (e.case === "done") {
          finished = true;
          // The list again first: the close pair's names come from it.
          await people.load();
          if (mine.signal.aborted) return;
          saved = { seconds: e.value.secondsOfSpeech, close: closeWarning(e.value.closeTo, people.view?.people ?? []) };
          stage = "done";
          announce = "";
        } else if (e.case === "failed") {
          finished = true;
          failure = e.value.message;
          stage = "failed";
          announce = "";
        }
      }
    } catch (e) {
      if (mine.signal.aborted) return;
      // Before the first message the satellite refused: say why, in the panel.
      if (!began) {
        recorder.here = false;
        error = errorSentence(e);
        stage = "before";
        return;
      }
    }
    if (mine.signal.aborted) return;
    recorder.here = false;
    if (!finished) {
      if (!began) {
        error = "The connection ended before the enrollment started.";
        stage = "before";
        return;
      }
      failure = "The connection ended. Try again.";
      stage = "failed";
      announce = "";
    }
  }

  // Cancel, while the satellite listens: nothing is stored.
  function cancel() {
    abort?.abort();
    recorder.here = false;
    canceled = true;
    stage = "failed";
    announce = "";
  }
</script>

<p class="sr" role="status">{announce}</p>

<div class="panel" bind:this={panel}>
{#if stage === "before"}
    <p class="pn-field-label">{enrollHeading(name, again)}</p>
    <ul>
      <li>{name} stands at the satellite, within 1 m, and presses Action when the ring asks.</li>
      <li>
        {name} then speaks for {ENROLL_SECONDS} seconds: reads the text that appears, or talks about anything. Children
        who can't read can just talk.
      </li>
      <li>Nobody else talks, and a TV or radio is off.</li>
      <li>Stay on this page until it's done. Leaving it cancels the enrollment.</li>
      <li>No audio leaves the satellite. It keeps only a voiceprint.</li>
      {#if again}<li>The old voiceprint is replaced only when the new one passes the checks.</li>{/if}
    </ul>
    {#if blocked}<p class="pn-field-hint">{blocked}</p>{/if}
    {#if error}<span class="pn-field-error" role="alert"><Icon name="alert" />{error}</span>{/if}
    <div class="actions">
      <button
        class="pn-btn pn-btn-primary"
        type="button"
        disabled={!!blocked}
        onclick={() => {
          error = null;
          stage = "confirming";
        }}>Start</button
      >
      <button class="pn-btn" type="button" onclick={onclose}>Not now</button>
    </div>
{:else if stage === "confirming"}
    <ConfirmAtSatellite
      purpose={ConfirmationPurpose.ENROLL}
      who={name}
      ongrant={(grant) => void listen(grant)}
      oncancel={() => (stage = "before")}
    />
{:else if stage === "starting"}
  <p role="status">Starting the enrollment…</p>
{:else if stage === "listening"}
    <!-- Not a live region: it changes every second. The start and the end are announced. -->
    <p class="pn-notice-title">Listening to {name}. {secondsWord(left)} left.</p>
    <div class="meter">
      <span id="enroll-level-label" class="pn-field-hint">Level</span>
      <span
        class="pn-step-bar"
        role="meter"
        aria-labelledby="enroll-level-label"
        aria-valuemin="0"
        aria-valuemax="100"
        aria-valuenow={levelPercent(level)}
      ><span style:--pct="{levelPercent(level)}%"></span></span>
    </div>
    <div class="meter">
      <span id="enroll-speech-label" class="pn-field-hint">
        Speech so far: {Math.floor(speech)} of {ENROLL_SPEECH_NEEDED} seconds needed
      </span>
      <span
        class="pn-step-bar"
        role="meter"
        aria-labelledby="enroll-speech-label"
        aria-valuemin="0"
        aria-valuemax={ENROLL_SPEECH_NEEDED}
        aria-valuenow={Math.min(speech, ENROLL_SPEECH_NEEDED)}
      ><span style:--pct="{Math.min(1, speech / ENROLL_SPEECH_NEEDED) * 100}%"></span></span>
    </div>
    <p class="pn-field-hint">Read this aloud, or talk about anything. Nobody else talks.</p>
    <p class="passage">{ENROLL_PASSAGE}</p>
    <div class="actions">
      <button class="pn-btn" type="button" onclick={cancel}>Cancel</button>
    </div>
{:else if stage === "done"}
    <div class="pn-notice" data-tone={saved?.close ? "caution" : "ok"} role="status">
      <Icon name={saved?.close ? "alert" : "check"} class="pn-icon pn-notice-icon" />
      <div class="pn-notice-body">
        <p class="pn-notice-title">{enrolledLine(name, saved?.seconds ?? 0)}</p>
        {#if saved?.close}<p>{saved.close}</p>{/if}
      </div>
    </div>
    <div class="actions">
      <button class="pn-btn" type="button" onclick={onclose}>Close</button>
    </div>
{:else if canceled}
    <div class="pn-notice" role="status">
      <Icon name="info" class="pn-icon pn-notice-icon" />
      <div class="pn-notice-body">
        <p class="pn-notice-title">Canceled</p>
        <p>{again ? `${name}'s earlier voiceprint is kept.` : "Nothing was saved."}</p>
      </div>
    </div>
    <div class="actions">
      <button class="pn-btn pn-btn-primary" type="button" disabled={!!blocked} onclick={() => (stage = "before")}>
        Try again
      </button>
      <button class="pn-btn" type="button" onclick={onclose}>Close</button>
    </div>
{:else}
    <div class="pn-notice" data-tone="danger" role="alert">
      <Icon name="alert" class="pn-icon pn-notice-icon" />
      <div class="pn-notice-body">
        <p class="pn-notice-title">Nothing was saved</p>
        <p>{failure}</p>
      </div>
    </div>
    <div class="actions">
      <button class="pn-btn pn-btn-primary" type="button" disabled={!!blocked} onclick={() => (stage = "before")}>
        Try again
      </button>
      <button class="pn-btn" type="button" onclick={onclose}>Close</button>
    </div>
{/if}
</div>

<style>
  .sr {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip-path: inset(50%);
    white-space: nowrap;
  }
  .panel {
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
    /* Cancel never hides under the phone's tab bar. */
    scroll-margin-bottom: calc(var(--tabbar-h, 0px) + var(--space-4));
  }
  .panel p,
  .panel ul {
    margin: 0;
  }
  ul {
    padding-left: var(--space-6);
    display: flex;
    flex-direction: column;
    gap: var(--space-1);
  }
  .meter {
    display: flex;
    flex-direction: column;
  }
  .passage {
    padding: var(--space-3);
    border-radius: var(--radius-sm);
    background: var(--surface-sunken);
    line-height: 24px;
  }
  .actions {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-2) var(--space-3);
  }
</style>
