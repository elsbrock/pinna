// The virtual Echo Dot (ADR 0014): in the development VM, this browser is the
// satellite's microphone, speaker and buttons, over pinnad's /virtual-device
// socket. Kept outside the pages, so audio keeps flowing between sections.
import type { Frame } from "../../../design/ring.ts";
import { frameFromHex } from "./ledframe.ts";
import { MIC_RATE, Resampler, SPEAKER_RATE, fromS16, rms, toS16 } from "./pcm.ts";

export type VirtualButton = "action" | "volume_up" | "volume_down" | "mute";
export type Link = "off" | "connecting" | "on" | "replaced" | "failed";
export type Mic = "off" | "asking" | "on" | "denied" | "insecure" | "missing" | "busy";

/** 20 ms of microphone audio per message. */
const CHUNK = MIC_RATE / 50;
/** Speaker audio is scheduled this far ahead; more than MAX_AHEAD behind the clock is dropped. */
const LEAD_S = 0.08;
const MAX_AHEAD_S = 0.5;
const BACKOFF_MS = [1_000, 2_000, 5_000];
/**
 * pinnad's volume curve tops out at -12 dB for the Dot's amplifier; +12 dB
 * here makes volume 100 full scale in the browser.
 */
const SPEAKER_GAIN = 4;

// Mono mix of whatever is connected to it, posted to the page in render blocks.
const TAP = `
registerProcessor("pinna-tap", class extends AudioWorkletProcessor {
  process(inputs) {
    const ch = inputs[0];
    if (ch && ch.length) {
      const mono = new Float32Array(ch[0].length);
      for (const c of ch) for (let i = 0; i < mono.length; i++) mono[i] += c[i] / ch.length;
      this.port.postMessage(mono, [mono.buffer]);
    }
    return true;
  }
});`;

class VirtualDot {
  link = $state<Link>("off");
  /** Why the link failed, for the panel. */
  problem = $state("");
  /** Reconnection attempts since the link was lost. */
  retries = $state(0);
  /** Why the last audio file did not play. */
  fileProblem = $state("");
  mic = $state<Mic>("off");
  /** Microphone level as sent, 0..1. */
  level = $state(0);
  /** Name of the audio file playing as the microphone. */
  file = $state<string | null>(null);
  speaker = $state(true);
  /** Buttons held down in this browser. */
  held = $state<VirtualButton[]>([]);
  /** The LED ring as pinnad drew it, while connected. */
  ring = $state<Frame | null>(null);

  #ws: WebSocket | null = null;
  #ctx: AudioContext | null = null;
  #tap: AudioWorkletNode | null = null;
  #out: GainNode | null = null;
  #stream: MediaStream | null = null;
  #micNode: MediaStreamAudioSourceNode | null = null;
  #fileNode: AudioBufferSourceNode | null = null;
  #resampler: Resampler | null = null;
  #pending: number[] = [];
  #playAt = 0;
  #retry: ReturnType<typeof setTimeout> | null = null;

  /** From a click: browsers start audio only after a user gesture. */
  async connect() {
    if (this.link === "connecting" || this.link === "on") return;
    // Before the await: a second click must not start a second connection.
    this.link = "connecting";
    this.problem = "";
    this.retries = 0;
    try {
      await this.#audio();
    } catch (e) {
      this.#release();
      this.link = "failed";
      this.problem = `This browser cannot run the audio: ${e instanceof Error ? e.message : e}`;
      return;
    }
    this.#open();
    // The same click: the satellite is for talking to.
    void this.startMic();
  }

  disconnect() {
    const ws = this.#ws;
    this.#ws = null;
    ws?.close(1000);
    this.#release();
    this.link = "off";
  }

  /** Let go of everything this page holds: buttons, microphone, file, audio. */
  #release() {
    if (this.#retry) clearTimeout(this.#retry);
    this.#retry = null;
    this.releaseAll();
    this.stopFile();
    this.stopMic();
    void this.#ctx?.close();
    this.#ctx = this.#tap = this.#out = null;
    this.level = 0;
    this.ring = null;
  }

