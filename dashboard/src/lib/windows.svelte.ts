// The two windows a dashboard password closes until the owner opens them
// (ADR 0029): the ESPHome API for Home Assistant, and Join for the satellite
// group. Both come from GetSecurity, which needs no session; opening one
// does. The seconds left count down on this page's clock (`device.now`)
// between reads.
import type { Security } from "../gen/pinna/v1/device_pb.ts";
import { client, device, errorSentence } from "./device.svelte.ts";
import { toasts } from "./toast.svelte.ts";

const EVERY_MS = 15_000;
const READ_TIMEOUT_MS = 10_000;

class Windows {
  /** The access state as the satellite last answered it; null until it has. */
  security = $state.raw<Security | null>(null);
  /** When `security` arrived (Date.now()), for the countdown. */
  receivedAt = $state(0);
  /** Why a window couldn't be opened, until the next try. */
  error = $state<string | null>(null);
  /** A window is being opened. */
  opening = $state(false);

  #users = 0;
  #timer: ReturnType<typeof setInterval> | undefined;
  #visible = () => {
    if (document.visibilityState === "visible") void this.load();
  };

  get passwordSet() {
    return !!this.security?.passwordSet;
  }

  /** Seconds left of the window Home Assistant needs to add this satellite. */
  get homeAssistantLeft() {
    return this.#left(this.security?.homeAssistantWindowLeftS ?? 0);
  }

  /** Seconds left in which another satellite may add this one to its group. */
  get groupJoinLeft() {
    return this.#left(this.security?.groupJoinLeftS ?? 0);
  }

  #left(seconds: number) {
    return Math.max(0, seconds - Math.floor((device.now - this.receivedAt) / 1000));
  }

  #take(security: Security | undefined) {
    if (!security) return;
    this.security = security;
    this.receivedAt = Date.now();
  }

  async load() {
    try {
      this.#take((await client.getSecurity({}, { timeoutMs: READ_TIMEOUT_MS })).security);
    } catch {
      // The pages that show it have their own word for an unreachable satellite.
    }
  }

  /** Read for one page; returns the stop for an $effect. */
  watch(): () => void {
    if (this.#users++ === 0) {
      void this.load();
      this.#timer = setInterval(() => {
        if (document.visibilityState === "visible") void this.load();
      }, EVERY_MS);
      document.addEventListener("visibilitychange", this.#visible);
    }
    return () => {
      if (--this.#users > 0) return;
      clearInterval(this.#timer);
      document.removeEventListener("visibilitychange", this.#visible);
      this.error = null;
    };
  }

  async #open(run: () => Promise<{ security?: Security }>, done: string) {
    if (this.opening) return;
    this.opening = true;
    this.error = null;
    try {
      this.#take((await run()).security);
      toasts.show(done);
    } catch (e) {
      this.error = errorSentence(e);
    } finally {
      this.opening = false;
    }
  }

  /** Let Home Assistant add this satellite for 15 minutes. */
  openHomeAssistant() {
    return this.#open(() => client.openHomeAssistantWindow({}), "Home Assistant can add this satellite for 15 minutes");
  }

  /** Let another satellite add this one to its group for 10 minutes. */
  allowGroupJoin() {
    return this.#open(() => client.allowGroupJoin({}), "Another satellite can add this one for 10 minutes");
  }
}

export const windows = new Windows();
