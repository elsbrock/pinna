// Identify a device in fastboot and decide what this page can do with it
// (ADR 0016, stage 1: read-only).
import { Fastboot, FastbootError } from "./fastboot.ts";

export type ModelId = "echo-dot-2" | "echo-2";

export const MODELS: Record<ModelId, { name: string; product: string }> = {
  "echo-dot-2": { name: "Echo Dot (2nd gen)", product: "BISCUIT" },
  "echo-2": { name: "Echo (2nd gen)", product: "RADAR" },
};

/**
 * LK builds the unlock has a pinned payload for, per fastboot product. Any
 * other build is refused (ADR 0016) until its payload is pinned.
 */
export const UNLOCK_BUILDS: Record<string, readonly string[]> = {
  BISCUIT: ["63cb91b-20221007_072309"],
  RADAR: ["59779ca-20220524_183401", "63cb91b-20221007_072309", "63cb91b-20221007_073612"],
};

/** What Pinna writes; each must exist under exactly this name. */
export const PARTITIONS = ["boot_a", "boot_b", "system_a", "system_b", "userdata"] as const;
export type Partition = (typeof PARTITIONS)[number];

export type Bootloader =
  | { kind: "kaeru"; version: string }
  | { kind: "stock"; reply: string }
  | { kind: "unknown"; reply: string };

export type DeviceReport = {
  product: string;
  serial: string | null;
  lkBuild: string | null;
  maxDownload: string | null;
  unlocked: string | null;
  bootloader: Bootloader;
  partitions: Record<Partition, number | null>;
};

// How LK refuses a command it doesn't know. Kaeru answers `oem kaeru-version`;
// a stock bootloader refuses it.
const REFUSED = /unknown command|not allowed|not supported|unsupported|invalid command|unrecognized|restricted on locked/i;

function parseSize(value: string | null): number | null {
  if (value === null) return null;
  const hex = value.trim().replace(/^0x/i, "");
  if (!/^[0-9a-f]+$/i.test(hex)) return null;
  return Number.parseInt(hex, 16);
}

/**
 * Ask the device what it is. Read-only: getvar, then `oem kaeru-version`.
 * Throws a FastbootError when a request gets no reply; `progress` names each
 * step for the page.
 */
export async function identify(fb: Fastboot, progress: (step: string) => void = () => {}): Promise<DeviceReport> {
  progress("Reading the product");
  const product = await fb.getvar("product");
  progress("Reading the bootloader's details");
  const serial = await fb.getvarOrNull("serialno");
  const lkBuild = await fb.getvarOrNull("lk_build_desc");
  const maxDownload = await fb.getvarOrNull("max-download-size");
  const unlocked = await fb.getvarOrNull("unlocked");

  progress("Reading the partition sizes");
  const partitions = {} as Record<Partition, number | null>;
  for (const name of PARTITIONS) partitions[name] = parseSize(await fb.getvarOrNull(`partition-size:${name}`));

  progress("Asking for Kaeru");
  let bootloader: Bootloader;
  try {
    const reply = await fb.command("oem kaeru-version");
    const version = [...reply.info, reply.message].map((s) => s.trim()).filter(Boolean).join(" ");
    bootloader = { kind: "kaeru", version };
  } catch (error) {
    if (!(error instanceof FastbootError) || error.kind !== "fail") throw error;
    bootloader = REFUSED.test(error.message) ? { kind: "stock", reply: error.message } : { kind: "unknown", reply: error.message };
  }

  return {
    product,
    serial: serial || null,
    lkBuild: lkBuild?.trim() || null,
    maxDownload: maxDownload?.trim() || null,
    unlocked: unlocked?.trim() ?? null,
    bootloader,
    partitions,
  };
}

export type Assessment = {
  /**
   * - `ready`: unlocked (Kaeru) with Pinna's partitions; Pinna can go on.
   * - `needs-unlock`: stock, and the unlock knows its LK build.
   * - `unknown-build`: stock, and the unlock does not know its LK build yet.
   * - `stop`: the page can't install this device.
   */
  state: "ready" | "needs-unlock" | "unknown-build" | "stop";
  headline: string;
  detail: string;
};

