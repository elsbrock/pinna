// Split an image into downloads the device accepts (ADR 0016). An image that
// fits in one download goes as it is. A bigger one goes as Android sparse
// images, as the fastboot tool sends it: each covers the whole partition,
// carries its own blocks as RAW and skips the rest (DONT_CARE).

/** One download: its bytes, in parts, so that the image is never copied. */
export type Payload = { parts: Uint8Array[]; size: number };

export const SPARSE_MAGIC = 0xed26ff3a;
const FILE_HEADER = 28;
const CHUNK_HEADER = 12;
const CHUNK_RAW = 0xcac1;
const CHUNK_DONT_CARE = 0xcac3;
export const BLOCK = 4096;

export function isSparse(image: Uint8Array): boolean {
  return image.length >= 4 && new DataView(image.buffer, image.byteOffset, 4).getUint32(0, true) === SPARSE_MAGIC;
}

function fileHeader(totalBlocks: number, chunks: number): Uint8Array {
  const bytes = new Uint8Array(FILE_HEADER);
  const v = new DataView(bytes.buffer);
  v.setUint32(0, SPARSE_MAGIC, true);
  v.setUint16(4, 1, true); // major version
  v.setUint16(6, 0, true); // minor version
  v.setUint16(8, FILE_HEADER, true);
  v.setUint16(10, CHUNK_HEADER, true);
  v.setUint32(12, BLOCK, true);
  v.setUint32(16, totalBlocks, true);
  v.setUint32(20, chunks, true);
  v.setUint32(24, 0, true); // no checksum
  return bytes;
}

function chunkHeader(type: number, blocks: number, dataBytes: number): Uint8Array {
  const bytes = new Uint8Array(CHUNK_HEADER);
  const v = new DataView(bytes.buffer);
  v.setUint16(0, type, true);
  v.setUint32(4, blocks, true);
  v.setUint32(8, CHUNK_HEADER + dataBytes, true);
  return bytes;
}

/**
 * The downloads that write `image` from the start of a partition, each at
 * most `maxDownload` bytes. The last block is padded with zeros.
 */
export function payloads(image: Uint8Array, maxDownload: number): Payload[] {
  if (image.length === 0) throw new Error("the image is empty");
  if (isSparse(image)) throw new Error("the image is already a sparse image");
  if (image.length <= maxDownload) return [{ parts: [image], size: image.length }];

  const perPiece = Math.floor((maxDownload - FILE_HEADER - 3 * CHUNK_HEADER) / BLOCK);
  if (perPiece < 1) throw new Error(`a download of ${maxDownload} bytes can't hold one block`);
  const totalBlocks = Math.ceil(image.length / BLOCK);
  const pad = totalBlocks * BLOCK - image.length;

  const result: Payload[] = [];
  for (let start = 0; start < totalBlocks; start += perPiece) {
    const end = Math.min(start + perPiece, totalBlocks);
    const parts: Uint8Array[] = [];
    let chunks = 0;
    if (start > 0) {
      parts.push(chunkHeader(CHUNK_DONT_CARE, start, 0));
      chunks++;
    }
    parts.push(chunkHeader(CHUNK_RAW, end - start, (end - start) * BLOCK));
    parts.push(image.subarray(start * BLOCK, Math.min(end * BLOCK, image.length)));
    if (end === totalBlocks && pad > 0) parts.push(new Uint8Array(pad));
    chunks++;
    if (end < totalBlocks) {
      parts.push(chunkHeader(CHUNK_DONT_CARE, totalBlocks - end, 0));
      chunks++;
    }
    parts.unshift(fileHeader(totalBlocks, chunks));
    result.push({ parts, size: parts.reduce((n, p) => n + p.length, 0) });
  }
  return result;
}
