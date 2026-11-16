// The fastboot protocol over any transport that moves whole USB transfers
// (ADR 0016). The page uses WebUSB; the tests use a scripted device.
//
// The host sends one ASCII command per transfer. The device answers with one
// or more transfers that start with a 4-byte status, then text up to the
// first NUL: INFO and TEXT report progress and more follow; OKAY and FAIL end
// the command. `download:<size>` gets DATA<size>, then the host sends the
// data, then the device answers OKAY.

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
/** Data goes out in transfers of at most this size, for progress. */
const WRITE_CHUNK = 1 << 20;

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

  async command(command: string, timeoutMs = this.#timeoutMs): Promise<Reply> {
    await this.#send(command);
    const reply = await this.#reply(command, timeoutMs);
    if (reply.status === "DATA") {
      throw this.#fatal("protocol", command, `unexpected DATA reply to ${command}`);
    }
    return { message: reply.message, info: reply.info };
  }

  /**
   * Send `parts` as one download of their total size: the device's next
   * `flash:` writes it. Throws a FastbootError like `command`.
   */
  async download(parts: Uint8Array[], onProgress: (sent: number) => void = () => {}, timeoutMs = this.#timeoutMs): Promise<void> {
    const size = parts.reduce((sum, part) => sum + part.length, 0);
    if (size === 0 || size > 0xffffffff) throw new FastbootError("protocol", "download", `can't download ${size} bytes`);
    const command = `download:${size.toString(16).padStart(8, "0")}`;
    await this.#send(command);
    const accepted = await this.#reply(command, timeoutMs);
    if (accepted.status !== "DATA" || accepted.size !== size) {
      throw this.#fatal("protocol", command, `the device didn't accept ${size} bytes`);
    }

    let sent = 0;
    for (const part of parts) {
      for (let offset = 0; offset < part.length; offset += WRITE_CHUNK) {
        const chunk = part.subarray(offset, Math.min(offset + WRITE_CHUNK, part.length));
        try {
          await this.#transport.write(chunk);
        } catch (error) {
          throw this.#fatal("transport", command, `sending the data failed: ${String(error)}`);
        }
        sent += chunk.length;
        onProgress(sent);
      }
    }
    const done = await this.#reply(command, timeoutMs);
    if (done.status !== "OKAY") throw this.#fatal("protocol", command, `unexpected DATA reply after ${command}`);
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

  async #send(command: string): Promise<void> {
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
  }

  /** Read until OKAY or DATA; FAIL throws (the connection stays usable). */
  async #reply(command: string, timeoutMs: number): Promise<({ status: "OKAY" } & Reply) | { status: "DATA"; size: number; info: string[] }> {
    const info: string[] = [];
    for (;;) {
      let reply: Uint8Array;
      try {
        reply = await this.#transport.read(timeoutMs);
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
          return { status, message: text, info };
        case "DATA":
          if (!/^[0-9a-fA-F]{8}$/.test(text)) {
            throw this.#fatal("protocol", command, `unexpected reply to ${command}: ${JSON.stringify(status + text)}`);
          }
          return { status, size: Number.parseInt(text, 16), info };
        case "FAIL":
          throw new FastbootError("fail", command, text || "FAIL");
        default:
          throw this.#fatal("protocol", command, `unexpected reply to ${command}: ${JSON.stringify(status + text)}`);
      }
    }
  }

  #fatal(kind: FastbootErrorKind, command: string, message: string): FastbootError {
    this.#broken = message;
    return new FastbootError(kind, command, message);
  }
}
