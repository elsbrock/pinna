// The fastboot protocol over any transport that moves whole USB transfers
// (ADR 0016). The page uses WebUSB; the tests use a scripted device.
//
// The host sends one ASCII command per transfer. The device answers with one
// or more transfers that start with a 4-byte status, then text up to the
// first NUL: INFO and TEXT report progress and more follow; OKAY and FAIL end
// the command.

export interface Transport {
  /** Send one transfer. */
  write(bytes: Uint8Array): Promise<void>;
  /** Receive one transfer. Rejects with a TimeoutError after `timeoutMs`. */
  read(timeoutMs: number): Promise<Uint8Array>;
}

export class TimeoutError extends Error {
  constructor() {
    super("no reply");
    this.name = "TimeoutError";
  }
}

/**
 * - `fail`: the device answered FAIL; the connection is fine.
 * - `timeout`: no reply in time. The connection is not used again.
 * - `transport`: the transfer itself failed (disconnected, stalled).
 * - `protocol`: a reply this code does not understand.
 */
export type FastbootErrorKind = "fail" | "timeout" | "transport" | "protocol";

export class FastbootError extends Error {
  readonly kind: FastbootErrorKind;
  readonly command: string;

  constructor(kind: FastbootErrorKind, command: string, message: string) {
    super(message);
    this.name = "FastbootError";
    this.kind = kind;
    this.command = command;
  }
}

export type Reply = {
  /** The text after OKAY. */
  message: string;
  /** INFO and TEXT lines, in order. */
  info: string[];
};

/** The protocol's limit for one command. */
const MAX_COMMAND = 64;
const DEFAULT_TIMEOUT_MS = 5000;

function decode(bytes: Uint8Array): string {
  const end = bytes.indexOf(0);
  let text = "";
  for (const b of end === -1 ? bytes : bytes.subarray(0, end)) text += String.fromCharCode(b);
  return text;
}

export class Fastboot {
  readonly #transport: Transport;
  readonly #timeoutMs: number;
  #broken: string | null = null;

  constructor(transport: Transport, timeoutMs = DEFAULT_TIMEOUT_MS) {
    this.#transport = transport;
    this.#timeoutMs = timeoutMs;
  }

  /**
   * Why the connection must not be used again, or null. After a request
   * without a reply nothing more is sent: the device may still be working on
   * it, and on some stock units every request resets the device.
   */
  get broken(): string | null {
    return this.#broken;
  }

  async command(command: string): Promise<Reply> {
    if (this.#broken) throw new FastbootError("transport", command, this.#broken);
    if (command.length === 0 || command.length > MAX_COMMAND || !/^[\x20-\x7e]+$/.test(command)) {
      throw new FastbootError("protocol", command, `not a valid fastboot command: ${JSON.stringify(command)}`);
    }

    const bytes = new Uint8Array(command.length);
    for (let i = 0; i < command.length; i++) bytes[i] = command.charCodeAt(i);
    try {
      await this.#transport.write(bytes);
    } catch (error) {
      throw this.#fatal("transport", command, `sending ${command} failed: ${String(error)}`);
    }

    const info: string[] = [];
    for (;;) {
      let reply: Uint8Array;
      try {
        reply = await this.#transport.read(this.#timeoutMs);
      } catch (error) {
        if (error instanceof TimeoutError) {
          throw this.#fatal("timeout", command, `no reply to ${command}`);
        }
        throw this.#fatal("transport", command, `reading the reply to ${command} failed: ${String(error)}`);
      }
      if (reply.length < 4) {
        throw this.#fatal("protocol", command, `reply to ${command} is ${reply.length} bytes`);
      }
      const status = decode(reply.subarray(0, 4));
      const text = decode(reply.subarray(4));
      switch (status) {
        case "INFO":
        case "TEXT":
          info.push(text);
          continue;
        case "OKAY":
          return { message: text, info };
        case "FAIL":
          throw new FastbootError("fail", command, text || "FAIL");
        default:
          throw this.#fatal("protocol", command, `unexpected reply to ${command}: ${JSON.stringify(status + text)}`);
      }
    }
  }

  /** The value of a variable; a FAIL reply throws. */
  async getvar(name: string): Promise<string> {
    return (await this.command(`getvar:${name}`)).message;
  }

  /** The value of a variable, or null when the device answers FAIL. */
  async getvarOrNull(name: string): Promise<string | null> {
    try {
      return await this.getvar(name);
    } catch (error) {
      if (error instanceof FastbootError && error.kind === "fail") return null;
      throw error;
    }
  }

  #fatal(kind: FastbootErrorKind, command: string, message: string): FastbootError {
    this.#broken = message;
    return new FastbootError(kind, command, message);
  }
}
