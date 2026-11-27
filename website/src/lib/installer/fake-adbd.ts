// An adbd for the tests, as TWRP's on the Echo Dot (5th gen) behaves: it
// answers CNXN with a banner, runs shell protocol v2 commands through a
// handler, and holds the host to the protocol's rules (each message's magic
// and checksum, one WRTE in flight per stream, payloads within the size it
// announced).
import { TimeoutError, type Transport } from "./fastboot.ts";

const CNXN = 0x4e584e43;
const AUTH = 0x48545541;
const OPEN = 0x4e45504f;
const OKAY = 0x59414b4f;
const WRTE = 0x45545257;
const CLSE = 0x45534c43;

export const TWRP_BANNER = "recovery::ro.product.name=omni_cannoli;ro.product.model=Echo Dot (5th Gen);ro.product.device=cannoli;features=shell_v2,cmd,stat_v2";

/** What a command does: its output and exit status, or "hang" to never answer. */
export type Outcome = { stdout?: string; stderr?: string; exit: number } | "hang";

export type FakeOptions = {
  banner?: string;
  /** Ask for a key instead of answering CNXN. */
  auth?: boolean;
  /** The payload size the device announces. */
  maxPayload?: number;
  /** The most the device puts in one WRTE, so that packets span them. */
  chunk?: number;
  /**
   * The command, once its stdin is closed. With `exitAfter`, it ends once
   * that many bytes of stdin have come, without reading the rest.
   */
  run: (command: string, stdin: Uint8Array) => Outcome;
  exitAfter?: (command: string) => number | null;
};

type Stream = {
  local: number;
  remote: number;
  command: string;
  stdin: Uint8Array[];
  pending: Uint8Array;
  /** Shell packets still to send to the host, one WRTE each. */
  outgoing: Uint8Array[];
  /** A WRTE to the host that it hasn't acknowledged yet. */
  waiting: boolean;
  done: boolean;
};

const ascii = (bytes: Uint8Array) => new TextDecoder().decode(bytes);

function message(command: number, arg0: number, arg1: number, data: Uint8Array = new Uint8Array(0)): Uint8Array[] {
  const head = new Uint8Array(24);
  const v = new DataView(head.buffer);
  v.setUint32(0, command, true);
  v.setUint32(4, arg0, true);
  v.setUint32(8, arg1, true);
  v.setUint32(12, data.length, true);
  v.setUint32(20, ~command >>> 0, true);
  return data.length > 0 ? [head, data] : [head];
}

function packet(id: number, data: Uint8Array): Uint8Array {
  const out = new Uint8Array(5 + data.length);
  out[0] = id;
  new DataView(out.buffer).setUint32(1, data.length, true);
  out.set(data, 5);
  return out;
}

export class FakeAdbd implements Transport {
  /** Commands the host ran, in order. */
  readonly commands: string[] = [];
  /** Each command's stdin, by its position in `commands`. */
  readonly stdins: Uint8Array[] = [];
  reboots = 0;
  readonly #opts: FakeOptions;
  readonly #out: Uint8Array[] = [];
  #header: DataView | null = null;
  #streams = new Map<number, Stream>();
  #nextRemote = 100;
  /** The host's WRTE whose OKAY it hasn't read yet. */
  #unread = new Set<number>();
  #gone = false;

  constructor(opts: FakeOptions) {
    this.#opts = opts;
  }

  get maxPayload(): number {
    return this.#opts.maxPayload ?? 1 << 20;
  }

  async write(bytes: Uint8Array): Promise<void> {
    if (this.#gone) throw new Error("USB write: disconnected");
    if (!this.#header) {
      if (bytes.length !== 24) throw new Error(`a ${bytes.length}-byte transfer where a header belongs`);
      const v = new DataView(bytes.slice().buffer);
      if (v.getUint32(20, true) !== ~v.getUint32(0, true) >>> 0) throw new Error("a header with a wrong magic");
      if (v.getUint32(12, true) === 0) return this.#handle(v, new Uint8Array(0));
      this.#header = v;
      return;
    }
    const v = this.#header;
    this.#header = null;
    if (bytes.length !== v.getUint32(12, true)) throw new Error("a payload of another length than its header says");
    let sum = 0;
    for (const b of bytes) sum += b;
    if (sum >>> 0 !== v.getUint32(16, true)) throw new Error("a payload with a wrong checksum");
    this.#handle(v, bytes.slice());
  }

  async read(_timeoutMs: number, length?: number): Promise<Uint8Array> {
    const next = this.#out.shift();
    if (!next) throw new TimeoutError();
    if (length !== undefined && next.length > length) {
      this.#out.unshift(next.subarray(length));
      return next.subarray(0, length);
    }
    // The host has read an OKAY for its WRTE: it may send the next one.
    const v = next.length === 24 ? new DataView(next.buffer, next.byteOffset, 24) : null;
    if (v && v.getUint32(0, true) === OKAY) this.#unread.delete(v.getUint32(8, true));
    return next;
  }

  #send(parts: Uint8Array[]): void {
    this.#out.push(...parts);
  }

