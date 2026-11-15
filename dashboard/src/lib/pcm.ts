// Audio for the virtual Echo Dot's socket (ADR 0014): the browser's microphone
// goes up as S16LE mono at 16 kHz, the speaker comes down as S16LE mono at 48 kHz.

export const MIC_RATE = 16_000;
export const SPEAKER_RATE = 48_000;

/** Float samples (-1..1) to S16LE bytes. */
export function toS16(samples: Float32Array): ArrayBuffer {
  const out = new DataView(new ArrayBuffer(samples.length * 2));
  for (let i = 0; i < samples.length; i++) {
    const s = Math.max(-1, Math.min(1, samples[i]));
    out.setInt16(i * 2, Math.round(s < 0 ? s * 32768 : s * 32767), true);
  }
  return out.buffer;
}

/** S16LE bytes to float samples (-1..1). A trailing odd byte is ignored. */
export function fromS16(bytes: ArrayBuffer): Float32Array<ArrayBuffer> {
  const view = new DataView(bytes);
  const out = new Float32Array(bytes.byteLength >> 1);
  for (let i = 0; i < out.length; i++) out[i] = view.getInt16(i * 2, true) / 32768;
  return out;
}

/** Root mean square of a block, 0..1. */
export function rms(samples: Float32Array): number {
  if (!samples.length) return 0;
  let sum = 0;
  for (const s of samples) sum += s * s;
  return Math.sqrt(sum / samples.length);
}

/**
 * Any rate down to `outRate`: a windowed-sinc low-pass below the new Nyquist
 * frequency, then linear interpolation between filtered samples. State carries
 * over between blocks, so a stream can be fed in pieces of any size.
 */
export class Resampler {
  readonly #taps: Float32Array;
  readonly #hist: Float32Array;
  readonly #step: number;
  #pos = 0;
  /** Index of the next input sample, and position of the next output one. */
  #n = 0;
  #next = 0;
  #prev = 0;

  constructor(inRate: number, outRate = MIC_RATE) {
    if (outRate > inRate) throw new RangeError(`cannot resample ${inRate} Hz up to ${outRate} Hz`);
    this.#step = inRate / outRate;
    const n = 47;
    const fc = (0.45 * outRate) / inRate;
    const m = n - 1;
    const taps = new Float32Array(n);
    let sum = 0;
    for (let i = 0; i < n; i++) {
      const x = i - m / 2;
      const sinc = x === 0 ? 2 * fc : Math.sin(2 * Math.PI * fc * x) / (Math.PI * x);
      const t = i / m;
      const w = 0.42 - 0.5 * Math.cos(2 * Math.PI * t) + 0.08 * Math.cos(4 * Math.PI * t);
      taps[i] = sinc * w;
      sum += taps[i];
    }
    for (let i = 0; i < n; i++) taps[i] /= sum;
    this.#taps = taps;
    this.#hist = new Float32Array(n);
  }

  push(input: Float32Array): Float32Array {
    const n = this.#taps.length;
    const out: number[] = [];
    for (const x of input) {
      this.#hist[this.#pos] = x;
      this.#pos = (this.#pos + 1) % n;
      let y = 0;
      for (let k = 0; k < n; k++) y += this.#taps[k] * this.#hist[(this.#pos + k) % n];
      // y is the filtered sample at index #n, #prev the one at #n - 1.
      while (this.#next <= this.#n) {
        const frac = this.#next - (this.#n - 1);
        out.push(this.#prev + (y - this.#prev) * frac);
        this.#next += this.#step;
      }
      this.#prev = y;
      this.#n += 1;
    }
    return Float32Array.from(out);
  }
}
