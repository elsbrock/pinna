// Install Pinna on an unlocked device (ADR 0016, stage 2). The same writes,
// in the same order, as `flash-gen2.sh root` and then `flash-gen2.sh boot`.
import type { DeviceReport, Partition } from "./device.ts";
import { Fastboot, FastbootError } from "./fastboot.ts";
import { payloads, type Payload } from "./sparse.ts";

export type Write = { partition: Partition; label: string; payloads: Payload[] };

export class InstallError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "InstallError";
  }
}

/** The data the device accepts at once, from `max-download-size`. */
export function parseMaxDownload(value: string | null): number | null {
  if (!value) return null;
  const hex = value.trim().replace(/^0x/i, "");
  if (!/^[0-9a-f]+$/i.test(hex)) return null;
  const n = Number.parseInt(hex, 16);
  return n > 0 ? n : null;
}

const MiB = 1 << 20;

/**
 * What the install writes, in order. Throws an InstallError when an image
 * doesn't fit its partition, before anything is written.
 */
export function plan(report: DeviceReport, images: { boot: Uint8Array; root: Uint8Array }): Write[] {
  const max = parseMaxDownload(report.maxDownload);
  if (!max) throw new InstallError("The device doesn't say how much data it accepts at once (max-download-size).");
  const fits = (partition: Partition, image: Uint8Array, what: string) => {
    const size = report.partitions[partition];
    if (size === null) throw new InstallError(`The device has no ${partition} partition.`);
    if (image.length > size) {
      throw new InstallError(`The ${what} (${image.length} bytes) is bigger than ${partition} (${size} bytes).`);
    }
  };
  fits("system_a", images.root, "root image");
  fits("boot_a", images.boot, "boot image");
  fits("boot_b", images.boot, "boot image");
  fits("system_b", new Uint8Array(MiB), "cleared start");
  fits("userdata", new Uint8Array(4 * MiB), "cleared start");

  const boot = payloads(images.boot, max);
  return [
    { partition: "system_a", label: "Writing the root image to system_a", payloads: payloads(images.root, max) },
    // No image in system_b, so slot a boots, whatever misc says.
    { partition: "system_b", label: "Clearing system_b", payloads: payloads(new Uint8Array(MiB), max) },
    // Pinna formats userdata as /var on the first start.
    { partition: "userdata", label: "Erasing userdata", payloads: payloads(new Uint8Array(4 * MiB), max) },
    { partition: "boot_a", label: "Writing the boot image to boot_a", payloads: boot },
    { partition: "boot_b", label: "Writing the boot image to boot_b", payloads: boot },
  ];
}

export type Progress = {
  label: string;
  /** Sending the data, or waiting while the device writes it. */
  phase: "sending" | "writing" | "restarting";
  /** Bytes sent of all writes. */
  sent: number;
  total: number;
};

const DOWNLOAD_TIMEOUT_MS = 30_000;
/** Writing 100 MiB to the eMMC takes seconds; leave room for a slow one. */
const FLASH_TIMEOUT_MS = 180_000;

/**
 * Carry out `writes`, then restart the device. A FastbootError stops it where
 * it is: the device stays in fastboot with Kaeru untouched, and the install
 * can start again.
 */
export async function install(fb: Fastboot, writes: Write[], onProgress: (p: Progress) => void = () => {}): Promise<void> {
  const total = writes.reduce((n, w) => n + w.payloads.reduce((m, p) => m + p.size, 0), 0);
  let before = 0;
  for (const write of writes) {
    for (const [i, payload] of write.payloads.entries()) {
      const label = write.payloads.length > 1 ? `${write.label} (part ${i + 1} of ${write.payloads.length})` : write.label;
      onProgress({ label, phase: "sending", sent: before, total });
      await fb.download(payload.parts, (sent) => onProgress({ label, phase: "sending", sent: before + sent, total }), DOWNLOAD_TIMEOUT_MS);
      before += payload.size;
      onProgress({ label, phase: "writing", sent: before, total });
      await fb.command(`flash:${write.partition}`, FLASH_TIMEOUT_MS);
    }
  }

  onProgress({ label: "Restarting the device", phase: "restarting", sent: total, total });
  try {
    await fb.command("reboot");
  } catch (error) {
    // The device may restart before its reply gets out; everything is written.
    if (!(error instanceof FastbootError) || error.kind === "fail") throw error;
  }
}
