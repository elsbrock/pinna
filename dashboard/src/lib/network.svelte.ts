// GetNetwork, read every 5 s while some page shows it and the tab is visible.
// Overview (the Wi-Fi tile) and Network share one poll.
import { ConnectError } from "@connectrpc/connect";

import type { GetNetworkResponse } from "../gen/pinna/v1/device_pb.ts";
import { client } from "./device.svelte.ts";

const EVERY_MS = 5_000;

class Network {
  current = $state<GetNetworkResponse | null>(null);
  error = $state<string | null>(null);
  #users = 0;
  #timer: ReturnType<typeof setInterval> | undefined;

  async #read() {
    if (document.visibilityState !== "visible") return;
    try {
      this.current = await client.getNetwork({}, { timeoutMs: 5_000 });
      this.error = null;
    } catch (e) {
      if (this.#users) this.error = ConnectError.from(e).rawMessage;
    }
  }

  /** Start reading for one user; returns the stop for an $effect. */
  watch(): () => void {
    if (this.#users++ === 0) {
      void this.#read();
      this.#timer = setInterval(() => void this.#read(), EVERY_MS);
    }
    return () => {
      if (--this.#users === 0) clearInterval(this.#timer);
    };
  }

  /** The Wi-Fi interface, if there is one. */
  get wireless() {
    return this.current?.interfaces.find((i) => i.wireless);
  }

  /** The first IPv4 address on any interface but USB. */
  get ipv4() {
    for (const i of this.current?.interfaces ?? []) {
      if (i.name.startsWith("usb")) continue;
      const a = i.addresses.find((x) => !x.includes(":"));
      if (a) return a.replace(/\/\d+$/, "");
    }
    return null;
  }
}

export const network = new Network();
