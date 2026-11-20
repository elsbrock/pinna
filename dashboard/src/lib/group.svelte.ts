// The satellite group (ADR 0026) as the Satellite group page and its row on Assistant see it:
// AssistantService's view, read every 30 s while the page shows it and the tab
// is visible, and its changes: the switches, Add and Remove. The group's key
// never reaches the page.
import { createClient } from "@connectrpc/connect";
import { createConnectTransport } from "@connectrpc/connect-web";

import { AssistantService, type SatelliteGroupView } from "../gen/pinna/v1/assistant_pb.ts";
import { errorSentence } from "./device.svelte.ts";
import { type Change, happened, type Refusal, refusalToast, removedText, Replies } from "./group.ts";
import { toasts } from "./toast.svelte.ts";

const client = createClient(AssistantService, createConnectTransport({ baseUrl: location.origin }));

const EVERY_MS = 30_000;
const READ_TIMEOUT_MS = 10_000;
// An Add waits for the satellite added, up to about 15 s; leaving and Remove
// tell the other members first. Past this, the satellite isn't answering.
const CHANGE_TIMEOUT_MS = 30_000;
// The search runs about once a minute; a member just removed shows under
// Other satellites only after its next run.
const REMOVED_MS = 120_000;

class SatelliteGroup {
  /** The group as the satellite last answered it; null until it has. */
  view = $state.raw<SatelliteGroupView | null>(null);
  /** Why the group couldn't be read, while it can't. */
  error = $state<string | null>(null);
  /** The change that runs; the page waits for its answer. */
  running = $state.raw<Change | null>(null);
  /** The last change refused, until the next one, until a view read since
   *  shows it happened all the same, or until the page closes. */
  refused = $state.raw<Refusal | null>(null);
  /** The member just removed, for two minutes: the search lists it as it
   *  is now only after its next run. */
  removed = $state.raw<{ mac: string; name: string } | null>(null);

  #replies = new Replies();
  #users = 0;
  #timer: ReturnType<typeof setInterval> | undefined;
  #removedTimer: ReturnType<typeof setTimeout> | undefined;
  #visible = () => {
    if (document.visibilityState === "visible") void this.load();
  };

  async load() {
    const ticket = this.#replies.read();
    try {
      const reply = await client.getSatelliteGroup({}, { timeoutMs: READ_TIMEOUT_MS });
      if (!this.#replies.fresh(ticket)) return;
      this.view = reply.group ?? null;
      this.error = null;
      if (this.refused && reply.group && happened(this.refused.change, reply.group)) this.refused = null;
    } catch (e) {
      // A change's answer, or the next read, says more.
      if (!this.running) this.error = errorSentence(e);
    }
  }

  /** Start reading for one page; returns the stop for an $effect. */
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
      this.refused = null;
      this.#forgetRemoved();
    };
  }

  #forgetRemoved() {
    clearTimeout(this.#removedTimer);
    this.removed = null;
  }

  /** Run a change; its answer is the view. A refusal stays for the page to
   *  show by what was changed, and the view is read again: the group or the
   *  search may have changed meanwhile. With no page open, a toast says it,
   *  as one says a success. */
  async #change(
    change: Change,
    name: string,
    run: () => Promise<{ group?: SatelliteGroupView }>,
    done: string | ((after: SatelliteGroupView | undefined) => string),
  ): Promise<boolean> {
    if (this.running) return false;
    this.running = change;
    this.refused = null;
    this.#forgetRemoved();
    this.#replies.start();
    let ok = false;
    try {
      const reply = await run();
      if (reply.group) {
        this.view = reply.group;
        this.error = null;
      }
      toasts.show(typeof done === "string" ? done : done(reply.group));
      ok = true;
    } catch (e) {
      const refused = { change, name, sentence: errorSentence(e) };
      const toast = refusalToast(refused, this.#users > 0);
      if (toast) toasts.show(toast, "danger");
      else this.refused = refused;
    } finally {
      this.#replies.end();
      this.running = null;
    }
    if (!ok) void this.load();
    return ok;
  }

  /** Turn Satellite group on or off. Off leaves the group; the members are told. */
  setOn(on: boolean) {
    const leaves = !on && !!this.view?.inGroup;
    return this.#change(
      { kind: "on" },
      "Satellite group",
      () => client.setSatelliteGroup({ on }, { timeoutMs: CHANGE_TIMEOUT_MS }),
      on ? "Satellite group is on" : leaves ? "This satellite left the group" : "Satellite group is off",
    );
  }

  setControl(on: boolean) {
    return this.#change(
      { kind: "control" },
      "Control from the group",
      () => client.setSatelliteGroup({ control: on }, { timeoutMs: CHANGE_TIMEOUT_MS }),
      "Saved",
    );
  }

  /** Add a satellite the search found, by its MAC as the candidate has it. */
  add(mac: string, name: string) {
    return this.#change(
      { kind: "add", mac },
      name,
      () => client.addGroupMember({ mac }, { timeoutMs: CHANGE_TIMEOUT_MS }),
      `${name} added`,
    );
  }

  async remove(mac: string, name: string) {
    const ok = await this.#change(
      { kind: "remove", mac },
      name,
      () => client.removeGroupMember({ mac }, { timeoutMs: CHANGE_TIMEOUT_MS }),
      (after) => removedText(name, after),
    );
    if (ok) {
      this.removed = { mac, name };
      this.#removedTimer = setTimeout(() => (this.removed = null), REMOVED_MS);
    }
    return ok;
  }
}

export const satelliteGroup = new SatelliteGroup();
