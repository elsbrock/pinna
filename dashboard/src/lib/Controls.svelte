<!-- What people reach for: volume, microphones, the wake word, and starting
     or stopping a conversation. -->
<script lang="ts">
  import { device } from "./device.svelte.ts";
  import Group from "./Group.svelte";
  import Icon from "./Icon.svelte";
  import Slider from "./Slider.svelte";
  import SwitchRow from "./SwitchRow.svelte";
  import { canStop, micHint, startBlocked, wakeHint } from "./view.ts";

  const s = $derived(device.status);
  const live = $derived(device.link === "live");
  const startReason = $derived(startBlocked(s, device.link));
  const stoppable = $derived(canStop(s, device.link));
</script>

<Group title="Controls">
  <div class="pn-row is-stack">
    <Slider label="Volume" value={s?.volume ?? 0} disabled={!live} commit={(v) => device.setVolume(v)} />
  </div>
  <SwitchRow
    id="mics"
    label="Microphones"
    checked={!!s && !s.muted}
    disabled={!live || device.busy || !!s?.hardwareMuted}
    hint={s ? micHint(s) : undefined}
    toggle={(on) => device.setMicrophones(on)}
  />
  {#if s}
    <SwitchRow
      id="wake"
      label="Wake word"
      checked={!!s.settings?.wakeWord}
      disabled={!live || device.busy}
      hint={wakeHint(s)}
      toggle={(on) => device.updateSettings({ wakeWord: on })}
    />
  {/if}
  <div class="pn-row is-actions">
    <button
      class="pn-btn pn-btn-primary pn-btn-lg"
      disabled={!!startReason || device.busy}
      aria-describedby={startReason && !stoppable ? "start-reason" : undefined}
      onclick={() => device.startConversation()}
    >
      <Icon name="mic" />Start conversation
    </button>
    <button class="pn-btn pn-btn-lg" disabled={!stoppable || device.busy} onclick={() => device.stop()}>Stop</button>
  </div>
  {#if startReason && !stoppable}
    <div class="pn-row"><span id="start-reason" class="pn-row-hint">{startReason}</span></div>
  {/if}
</Group>
