<!-- Voice & sound › Wake word: the sensitivity, and the recent wakes and near
     misses with the setting that would have ignored or caught each one. -->
<script lang="ts">
  import Device from "../lib/Device.svelte";
  import { device } from "../lib/device.svelte.ts";
  import Group from "../lib/Group.svelte";
  import Slider from "../lib/Slider.svelte";
  import {
    calibrationAdvice,
    calibrationBlocked,
    scoreText,
    thresholdFor,
    wakeAdvice,
    wakeHint,
    when,
  } from "../lib/view.ts";

  const s = $derived(device.status);
  const set = $derived(s?.settings);
  const live = $derived(device.link === "live");
  const phrase = $derived(s?.wakeWord?.phrase || "the wake word");
  // pinnad sends the oldest first.
  const recent = $derived([...(s?.wakeWord?.recent ?? [])].reverse());
  // Which word woke it, as Home Assistant's "Last wake word" says.
  const heard = (e: { woke: boolean; phrase: string }) =>
    e.woke ? (e.phrase ? `Woke on "${e.phrase}"` : "Woke") : "Almost woke";
  const now = $derived(new Date(device.now));

  const cal = $derived(s?.wakeWord?.calibration);
  const events = $derived([...(cal?.events ?? [])].reverse());
  const blocked = $derived(calibrationBlocked(s, device.link));
  const calHint = $derived(
    cal
      ? `Running. Say "${phrase}" from where you usually do; press + when it misses you, − when it wakes by itself. Done, the Action button, muting, or 5 minutes without a press ends it.`
      : (blocked ??
        `Press Calibrate, then press + on the satellite when it misses you and − when it wakes by itself, within 15 seconds. Outside a calibration they change the volume.`),
  );

  // While the slider is held, the hint and the list follow it.
  let held = $state<number | null>(null);
  const sensitivity = $derived(held ?? set?.wakeSensitivity ?? 0);

  // The last change made from a row, so that row's button can take it back.
  let changed = $state<{ key: string; from: number; to: number } | null>(null);

  async function setFrom(key: string, to: number) {
    if (!set) return;
    // Before the reply: the row's button must not go away in between.
    changed = { key, from: set.wakeSensitivity, to };
    await device.updateSettings({ wakeSensitivity: to });
    if (device.status?.settings?.wakeSensitivity !== to) changed = null;
  }

  async function back(from: number) {
    await device.updateSettings({ wakeSensitivity: from });
    changed = null;
  }
</script>

