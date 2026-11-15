<script lang="ts">
  import type { Effect } from "../../../design/ring.ts";
  import { device } from "../lib/device.svelte.ts";
  import Group from "../lib/Group.svelte";
  import Ring from "../lib/Ring.svelte";
  import Slider from "../lib/Slider.svelte";
  import SwitchRow from "../lib/SwitchRow.svelte";
  import { micHint, wakeHint } from "../lib/view.ts";

  const s = $derived(device.status);
  const set = $derived(s?.settings);
  const live = $derived(device.link === "live");

  // What the ring says, in the device's own colours and timings.
  const LEGEND: { effect: Effect; word: string; what: string; loop?: number }[] = [
    { effect: "listening", word: "Listening", what: "Heard the wake word; speak now." },
    { effect: "thinking", word: "Thinking", what: "Home Assistant is working on it." },
    { effect: "speaking", word: "Speaking", what: "Playing the reply." },
    { effect: "muted", word: "Muted", what: "The microphones are off." },
    { effect: "timer", word: "Timer", what: "How much of a timer is left." },
    { effect: "error", word: "Error", what: "Something failed: three flashes.", loop: 3000 },
  ];
</script>

{#if s && set}
  <Group title="Wake word">
    <SwitchRow
      id="wake"
      label="Wake word"
      checked={set.wakeWord}
      disabled={!live || device.busy}
      hint={wakeHint(s)}
      toggle={(on) => device.updateSettings({ wakeWord: on })}
    />
    <div class="pn-row is-stack">
      <Slider
        label="Sensitivity"
        value={set.wakeSensitivity}
        disabled={!live}
        hint="Higher wakes more easily, and more often by mistake."
        commit={(v) => device.updateSettings({ wakeSensitivity: v })}
      />
    </div>
  </Group>

  <Group title="Microphones">
    <SwitchRow
      id="mics"
      label="Microphones"
      checked={!s.muted}
      disabled={!live || device.busy || s.hardwareMuted}
      hint={micHint(s) ?? "The microphone button on the satellite turns them off too."}
      toggle={(on) => device.setMicrophones(on)}
    />
  </Group>

  <Group title="Sound">
    <div class="pn-row is-stack">
      <Slider
        label="Volume"
        value={s.volume}
        disabled={!live}
        hint="Replies follow this volume unless Home Assistant sets a reply volume."
        commit={(v) => device.setVolume(v)}
      />
    </div>
  </Group>

  <Group title="Light ring">
    <div class="pn-row is-stack">
      <Slider
        label="Brightness"
        value={set.ledBrightness}
        disabled={!live}
        hint="How bright the ring is in every state."
        commit={(v) => device.updateSettings({ ledBrightness: v })}
      />
    </div>
    <div class="pn-row">
      <div class="pn-row-text">
        <span class="pn-row-label">Identify</span>
        <span class="pn-row-hint">Run the ring's test pattern, to find this satellite among others.</span>
      </div>
      <button class="pn-btn" disabled={!live} onclick={() => device.identify()}>Identify</button>
    </div>
    <ul class="legend" aria-label="What the ring shows">
      {#each LEGEND as l (l.effect)}
        <li>
          <Ring effect={l.effect} value={l.effect === "timer" ? 0.6 : undefined} loop={l.loop} size={40} label="Ring: {l.word.toLowerCase()}" />
          <span><b>{l.word}</b>{l.what}</span>
        </li>
      {/each}
    </ul>
  </Group>

  <p class="pn-group-foot">
    These settings are shared with Home Assistant: a change here shows up there, and the other way round. The
    satellite keeps them across restarts.
  </p>
{/if}

<style>
  .legend {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(min(100%, 200px), 1fr));
    gap: 14px var(--space-4);
    margin: 0;
    padding: 14px var(--space-4) var(--space-4);
    border-top: 1px solid var(--line);
    list-style: none;
  }
  li {
    display: flex;
    align-items: center;
    gap: 10px;
    min-width: 0;
  }
  li span {
    font-size: 13px;
    line-height: 18px;
    color: var(--ink-muted);
  }
  b {
    display: block;
    font-size: 15px;
    line-height: 22px;
    font-weight: 650;
    color: var(--ink);
  }
</style>
