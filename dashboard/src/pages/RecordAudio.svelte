<!-- Records what the satellite hears (ADR 0030). A press of Action at the satellite
     starts it; the audio streams to this page, and nothing is stored on the
     satellite. Leaving the page aborts the stream, which ends the recording. -->
<script lang="ts">
  import { onMount } from "svelte";

  import { AudioSource, ConfirmationPurpose } from "../gen/pinna/v1/device_pb.ts";
  import { clock } from "../lib/clock.svelte.ts";
  import ConfirmAtSatellite from "../lib/ConfirmAtSatellite.svelte";
  import { client, device, errorSentence } from "../lib/device.svelte.ts";
  import Group from "../lib/Group.svelte";
  import Icon from "../lib/Icon.svelte";
  import { recorder } from "../lib/recorder.svelte.ts";
  import { security } from "../lib/security.svelte.ts";
  import { fileSize, recordingFile, recordingKind, secondsUntil, secondsWord } from "../lib/view.ts";
  import { wavDebug, wavMono16 } from "../lib/wav.ts";

  const SECONDS = 30;

  type File = { label: string; name: string; blob: Blob; length: string };

  let stage = $state<"idle" | "confirming" | "starting" | "recording" | "ended">("idle");
  let error = $state<string | null>(null);
  let deadline = $state(0);
  const left = $derived(secondsUntil(deadline, clock.now));
  let sources = $state<string[]>([]);
  let stopping = $state(false);
  let why = $state("");
  let files = $state.raw<File[]>([]);
  let laneTotal = $state(0);
  /** For screen readers: the recording started or ended, once each. */
  let announce = $state("");
  let abort: AbortController | null = null;

  const passwordSet = $derived(security.state?.passwordSet);
  const live = $derived(device.link === "live");
  // A voice being enrolled uses the microphones too (ADR 0038); it is no recording to start or stop here.
  const enrolling = $derived(recordingKind(device.status) === "enrollment");
  const SOURCE_NAMES: Record<number, string> = {
    [AudioSource.HEARD]: "What the satellite hears",
    [AudioSource.LANES]: "Microphone lanes",
    [AudioSource.BEFORE_GAIN]: "Before the gain",
    [AudioSource.REFERENCE]: "Echo reference",
  };

  onMount(() => {
    void security.refresh();
  });
  $effect(() => clock.use());
  // Leaving the page ends the recording on the satellite.
  $effect(() => () => {
    abort?.abort();
    recorder.here = false;
  });

  const seconds = (n: number) => `${n.toFixed(1)} s`;

  async function record(grant: string) {
    const mine = (abort = new AbortController());
    stage = "starting";
    error = null;
    files = [];
    sources = [];
    why = "";
    stopping = false;
    recorder.here = true;
    const heard: Uint8Array[] = [];
    const lanes: Uint8Array[] = [];
    const beforeGain: Uint8Array[] = [];
    const reference: Uint8Array[] = [];
    const into: Record<number, Uint8Array[]> = {
      [AudioSource.HEARD]: heard,
      [AudioSource.LANES]: lanes,
      [AudioSource.BEFORE_GAIN]: beforeGain,
      [AudioSource.REFERENCE]: reference,
    };
    let rate = 16_000;
    let laneCount = 0;
    let debug = false;
    let ended = "";
    let began = false;
    try {
      for await (const m of client.recordAudio({ grant, seconds: SECONDS }, { signal: mine.signal })) {
        const e = m.event;
        if (e.case === "started") {
          began = true;
          rate = e.value.sampleRate || rate;
          laneCount = e.value.lanes;
          debug = e.value.debug;
          clock.now = Date.now();
          deadline = clock.now + e.value.seconds * 1000;
          stage = "recording";
          announce = "Recording started.";
        } else if (e.case === "chunk") {
          into[e.value.source]?.push(e.value.data);
          const name = SOURCE_NAMES[e.value.source];
          if (name && !sources.includes(name)) sources.push(name);
        } else if (e.case === "ended") {
          ended = e.value.message;
        }
      }
    } catch (e) {
      if (mine.signal.aborted) return;
      // Before the first message the satellite refused: say why, as the field error.
      if (!began) {
        error = errorSentence(e);
        stage = "idle";
        recorder.here = false;
        return;
      }
    }
    if (mine.signal.aborted) return;
    recorder.here = false;
    if (!began) {
      error = "The connection ended before the recording started.";
      stage = "idle";
      return;
    }
    why = ended || "The connection ended.";
    const now = new Date();
    const name = device.status?.name || "pinna";
    const out: File[] = [];
    const wav = wavMono16(heard, rate);
    out.push({
      label: "What the satellite hears",
      name: recordingFile(name, "heard", now),
      blob: new Blob([wav as BlobPart], { type: "audio/wav" }),
      length: seconds((wav.length - 44) / 2 / rate),
    });
    if (debug) {
      const dbg = wavDebug({ lanes, laneCount, beforeGain, reference, rate });
      out.push({
        label: "Microphone lanes and echo cancellation",
        name: recordingFile(name, "debug", now),
        blob: new Blob([dbg as BlobPart], { type: "audio/wav" }),
        length: seconds((dbg.length - 68) / (3 * (laneCount + 2)) / rate),
      });
    }
    laneTotal = laneCount;
    files = out;
    stage = "ended";
    announce = `Recording ended. ${why}`;
  }

  async function stop() {
    stopping = true;
    try {
      await client.stopRecording({});
    } catch (e) {
      error = errorSentence(e);
      stopping = false;
    }
    // The stream's `ended` follows and ends the recording here.
  }

  function save(f: File) {
    const url = URL.createObjectURL(f.blob);
    const a = document.createElement("a");
    a.href = url;
    a.download = f.name;
    a.click();
    URL.revokeObjectURL(url);
  }
