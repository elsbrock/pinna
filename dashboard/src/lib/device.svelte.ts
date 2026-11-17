// The satellite as the page sees it: the latest Status from WatchStatus, the
// state of that connection, and the commands. Served from the satellite itself,
// so the API is on this page's own origin.
import { Code, ConnectError, createClient } from "@connectrpc/connect";
import { createConnectTransport } from "@connectrpc/connect-web";

import type { Frame } from "../../../design/ring.ts";
import {
  DeviceService,
  type ListSatellitesResponse,
  type MediaCommand,
  type SetRingLightRequest,
  type Status,
  type UpdateSettingsRequest,
} from "../gen/pinna/v1/device_pb.ts";
import { frameFromBytes } from "./ledframe.ts";
import { toasts } from "./toast.svelte.ts";
import { LED_TEST_STEP_MS, type Link } from "./view.ts";

const client = createClient(DeviceService, createConnectTransport({ baseUrl: location.origin }));

// WatchStatus only sends on change, so a satellite that drops off Wi-Fi leaves
// a quiet but open stream. A cheap GetStatus every PING_MS tells the two apart.
const PING_MS = 10_000;
const PING_TIMEOUT_MS = 5_000;
const BACKOFF_MS = [1_000, 2_000, 5_000];
/** WatchRing ended while WatchStatus did not: ask again after this long. */
const RING_RETRY_MS = 5_000;

class Device {
  status = $state<Status | null>(null);
  /** When `status` arrived (Date.now()), for ticking uptime and timers. */
  receivedAt = $state(0);
  link = $state<Link>("connecting");
  offlineSince = $state<Date | null>(null);
  busy = $state(false);
  /** Identify was pressed a moment ago: more presses wait. */
  identifying = $state(false);
  /** The step of the ring's test pattern while `status.ledTest`, for ringEffect. */
  testStep = $state(0);
  /** The ring as the satellite shows it (WatchRing), while live; null before
   *  its first frame. Kept, not followed, while this tab is hidden. */
  ring = $state.raw<Frame | null>(null);
  /** The media failure (error and URL) hidden on Now playing while something
   *  still plays, until the next play from here or until it clears. */
  mediaErrorDismissed = $state("");
  /** A restart was asked for: being unreachable is expected, not an error. */
  restarting = $state(false);
  /** A factory reset was asked for: the satellite may come back over USB only. */
  resetting = $state(false);
  /** A one-second clock for everything that counts. */
  now = $state(Date.now());
  /** The satellite answers, but not to the name this page is open by: it was
   *  its host isn't the dashboard address any more (ADR 0021). */
  misdirected = $state(false);

  #stream: AbortController | null = null;
  #wake: (() => void) | null = null;
  #version: string | null = null;
  #testTimer: ReturnType<typeof setInterval> | undefined;
  #ringStream: AbortController | null = null;

