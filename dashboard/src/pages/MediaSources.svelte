<!-- Voice & sound › Media sources: what may play music to the satellite
     (ADR 0011). Each source is off until switched on here or in Home
     Assistant; while it is on, the page says whether it actually runs. -->
<script lang="ts">
  import { AirPlayState, BluetoothState } from "../gen/pinna/v1/device_pb.ts";
  import Confirm from "../lib/Confirm.svelte";
  import { device } from "../lib/device.svelte.ts";
  import Group from "../lib/Group.svelte";
  import { network } from "../lib/network.svelte.ts";
  import SwitchRow from "../lib/SwitchRow.svelte";

  const s = $derived(device.status);
  const set = $derived(s?.settings);
  const live = $derived(device.link === "live");
  const airplay = $derived(s?.airplay);
  const unavailable = $derived(airplay?.state === AirPlayState.AIRPLAY_STATE_UNAVAILABLE);
  const hint = $derived(
    unavailable
      ? "This satellite has no working speaker, so it can't receive AirPlay. Logs, under System, says why."
      : "Play from an iPhone, iPad, Mac or Music Assistant. While it is on, anyone on your network can play to it without a password, and replace what it is playing.",
  );

  const bt = $derived(s?.bluetooth);
  const btUnavailable = $derived(bt?.state === BluetoothState.UNAVAILABLE);
  const btReady = $derived(bt?.state === BluetoothState.READY);
  const btFailed = $derived(bt?.state === BluetoothState.FAILED);
  const btHint = $derived(
    btUnavailable
      ? "This satellite has no Bluetooth, or no working speaker. Logs, under System, says why."
      : "Play from a phone or tablet nearby. Only phones paired with this satellite can connect.",
  );
  // Seconds left in the pairing window, counted from when the Status arrived,
  // as timers are: the satellite's clock and this one's don't matter.
  const pairingLeft = $derived(
    Math.max(0, (bt?.pairingSecondsLeft ?? 0) - Math.floor((device.now - device.receivedAt) / 1000)),
  );
  const pairing = $derived(btReady && pairingLeft > 0);
  const mmss = (s: number) => `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`;
  const phones = $derived(bt?.devices.length ?? 0);
  // Bluetooth shares the antenna with the Wi-Fi; on 2.4 GHz that shows.
  $effect(() => {
    if (set?.bluetooth) return network.watch();
  });
  const mhz = $derived(network.wireless?.wifi?.frequencyMhz ?? 0);
  const onTwoFour = $derived(mhz > 0 && mhz < 3000);
  // What a screen reader hears: once per change, not every second.
  const btSpoken = $derived(
    !set?.bluetooth || !bt
      ? ""
      : pairing
        ? `Ready to pair as “${bt.name}”.`
        : btReady
          ? `Ready as “${bt.name}”. ${phones === 1 ? "One phone" : `${phones} phones`} paired.`
          : btFailed
            ? "Bluetooth isn't running."
            : "Bluetooth is starting.",
  );
  // systemd's results (exit-code, signal, …) mean nothing to people.
  const SYSTEMD = ["exit-code", "signal", "core-dump", "watchdog", "start-limit-hit", "timeout", "resources", "systemctl"];
  function btReason(r: string): string {
    if (!r || SYSTEMD.includes(r)) return "Bluetooth stopped unexpectedly.";
    const t = r.charAt(0).toUpperCase() + r.slice(1);
    return /[.!?]$/.test(t) ? t : `${t}.`;
  }
  // Two quick presses would open the window and close it again.
  let lastPress = 0;
  function pressPairing() {
    const now = Date.now();
    if (device.busy || now - lastPress < 1000) return;
    lastPress = now;
    void (pairing ? device.stopBluetoothPairing() : device.startBluetoothPairing());
  }
  let forgetting = $state<{ address: string; name: string } | null>(null);
  let forgetOpen = $state(false);
  async function forget() {
    if (!forgetting) return;
    const { address, name } = forgetting;
    await device.forgetBluetoothDevice(address, name);
    // Its row is gone: keep the focus on the page.
    document.getElementById("bt-pair")?.focus();
  }
</script>

