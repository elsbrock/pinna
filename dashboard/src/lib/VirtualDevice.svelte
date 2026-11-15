<!-- The virtual Echo Dot (ADR 0014), only on the development VM's dashboard:
     this browser as the satellite's microphone, speaker and buttons. The
     buttons go down and up with the pointer or key, so long presses and
     volume repeat work as on the Dot. -->
<script lang="ts">
  import { device } from "./device.svelte.ts";
  import Group from "./Group.svelte";
  import Icon from "./Icon.svelte";
  import SwitchRow from "./SwitchRow.svelte";
  import { virtualDot as v, type VirtualButton } from "./virtual.svelte.ts";
  import { spokenWakeWords } from "./view.ts";

  const s = $derived(device.status);
  const on = $derived(v.link === "on");
  let picker = $state<HTMLInputElement>();

  const BUTTONS: { id: VirtualButton; text: string; label: string }[] = [
    { id: "volume_down", text: "−", label: "Volume down" },
    { id: "volume_up", text: "+", label: "Volume up" },
    { id: "action", text: "", label: "Action" },
    { id: "mute", text: "", label: "Microphone button" },
  ];

  const linkHint = $derived(
    {
      off: "Use this browser as the satellite's microphone, speaker and buttons.",
      connecting:
        v.retries === 0
          ? "Connecting to the satellite…"
          : `Lost the satellite. Trying again (attempt ${v.retries})…${v.retries >= 3 ? " Is just dev-vm still running?" : ""}`,
      on: "Connected. Hold a button as long as you would on the Dot.",
      replaced: "Another tab is the satellite's hardware now.",
      failed: v.problem,
    }[v.link],
  );

  const phrase = $derived(s ? spokenWakeWords(s) : "the wake word");
  const micHint = $derived.by(() => {
    if (v.file) return "Off while the audio file plays.";
    if (v.mic === "on" && !on) return "Not sent while the satellite is not connected.";
    switch (v.mic) {
      case "insecure":
        return "Browsers share the microphone only with localhost. Open the dashboard at http://localhost or 127.0.0.1, or tunnel it with ssh -L.";
      case "denied":
        return "The browser may not use the microphone. Allow it in this site's settings.";
      case "missing":
        return "No microphone found.";
      case "busy":
        return "The microphone is busy in another program, or unavailable.";
      case "asking":
        return "Waiting for the browser's permission…";
      case "on":
        if (s?.hardwareMuted) return "The microphone button is on: the satellite hears nothing.";
        if (s?.muted) return "The microphones are off (Controls): the satellite hears nothing.";
        if (!s?.settings?.wakeWord) return "The satellite hears this browser. The wake word is off: press Action to talk.";
        return `The satellite hears this browser. Say ${phrase}.`;
      default:
        return "Off: the satellite hears silence.";
    }
  });

  function down(b: VirtualButton, e: PointerEvent) {
    if (!on || e.button !== 0) return;
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
    v.press(b, true);
  }

  function key(b: VirtualButton, e: KeyboardEvent, pressed: boolean) {
    if (e.key !== " " && e.key !== "Enter") return;
    e.preventDefault();
    if (on && !e.repeat) v.press(b, pressed);
  }

  // A key held while the window loses focus would never come up.
  $effect(() => {
    const release = () => v.releaseAll();
    addEventListener("blur", release);
    return () => removeEventListener("blur", release);
  });

  function pick(e: Event) {
    const input = e.currentTarget as HTMLInputElement;
    const f = input.files?.[0];
    input.value = "";
    if (f) void v.playFile(f);
  }
</script>

