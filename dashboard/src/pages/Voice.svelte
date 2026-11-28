<script lang="ts">
  import { create } from "@bufbuild/protobuf";
  import { onMount } from "svelte";

  import { WakeWordIdsSchema } from "../gen/pinna/v1/device_pb.ts";
  import { device } from "../lib/device.svelte.ts";
  import { isStandalone } from "../lib/assistant.ts";
  import Group from "../lib/Group.svelte";
  import Icon from "../lib/Icon.svelte";
  import Select from "../lib/Select.svelte";
  import Slider from "../lib/Slider.svelte";
  import SwitchRow from "../lib/SwitchRow.svelte";
  import { people } from "../lib/people.svelte.ts";
  import { micHint, wakeHint, wakeWordsHint } from "../lib/view.ts";

  const s = $derived(device.status);
  // The People row shows how many people there are; it stays blank until they are read.
  onMount(() => void people.load());
  const set = $derived(s?.settings);
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
    <a class="pn-row" href="#/voice/wake-words">
      <span class="pn-row-text">
        <span class="pn-row-label">Wake words</span>
        <span class="pn-row-hint">Add your own wake word, or remove one</span>
      </span>
      <span class="pn-row-value is-mono">{words.length}</span>
      <Icon name="chevron-right" class="pn-icon pn-row-chev" />
    </a>
    <a class="pn-row" href="#/voice/people">
      <span class="pn-row-text">
        <span class="pn-row-label">People</span>
        <span class="pn-row-hint">Recognize who speaks, and give each person their own instructions</span>
      </span>
      <span class="pn-row-value is-mono">{people.view ? people.view.people.length : ""}</span>
      <Icon name="chevron-right" class="pn-icon pn-row-chev" />
    </a>
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
        <span class="pn-row-hint">Let phones, computers and Music Assistant play music on this satellite, or play a URL</span>
      </span>
      <span class="pn-row-value"
        >{[set.airplay && "AirPlay", set.bluetooth && "Bluetooth", set.sendspin && "Sendspin"]
          .filter(Boolean)
          .join(", ") || "Off"}</span
      >
      <Icon name="chevron-right" class="pn-icon pn-row-chev" />
    </a>
  </Group>

  <Group title="Ring">
    <a class="pn-row" href="#/voice/ring">
      <span class="pn-row-text">
        <span class="pn-row-label">Ring</span>
        <span class="pn-row-hint">Its brightness, the listening direction, the ring as a light, and what each state looks like</span>
      </span>
      <span class="pn-row-value is-mono">{set.ledBrightness}</span>
      <Icon name="chevron-right" class="pn-icon pn-row-chev" />
    </a>
  </Group>

  <p class="pn-group-foot">
    These settings are shared with Home Assistant: a change here shows up there, and the other way round. The
    satellite keeps them across restarts.
  </p>
{/if}