  start() {
    setInterval(() => (this.now = Date.now()), 1_000);
    setInterval(() => this.#ping(), PING_MS);
    const resume = () => {
      if (document.visibilityState !== "visible") return this.#stopRing(true);
      if (this.link === "live") {
        this.#ping();
        this.#watchRing();
      } else this.#wake?.();
    };
    document.addEventListener("visibilitychange", resume);
    addEventListener("online", resume);
    void this.#run();
  }

  #accept(s: Status | undefined) {
    if (!s) return;
    this.misdirected = false;
    // A new image after an update: its page may differ from this one.
    if (this.#version && s.version !== this.#version) location.reload();
    this.#version ??= s.version;
    // The test pattern started: step through it on the device's beat.
    if (s.ledTest && !this.status?.ledTest) {
      this.testStep = 0;
      clearInterval(this.#testTimer);
      this.#testTimer = setInterval(() => this.testStep++, LED_TEST_STEP_MS);
    } else if (!s.ledTest) clearInterval(this.#testTimer);
    this.status = s;
    this.receivedAt = Date.now();
    this.link = "live";
    this.restarting = false;
    this.resetting = false;
    this.offlineSince = null;
    this.#watchRing();
  }

  /** Follow the ring while the satellite is live and this tab is seen. */
  #watchRing() {
    if (this.#ringStream || this.link !== "live" || document.visibilityState !== "visible") return;
    const stream = (this.#ringStream = new AbortController());
    void (async () => {
      try {
        for await (const res of client.watchRing({}, { signal: stream.signal })) {
          this.ring = frameFromBytes(res.frame) ?? this.ring;
        }
      } catch {
        // Unreachable or aborted: WatchStatus and the ping tell which.
      }
      if (this.#ringStream !== stream) return;
      this.#stopRing();
      setTimeout(() => this.#watchRing(), RING_RETRY_MS);
    })();
  }

  /** `keep`: leave the last frame up, for a tab that is only hidden. */
  #stopRing(keep = false) {
    this.#ringStream?.abort();
    this.#ringStream = null;
    if (!keep) this.ring = null;
  }

  /** 421 Misdirected Request: the satellite refuses this page's host name. */
  #misdirected(e: unknown) {
    if (ConnectError.from(e).rawMessage === "HTTP 421") this.misdirected = true;
  }

  #offline() {
    if (this.link !== "offline") this.offlineSince = new Date();
    this.link = "offline";
    this.#stopRing();
  }

  async #run() {
    for (let attempt = 0; ; attempt++) {
      const stream = (this.#stream = new AbortController());
      try {
        // The current state first, then one message per change.
        for await (const res of client.watchStatus({}, { signal: stream.signal })) {
          this.#accept(res.status);
          attempt = 0;
        }
      } catch (e) {
        // Unreachable, pinnad restarting, or aborted by a failed ping.
        this.#misdirected(e);
      }
      this.#offline();
      const delay = BACKOFF_MS[Math.min(attempt, BACKOFF_MS.length - 1)];
      await new Promise<void>((resolve) => {
        const timer = setTimeout(resolve, delay);
        this.#wake = () => {
          clearTimeout(timer);
          resolve();
        };
      });
      this.#wake = null;
    }
  }

  async #ping() {
    if (this.link !== "live" || document.visibilityState !== "visible") return;
    try {
      await client.getStatus({}, { timeoutMs: PING_TIMEOUT_MS });
    } catch (e) {
      this.#misdirected(e);
      this.#offline();
      this.#stream?.abort();
    }
  }

  /** Run a command; its reply's Status wins. A rejection shows as a toast,
   *  or, `inline`, only comes back for a form to show. Resolves to the
   *  rejection's sentence, or null. Without `replyStatus`, the reply's Status
   *  is ignored: it was taken before the change finished, and arriving after
   *  WatchStatus's newer one, it would put the old state back. */
  async #command(
    run: () => Promise<{ status?: Status }>,
    done?: string,
    inline = false,
    replyStatus = true,
  ): Promise<string | null> {
    this.busy = true;
    try {
      const reply = await run();
      if (replyStatus) this.#accept(reply.status);
      if (done) toasts.show(done);
      return null;
    } catch (e) {
      const why = errorSentence(e);
      if (!inline) toasts.show(why, "danger");
      // The satellite may be gone: find out now, not at the next ping.
      if (unreachable(e)) void this.#ping();
      return why;
    } finally {
      this.busy = false;
    }
  }

