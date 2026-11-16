// A fastboot device for the tests: answers each command from a script.
import { TimeoutError, type Transport } from "./fastboot.ts";

/** Replies to one command: status+text strings such as "OKAY0x1000", or "timeout". */
export type Script = Record<string, string[] | "timeout">;

export class ScriptedDevice implements Transport {
  /** Commands, in order. Data sent after DATA is not listed here. */
  readonly sent: string[] = [];
  /** Each completed download's data, in order. */
  readonly downloads: Uint8Array[] = [];
  readonly #script: Script;
  #pending: string[] = [];
  #timedOut = false;
  /** Bytes of a download still to come, and what came so far. */
  #expecting = 0;
  #received: Uint8Array[] = [];

  constructor(script: Script) {
    this.#script = script;
  }

  async write(bytes: Uint8Array): Promise<void> {
    if (this.#expecting > 0) {
      if (bytes.length > this.#expecting) throw new Error("more data than the download announced");
      this.#received.push(bytes.slice());
      this.#expecting -= bytes.length;
      if (this.#expecting === 0) {
        const total = this.#received.reduce((n, b) => n + b.length, 0);
        const data = new Uint8Array(total);
        let at = 0;
        for (const b of this.#received) {
          data.set(b, at);
          at += b.length;
        }
        this.downloads.push(data);
        this.#received = [];
        this.#pending = ["OKAY"];
      }
      return;
    }

    const command = String.fromCharCode(...bytes);
    this.sent.push(command);
    const replies = this.#script[command];
    if (replies === "timeout") {
      this.#timedOut = true;
      this.#pending = [];
      return;
    }
    this.#timedOut = false;
    const download = /^download:([0-9a-f]{8})$/.exec(command);
    if (download && !replies) {
      // Unless the script says otherwise, accept any download.
      this.#pending = [`DATA${download[1]}`];
      this.#expecting = Number.parseInt(download[1], 16);
      return;
    }
    // A command outside the script gets what an LK answers to an unknown command.
    this.#pending = replies ? [...replies] : ["FAILunknown command"];
  }

  async read(_timeoutMs: number): Promise<Uint8Array> {
    const next = this.#pending.shift();
    if (this.#timedOut || next === undefined) throw new TimeoutError();
    const bytes = new Uint8Array(next.length + 1);
    for (let i = 0; i < next.length; i++) bytes[i] = next.charCodeAt(i);
    return bytes;
  }
}