{#if s && set}
  <Group>
    <div class="pn-row">
      <div class="pn-row-text">
        <span class="pn-row-label">Wake word</span>
        <span class="pn-row-hint">{wakeHint(s)}</span>
      </div>
    </div>
    <div class="pn-row is-stack">
      <Slider
        label="Sensitivity"
        value={set.wakeSensitivity}
        disabled={!live}
        hint="Wakes at a score of {scoreText(
          thresholdFor(sensitivity),
          true,
        )} or more. Higher wakes more easily, and more often by mistake."
        commit={(v) => device.updateSettings({ wakeSensitivity: v })}
        preview={(v) => (held = v)}
      />
    </div>
    <div class="pn-row cal-row">
      <div class="pn-row-text">
        <span class="pn-row-label">Calibration</span>
        <span id="cal-hint" class="pn-row-hint">{calHint}</span>
      </div>
      <span class="cal-actions">
        {#if cal}
          <!-- aria-disabled, not disabled: disabling the pressed button would drop the focus. -->
          <button
            class="pn-btn pn-btn-sm"
            disabled={!live}
            aria-disabled={device.busy || set.wakeSensitivity === cal.startedFrom || undefined}
            onclick={() => {
              if (!device.busy && set.wakeSensitivity !== cal.startedFrom)
                void device.updateSettings({ wakeSensitivity: cal.startedFrom });
            }}
            >Back to {cal.startedFrom}</button
          >
        {/if}
        <!-- One button for both, so focus stays on it as the label changes. -->
        <button
          class="pn-btn"
          disabled={!live || (!cal && blocked !== null)}
          aria-disabled={device.busy || undefined}
          aria-describedby="cal-hint"
          onclick={() => {
            if (!device.busy) void (cal ? device.stopCalibration() : device.startCalibration());
          }}>{cal ? "Done" : "Calibrate"}</button
        >
      </span>
    </div>
    {#snippet foot()}Home Assistant's Wake word sensitivity is the same setting.{/snippet}
  </Group>

  {#if cal}
    <Group title="Calibration">
      {#snippet aside()}{#if events.length}{events.length}{/if}{/snippet}
      {#if events.length}
        {@const latest = calibrationAdvice(events[0])}
        <!-- Announce each new event; the list itself is read in order. -->
        <p class="sr" role="status">Calibration: {latest.label} — {latest.text}</p>
        {#each events as e (e.id)}
          {@const a = calibrationAdvice(e)}
          <div class="pn-row">
            <div class="pn-row-text">
              <span class="pn-row-label">{a.label}</span>
              <span class="pn-row-hint">{a.text}</span>
            </div>
          </div>
        {/each}
      {:else}
        <div class="pn-row">
          <div class="pn-row-text">
            <span class="pn-row-label">Nothing yet</span>
            <span class="pn-row-hint">Say "{phrase}" from where you usually do, and press + or − on the satellite.</span>
          </div>
        </div>
      {/if}
      {#snippet foot()}Started at sensitivity {cal.startedFrom}. Newest first; it clears when the calibration ends.{/snippet}
    </Group>
  {/if}

  <Group title="Recent wakes">
    {#snippet aside()}{#if recent.length}{recent.length}{/if}{/snippet}
    {#if recent.length}
      {#each recent as e (`${e.unixMs}-${e.woke}`)}
        {@const key = `${e.unixMs}-${e.woke}`}
        {@const a = wakeAdvice(e, sensitivity, recent)}
        {@const undo = changed?.key === key && set.wakeSensitivity === changed.to ? changed : null}
        <div class="pn-row">
          <div class="pn-row-text">
            <span class="pn-row-label">{heard(e)} · {when(Number(e.unixMs), now)}</span>
            <span id="wk-{key}" class="pn-row-hint">{a.text}</span>
          </div>
          <!-- One button for both, so focus stays on it when it turns into Back. -->
          {#if undo || a.set !== undefined}
            <!-- aria-disabled while busy: disabling it would drop the focus. -->
            <button
              class="pn-btn pn-btn-sm"
              disabled={!live}
              aria-disabled={device.busy || undefined}
              aria-describedby="wk-{key}"
              onclick={() => {
                if (!device.busy) void (undo ? back(undo.from) : setFrom(key, a.set!));
              }}
              >{undo ? `Back to ${undo.from}` : `Set to ${a.set}`}</button
            >
          {/if}
        </div>
      {/each}
    {:else}
      <div class="pn-empty">
        <Device model={s.model} effect="idle" width={112} label="Ring off" />
        <span class="pn-empty-title">Nothing yet</span>
        <p>
          {s.wakeWord?.enabled
            ? `Say "${phrase}" from where you usually do. Each try shows up here with its score.`
            : wakeHint(s)}
        </p>
      </div>
    {/if}
    {#snippet foot()}
      The score is how sure the satellite was that it heard "{phrase}", from 0 to 1. Only scores and the wake word's name
      are kept, not what was said, and the list starts again when the satellite restarts.
    {/snippet}
  </Group>
{/if}

<style>
  .cal-actions {
    display: flex;
    flex: none;
    align-items: center;
    gap: 12px;
  }
  /* On phones the instructions get the full width, with the buttons below. */
  @media (max-width: 560px) {
    .cal-row {
      display: block;
      padding: 14px 16px 12px;
    }
    .cal-actions {
      margin-top: 10px;
    }
  }
  .sr {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip-path: inset(50%);
    white-space: nowrap;
  }
</style>