  setVolume(percent: number) {
    return this.#command(() => client.setVolume({ percent }), "Saved");
  }
  setMicrophones(on: boolean) {
    return this.#command(() => client.setMute({ muted: !on }), on ? "Microphones on" : "Microphones off");
  }
  updateSettings(patch: Partial<Omit<UpdateSettingsRequest, "$typeName">>) {
    return this.#command(() => client.updateSettings(patch), "Saved");
  }
  /** updateSettings for a form: a rejection comes back to show by the field. */
  saveSettings(patch: Partial<Omit<UpdateSettingsRequest, "$typeName">>, done = "Saved") {
    return this.#command(() => client.updateSettings(patch), done, true);
  }
  /** The IANA zones the satellite has. */
  async listTimeZones(): Promise<string[]> {
    return (await client.listTimeZones({})).zones;
  }
  /** The other satellites on the network (ADR 0021); takes up to a second and a half. */
  listSatellites(): Promise<ListSatellitesResponse> {
    return client.listSatellites({});
  }
  startConversation() {
    return this.#command(() => client.startConversation({}));
  }
  stop() {
    return this.#command(() => client.stopConversation({}));
  }
  /** Run the ring's test pattern, to find this satellite among others. */
  async identify() {
    if (this.identifying) return;
    this.identifying = true;
    setTimeout(() => (this.identifying = false), 3_000);
    try {
      await client.ledTest({});
      toasts.show("The ring runs its test pattern.", "info");
    } catch (e) {
      toasts.show(errorSentence(e), "danger");
    }
  }
  /** The ring as a light, like Home Assistant's "LED ring" light. */
  setRingLight(patch: Partial<Omit<SetRingLightRequest, "$typeName">>) {
    return this.#command(() => client.setRingLight(patch));
  }
  /** Resume, pause, stop, mute or unmute the media player. */
  // The media lane reports its new state after the reply: WatchStatus brings it.
  controlMedia(command: MediaCommand) {
    return this.#command(() => client.controlMedia({ command }), undefined, false, false);
  }
  /** The media player's volume: a stream's own while AirPlay or Bluetooth plays. */
  setMediaVolume(volume: number) {
    return this.#command(() => client.controlMedia({ volume }), undefined, false, false);
  }
  /** Play a URL; a rejection comes back for the form to show. Whether it
   *  plays shows in Status.media, which the page follows. */
  playMedia(url: string) {
    this.mediaErrorDismissed = "";
    return this.#command(() => client.playMedia({ url }), undefined, true, false);
  }
  clearNotification() {
    return this.#command(() => client.clearNotification({}), "Notification cleared");
  }
  /** Tune the wake word sensitivity with the satellite's + and − buttons. */
  startCalibration() {
    return this.#command(() => client.startCalibration({}), "Calibrating");
  }
  startBluetoothPairing() {
    return this.#command(() => client.startBluetoothPairing({}), "Ready to pair");
  }
  stopBluetoothPairing() {
    return this.#command(() => client.stopBluetoothPairing({}), "Pairing stopped");
  }
  forgetBluetoothDevice(address: string, name: string) {
    return this.#command(() => client.forgetBluetoothDevice({ address }), `Forgot ${name}`);
  }
  stopCalibration() {
    return this.#command(() => client.stopCalibration({}), "Calibration done");
  }
  /** Ask for a restart; returns why it failed, for the caller to show. */
  /** Forget everything set on the satellite and restart it (ADR 0010). */
  async factoryReset(name: string): Promise<string | null> {
    this.resetting = true;
    const failed = await this.restart(() => client.factoryReset({ name }));
    if (failed) this.resetting = false;
    return failed;
  }

  async restart(run: () => Promise<unknown> = () => client.reboot({})): Promise<string | null> {
    this.busy = true;
    this.restarting = true;
    try {
      await run();
      return null;
    } catch (e) {
      this.restarting = false;
      return errorSentence(e);
    } finally {
      this.busy = false;
    }
  }
}

/** The API's errors are lowercase fragments ("the microphone is muted"). */
const sentence = (m: string) => (m ? m[0].toUpperCase() + m.slice(1).replace(/\.?$/, ".") : "The satellite refused.");

/** The request never got an answer: no network, or the satellite is down. */
/** No answer came: the request failed, timed out, or a proxy said so. An
 *  `unavailable` with pinnad's own sentence is an answer, and says why. */
function unreachable(e: unknown): boolean {
  const err = ConnectError.from(e);
  return (
    err.code === Code.DeadlineExceeded ||
    /failed to fetch|networkerror|load failed|^HTTP 50[234]$/i.test(err.rawMessage)
  );
}

/** An error in words for the page: the API's own sentence, or that it can't be reached. */
export function errorSentence(e: unknown): string {
  if (unreachable(e)) return "Can't reach the satellite. Check that it has power and Wi-Fi.";
  return sentence(ConnectError.from(e).rawMessage);
}

export const device = new Device();
export { client };