export function assess(report: DeviceReport, stated: ModelId): Assessment {
  const expected = MODELS[stated];
  const known = Object.values(MODELS).find((m) => m.product === report.product);

  if (!known) {
    return {
      state: "stop",
      headline: "This isn't an Echo (2nd gen) or an Echo Dot (2nd gen)",
      detail: `The device reports the product ${JSON.stringify(report.product)}. Pinna supports BISCUIT (Echo Dot (2nd gen)) and RADAR (Echo (2nd gen)).`,
    };
  }
  if (known !== expected) {
    return {
      state: "stop",
      headline: `The device reports ${report.product}, not ${expected.product}`,
      detail:
        stated === "echo-2" && report.product === "BISCUIT"
          ? "An Echo (2nd gen) that reports BISCUIT still runs an old boot chain from an earlier unlock. This page can't install it."
          : `You chose ${expected.name}, but the device in fastboot is ${known.name}. Check which device is connected.`,
    };
  }
  if (report.bootloader.kind === "unknown") {
    return {
      state: "stop",
      headline: "The bootloader is neither stock nor Kaeru",
      detail: `It answered \`oem kaeru-version\` with ${JSON.stringify(report.bootloader.reply)}. This page doesn't know that reply, so it stops here.`,
    };
  }
  if (report.bootloader.kind === "stock") {
    if (report.lkBuild && UNLOCK_BUILDS[report.product]?.includes(report.lkBuild)) {
      return {
        state: "needs-unlock",
        headline: "This device is stock, and the unlock knows its bootloader",
        detail: `It needs the bootloader unlock before Pinna. The unlock has a payload for LK build ${report.lkBuild}.`,
      };
    }
    return {
      state: "unknown-build",
      headline: "This device is stock, and the unlock doesn't know its bootloader yet",
      detail: report.lkBuild
        ? `Its LK build is ${report.lkBuild}. The unlock has no payload for this build yet: report it, so that it can be added.`
        : "The device doesn't report its LK build (lk_build_desc), so the unlock can't choose a payload for it.",
    };
  }

  const missing = PARTITIONS.filter((name) => report.partitions[name] === null);
  if (missing.length > 0) {
    return {
      state: "stop",
      headline: "The partition layout isn't the one Pinna expects",
      detail: `The device doesn't report ${missing.join(", ")}. It may use a partition layout from an earlier unlock.`,
    };
  }
  return {
    state: "ready",
    headline: "This device can take Pinna",
    detail: `Its bootloader is unlocked (Kaeru${report.bootloader.version ? ` ${report.bootloader.version}` : ""}), and it has every partition Pinna writes.`,
  };
}

/** The serial number with everything after the model prefix hidden. */
export function maskSerial(serial: string | null): string {
  if (!serial) return "(none)";
  return serial.length <= 6 ? "…" : `${serial.slice(0, 6)}…`;
}

const hex = (n: number | null) => (n === null ? "missing" : `0x${n.toString(16)}`);

/** Plain text to paste into an issue. The serial number is masked. */
export function formatReport(report: DeviceReport, stated: ModelId, assessment: Assessment, browser: string): string {
  const bootloader =
    report.bootloader.kind === "kaeru"
      ? `Kaeru ${report.bootloader.version}`.trim()
      : `${report.bootloader.kind} (oem kaeru-version: ${report.bootloader.reply})`;
  return [
    "Pinna device check",
    `model (chosen): ${MODELS[stated].name}`,
    `product: ${report.product}`,
    `serialno: ${maskSerial(report.serial)}`,
    `bootloader: ${bootloader}`,
    `lk_build_desc: ${report.lkBuild ?? "(none)"}`,
    `unlocked: ${report.unlocked === null ? "(no answer)" : JSON.stringify(report.unlocked)}`,
    `max-download-size: ${report.maxDownload ?? "(none)"}`,
    ...PARTITIONS.map((name) => `partition-size:${name}: ${hex(report.partitions[name])}`),
    `result: ${assessment.state}: ${assessment.headline}`,
    `browser: ${browser}`,
  ].join("\n");
}
