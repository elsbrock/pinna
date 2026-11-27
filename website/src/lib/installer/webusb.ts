// The fastboot and adb transport over WebUSB (ADR 0016). Chromium-based browsers only.
import { TimeoutError, type Transport } from "./fastboot.ts";

// The parts of WebUSB this page uses. TypeScript's DOM library leaves WebUSB
// out, and the page takes no dependency for it.
type UsbEndpoint = { endpointNumber: number; direction: "in" | "out"; type: string; packetSize: number };
type UsbAlternate = {
  alternateSetting: number;
  interfaceClass: number;
  interfaceSubclass: number;
  interfaceProtocol: number;
  endpoints: UsbEndpoint[];
};
type UsbInterface = { interfaceNumber: number; alternates: UsbAlternate[] };
type UsbConfiguration = { configurationValue: number; interfaces: UsbInterface[] };

export type UsbDevice = {
  readonly vendorId: number;
  readonly productId: number;
  readonly serialNumber?: string;
  readonly opened: boolean;
  readonly configuration: UsbConfiguration | null;
  readonly configurations: UsbConfiguration[];
  open(): Promise<void>;
  close(): Promise<void>;
  selectConfiguration(value: number): Promise<void>;
  claimInterface(number: number): Promise<void>;
  selectAlternateInterface(number: number, alternate: number): Promise<void>;
  transferIn(endpoint: number, length: number): Promise<{ status: string; data?: DataView }>;
  transferOut(endpoint: number, data: Uint8Array): Promise<{ status: string; bytesWritten: number }>;
};

type Filter = { vendorId: number; productId: number };

type Usb = EventTarget & {
  requestDevice(options: { filters: Filter[] }): Promise<UsbDevice>;
};

/**
 * Fastboot's USB identity on the Echo Dot (2nd gen), stock and with Kaeru
 * (measured). The interface is vendor-specific: class ff, subclass 42,
 * protocol 03. The device class is 0, so a class filter wouldn't match.
 */
export const FASTBOOT_FILTERS = [{ vendorId: 0x0bb4, productId: 0x0c01 }];

/**
 * TWRP's adb on the Echo Dot (5th gen), from firebreak's Cannoli TWRP
 * (`cannoli_twrp/assets/usb.rc`): Google's recovery ID, product
 * "Cannoli-TWRP", one adb interface (ff/42/01).
 */
export const TWRP_FILTERS = [{ vendorId: 0x18d1, productId: 0xd001 }];

export type UsbInterfaceKind = "fastboot" | "adb";

const INTERFACES: Record<UsbInterfaceKind, { cls: number; subclass: number; protocol: number }> = {
  fastboot: { cls: 0xff, subclass: 0x42, protocol: 0x03 },
  adb: { cls: 0xff, subclass: 0x42, protocol: 0x01 },
};

/** WebUSB, or null where the browser or the page's origin doesn't offer it. */
export function webUsb(): Usb | null {
  if (typeof navigator === "undefined" || !globalThis.isSecureContext) return null;
  return (navigator as Navigator & { usb?: Usb }).usb ?? null;
}

/**
 * - `permission`: the browser may not open the device (Linux: no udev rule).
 * - `busy`: another program holds the interface (adb, fastboot, another tab),
 *   or on Windows no WinUSB driver is bound to it.
 * - `not-fastboot`: the device has no fastboot interface.
 * - `not-adb`: the device has no adb interface.
 */
export type ConnectProblem = "permission" | "busy" | "not-fastboot" | "not-adb";

export class ConnectError extends Error {
  readonly problem: ConnectProblem;

  constructor(problem: ConnectProblem, message: string) {
    super(message);
    this.name = "ConnectError";
    this.problem = problem;
  }
}

/** Ask the user to pick the device. Null when they close the dialog without one. */
export async function requestDevice(usb: Usb, filters: Filter[]): Promise<UsbDevice | null> {
  try {
    return await usb.requestDevice({ filters });
  } catch (error) {
    if (error instanceof DOMException && error.name === "NotFoundError") return null;
    throw error;
  }
}

