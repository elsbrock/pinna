// The adb protocol over any transport that moves whole USB transfers (ADR
// 0016): enough of it to run commands in the Echo Dot (5th gen)'s TWRP. The
// page uses WebUSB; the tests use a fake adbd.
//
// A message is a 24-byte header, then its payload in transfers of its own.
// The header holds the command, two arguments, the payload's length and the
// sum of its bytes, and the command's bitwise NOT. The host sends CNXN, and
// the device answers CNXN with its banner, or AUTH when it wants a key. A
// stream starts with OPEN and a service name, which the device accepts with
// OKAY. On a stream, each WRTE waits for the other side's OKAY before the
// next one; CLSE ends it.
//
// Commands run with shell protocol v2. Inside the stream, each packet is a
// 1-byte id, a 4-byte length and its data. The host sends stdin (0) and
// close-stdin (4); the device sends stdout (1), stderr (2) and the exit
// status (3). Unlike `exec:`, it reports the status, and the command keeps
// running after its input ends.
import { TimeoutError, type Transport } from "./fastboot.ts";

const CNXN = 0x4e584e43;
const AUTH = 0x48545541;
const OPEN = 0x4e45504f;
const OKAY = 0x59414b4f;
const WRTE = 0x45545257;
const CLSE = 0x45534c43;

const NAMES: Record<number, string> = { [CNXN]: "CNXN", [AUTH]: "AUTH", [OPEN]: "OPEN", [OKAY]: "OKAY", [WRTE]: "WRTE", [CLSE]: "CLSE" };

/** The version that lets either side skip the payload checksum; this page always sends one. */
const VERSION = 0x01000001;
/** The most this page sends or accepts in one payload. The device's CNXN may lower it. */
export const MAX_PAYLOAD = 1 << 20;
const HEADER = 24;

const STDIN = 0;
const STDOUT = 1;
const STDERR = 2;
const EXIT = 3;
const CLOSE_STDIN = 4;
const PACKET_HEADER = 5;

/** Output kept from one command; more is dropped. The page runs short commands. */
const OUTPUT_MAX = 1 << 20;
const DEFAULT_TIMEOUT_MS = 5000;
/**
 * The wait for the device to take one WRTE. adbd answers once the data is
 * in the command's pipe, so a command that writes NAND as it reads can hold
 * a full megabyte for a while.
 */
const WRITE_TIMEOUT_MS = 60_000;

/**
 * - `timeout`: no reply in time. The connection is not used again.
 * - `transport`: the transfer itself failed (disconnected, stalled).
 * - `protocol`: a reply this code does not understand.
 * - `auth`: the device asked for a key. TWRP on the Echo Dot (5th gen) never does.
 * - `refused`: the device closed a stream as it opened, so it has no such service.
 * - `unsupported`: the device lacks shell protocol v2.
 */
export type AdbErrorKind = "timeout" | "transport" | "protocol" | "auth" | "refused" | "unsupported";

export class AdbError extends Error {
  readonly kind: AdbErrorKind;

  constructor(kind: AdbErrorKind, message: string) {
    super(message);
    this.name = "AdbError";
    this.kind = kind;
  }
}

/** What the device says about itself in its CNXN: `recovery::ro.product.device=cannoli;…;features=shell_v2,…`. */
export type Banner = { state: string; props: Record<string, string>; features: string[] };

export function parseBanner(text: string): Banner {
  const clean = text.replace(/\0+$/, "");
  const sep = clean.indexOf("::");
  const state = sep === -1 ? clean.split(":")[0] : clean.slice(0, sep);
  const props: Record<string, string> = {};
  for (const pair of (sep === -1 ? "" : clean.slice(sep + 2)).split(";")) {
    const eq = pair.indexOf("=");
    if (eq > 0) props[pair.slice(0, eq)] = pair.slice(eq + 1);
  }
  const features = (props.features ?? "").split(",").filter(Boolean);
  delete props.features;
  return { state, props, features };
}

export type ShellResult = { exit: number; stdout: string; stderr: string };

export type ShellOptions = {
  /** Sent as the command's stdin, then closed. */
  stdin?: Uint8Array;
  /** Bytes of stdin the device has taken so far. */
  onSent?: (bytes: number) => void;
  /** The wait for the exit status once stdin is closed. */
  timeoutMs?: number;
};

type Message = { command: number; arg0: number; arg1: number; data: Uint8Array };

const encoder = new TextEncoder();
const decoder = new TextDecoder();

