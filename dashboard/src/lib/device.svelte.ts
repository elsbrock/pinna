// The satellite as the page sees it: the latest Status from WatchStatus, the
// state of that connection, and the commands. Served from the satellite itself,
// so the API is on this page's own origin.
import { ConnectError, createClient } from "@connectrpc/connect";
import { createConnectTransport } from "@connectrpc/connect-web";

import { DeviceService, type Status, type UpdateSettingsRequest } from "../gen/pinna/v1/device_pb.ts";
import { toasts } from "./toast.svelte.ts";
import type { Link } from "./view.ts";

const client = createClient(DeviceService, createConnectTransport({ baseUrl: location.origin }));

// WatchStatus only sends on change, so a satellite that drops off Wi-Fi leaves
// a quiet but open stream. A cheap GetStatus every PING_MS tells the two apart.
const PING_MS = 10_000;
const PING_TIMEOUT_MS = 5_000;
const BACKOFF_MS = [1_000, 2_000, 5_000];

class Device {
  status = $state<Status | null>(null);
  /** When `status` arrived (Date.now()), for ticking uptime and timers. */
  receivedAt = $state(0);
  link = $state<Link>("connecting");
  offlineSince = $state<Date | null>(null);
  busy = $state(false);
  /** A restart was asked for: being unreachable is expected, not an error. */
  restarting = $state(false);
  /** A one-second clock for everything that counts. */
  now = $state(Date.now());

  #stream: AbortController | null = null;
  #wake: (() => void) | null = null;
  #version: string | null = null;

  start() {
    setInterval(() => (this.now = Date.now()), 1_000);
    setInterval(() => this.#ping(), PING_MS);
    const resume = () => {
      if (document.visibilityState !== "visible") return;
      if (this.link === "live") this.#ping();
      else this.#wake?.();
    };
    document.addEventListener("visibilitychange", resume);
    addEventListener("online", resume);
    void this.#run();
  }

  #accept(s: Status | undefined) {
    if (!s) return;
    // A new image after an update: its page may differ from this one.
    if (this.#version && s.version !== this.#version) location.reload();
    this.#version ??= s.version;
    this.status = s;
    this.receivedAt = Date.now();
    this.link = "live";
    this.restarting = false;
    this.offlineSince = null;
  }

  #offline() {
    if (this.link !== "offline") this.offlineSince = new Date();
    this.link = "offline";
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
      } catch {
        // Unreachable, pinnad restarting, or aborted by a failed ping.
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
    } catch {
      this.#offline();
      this.#stream?.abort();
    }
  }

  /** Run a command; its reply's Status wins. A rejection shows as a toast. */
  async #command(run: () => Promise<{ status?: Status }>, done?: string) {
    this.busy = true;
    try {
      this.#accept((await run()).status);
      if (done) toasts.show(done);
    } catch (e) {
      toasts.show(sentence(ConnectError.from(e).rawMessage), "danger");
    } finally {
      this.busy = false;
    }
  }

  setVolume(percent: number) {
    return this.#command(() => client.setVolume({ percent }));
  }
  setMicrophones(on: boolean) {
    return this.#command(() => client.setMute({ muted: !on }));
  }
  updateSettings(patch: Partial<Omit<UpdateSettingsRequest, "$typeName">>) {
    return this.#command(() => client.updateSettings(patch), "Saved");
  }
  startConversation() {
    return this.#command(() => client.startConversation({}));
  }
  stop() {
    return this.#command(() => client.stopConversation({}));
  }
  /** Run the ring's test pattern, to find this satellite among others. */
  async identify() {
    try {
      await client.ledTest({});
      toasts.show("The ring runs its test pattern.", "info");
    } catch (e) {
      toasts.show(sentence(ConnectError.from(e).rawMessage), "danger");
    }
  }
  /** Ask for a restart; returns why it failed, for the caller to show. */
  async restart(): Promise<string | null> {
    this.busy = true;
    this.restarting = true;
    try {
      await client.reboot({});
      return null;
    } catch (e) {
      this.restarting = false;
      return sentence(ConnectError.from(e).rawMessage);
    } finally {
      this.busy = false;
    }
  }
}

/** The API's errors are lowercase fragments ("the microphone is muted"). */
const sentence = (m: string) => (m ? m[0].toUpperCase() + m.slice(1).replace(/\.?$/, ".") : "The satellite refused.");

export const device = new Device();
export { client };