<Group title="Virtual Echo Dot">
  <div class="pn-row">
    <div class="pn-row-text">
      <span class="pn-row-label">This browser as the hardware</span>
      <span class="pn-row-hint" role="status">{linkHint}</span>
    </div>
    {#if v.link === "replaced"}
      <button class="pn-btn" onclick={() => v.takeOver()}>Use it here</button>
    {:else if v.link === "off" || v.link === "failed"}
      <button class="pn-btn pn-btn-primary" onclick={() => v.connect()}>Connect</button>
    {:else}
      <button class="pn-btn" onclick={() => v.disconnect()}>Disconnect</button>
    {/if}
  </div>

  <div class="pn-row keys" aria-label="Buttons" role="group">
    {#each BUTTONS as b (b.id)}
      <button
        class="key"
        class:is-held={v.held.includes(b.id)}
        class:is-latched={b.id === "mute" && !!s?.hardwareMuted}
        aria-label={b.id === "mute" ? `${b.label}${s?.hardwareMuted ? " (microphones off)" : ""}` : b.label}
        title={b.label}
        disabled={!on}
        onpointerdown={(e) => down(b.id, e)}
        onpointerup={() => v.press(b.id, false)}
        onpointercancel={() => v.press(b.id, false)}
        onkeydown={(e) => key(b.id, e, true)}
        onkeyup={(e) => key(b.id, e, false)}
        onblur={() => v.press(b.id, false)}
      >
        {#if b.id === "mute"}
          <Icon name="mic" />
        {:else if b.id === "action"}
          <span class="dot"></span>
        {:else}
          {b.text}
        {/if}
      </button>
    {/each}
  </div>

  <SwitchRow
    id="virtual-mic"
    label="This browser's microphone"
    checked={v.mic === "on" || v.mic === "asking"}
    disabled={!on || v.mic === "insecure" || !!v.file}
    hint={micHint}
    toggle={(x) => (x ? v.startMic() : v.stopMic())}
  />
  {#if on && (v.mic === "on" || v.file)}
    <div class="pn-row level">
      <span class="pn-row-hint">Level</span>
      <span
        class="meter"
        role="meter"
        aria-label="Microphone level"
        aria-valuemin={0}
        aria-valuemax={100}
        aria-valuenow={Math.round(v.level * 100)}><span style:width="{Math.round(v.level * 100)}%"></span></span>
    </div>
  {/if}

  <div class="pn-row">
    <div class="pn-row-text">
      <span class="pn-row-label">Audio file as the microphone</span>
      <span class="pn-row-hint">
        {v.fileProblem ||
          (v.file ? `Playing ${v.file}.` : "A recording, such as the wake word, played instead of the microphone.")}
      </span>
    </div>
    <input bind:this={picker} type="file" accept="audio/*" hidden onchange={pick} />
    {#if v.file}
      <button class="pn-btn" aria-label="Stop playing {v.file}" onclick={() => v.stopFile()}>Stop</button>
    {:else}
      <button class="pn-btn" disabled={!on} onclick={() => picker?.click()}>Choose…</button>
    {/if}
  </div>

  <SwitchRow
    id="virtual-speaker"
    label="Speaker"
    checked={v.speaker}
    disabled={!on}
    hint="What the satellite plays, from this browser."
    toggle={(x) => v.setSpeaker(x)}
  />
</Group>

<style>
  .keys {
    justify-content: center;
    gap: var(--space-4);
    padding: 16px;
  }
  /* The Dot's four buttons: round, on its top. */
  .key {
    width: 52px;
    height: 52px;
    display: grid;
    place-items: center;
    border: 1px solid var(--line-strong);
    border-radius: var(--radius-full);
    background: var(--surface-sunken);
    color: var(--ink);
    font-size: 24px;
    line-height: 1;
    cursor: pointer;
    touch-action: none;
    user-select: none;
    transition:
      transform 0.06s,
      background-color 0.12s;
  }
  .key:focus-visible {
    outline: none;
    box-shadow: var(--focus-ring);
  }
  .key:disabled {
    opacity: 0.45;
    cursor: not-allowed;
  }
  .key.is-held {
    transform: scale(0.94);
    background: var(--line);
  }
  .key.is-latched {
    color: var(--led-error);
    border-color: var(--led-error);
  }
  .dot {
    width: 10px;
    height: 10px;
    border-radius: var(--radius-full);
    background: currentColor;
  }
  .level {
    min-height: 0;
    padding-top: 0;
    border-top: 0;
  }
  .meter {
    flex: 1;
    height: 6px;
    overflow: hidden;
    border-radius: var(--radius-full);
    background: var(--surface-sunken);
  }
  .meter > span {
    display: block;
    height: 100%;
    background: var(--listening);
    transition: width 0.06s linear;
  }
</style>
