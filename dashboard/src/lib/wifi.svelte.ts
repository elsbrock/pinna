// Wi-Fi networks (ADR 0008): the saved ones, a join and its steps, and a
// network on trial, which the satellite keeps only once someone keeps it or
// opens the dashboard at its new address. ListWifi is read every few seconds
// while a join or a trial runs, and now and then otherwise, while some page
// shows it.
import { Code, ConnectError } from "@connectrpc/connect";

import type {
  JoinWifiRequest,
  JoinWifiResponse,
  SavedWifi,
  WifiNetwork,
} from "../gen/pinna/v1/device_pb.ts";
import { client, errorSentence } from "./device.svelte.ts";
import { toasts } from "./toast.svelte.ts";
import { joinDone } from "./view.ts";

const BUSY_MS = 3_000;
const IDLE_MS = 30_000;

class Wifi {
  saved = $state<SavedWifi[] | null>(null);
  /** A join runs on the satellite (this page's, or another one's). */
  joining = $state(false);
  /** The network on trial, and when its `keepSeconds` were read. */
  trial = $state<JoinWifiResponse | null>(null);
  trialAt = $state(0);
  /** How the last join ended, since pinnad started. */
  last = $state<JoinWifiResponse | null>(null);
  /** No Wi-Fi to manage, and why (a VM; an Echo whose chip didn't start). */
  absent = $state<string | null>(null);
  error = $state<string | null>(null);
  /** This page's join: its steps so far. */
  steps = $state<JoinWifiResponse[]>([]);
  /** This page lost the satellite during its join. */
  lost = $state<string | null>(null);
  busy = $state(false);

  #users = 0;
  #timer: ReturnType<typeof setTimeout> | undefined;

  async refresh() {
    try {
      const r = await client.listWifi({}, { timeoutMs: 8_000 });
      this.saved = r.saved;
      this.joining = r.joining;
      this.trial = r.trial ?? null;
      this.trialAt = Date.now();
      this.last = r.lastJoin ?? null;
      this.absent = null;
      this.error = null;
      if (this.trial || this.last) this.lost = null;
    } catch (e) {
      const err = ConnectError.from(e);
      if (err.code === Code.FailedPrecondition) this.absent = err.rawMessage;
      else this.error = errorSentence(e);
    }
  }

  #schedule() {
    clearTimeout(this.#timer);
    if (!this.#users) return;
    const every = this.joining || this.trial || this.lost ? BUSY_MS : IDLE_MS;
    this.#timer = setTimeout(async () => {
      if (document.visibilityState === "visible") await this.refresh();
      this.#schedule();
    }, every);
  }

  /** Start reading for one user; returns the stop for an $effect. */
  watch(): () => void {
    if (this.#users++ === 0) void this.refresh().then(() => this.#schedule());
    return () => {
      if (--this.#users === 0) clearTimeout(this.#timer);
    };
  }

  /** Seconds left to keep the network on trial. */
  keepLeft(now: number): number {
    if (!this.trial) return 0;
    return Math.max(0, this.trial.keepSeconds - Math.floor((now - this.trialAt) / 1000));
  }

  async scan(): Promise<WifiNetwork[] | string> {
    try {
      return (await client.scanWifi({}, { timeoutMs: 20_000 })).networks;
    } catch (e) {
      return errorSentence(e);
    }
  }

  /** Try a network; resolves to a sentence when it was refused outright. */
  async join(req: Partial<Omit<JoinWifiRequest, "$typeName">>): Promise<string | null> {
    this.steps = [];
    this.lost = null;
    this.joining = true;
    this.#schedule();
    try {
      for await (const step of client.joinWifi(req)) {
        this.steps = [...this.steps, step];
        if (joinDone(step)) break;
      }
    } catch (e) {
      // Nothing changed when it was refused before the first step.
      if (this.steps.length === 0) {
        this.joining = false;
        return errorSentence(e);
      }
      // The satellite left this network mid-join, most likely.
      this.lost = req.ssid ?? "";
    } finally {
      await this.refresh();
      this.#schedule();
    }
    return null;
  }

  async keep(keep: boolean) {
    this.busy = true;
    try {
      const r = await client.keepWifi({ keep });
      if (r.result) toasts.show(r.result.message, keep ? "ok" : "info");
    } catch (e) {
      toasts.show(errorSentence(e), "danger");
    } finally {
      this.busy = false;
      this.steps = [];
      await this.refresh();
    }
  }

  async forget(ssid: string) {
    this.busy = true;
    try {
      this.saved = (await client.forgetWifi({ ssid })).saved;
      toasts.show(`Forgot ${ssid}`);
    } catch (e) {
      toasts.show(errorSentence(e), "danger");
    } finally {
      this.busy = false;
    }
  }
}

export const wifi = new Wifi();