export class WebUsbTransport implements Transport {
  readonly device: UsbDevice;
  readonly #in: number;
  readonly #out: number;
  readonly #packet: number;
  /** After a transfer that fills whole packets, send an empty one (adb only). */
  readonly #zlp: number;

  private constructor(device: UsbDevice, endpointIn: number, endpointOut: number, packet: number, zlp: number) {
    this.device = device;
    this.#in = endpointIn;
    this.#out = endpointOut;
    this.#packet = packet;
    this.#zlp = zlp;
  }

  /** Open the device and claim its fastboot or adb interface. */
  static async open(device: UsbDevice, kind: UsbInterfaceKind = "fastboot"): Promise<WebUsbTransport> {
    const want = INTERFACES[kind];
    try {
      if (!device.opened) await device.open();
    } catch (error) {
      throw new ConnectError("permission", `the browser could not open the device: ${String(error)}`);
    }
    if (!device.configuration) await device.selectConfiguration(device.configurations[0]?.configurationValue ?? 1);

    for (const iface of device.configuration?.interfaces ?? []) {
      for (const alt of iface.alternates) {
        if (alt.interfaceClass !== want.cls || alt.interfaceSubclass !== want.subclass || alt.interfaceProtocol !== want.protocol) {
          continue;
        }
        const endpointIn = alt.endpoints.find((e) => e.direction === "in" && e.type === "bulk");
        const endpointOut = alt.endpoints.find((e) => e.direction === "out" && e.type === "bulk");
        if (!endpointIn || !endpointOut) continue;
        try {
          await device.claimInterface(iface.interfaceNumber);
          if (alt.alternateSetting !== 0) await device.selectAlternateInterface(iface.interfaceNumber, alt.alternateSetting);
        } catch (error) {
          await device.close().catch(() => {});
          throw new ConnectError("busy", `the browser could not claim the ${kind} interface: ${String(error)}`);
        }
        // The adb host ends a transfer that fills whole packets with an empty
        // one, so that an adbd reading into a larger buffer sees its end.
        // Fastboot must not get one: the Echo Dot's LK takes any short
        // transfer as the end of a download.
        const zlp = kind === "adb" ? endpointOut.packetSize : 0;
        return new WebUsbTransport(device, endpointIn.endpointNumber, endpointOut.endpointNumber, endpointIn.packetSize, zlp);
      }
    }
    await device.close().catch(() => {});
    throw new ConnectError(kind === "adb" ? "not-adb" : "not-fastboot", `the device has no ${kind} interface`);
  }

  async write(bytes: Uint8Array): Promise<void> {
    const result = await this.device.transferOut(this.#out, bytes);
    if (result.status !== "ok") throw new Error(`USB write: ${result.status}`);
    if (this.#zlp > 0 && bytes.length > 0 && bytes.length % this.#zlp === 0) {
      const end = await this.device.transferOut(this.#out, new Uint8Array(0));
      if (end.status !== "ok") throw new Error(`USB write: ${end.status}`);
    }
  }

  async read(timeoutMs: number, length?: number): Promise<Uint8Array> {
    let timer: ReturnType<typeof setTimeout> | undefined;
    const timeout = new Promise<never>((_, reject) => {
      timer = setTimeout(() => reject(new TimeoutError()), timeoutMs);
    });
    try {
      // A fastboot reply is one short transfer; ask for a whole packet or more.
      const result = await Promise.race([this.device.transferIn(this.#in, length ?? Math.max(this.#packet, 512)), timeout]);
      if (result.status !== "ok" || !result.data) throw new Error(`USB read: ${result.status}`);
      return new Uint8Array(result.data.buffer, result.data.byteOffset, result.data.byteLength);
    } finally {
      clearTimeout(timer);
    }
  }

  /** Release the device; a transfer still waiting for a reply ends with it. */
  async close(): Promise<void> {
    await this.device.close().catch(() => {});
  }
}
