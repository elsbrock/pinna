// WAV files for a recording (ADR 0030), written in the browser from the
// streams the satellite sends. No Svelte and no runtime imports, so
// `node --test` runs it.

/** KSDATAFORMAT_SUBTYPE_PCM, as the bytes of a GUID in a WAVE_FORMAT_EXTENSIBLE header. */
const PCM_GUID = [0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xaa, 0x00, 0x38, 0x9b, 0x71];

/** The chunks as one run of bytes, cut to a whole number of `frame`-byte frames: a chunk may end inside one. */
function joined(chunks: Uint8Array[], frame: number): Uint8Array {
  let total = 0;
  for (const c of chunks) total += c.length;
  const out = new Uint8Array(total - (total % frame));
  let at = 0;
  for (const c of chunks) {
    const room = out.length - at;
    if (room <= 0) break;
    out.set(room < c.length ? c.subarray(0, room) : c, at);
    at += c.length;
  }
  return out;
}

const ascii = (view: DataView, at: number, text: string) => {
  for (let i = 0; i < text.length; i++) view.setUint8(at + i, text.charCodeAt(i));
};

/** A mono, 16-bit PCM WAV from s16le chunks. A trailing odd byte is dropped. */
export function wavMono16(pcm: Uint8Array[], rate: number): Uint8Array {
  const data = joined(pcm, 2);
  const out = new Uint8Array(44 + data.length);
  const view = new DataView(out.buffer);
  ascii(view, 0, "RIFF");
  view.setUint32(4, 36 + data.length, true);
  ascii(view, 8, "WAVE");
  ascii(view, 12, "fmt ");
  view.setUint32(16, 16, true);
  view.setUint16(20, 1, true);
  view.setUint16(22, 1, true);
  view.setUint32(24, rate, true);
  view.setUint32(28, rate * 2, true);
  view.setUint16(32, 2, true);
  view.setUint16(34, 16, true);
  ascii(view, 36, "data");
  view.setUint32(40, data.length, true);
  out.set(data, 44);
  return out;
}

/**
 * One 24-bit WAV (WAVE_FORMAT_EXTENSIBLE, no speaker positions) with
 * `laneCount + 2` channels: the lanes as they come, then the signal before
 * pinnad's gain, then the echo reference. The streams start together but end
 * at different lengths: the shorter ones are padded with silence at the end.
 */
export function wavDebug(parts: {
  /** s24le, interleaved, `laneCount` lanes. */
  lanes: Uint8Array[];
  laneCount: number;
  /** s16le mono. */
  beforeGain: Uint8Array[];
  /** s16le mono. */
  reference: Uint8Array[];
  rate: number;
}): Uint8Array {
  const { laneCount, rate } = parts;
  const channels = laneCount + 2;
  const lanes = joined(parts.lanes, 3 * laneCount);
  const before = joined(parts.beforeGain, 2);
  const reference = joined(parts.reference, 2);
  const frames = Math.max(lanes.length / (3 * laneCount), before.length / 2, reference.length / 2);
  const align = 3 * channels;
  const size = frames * align;
  const out = new Uint8Array(68 + size);
  const view = new DataView(out.buffer);
  ascii(view, 0, "RIFF");
  view.setUint32(4, 60 + size, true);
  ascii(view, 8, "WAVE");
  ascii(view, 12, "fmt ");
  view.setUint32(16, 40, true);
  view.setUint16(20, 0xfffe, true);
  view.setUint16(22, channels, true);
  view.setUint32(24, rate, true);
  view.setUint32(28, rate * align, true);
  view.setUint16(32, align, true);
  view.setUint16(34, 24, true);
  view.setUint16(36, 22, true);
  view.setUint16(38, 24, true);
  view.setUint32(40, 0, true);
  out.set(PCM_GUID, 44);
  ascii(view, 60, "data");
  view.setUint32(64, size, true);
  const laneFrames = lanes.length / (3 * laneCount);
  for (let f = 0; f < frames; f++) {
    const at = 68 + f * align;
    if (f < laneFrames) out.set(lanes.subarray(f * 3 * laneCount, (f + 1) * 3 * laneCount), at);
    // A 16-bit sample is the top two bytes of a 24-bit one; the low byte stays zero.
    if (f * 2 < before.length) out.set(before.subarray(f * 2, f * 2 + 2), at + 3 * laneCount + 1);
    if (f * 2 < reference.length) out.set(reference.subarray(f * 2, f * 2 + 2), at + 3 * laneCount + 4);
  }
  return out;
}