  #handle(v: DataView, data: Uint8Array): void {
    const command = v.getUint32(0, true);
    const arg0 = v.getUint32(4, true);
    const arg1 = v.getUint32(8, true);
    if (data.length > this.maxPayload) throw new Error(`a ${data.length}-byte payload, over the ${this.maxPayload} announced`);
    switch (command) {
      case CNXN:
        if (this.#opts.auth) this.#send(message(AUTH, 1, 0, new Uint8Array(20)));
        else this.#send(message(CNXN, 0x01000001, this.maxPayload, new TextEncoder().encode(this.#opts.banner ?? TWRP_BANNER)));
        return;
      case OPEN: {
        const service = ascii(data).replace(/\0$/, "");
        if (service === "reboot:") {
          this.reboots++;
          this.#gone = true;
          return;
        }
        const shell = /^shell,v2,raw:(.*)$/s.exec(service);
        if (!shell) {
          this.#send(message(CLSE, 0, arg0));
          return;
        }
        const remote = this.#nextRemote++;
        this.#streams.set(arg0, { local: arg0, remote, command: shell[1], stdin: [], pending: new Uint8Array(0), outgoing: [], waiting: false, done: false });
        this.commands.push(shell[1]);
        this.stdins.push(new Uint8Array(0));
        this.#send(message(OKAY, remote, arg0));
        return;
      }
      case WRTE: {
        const s = this.#streams.get(arg0);
        // Like adbd, a WRTE on a stream it closed meanwhile gets no answer.
        if (!s || s.remote !== arg1) return;
        if (this.#unread.has(arg0)) throw new Error("a WRTE before the OKAY for the one before");
        this.#unread.add(arg0);
        this.#send(message(OKAY, s.remote, s.local));
        this.#shellPackets(s, data);
        return;
      }
      case OKAY: {
        const s = this.#streams.get(arg0);
        if (!s || s.remote !== arg1) return;
        if (!s.waiting) throw new Error("an OKAY for a WRTE the device didn't send");
        s.waiting = false;
        this.#flush(s);
        return;
      }
      case CLSE:
        return;
      default:
        throw new Error(`a command this adbd doesn't know: 0x${command.toString(16)}`);
    }
  }

  #shellPackets(s: Stream, data: Uint8Array): void {
    if (s.done) return;
    const all = new Uint8Array(s.pending.length + data.length);
    all.set(s.pending);
    all.set(data, s.pending.length);
    let at = 0;
    while (all.length - at >= 5) {
      const len = new DataView(all.buffer, at + 1, 4).getUint32(0, true);
      if (all.length - at - 5 < len) break;
      const id = all[at];
      const body = all.slice(at + 5, at + 5 + len);
      at += 5 + len;
      if (id === 0) {
        s.stdin.push(body);
        const limit = this.#opts.exitAfter?.(s.command) ?? null;
        if (limit !== null && this.#stdinLength(s) >= limit) return this.#finish(s);
      } else if (id === 4) {
        return this.#finish(s);
      } else {
        throw new Error(`a shell packet the host shouldn't send: ${id}`);
      }
    }
    s.pending = all.slice(at);
  }

  #stdinLength(s: Stream): number {
    return s.stdin.reduce((n, b) => n + b.length, 0);
  }

  #finish(s: Stream): void {
    s.done = true;
    const stdin = new Uint8Array(this.#stdinLength(s));
    let at = 0;
    for (const b of s.stdin) {
      stdin.set(b, at);
      at += b.length;
    }
    this.stdins[this.commands.lastIndexOf(s.command)] = stdin;
    const outcome = this.#opts.run(s.command, stdin);
    if (outcome === "hang") return;
    const enc = new TextEncoder();
    const bytes: Uint8Array[] = [];
    if (outcome.stdout) bytes.push(packet(1, enc.encode(outcome.stdout)));
    if (outcome.stderr) bytes.push(packet(2, enc.encode(outcome.stderr)));
    bytes.push(packet(3, Uint8Array.of(outcome.exit)));
    const stream = new Uint8Array(bytes.reduce((n, b) => n + b.length, 0));
    at = 0;
    for (const b of bytes) {
      stream.set(b, at);
      at += b.length;
    }
    const chunk = this.#opts.chunk ?? stream.length;
    for (let i = 0; i < stream.length; i += chunk) s.outgoing.push(stream.slice(i, i + chunk));
    this.#flush(s);
  }

  #flush(s: Stream): void {
    if (s.waiting) return;
    const next = s.outgoing.shift();
    if (next) {
      s.waiting = true;
      this.#send(message(WRTE, s.remote, s.local, next));
    } else if (s.done) {
      this.#send(message(CLSE, s.remote, s.local));
      this.#streams.delete(s.local);
    }
  }
}
