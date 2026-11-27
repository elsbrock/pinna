// Check and install an Echo Dot (5th gen) in TWRP, over adb (ADR 0016, the
// Echo Dot (5th gen) section): what `scripts/install-echo-dot-5.sh check` and
// `install` do, in the same order.
import type { Adb, ShellOptions } from "./adb.ts";
import { maskSerial, type Assessment } from "./device.ts";
import { InstallError, type Progress } from "./install.ts";
import { sha256 } from "./release.ts";

/** idme's device type of the C2N6L4 (install-echo-dot-5.sh). */
export const DEVICE_TYPE_ID = "A4ZXE0RM7LQ7A";
/** The UBI volumes the install needs: it writes boot_a, system_a and the slot record in misc. */
export const VOLUMES = ["boot_a", "system_a", "misc"] as const;
/** Where the tool goes: TWRP's /tmp is RAM, so nothing on the device changes. */
const TOOL = "/tmp/pinna-bootctl";
/** Writing a volume and reading it back took about 20 s for the root on a C2N6L4; leave room. */
const WRITE_VOLUME_TIMEOUT_MS = 600_000;

export type Volume = { node: string; name: string; bytes: number };

export type TwrpReport = {
  /** The USB serial number, which is the device's. */
  serial: string | null;
  /** The banner's state: TWRP's adbd says "recovery". */
  state: string;
  device: string | null;
  twrp: string | null;
  deviceTypeId: string | null;
  volumes: Volume[];
};

export type TwrpImages = { boot: Uint8Array; root: Uint8Array; bootctl: Uint8Array };

/** A command in TWRP that ended with an exit status other than 0. */
export class CommandError extends Error {
  readonly step: string;
  readonly exit: number;
  readonly output: string;

  constructor(step: string, exit: number, output: string, message: string) {
    super(message);
    this.name = "CommandError";
    this.step = step;
    this.exit = exit;
    this.output = output;
  }
}

/** A command's stdout without TWRP's carriage returns, trimmed. */
async function read(adb: Adb, command: string): Promise<string> {
  return (await adb.shell(command)).stdout.replace(/\r/g, "").trim();
}

/** The UBI volumes, from lines of "node name data_bytes". */
export function parseVolumes(text: string): Volume[] {
  const volumes: Volume[] = [];
  for (const line of text.split("\n")) {
    const [node, name, bytes] = line.trim().split(/\s+/);
    if (node && name && /^\d+$/.test(bytes ?? "")) volumes.push({ node, name, bytes: Number(bytes) });
  }
  return volumes;
}

/** Ask TWRP what the device is. Read-only. */
export async function identifyTwrp(adb: Adb, serial: string | null, progress: (step: string) => void = () => {}): Promise<TwrpReport> {
  progress("Connecting to TWRP");
  const banner = adb.banner ?? (await adb.connect());
  progress("Reading TWRP's version");
  const twrp = await read(adb, "getprop ro.twrp.version");
  progress("Reading the device type");
  const deviceTypeId = (await read(adb, "cat /proc/idme/device_type_id 2>/dev/null")).replace(/\0/g, "").trim();
  progress("Reading the UBI volumes");
  const volumes = parseVolumes(
    await read(adb, 'for d in /sys/class/ubi/ubi*_*; do echo "$(basename $d) $(cat $d/name) $(cat $d/data_bytes)"; done'),
  );
  return {
    serial: serial || null,
    state: banner.state,
    device: banner.props["ro.product.device"] ?? null,
    twrp: twrp || null,
    deviceTypeId: deviceTypeId || null,
    volumes,
  };
}

export function assessTwrp(report: TwrpReport): Assessment {
  if (!report.twrp) {
    return {
      state: "stop",
      headline: "The device doesn't run TWRP",
      detail: "It answers over adb, but reports no TWRP version. Start TWRP as described in step 3, and connect again.",
    };
  }
  if (report.deviceTypeId !== DEVICE_TYPE_ID) {
    return {
      state: "stop",
      headline: "This isn't an Echo Dot (5th gen), model C2N6L4",
      detail: `Its device type is ${report.deviceTypeId ? JSON.stringify(report.deviceTypeId) : "missing"}; an Echo Dot (5th gen), model C2N6L4, reports ${DEVICE_TYPE_ID}.`,
    };
  }
  const missing = VOLUMES.filter((name) => !report.volumes.some((v) => v.name === name));
  if (missing.length > 0) {
    return {
      state: "stop",
      headline: "The volumes aren't the ones Pinna expects",
      detail: `TWRP doesn't show the UBI volume${missing.length > 1 ? "s" : ""} ${missing.join(", ")}.`,
    };
  }
  return {
    state: "ready",
    headline: "This device can take Pinna",
    detail: `It runs TWRP ${report.twrp}, and has the volumes Pinna writes.`,
  };
}

const volume = (report: TwrpReport, name: string): Volume => {
  const v = report.volumes.find((x) => x.name === name);
  if (!v) throw new InstallError(`The device has no UBI volume ${name}.`);
  return v;
};

