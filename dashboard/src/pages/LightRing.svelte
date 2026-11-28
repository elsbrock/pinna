<!-- Voice & sound › Ring: its brightness, the listening direction, the ring as a
     light, and what each state looks like. -->
<script lang="ts">
  import type { Effect } from "../../../design/ring.ts";
  import { device } from "../lib/device.svelte.ts";
  import { isStandalone } from "../lib/assistant.ts";
  import Group from "../lib/Group.svelte";
  import ColorRow from "../lib/ColorRow.svelte";
  import Ring from "../lib/Ring.svelte";
  import Select from "../lib/Select.svelte";
  import Slider from "../lib/Slider.svelte";
  import SwitchRow from "../lib/SwitchRow.svelte";

  const s = $derived(device.status);
  const set = $derived(s?.settings);
  const light = $derived(s?.ringLight);
  const hex = (rgb: number) => `#${rgb.toString(16).padStart(6, "0")}`;
  const live = $derived(device.link === "live");
  const standalone = $derived(isStandalone(s));

  // What the ring says, in the device's own colours and timings.
  const legend: { effect: Effect; word: string; what: string }[] = $derived([
    { effect: "listening", word: "Listening", what: "Heard the wake word; speak now." },
    {
      effect: "thinking",
      word: "Thinking",
      what: standalone ? "The brain is working on it." : "Home Assistant is working on it.",
    },
    { effect: "speaking", word: "Speaking", what: "Playing the reply." },
    { effect: "muted", word: "Muted", what: "The microphones are off." },
    { effect: "timer", word: "Timer", what: "How much of a timer is left." },
    {
      effect: "notification",
      word: "Notification",
      what: standalone
        ? "A notification is waiting. Press the Action button to clear it."
        : "Home Assistant has a notification. Press the Action button to clear it.",
    },
    { effect: "pairing", word: "Pairing", what: "Bluetooth pairing is open: a phone nearby can pair now." },
    {
      effect: "setup",
      word: "Setup",
      what: "No Wi-Fi network is saved: join the satellite's setup hotspot to choose one.",
    },
    { effect: "error", word: "Error", what: "A request failed: three flashes. Logs, under System, says why." },
  ]);
</script>

{#if s && set}
  <Group title="Ring">
    <div class="pn-row is-stack">
      <Slider
        label="Brightness"
        value={set.ledBrightness}
        disabled={!live}
        hint="How bright the ring is in every state."
        commit={(v) => device.updateSettings({ ledBrightness: v })}
      />
    </div>
    <SwitchRow
      id="listening-direction"
      label="Listening direction"
      checked={set.listeningDirection}
      disabled={!live}
      busy={device.busy}
      hint="While it listens, the ring points at whoever is talking."
      toggle={(on) => device.updateSettings({ listeningDirection: on })}
    />
    <div class="pn-row">
      <div class="pn-row-text">
        <span class="pn-row-label">Identify</span>
        <span class="pn-row-hint">Run the ring's test pattern, to find this satellite among others.</span>
      </div>
      <button class="pn-btn" disabled={!live} onclick={() => device.identify()}>Identify</button>
    </div>
  </Group>

  {#if light}
    <Group title="Light">
      <SwitchRow
        id="ring-light"
        label="Light"
        checked={light.on}
        disabled={!live}
        busy={device.busy}
        hint="A colour of your own while the satellite is idle. A timer or a notification shows instead while there is one. It goes off when the satellite restarts. Home Assistant calls it the LED ring light."
        toggle={(on) => device.setRingLight({ on })}
      />
      {#if light.on}
        <ColorRow
          id="ring-light-colour"
          label="Colour"
          value={hex(light.rgb)}
          disabled={!live}
          choose={(v) => device.setRingLight({ rgb: parseInt(v.slice(1), 16) })}
        />
        <div class="pn-row is-stack">
          <Slider
            label="Light brightness"
            value={light.brightness}
            disabled={!live}
            hint="Within the ring's brightness above."
            commit={(v) => device.setRingLight({ brightness: v })}
          />
        </div>
        <Select
          id="ring-light-effect"
          label="Effect"
          value={light.effect}
          options={light.effects.map((e) => ({ id: e, label: e }))}
          disabled={!live}
          choose={(effect) => device.setRingLight({ effect })}
        />
      {/if}
    </Group>
  {/if}

  <Group title="What the ring shows">
    <ul class="legend">
      {#each legend as l (l.effect)}
        <li>
          <Ring
            effect={l.effect}
            value={l.effect === "timer" ? 0.6 : undefined}
            still
            size={32}
            label="Ring: {l.word.toLowerCase()}"
          />
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