function sum(data: Uint8Array): number {
  let s = 0;
  for (let i = 0; i < data.length; i++) s += data[i];
  return s >>> 0;
}

function header(command: number, arg0: number, arg1: number, data: Uint8Array): Uint8Array {
  const bytes = new Uint8Array(HEADER);
  const v = new DataView(bytes.buffer);
  v.setUint32(0, command, true);
  v.setUint32(4, arg0, true);
  v.setUint32(8, arg1, true);
  v.setUint32(12, data.length, true);
  v.setUint32(16, sum(data), true);
  v.setUint32(20, ~command >>> 0, true);
  return bytes;
}

function packet(id: number, data: Uint8Array): Uint8Array {
  const bytes = new Uint8Array(PACKET_HEADER + data.length);
  bytes[0] = id;
  new DataView(bytes.buffer).setUint32(1, data.length, true);
  bytes.set(data, PACKET_HEADER);
  return bytes;
}

/** Shell protocol v2 packets from the device, which may span WRTE payloads. */
class ShellOutput {
  #pending = new Uint8Array(0);
  readonly stdout: Uint8Array[] = [];
  readonly stderr: Uint8Array[] = [];
  #kept = 0;
  exit: number | null = null;
  closed = false;

  push(data: Uint8Array): void {
    const all = new Uint8Array(this.#pending.length + data.length);
    all.set(this.#pending);
    all.set(data, this.#pending.length);
    let at = 0;
    while (all.length - at >= PACKET_HEADER) {
      const length = new DataView(all.buffer, at + 1, 4).getUint32(0, true);
      if (all.length - at - PACKET_HEADER < length) break;
      const id = all[at];
      const body = all.subarray(at + PACKET_HEADER, at + PACKET_HEADER + length);
      if (id === EXIT && body.length > 0) this.exit = body[0];
      else if ((id === STDOUT || id === STDERR) && this.#kept + body.length <= OUTPUT_MAX) {
        (id === STDOUT ? this.stdout : this.stderr).push(body.slice());
        this.#kept += body.length;
      }
      at += PACKET_HEADER + length;
    }
    this.#pending = all.slice(at);
  }

  text(parts: Uint8Array[]): string {
    return parts.map((p) => decoder.decode(p, { stream: true })).join("") + decoder.decode();
  }
}

export class Adb {
  readonly #transport: Transport;
  readonly #timeoutMs: number;
  #broken: string | null = null;
  #maxPayload = MAX_PAYLOAD;
  #nextId = 1;
  banner: Banner | null = null;

  constructor(transport: Transport, timeoutMs = DEFAULT_TIMEOUT_MS) {
    this.#transport = transport;
    this.#timeoutMs = timeoutMs;
  }

  /** Why the connection must not be used again, or null. */
  get broken(): string | null {
    return this.#broken;
  }

  /** Send CNXN and read the device's banner. */
  async connect(): Promise<Banner> {
    await this.#send(CNXN, VERSION, MAX_PAYLOAD, encoder.encode("host::features=shell_v2"));
    // A session the device still had open may leave messages behind; CNXN
    // resets it, so skip them.
    for (let i = 0; i < 16; i++) {
      const m = await this.#receive(this.#timeoutMs, "CNXN");
      if (m.command === AUTH) {
        throw this.#fatal("auth", "the device asks for an adb key, which this page doesn't send");
      }
      if (m.command !== CNXN) continue;
      this.#maxPayload = Math.min(MAX_PAYLOAD, m.arg1 || MAX_PAYLOAD);
      const banner = parseBanner(decoder.decode(m.data));
      if (!banner.features.includes("shell_v2")) {
        throw this.#fatal("unsupported", "the device's adb lacks shell protocol v2, which the page needs for a command's exit status");
      }
      this.banner = banner;
      return banner;
    }
    throw this.#fatal("protocol", "the device didn't answer CNXN");
  }

  /** Run `command` in a shell without a terminal, and return its output and exit status. */
  async shell(command: string, opts: ShellOptions = {}): Promise<ShellResult> {
    const local = this.#nextId++;
    const what = JSON.stringify(command);
    await this.#send(OPEN, local, 0, encoder.encode(`shell,v2,raw:${command}\0`));
    let remote = 0;
    for (;;) {
      const m = await this.#receive(this.#timeoutMs, what);
      if (m.arg1 !== local) continue;
      if (m.command === CLSE) throw new AdbError("refused", `the device refused to run ${what}`);
      if (m.command === OKAY) {
        remote = m.arg0;
        break;
      }
      throw this.#fatal("protocol", `unexpected ${NAMES[m.command] ?? "message"} while opening ${what}`);
    }

    const out = new ShellOutput();
    const stdin = opts.stdin ?? new Uint8Array(0);
    const per = this.#maxPayload - PACKET_HEADER;
    for (let at = 0; at < stdin.length && !out.closed; at += per) {
      const chunk = stdin.subarray(at, at + per);
      await this.#write(local, remote, packet(STDIN, chunk), out, what);
      opts.onSent?.(at + chunk.length);
    }
    if (!out.closed) await this.#write(local, remote, packet(CLOSE_STDIN, new Uint8Array(0)), out, what);
    while (!out.closed) await this.#take(await this.#receive(opts.timeoutMs ?? this.#timeoutMs, what), local, remote, out);
    if (out.exit === null) throw new AdbError("protocol", `${what} ended without an exit status`);
    // Output from a command that stopped early is part of the answer: the
    // caller decides from the exit status.
    return { exit: out.exit, stdout: out.text(out.stdout), stderr: out.text(out.stderr) };
  }

  /** Ask the device to restart. It may go away before it answers. */
  async reboot(): Promise<void> {
    await this.#send(OPEN, this.#nextId++, 0, encoder.encode("reboot:\0"));
  }

  /** Send one WRTE and wait for its OKAY, taking the device's output meanwhile. */
  async #write(local: number, remote: number, data: Uint8Array, out: ShellOutput, what: string): Promise<void> {
    await this.#send(WRTE, local, remote, data);
    for (;;) {
      const m = await this.#receive(WRITE_TIMEOUT_MS, what);
      if (m.command === OKAY && m.arg1 === local) return;
      await this.#take(m, local, remote, out);
      // The command ended before it read all its input.
      if (out.closed) return;
    }
  }

  async #take(m: Message, local: number, remote: number, out: ShellOutput): Promise<void> {
    if (m.arg1 !== local) return;
    if (m.command === WRTE) {
      out.push(m.data);
      // The device sends its next WRTE only after this.
      await this.#send(OKAY, local, remote, new Uint8Array(0));
    } else if (m.command === CLSE) {
      out.closed = true;
    }
  }

  async #send(command: number, arg0: number, arg1: number, data: Uint8Array): Promise<void> {
    if (this.#broken) throw new AdbError("transport", this.#broken);
    try {
      await this.#transport.write(header(command, arg0, arg1, data));
      if (data.length > 0) await this.#transport.write(data);
    } catch (error) {
      throw this.#fatal("transport", `sending ${NAMES[command]} failed: ${String(error)}`);
    }
  }

  async #receive(timeoutMs: number, what: string): Promise<Message> {
    if (this.#broken) throw new AdbError("transport", this.#broken);
    const head = await this.#read(HEADER, timeoutMs, what);
    const v = new DataView(head.buffer, head.byteOffset, HEADER);
    const command = v.getUint32(0, true);
    const length = v.getUint32(12, true);
    if (v.getUint32(20, true) !== ~command >>> 0 || !(command in NAMES)) {
      throw this.#fatal("protocol", `a message from the device that isn't adb, while waiting for ${what}`);
    }
    if (length > MAX_PAYLOAD) throw this.#fatal("protocol", `a ${length}-byte payload from the device, more than it may send`);
    const data = length > 0 ? await this.#read(length, timeoutMs, what) : new Uint8Array(0);
    return { command, arg0: v.getUint32(4, true), arg1: v.getUint32(8, true), data };
  }

  /** Exactly `length` bytes, over as many transfers as the device sends them in. */
  async #read(length: number, timeoutMs: number, what: string): Promise<Uint8Array> {
    const parts: Uint8Array[] = [];
    let got = 0;
    while (got < length) {
      let bytes: Uint8Array;
      try {
        bytes = await this.#transport.read(timeoutMs, length - got);
      } catch (error) {
        if (error instanceof TimeoutError) throw this.#fatal("timeout", `no reply while waiting for ${what}`);
        throw this.#fatal("transport", `reading from the device failed: ${String(error)}`);
      }
      if (bytes.length > length - got) throw this.#fatal("protocol", "the device sent more than the message holds");
      parts.push(bytes);
      got += bytes.length;
    }
    if (parts.length === 1) return parts[0];
    const all = new Uint8Array(length);
    let at = 0;
    for (const p of parts) {
      all.set(p, at);
      at += p.length;
    }
    return all;
  }

  #fatal(kind: AdbErrorKind, message: string): AdbError {
    this.#broken = message;
    return new AdbError(kind, message);
  }
}