</script>

<p class="sr" role="status">{announce}</p>

{#if passwordSet === false}
  <div class="pn-notice" data-tone="caution" role="status">
    <Icon name="alert" class="pn-icon pn-notice-icon" />
    <div class="pn-notice-body">
      <p class="pn-notice-title">Recording needs a dashboard password.</p>
      <p>
        The Action button also starts requests, so a press that starts a recording must come from someone who signed in.
      </p>
      <p><a href="#/system/password">Set a password under System › Password and tokens</a></p>
    </div>
  </div>
{:else if passwordSet}
  <Group title="Record">
    {#snippet foot()}
      The files are the room's audio. Send them privately, not in a public issue. Nothing is stored on the satellite.
    {/snippet}
    {#if stage === "confirming"}
      <div class="pn-row is-stack">
        <ConfirmAtSatellite
          purpose={ConfirmationPurpose.RECORDING}
          ongrant={(grant) => void record(grant)}
          oncancel={() => (stage = "idle")}
        />
      </div>
    {:else if stage === "starting"}
      <div class="pn-row is-stack"><p role="status">Starting the recording…</p></div>
    {:else if stage === "recording"}
      <div class="pn-row is-stack">
        <!-- Not a live region: it changes every second. The start and the end are announced. -->
        <p class="pn-notice-title">Recording. {secondsWord(left)} left.</p>
        <p class="muted">
          Receiving: {sources.length ? sources.join(", ") : "nothing yet"}.
        </p>
        <div class="actions">
          <button class="pn-btn pn-btn-primary" type="button" disabled={stopping} onclick={stop}>Stop</button>
        </div>
      </div>
    {:else}
      <div class="pn-row">
        <div class="pn-row-text">
          <span class="pn-row-label">Record from the microphones</span>
          <span class="pn-row-hint">
            {#if enrolling}
              A browser at {device.status?.recording?.from} is enrolling a voice. You can record when it's done.
            {:else}
              Up to {SECONDS} seconds. You press Action on the satellite to start. The ring shows Listening while it
              records, and muting ends it.
            {/if}
          </span>
        </div>
        <button
          class="pn-btn pn-btn-primary"
          type="button"
          disabled={!live || !security.state?.signedIn || enrolling}
          onclick={() => {
            error = null;
            stage = "confirming";
          }}
        >
          Record
        </button>
      </div>
    {/if}
    {#if error}
      <div class="pn-row">
        <span class="pn-field-error" role="alert"><Icon name="alert" />{error}</span>
      </div>
    {/if}
  </Group>

  {#if stage === "ended"}
    <Group title="Recording">
      <div class="pn-row is-stack">
        <p>{why}</p>
      </div>
      {#each files as f (f.name)}
        <div class="pn-row">
          <div class="pn-row-text">
            <span class="pn-row-label">{f.label}</span>
            <span class="pn-row-hint is-mono">{f.name} · {f.length} · {fileSize(f.blob.size)}</span>
          </div>
          <button class="pn-btn" type="button" onclick={() => save(f)}>Save</button>
        </div>
      {/each}
      {#if files.length > 1}
        <div class="pn-row is-stack">
          <p class="muted">
            The channels of the second file aren't aligned to the sample. Channels 1 to {laneTotal} are the lanes as
            the codec delivers them. Channel {laneTotal + 1} is the signal before Pinna's gain, and channel {laneTotal +
              2} is the echo reference.
          </p>
        </div>
      {/if}
    </Group>
  {/if}
{:else if security.error}
  <div class="pn-notice" data-tone="danger" role="alert">
    <Icon name="alert" class="pn-icon pn-notice-icon" />
    <div class="pn-notice-body">
      <p class="pn-notice-title">Whether a dashboard password is set couldn't be read</p>
      <p>{security.error}</p>
      <div><button class="pn-btn" type="button" onclick={() => security.refresh()}>Try again</button></div>
    </div>
  </div>
{:else}
  <span class="pn-skeleton" style:height="72px"></span>
{/if}

<style>
  .sr {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip-path: inset(50%);
    white-space: nowrap;
  }
  .muted {
    color: var(--ink-muted);
  }
  p {
    margin: 0;
  }
  .actions {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-2) var(--space-3);
    margin-top: var(--space-2);
  }
</style>