{#if s && set}
  <Group title="AirPlay">
    {#snippet foot()}
      It plays on its own: it can't join a group with HomePods or other AirPlay 2 speakers. The phone or computer sets
      the music's volume; Volume, on Voice &amp; sound, sets how loud the satellite is overall. AirPlay is also a
      switch in Home Assistant, on this satellite's device page.
    {/snippet}
    <SwitchRow
      id="airplay"
      label="AirPlay"
      checked={set.airplay}
      disabled={!live || unavailable}
      busy={device.busy}
      {hint}
      toggle={(on) => device.updateSettings({ airplay: on })}
    />
    {#if set.airplay && airplay}
      <div class="pn-row" aria-live="polite">
        <div class="pn-row-text">
          {#if airplay.state === AirPlayState.AIRPLAY_STATE_READY}
            <span class="pn-row-label">Ready as “{airplay.name}”</span>
            <span class="pn-row-hint">
              On an iPhone or Mac, open AirPlay and choose “{airplay.name}”. Not in the list? The phone has to be on
              the same network as the satellite: a guest Wi-Fi or a separate VLAN hides it.
            </span>
          {:else if airplay.state === AirPlayState.AIRPLAY_STATE_FAILED}
            <span class="pn-row-label">Not running</span>
            <span class="pn-row-hint is-danger">AirPlay didn't start. Logs, under System, says why.</span>
          {:else}
            <span class="pn-row-label">Starting…</span>
            <span class="pn-row-hint">The receiver takes a few seconds to start.</span>
          {/if}
        </div>
      </div>
    {/if}
  </Group>

  <Group title="Bluetooth">
    {#snippet foot()}
      The phone sets the music's volume; Volume, on Voice &amp; sound, sets how loud the satellite is overall. Bluetooth
      audio and Bluetooth pairing are also switches in Home Assistant, on this satellite's device page.
    {/snippet}
    <SwitchRow
      id="bluetooth"
      label="Bluetooth"
      checked={set.bluetooth}
      disabled={!live || btUnavailable}
      busy={device.busy}
      hint={btHint}
      toggle={(on) => device.updateSettings({ bluetooth: on })}
    />
    <p class="sr" role="status">{btSpoken}</p>
    {#if set.bluetooth && bt}
      <div class="pn-row">
        <div class="pn-row-text">
          {#if pairing}
            <span class="pn-row-label">Ready to pair as “{bt.name}”</span>
            <span id="bt-hint" class="pn-row-hint">
              On the phone, open Bluetooth settings and choose “{bt.name}”. Open for {mmss(pairingLeft)} more, or until a
              phone pairs. Anyone nearby can pair while it is open.
            </span>
          {:else if btReady}
            <span class="pn-row-label">Ready as “{bt.name}”</span>
            <span id="bt-hint" class="pn-row-hint">
              {#if phones === 0}
                No phone paired yet. Phones can't see “{bt.name}” until you select Pair a phone.
              {:else}
                A paired phone connects from its Bluetooth settings. To add another, select Pair a phone.
              {/if}
            </span>
          {:else if btFailed}
            <span class="pn-row-label">Not running</span>
            <span id="bt-hint" class="pn-row-hint is-danger">{btReason(bt.reason)} Logs, under System, shows why.</span>
          {:else}
            <span class="pn-row-label">Starting…</span>
            <span class="pn-row-hint">Bluetooth takes a few seconds to start.</span>
          {/if}
        </div>
        {#if btReady}
          <!-- aria-disabled, not disabled: disabling the pressed button would drop the focus. -->
          <button
            id="bt-pair"
            class="pn-btn"
            disabled={!live}
            aria-disabled={device.busy || undefined}
            aria-describedby="bt-hint"
            onclick={pressPairing}>{pairing ? "Stop pairing" : "Pair a phone"}</button
          >
        {:else if btFailed}
          <button
            class="pn-btn"
            disabled={!live}
            aria-disabled={device.busy || undefined}
            aria-describedby="bt-hint"
            onclick={() => {
              if (!device.busy) void device.updateSettings({ bluetooth: true });
            }}>Try again</button
          >
        {/if}
      </div>
      {#if onTwoFour}
        <div class="pn-row">
          <div class="pn-row-text">
            <span class="pn-row-label">2.4 GHz Wi-Fi</span>
            <span class="pn-row-hint">
              Bluetooth shares the satellite's antenna with the Wi-Fi: while music plays over Bluetooth, voice replies
              start a little later and updates can stall. A 5 GHz Wi-Fi avoids that.
            </span>
          </div>
        </div>
      {/if}
      {#each bt.devices as d (d.address)}
        <div class="pn-row">
          <div class="pn-row-text">
            <span class="pn-row-label">{d.name}</span>
            <span class="pn-row-hint">{d.connected ? "Connected" : "Paired"}</span>
          </div>
          <button
            class="pn-btn pn-btn-quiet pn-btn-sm"
            disabled={!live}
            aria-disabled={device.busy || undefined}
            onclick={() => {
              if (device.busy) return;
              forgetting = { address: d.address, name: d.name };
              forgetOpen = true;
            }}
            aria-label="Forget {d.name}"
          >
            Forget
          </button>
        </div>
      {/each}
    {/if}
  </Group>
  <Confirm bind:open={forgetOpen} title="Forget {forgetting?.name}?" action="Forget" danger confirm={forget}>
    <p>
      It can't connect or play any more. To pair it again, also remove “{bt?.name}” from its Bluetooth settings, then
      select Pair a phone.
    </p>
  </Confirm>
{:else}
  <span class="pn-skeleton" style:height="120px"></span>
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
  .is-danger {
    color: var(--danger);
    font-weight: 650;
  }
</style>
