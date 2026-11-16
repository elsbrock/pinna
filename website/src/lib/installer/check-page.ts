// The read-only check on /install/ (ADR 0016, stage 1).
import { assess, formatReport, identify, maskSerial, MODELS, PARTITIONS, type Bootloader, type ModelId } from "./device.ts";
import { Fastboot, FastbootError } from "./fastboot.ts";
import { ConnectError, requestFastbootDevice, webUsb, WebUsbTransport } from "./webusb.ts";

const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;

type Platform = "linux" | "windows" | "mac" | "chromeos" | "android" | "other";

function platform(): Platform {
  const ua = navigator.userAgent;
  if (/CrOS/.test(ua)) return "chromeos";
  if (/Android/.test(ua)) return "android";
  if (/Windows/.test(ua)) return "windows";
  if (/Mac OS X/.test(ua)) return "mac";
  if (/Linux/.test(ua)) return "linux";
  return "other";
}

function browserName(): string {
  const data = (navigator as Navigator & { userAgentData?: { brands: { brand: string; version: string }[] } }).userAgentData;
  const brands = (data?.brands ?? []).filter((b) => !/not.a.brand/i.test(b.brand));
  const brand = brands.find((b) => b.brand !== "Chromium") ?? brands[0];
  return `${brand ? `${brand.brand} ${brand.version}` : "unknown browser"} on ${platform()}`;
}

function bootloaderText(bootloader: Bootloader): string {
  if (bootloader.kind === "stock") return "stock (locked)";
  if (bootloader.kind === "unknown") return "unknown";
  return bootloader.version ? `unlocked (Kaeru ${bootloader.version})` : "unlocked (Kaeru)";
}

export function startCheckPage(): void {
  const form = $<HTMLFormElement>("install-check");
  const connect = $<HTMLButtonElement>("install-connect");
  const status = $<HTMLParagraphElement>("install-status");
  const problem = $<HTMLDivElement>("install-problem");
  const result = $<HTMLElement>("install-result");

  const usb = webUsb();
  if (!usb) {
    $("install-unsupported").hidden = false;
    status.textContent = "This browser can't connect to the device.";
    return;
  }

  const chosenModel = (): ModelId | null => {
    const value = new FormData(form).get("model");
    return value === "echo-dot-2" || value === "echo-2" ? value : null;
  };
  form.addEventListener("change", () => {
    connect.disabled = chosenModel() === null;
    if (!connect.disabled) status.textContent = "";
  });

  const showProblem = (title: string, text: string) => {
    $("install-problem-title").textContent = title;
    $("install-problem-text").textContent = text;
    problem.hidden = false;
  };

  connect.addEventListener("click", async () => {
    const model = chosenModel();
    if (!model) return;
    problem.hidden = true;
    result.hidden = true;
    connect.disabled = true;
    status.textContent = "Waiting for you to choose the device…";

    let transport: WebUsbTransport | null = null;
    try {
      const device = await requestFastbootDevice(usb);
      if (!device) {
        status.textContent = "No device chosen. If it wasn't in the list, see “The device isn't in the list” below.";
        return;
      }
      transport = await WebUsbTransport.open(device);
      const report = await identify(new Fastboot(transport), (step) => (status.textContent = `${step}…`));
      const assessment = assess(report, model);

      $("install-headline").textContent = assessment.headline;
      $("install-detail").textContent = assessment.detail;
      $("install-next").textContent = {
        ready: "Installing from this page comes next. Until then, follow Install Pinna.",
        "needs-unlock": "Unlocking from this page comes later. Until then, this page can't install it.",
        "unknown-build": "Open an issue with the report below, so that the build can be added.",
        stop: "",
      }[assessment.state];

      const facts: [string, string][] = [
        ["Model", MODELS[model].name],
        ["Product", report.product],
        ["Serial number", maskSerial(report.serial)],
        ["Bootloader", bootloaderText(report.bootloader)],
        ["LK build", report.lkBuild ?? "not reported"],
        ...PARTITIONS.map((name): [string, string] => {
          const size = report.partitions[name];
          return [name, size === null ? "missing" : `${(size / 2 ** 20).toFixed(0)} MiB`];
        }),
      ];
      const dl = $("install-facts");
      dl.replaceChildren(
        ...facts.flatMap(([term, value]) => {
          const dt = document.createElement("dt");
          dt.className = "text-ink-muted";
          dt.textContent = term;
          const dd = document.createElement("dd");
          dd.className = "font-mono";
          dd.textContent = value;
          return [dt, dd];
        }),
      );

      const text = formatReport(report, model, assessment, browserName());
      $<HTMLTextAreaElement>("install-report").value = text;
      const issue = $<HTMLAnchorElement>("install-issue");
      const url = new URL(issue.href);
      url.searchParams.set("title", `Device check: ${report.product}, ${assessment.state}`);
      url.searchParams.set("body", "```\n" + text + "\n```\n");
      issue.href = url.toString();

      result.hidden = false;
      status.textContent = "Done. Nothing was written to the device.";
    } catch (error) {
      status.textContent = "";
      if (error instanceof ConnectError) {
        if (error.problem === "permission") {
          showProblem(
            "The browser can't open the device",
            platform() === "linux"
              ? "Linux doesn't let your user open it yet. Add the udev rule under “The browser can't open the device” below, then plug the device in again."
              : `${error.message}. See “The browser can't open the device” below.`,
          );
        } else if (error.problem === "busy") {
          showProblem(
            "The browser can't use the device",
            platform() === "windows"
              ? "Windows has no WinUSB driver for it, or another program uses it. See “The browser can't use the device” below."
              : "Another program uses it: fastboot, adb, or this page in another tab. Close them and connect again.",
          );
        } else {
          showProblem("This isn't a device in fastboot", "Start the device in fastboot as described in step 2, and connect again.");
        }
      } else if (error instanceof FastbootError && error.kind === "timeout") {
        showProblem(
          "The device doesn't answer",
          "The page stopped after the first request without a reply and sent nothing more. See “The device doesn't answer” below.",
        );
      } else if (error instanceof FastbootError) {
        showProblem("The check stopped", `${error.message}. Unplug the device, start it in fastboot again, and connect again.`);
      } else {
        showProblem("The check stopped", String(error));
      }
    } finally {
      await transport?.close();
      connect.disabled = chosenModel() === null;
    }
  });

  $("install-copy").addEventListener("click", async () => {
    const text = $<HTMLTextAreaElement>("install-report").value;
    try {
      await navigator.clipboard.writeText(text);
      status.textContent = "Report copied.";
    } catch {
      $<HTMLTextAreaElement>("install-report").select();
      status.textContent = "Select the report and copy it with Ctrl+C.";
    }
  });

}
