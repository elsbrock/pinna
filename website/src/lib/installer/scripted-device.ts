// A fastboot device for the tests: answers each command from a script.
import { TimeoutError, type Transport } from "./fastboot.ts";

/** Replies to one command: status+text strings such as "OKAY0x1000", or "timeout". */
export type Script = Record<string, string[] | "timeout">;

export class ScriptedDevice implements Transport {
  readonly sent: string[] = [];
  readonly #script: Script;
  #pending: string[] = [];
  #timedOut = false;

  constructor(script: Script) {
    this.#script = script;
  }

  async write(bytes: Uint8Array): Promise<void> {
    const command = String.fromCharCode(...bytes);
    this.sent.push(command);
    const replies = this.#script[command];
    if (replies === "timeout") {
      this.#timedOut = true;
      this.#pending = [];
    } else {
      this.#timedOut = false;
      // A command outside the script gets what an LK answers to an unknown command.
      this.#pending = replies ? [...replies] : ["FAILunknown command"];
    }
  }

  async read(_timeoutMs: number): Promise<Uint8Array> {
    const next = this.#pending.shift();
    if (this.#timedOut || next === undefined) throw new TimeoutError();
    const bytes = new Uint8Array(next.length + 1);
    for (let i = 0; i < next.length; i++) bytes[i] = next.charCodeAt(i);
    return bytes;
  }
}
