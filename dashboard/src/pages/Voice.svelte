<script lang="ts">
  import { create } from "@bufbuild/protobuf";

  import type { Effect } from "../../../design/ring.ts";
  import { WakeWordIdsSchema } from "../gen/pinna/v1/device_pb.ts";
  import { device } from "../lib/device.svelte.ts";
  import { isStandalone } from "../lib/assistant.ts";
  import Group from "../lib/Group.svelte";
  import Icon from "../lib/Icon.svelte";
  import ColorRow from "../lib/ColorRow.svelte";
  import Ring from "../lib/Ring.svelte";
  import Select from "../lib/Select.svelte";
  import Slider from "../lib/Slider.svelte";
  import SwitchRow from "../lib/SwitchRow.svelte";
  import { micHint, wakeHint, wakeWordsHint } from "../lib/view.ts";

  const s = $derived(device.status);
  const set = $derived(s?.settings);
  const light = $derived(s?.ringLight);
  const hex = (rgb: number) => `#${rgb.toString(16).padStart(6, "0")}`;
  const live = $derived(device.link === "live");

  // Two words at most, chosen in Home Assistant while it is connected in
  // Home Assistant mode, here otherwise (ADR 0013, ADR 0017). `first`/`second`
  // are positions in the active list.
  const words = $derived(s?.wakeWord?.available ?? []);
  const chosen = $derived(set?.wakeWords ?? []);
  const first = $derived(chosen[0] ?? "");
  const second = $derived(chosen[1] ?? "");
  const standalone = $derived(isStandalone(s));
  const locked = $derived(!live || (!!s?.haConnected && !standalone) || words.length === 0);
  const lockHint = $derived(s ? wakeWordsHint(s) : undefined);
  const asOptions = (except: string) =>
    words.filter((w) => w.id !== except).map((w) => ({ id: w.id, label: w.phrase }));
  // The first list names the empty state; the second offers "None" outright.
  const firstOptions = $derived([
    ...(chosen.length === 0 ? [{ id: "", label: "No wake word" }] : []),
    ...asOptions(second),
  ]);
  const secondOptions = $derived([{ id: "", label: "None" }, ...asOptions(first)]);

  function chooseWords(which: "first" | "second", id: string) {
    const ids = (which === "first" ? [id, second] : [first, id]).filter(Boolean);
    return device.updateSettings({ wakeWords: create(WakeWordIdsSchema, { ids }) });
  }

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
    { effect: "error", word: "Error", what: "A request failed: three flashes. Logs, under System, says why." },
  ]);
</script>

{#if s && set}
  <Group title="Wake word">
    <SwitchRow
      id="wake"
      label="Wake word"
      checked={set.wakeWord}
      disabled={!live}
      busy={device.busy}
      hint={wakeHint(s)}
      toggle={(on) => device.updateSettings({ wakeWord: on })}
    />
    <Select
      id="wake-word"
      label="Wake word"
      value={first}
      options={firstOptions}
      hint={lockHint}
      disabled={locked}
      choose={(id) => chooseWords("first", id)}
    />
    <Select
      id="wake-word-2"
      label="Second wake word"
      value={second}
      options={secondOptions}
      hint={lockHint ?? 'Optional. "None" leaves just the word above.'}
      disabled={locked}
      choose={(id) => chooseWords("second", id)}
    />
    <a class="pn-row" href="#/voice/wake-word">
      <span class="pn-row-text">
        <span class="pn-row-label">Sensitivity</span>
        <span class="pn-row-hint">How easily it wakes, its recent wakes, and tuning it from the satellite's buttons</span>
      </span>
      <span class="pn-row-value is-mono">{set.wakeSensitivity}</span>
      <Icon name="chevron-right" class="pn-icon pn-row-chev" />
    </a>
  </Group>

  <Group title="Microphones">
    <SwitchRow
      id="mics"
      label="Microphones"
      checked={!s.muted}
      disabled={!live || s.hardwareMuted}
      busy={device.busy}
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
        hint="Everything the satellite plays: music, chimes and replies."
        commit={(v) => device.setVolume(v)}
      />
    </div>
    <SwitchRow
      id="reply-follows"
      label="Replies follow Volume"
      checked={set.replyVolume === undefined}
      disabled={!live}
      busy={device.busy}
      hint="Off: replies and announcements get their own volume."
      toggle={(on) => device.updateSettings({ replyFollowsVolume: on })}
    />
    {#if set.replyVolume !== undefined}
      <div class="pn-row is-stack">
        <Slider
          label="Reply volume"
          value={set.replyVolume}
          disabled={!live}
          hint={set.replyVolume > s.volume
            ? `Replies, announcements and timer alarms play at ${s.volume} now: never louder than Volume.`
            : "Replies, announcements and timer alarms. They are never louder than Volume."}
          commit={(v) => device.updateSettings({ replyVolume: v })}
        />
      </div>
    {/if}
    <SwitchRow
      id="wake-sound"
      label="Wake sound"
      checked={set.wakeSound}
      disabled={!live}
      busy={device.busy}
      hint="A short chime when it starts listening."
      toggle={(on) => device.updateSettings({ wakeSound: on })}
    />
    <a class="pn-row" href="#/voice/media-sources">
      <span class="pn-row-text">
        <span class="pn-row-label">Media sources</span>
        <span class="pn-row-hint">Let phones and computers play music on this satellite, or play a URL</span>
      </span>
      <span class="pn-row-value"
        >{[set.airplay && "AirPlay", set.bluetooth && "Bluetooth"].filter(Boolean).join(", ") || "Off"}</span
      >
      <Icon name="chevron-right" class="pn-icon pn-row-chev" />
    </a>
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
    <SwitchRow
      id="listening-direction"
      label="Listening direction"
      checked={set.listeningDirection}
      disabled={!live}
      busy={device.busy}
      hint="While it listens, the ring points at whoever is talking."
      toggle={(on) => device.updateSettings({ listeningDirection: on })}
    />
    {#if light}
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
    {/if}
    <div class="pn-row">
      <div class="pn-row-text">
        <span class="pn-row-label">Identify</span>
        <span class="pn-row-hint">Run the ring's test pattern, to find this satellite among others.</span>
      </div>
      <button class="pn-btn" disabled={!live} onclick={() => device.identify()}>Identify</button>
    </div>
    <ul class="legend" aria-label="What the ring shows">
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