  press(button: VirtualButton, pressed: boolean) {
    const held = this.held.includes(button);
    if (pressed === held) return;
    this.held = pressed ? [...this.held, button] : this.held.filter((b) => b !== button);
    if (this.#ws?.readyState === WebSocket.OPEN) this.#ws.send(JSON.stringify({ button, pressed }));
  }

  releaseAll() {
    for (const b of this.held) this.press(b, false);
  }

  async startMic() {
    if (!this.#ctx || !this.#tap || this.mic === "asking" || this.mic === "on") return;
    if (!navigator.mediaDevices?.getUserMedia) {
      this.mic = "insecure";
      return;
    }
    this.mic = "asking";
    try {
      this.#stream = await navigator.mediaDevices.getUserMedia({
        // The browser's echo canceller keeps the speaker out of the microphone;
        // noise suppression would blunt the wake word.
        audio: { echoCancellation: true, noiseSuppression: false, autoGainControl: true },
      });
    } catch (e) {
      const name = e instanceof DOMException ? e.name : "";
      this.mic =
        name === "NotAllowedError" ? "denied" : name === "NotFoundError" || name === "OverconstrainedError" ? "missing" : "busy";
      return;
    }
    if (!this.#ctx) {
      // Disconnected while the browser asked.
      this.#stream.getTracks().forEach((t) => t.stop());
      this.#stream = null;
      this.mic = "off";
      return;
    }
    this.#micNode = this.#ctx.createMediaStreamSource(this.#stream);
    if (!this.#fileNode) this.#micNode.connect(this.#tap);
    this.mic = "on";
  }

  stopMic() {
    this.#micNode?.disconnect();
    this.#micNode = null;
    this.#stream?.getTracks().forEach((t) => t.stop());
    this.#stream = null;
    if (this.mic !== "insecure") this.mic = "off";
    if (!this.#fileNode) this.level = 0;
  }

  /** Play a recording as the microphone, instead of it, until it ends. */
  async playFile(file: File) {
    if (!this.#ctx || !this.#tap) return;
    let buffer: AudioBuffer;
    try {
      buffer = await this.#ctx.decodeAudioData(await file.arrayBuffer());
    } catch {
      this.fileProblem = `${file.name} is not an audio file this browser can read.`;
      return;
    }
    this.stopFile();
    const node = this.#ctx.createBufferSource();
    node.buffer = buffer;
    node.connect(this.#tap);
    this.#micNode?.disconnect();
    node.onended = () => {
      if (this.#fileNode !== node) return;
      this.#fileNode = null;
      this.file = null;
      this.level = 0;
      if (this.#tap) this.#micNode?.connect(this.#tap);
    };
    this.#fileNode = node;
    this.file = file.name;
    this.fileProblem = "";
    node.start();
  }

  stopFile() {
    const node = this.#fileNode;
    if (!node) return;
    this.#fileNode = null;
    node.onended = null;
    node.stop();
    node.disconnect();
    this.file = null;
    if (this.#tap) this.#micNode?.connect(this.#tap);
  }

  setSpeaker(on: boolean) {
    this.speaker = on;
    if (this.#out) this.#out.gain.value = on ? SPEAKER_GAIN : 0;
  }

  async #audio() {
    if (this.#ctx) return;
    const ctx = new AudioContext();
    const url = URL.createObjectURL(new Blob([TAP], { type: "text/javascript" }));
    try {
      await ctx.audioWorklet.addModule(url);
    } finally {
      URL.revokeObjectURL(url);
    }
    await ctx.resume();
    const tap = new AudioWorkletNode(ctx, "pinna-tap", { numberOfOutputs: 0 });
    tap.port.onmessage = (e: MessageEvent<Float32Array>) => this.#capture(e.data);
    const out = ctx.createGain();
    out.gain.value = this.speaker ? SPEAKER_GAIN : 0;
    out.connect(ctx.destination);
    this.#ctx = ctx;
    this.#tap = tap;
    this.#out = out;
    this.#resampler = new Resampler(ctx.sampleRate);
    this.#pending = [];
    this.#playAt = 0;
  }

  #open() {
    this.link = "connecting";
    const ws = new WebSocket(`${location.protocol === "https:" ? "wss" : "ws"}://${location.host}/virtual-device`);
    ws.binaryType = "arraybuffer";
    this.#ws = ws;
    ws.onopen = () => {
      if (this.#ws !== ws) return;
      this.link = "on";
      this.retries = 0;
    };
    ws.onmessage = (e) => {
      if (this.#ws !== ws) return;
      if (e.data instanceof ArrayBuffer) this.#play(e.data);
      else if (typeof e.data === "string") this.#message(e.data);
    };
    ws.onclose = (e) => {
      if (this.#ws !== ws) return;
      this.#ws = null;
      // Keys held here are released on the satellite when the socket ends.
      this.held = [];
      this.ring = null;
      if (e.code === 4000) {
        // Another tab took over; taking it back is a click, never automatic
        // (two tabs would take turns forever). This one lets go of the microphone.
        this.#release();
        this.link = "replaced";
      } else if (e.code === 4001) {
        this.#release();
        this.link = "failed";
        this.problem =
          "The VM's audio graph is not running. Its log: journalctl -u pinna-pipewire, over the SSH command just dev-vm printed.";
      } else {
        // pinnad restarting, or the VM going away: try again.
        this.link = "connecting";
        const delay = BACKOFF_MS[Math.min(this.retries++, BACKOFF_MS.length - 1)];
        this.#retry = setTimeout(() => {
          this.#retry = null;
          if (this.#ctx) this.#open();
        }, delay);
      }
    };
  }

  /** Take over from the tab that has it now. */
  takeOver() {
    if (this.link === "replaced") void this.connect();
  }

  #message(text: string) {
    try {
      const m = JSON.parse(text) as { ring?: unknown };
      if (typeof m.ring === "string") this.ring = frameFromHex(m.ring) ?? this.ring;
    } catch {
      // Not for this version of the page.
    }
  }

  #capture(block: Float32Array) {
    const ws = this.#ws;
    if (!this.#resampler || !ws || ws.readyState !== WebSocket.OPEN) {
      this.level = 0;
      return;
    }
    const out = this.#resampler.push(block);
    for (const s of out) this.#pending.push(s);
    while (this.#pending.length >= CHUNK) {
      const chunk = Float32Array.from(this.#pending.splice(0, CHUNK));
      this.level = Math.min(1, rms(chunk) * 4);
      ws.send(toS16(chunk));
    }
  }

  #play(bytes: ArrayBuffer) {
    const ctx = this.#ctx;
    if (!ctx || !this.#out) return;
    const samples = fromS16(bytes);
    if (!samples.length) return;
    const now = ctx.currentTime;
    if (this.#playAt < now) this.#playAt = now + LEAD_S;
    // The VM's clock runs ahead of this one: skip rather than lag further.
    if (this.#playAt > now + MAX_AHEAD_S) return;
    const buf = ctx.createBuffer(1, samples.length, SPEAKER_RATE);
    buf.copyToChannel(samples, 0);
    const node = ctx.createBufferSource();
    node.buffer = buf;
    node.connect(this.#out);
    node.start(this.#playAt);
    this.#playAt += buf.duration;
  }
}

export const virtualDot = new VirtualDot();