/** The volumes the install writes. Throws an InstallError, before anything is written, when an image doesn't fit. */
export function planTwrp(report: TwrpReport, images: TwrpImages): { boot: Volume; system: Volume; misc: Volume } {
  const boot = volume(report, "boot_a");
  const system = volume(report, "system_a");
  const misc = volume(report, "misc");
  if (images.boot.length > boot.bytes) throw new InstallError(`The boot image (${images.boot.length} bytes) is bigger than boot_a (${boot.bytes} bytes).`);
  if (images.root.length > system.bytes) throw new InstallError(`The root image (${images.root.length} bytes) is bigger than system_a (${system.bytes} bytes).`);
  return { boot, system, misc };
}

/** The last line a failed command printed, for the page. */
const lastLine = (text: string) =>
  text
    .replace(/\r/g, "")
    .split("\n")
    .map((l) => l.trim())
    .filter(Boolean)
    .pop() ?? "";

/**
 * Copy pinna-bootctl into TWRP, write boot_a and system_a, start slot A on
 * trial, and restart the device. Only those two volumes and the slot record
 * in misc are written. A failure stops it where it is: TWRP keeps running,
 * and until `set-active` the slot record is as it was.
 */
export async function installTwrp(adb: Adb, report: TwrpReport, images: TwrpImages, onProgress: (p: Progress) => void = () => {}): Promise<void> {
  const { misc } = planTwrp(report, images);
  const total = images.bootctl.length + images.boot.length + images.root.length;
  let before = 0;

  const run = async (label: string, command: string, opts: ShellOptions & { failed: string }) => {
    onProgress({ label, phase: opts.stdin ? "sending" : "writing", sent: before, total });
    const result = await adb.shell(command, {
      ...opts,
      onSent: (n) => onProgress({ label, phase: n === opts.stdin?.length ? "writing" : "sending", sent: before + n, total }),
    });
    before += opts.stdin?.length ?? 0;
    const output = `${result.stderr}\n${result.stdout}`;
    if (result.exit !== 0) {
      const said = lastLine(output);
      throw new CommandError(label, result.exit, output, `${opts.failed}${said ? ` It said: ${said}` : ""}`);
    }
    return result.stdout.replace(/\r/g, "").trim();
  };

  const nothing = "Nothing on the device was written.";
  await run("Copying pinna-bootctl to TWRP", `cat > ${TOOL}.part && chmod 755 ${TOOL}.part && mv ${TOOL}.part ${TOOL}`, {
    stdin: images.bootctl,
    failed: `TWRP couldn't store pinna-bootctl in /tmp. ${nothing}`,
  });
  const kind = await run("Checking the storage", `${TOOL} storage-kind`, { failed: `pinna-bootctl doesn't run in TWRP. ${nothing}` });
  if (kind !== "ubi") throw new InstallError(`pinna-bootctl in TWRP sees ${JSON.stringify(kind)} storage, not UBI. ${nothing}`);
  const loader = await run("Checking the bootloader", `${TOOL} bootloader`, { failed: `pinna-bootctl doesn't run in TWRP. ${nothing}` });
  if (loader !== "amazon-uboot") throw new InstallError(`pinna-bootctl in TWRP sees the bootloader ${JSON.stringify(loader)}, not Amazon's U-Boot. ${nothing}`);
  // Slot A starts on trial only with slot B (TWRP) good to fall back to:
  // check before slot A is overwritten, not after.
  const slots = await run("Reading the slot record", `${TOOL} --misc /dev/${misc.node} slot-status`, { failed: `pinna-bootctl couldn't read the slot record. ${nothing}` });
  if (!/^b: good/m.test(slots)) {
    throw new InstallError(`Slot B isn't marked good in the slot record, so slot A couldn't start on trial. ${nothing}`);
  }

  const rest = "TWRP still runs, and the slot record is as it was: install again before you restart the device.";
  for (const [name, bytes, what] of [
    ["boot_a", images.boot, "boot image"],
    ["system_a", images.root, "root image"],
  ] as const) {
    await run(`Writing the ${what} to ${name}`, `${TOOL} write-volume ${name} --size ${bytes.length} --sha256 ${await sha256(bytes)}`, {
      stdin: bytes,
      timeoutMs: WRITE_VOLUME_TIMEOUT_MS,
      failed: `Writing ${name} failed. ${rest}`,
    });
  }
  await run("Setting slot A to start", `${TOOL} --misc /dev/${misc.node} set-active a`, {
    failed: "boot_a and system_a are written, but slot A couldn't be set to start. TWRP still runs: install again before you restart the device.",
  });

  onProgress({ label: "Restarting the device", phase: "restarting", sent: total, total });
  try {
    await adb.reboot();
  } catch {
    // The device may restart before the request is out; everything is written.
  }
}

/** Plain text to paste into an issue. The serial number is masked. */
export function formatTwrpReport(report: TwrpReport, assessment: Assessment, browser: string): string {
  return [
    "Pinna device check",
    "model (chosen): Echo Dot (5th gen)",
    `serialno: ${maskSerial(report.serial)}`,
    `adb state: ${report.state}`,
    `ro.product.device: ${report.device ?? "(none)"}`,
    `ro.twrp.version: ${report.twrp ?? "(none)"}`,
    `device_type_id: ${report.deviceTypeId ?? "(none)"}`,
    ...report.volumes.map((v) => `volume ${v.name} (${v.node}): ${v.bytes} bytes`),
    `result: ${assessment.state}: ${assessment.headline}`,
    `browser: ${browser}`,
  ].join("\n");
}
