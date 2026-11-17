<!-- The media player, as Home Assistant's media player shows it: what plays,
     pause, resume, stop, a stream's own volume and the music's own mute. What plays is the
     track its source names, where it names one, and where it comes from (ADR 0011). Shown on
     every satellite with a speaker; with nothing to show, it says how to play something, as
     Timers does. -->
<script lang="ts">
  import { MediaCommand, MediaState } from "../gen/pinna/v1/device_pb.ts";
  import { device } from "./device.svelte.ts";
  import Group from "./Group.svelte";
  import Slider from "./Slider.svelte";
  import SwitchRow from "./SwitchRow.svelte";
  import { byline, mediaFrom } from "./view.ts";

  const m = $derived(device.status?.media);
  const set = $derived(device.status?.settings);
  const live = $derived(device.link === "live");
  const active = $derived(
    m?.state === MediaState.PLAYING || m?.state === MediaState.PAUSED || m?.state === MediaState.ANNOUNCING,
  );
  // A failure stays until the next play; dismissing it hides it on this page.
  const failed = $derived(!!m && !active && !!m.error && m.error !== device.mediaErrorDismissed);
  const STATE: Partial<Record<MediaState, string>> = {
    [MediaState.PLAYING]: "Playing",
    [MediaState.PAUSED]: "Paused",
    [MediaState.ANNOUNCING]: "Announcing",
  };
  const control = (command: MediaCommand) => {
    if (!device.busy) void device.controlMedia(command);
  };
</script>

{#if m?.available}
  <Group title="Now playing">
    {#if active}
      {@const from = mediaFrom(m)}
      {@const by = byline(m)}
      <div class="pn-row">
        <div class="pn-row-text">
          <!-- Without a title, the state is the label; with one, a state other
               than Playing leads the line that says where it comes from. -->
          <span class="pn-row-label">{m.title || STATE[m.state]}</span>
          {#if by}<span class="pn-row-hint">{by}</span>{/if}
          <span class="pn-row-hint"
            >{#if m.title && m.state !== MediaState.PLAYING}{`${STATE[m.state]} · `}{/if}<span
              class:is-mono={from.url}>{from.text}</span
            ></span
          >
        </div>
        <div class="actions">
          <!-- A stream can't pause: pausing ends its session, as Stop does. -->
          {#if !m.stream && m.state !== MediaState.ANNOUNCING}
            <button
              class="pn-btn"
              disabled={!live}
              aria-disabled={device.busy || undefined}
              onclick={() => control(m.state === MediaState.PAUSED ? MediaCommand.PLAY : MediaCommand.PAUSE)}
              >{m.state === MediaState.PAUSED ? "Resume" : "Pause"}</button
            >
          {/if}
          <button
            class="pn-btn"
            disabled={!live}
            aria-disabled={device.busy || undefined}
            onclick={() => control(MediaCommand.STOP)}>Stop</button
          >
        </div>
      </div>
    {:else if failed}
      <div class="pn-row">
        <div class="pn-row-text">
          <span class="pn-row-label">Couldn't play</span>
          <span class="pn-row-hint is-danger">{m.error}</span>
          {#if m.url}<span class="pn-row-hint is-mono">{m.url}</span>{/if}
        </div>
        <button class="pn-btn pn-btn-quiet pn-btn-sm" onclick={() => (device.mediaErrorDismissed = m.error)}>Dismiss</button>
      </div>
    {:else if m.muted}
      <div class="pn-row">
        <div class="pn-row-text">
          <span class="pn-row-label">Not playing</span>
          <span class="pn-row-hint">The music stays muted until you turn Mute music off.</span>
        </div>
      </div>
    {:else}
      <div class="pn-empty">
        <span class="pn-empty-title">Not playing</span>
        <p>
          {#if set?.airplay || set?.bluetooth}
            Play to this satellite from a phone or computer, or play a URL under <a href="#/voice/media-sources"
              >Media sources</a
            >.
          {:else}
            Turn on AirPlay or Bluetooth, or play a URL, under <a href="#/voice/media-sources">Media sources</a>.
          {/if}
        </p>
      </div>
    {/if}
    {#if active && m.stream}
      <div class="pn-row is-stack">
        <Slider
          label="Music volume"
          value={m.volume}
          disabled={!live}
          hint="How loud this music plays here; the phone's own slider doesn't move. Volume sets how loud the satellite is overall."
          commit={(v) => device.setMediaVolume(v)}
        />
      </div>
    {/if}
    {#if active || m.muted}
      <SwitchRow
        id="media-muted"
        label="Mute music"
        checked={m.muted}
        disabled={!live}
        busy={device.busy}
        hint="Only the music: replies, chimes and alarms still sound."
        toggle={(on) => device.controlMedia(on ? MediaCommand.MUTE : MediaCommand.UNMUTE)}
      />
    {/if}
  </Group>
{/if}

<style>
  .actions {
    display: flex;
    flex: none;
    gap: 8px;
  }
  .is-mono {
    font-family: var(--font-mono);
    overflow-wrap: anywhere;
  }
  .is-danger {
    color: var(--danger);
    font-weight: 650;
  }
</style>
