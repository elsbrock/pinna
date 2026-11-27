// /install/ (ADR 0016): check a device in fastboot, or an Echo Dot (5th gen)
// in TWRP, then install Pinna on it.
import { Adb, AdbError } from "./adb.ts";
import {
  assess,
  formatReport,
  identify,
  maskSerial,
  MODEL_NAMES,
  PARTITIONS,
  type Assessment,
  type Bootloader,
  type DeviceReport,
  type FastbootModelId,
  type ModelId,
} from "./device.ts";
import { Fastboot, FastbootError } from "./fastboot.ts";
import { assessTwrp, CommandError, formatTwrpReport, identifyTwrp, installTwrp, type TwrpReport } from "./twrp.ts";
import { install, InstallError, plan, type Progress } from "./install.ts";
import { DownloadError, fetchManifest, fetchRelease, parseKey, type Channel } from "./download.ts";
import { matchRelease, type Release } from "./release.ts";

/** keys/release.pub, put in at build time (astro.config.mjs). */
declare const __PINNA_RELEASE_KEY__: string;
import { ConnectError, FASTBOOT_FILTERS, requestDevice, TWRP_FILTERS, webUsb, WebUsbTransport, type UsbDevice } from "./webusb.ts";

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

/** An error message as a sentence: capital first letter, one full stop. */
function sentence(text: string): string {
  const t = text.trim().replace(/\.+$/, "");
  return t ? `${t[0].toUpperCase()}${t.slice(1)}.` : "";
}

const MiB = (n: number) => (n / 2 ** 20).toFixed(0);

const TONE: Record<Assessment["state"], "ok" | "caution" | "danger"> = {
  ready: "ok",
  "needs-unlock": "caution",
  "unknown-build": "caution",
  stop: "danger",
};

const NEXT: Record<Assessment["state"], string> = {
  ready: "",
  "needs-unlock": "This page can't unlock it yet. Unplug the device; it starts as before.",
  "unknown-build": "Open an issue with the report below, so that the build can be added. Unplug the device; it starts as before.",
  stop: "Unplug the device; it starts as before. If the result looks wrong, open an issue with the report below.",
};

/** What the check found: a device in fastboot, or an Echo Dot (5th gen) in TWRP. */
type Checked =
  | { kind: "fastboot"; report: DeviceReport; assessment: Assessment; model: FastbootModelId }
  | { kind: "twrp"; report: TwrpReport; assessment: Assessment; model: "echo-dot-5" };

/** Where to start again after the device went away, by how the model connects. */
const RESTART: Record<Checked["kind"], string> = {
  fastboot: "start it in fastboot again (step 3)",
  twrp: "start TWRP again (step 3)",
};

/** Open the help entry (a <details>) that the address's #fragment names. */
function openHelp(): void {
  const target = location.hash ? document.getElementById(decodeURIComponent(location.hash.slice(1))) : null;
  if (target instanceof HTMLDetailsElement) target.open = true;
}

export function startInstallPage(): void {
  openHelp();
  window.addEventListener("hashchange", openHelp);
  // A second click on the same link changes no hash, so open it on click too.
  document.addEventListener("click", (event) => {
    const link = (event.target as HTMLElement).closest<HTMLAnchorElement>('a[href^="#"]');
    const target = link ? document.getElementById(decodeURIComponent(link.hash.slice(1))) : null;
    if (target instanceof HTMLDetailsElement) target.open = true;
  });

  const form = $<HTMLFormElement>("install-form");
  const files = $<HTMLInputElement>("install-files");
  const connect = $<HTMLButtonElement>("install-connect");
  const status = $<HTMLParagraphElement>("install-status");
  const result = $<HTMLDivElement>("install-result");
  const confirm = $<HTMLElement>("install-confirm");
  const agree = $<HTMLInputElement>("install-agree");
  const start = $<HTMLButtonElement>("install-start");
  const startHint = $<HTMLSpanElement>("install-start-hint");
  const problem = $<HTMLDivElement>("install-problem");
  const reportBox = $<HTMLElement>("install-report-box");
  const done = $<HTMLDivElement>("install-done");
  const download = $<HTMLButtonElement>("install-download");

  const usb = webUsb();
  if (!usb) {
    $("install-unsupported").hidden = false;
    // Nothing on the page works without WebUSB: don't offer it.
    for (const el of form.querySelectorAll<HTMLInputElement | HTMLButtonElement>("input, button")) el.disabled = true;
    connect.disabled = true;
    status.textContent = "This browser can't connect to the device.";
    return;
  }

  // What the page knows. `busy` blocks every control while the device is in use.
  let busy = false;
  let release: Release | null = null;
  let device: UsbDevice | null = null;
  let checked: Checked | null = null;
  // Where the last attempt stopped: marks that step as failed.
  let failedAt: "check" | "install" | null = null;
  // Whether `release` came from Download (and so from the chosen channel).
  let downloaded = false;

  // The five steps on the page, in order, and what each one waits for.
  const STEPS = ["model", "release", "fastboot", "connect", "install"].map((name) => $(`install-step-${name}`));
  type StepState = "done" | "current" | "pending" | "failed";

  const chosenModel = (): ModelId | null => {
    // Not FormData: it leaves out the radios while they are disabled.
    const value = form.querySelector<HTMLInputElement>('input[name="model"]:checked')?.value;
    return value === "echo-dot-2" || value === "echo-2" || value === "echo-dot-5" ? value : null;
  };

  const sync = () => {
    const model = chosenModel();
    for (const input of form.querySelectorAll("input")) input.disabled = busy;
    connect.disabled = busy || model === null;
    // The files differ by model: choose it first.
    download.disabled = busy || model === null;
    files.disabled = busy || model === null;
    if (!busy && !checked && status.textContent === "Choose your device first." && model) status.textContent = "";
    const ready = checked?.assessment.state === "ready";
    confirm.hidden = !ready;
    agree.disabled = busy;
    start.disabled = busy || !ready || !release || !agree.checked;
    startHint.textContent = !ready || busy ? "" : !release ? "Get Pinna in step 2 first." : !agree.checked ? "Tick the box first." : "";

    // The first step that isn't done is the one to do now.
    const installed = !done.hidden;
    const isDone = [model !== null, release !== null, checked !== null, ready, installed].map((d) => installed || d);
    const isFailed = [false, false, false, failedAt === "check" || (checked !== null && !ready), failedAt === "install"];
    // Steps after a failed one wait until it is fixed.
    let current = false;
    let failed = false;
    STEPS.forEach((step, i) => {
      let state: StepState = "pending";
      if (isDone[i]) state = "done";
      else if (isFailed[i]) state = "failed";
      else if (!current && !failed) {
        state = "current";
        current = true;
      }
      if (state === "failed") failed = true;
      step.dataset.state = state;
    });
  };

  const clearOutcome = () => {
    for (const el of [problem, result, reportBox, done]) el.hidden = true;
    checked = null;
    failedAt = null;
    agree.checked = false;
    $("install-progress-box").hidden = true;
  };

  const showProblem = (title: string, text: string, link?: { href: string; label: string }) => {
    $("install-problem-title").textContent = title;
    const body = $("install-problem-text");
    body.replaceChildren(text);
    if (link) {
      const a = document.createElement("a");
      a.className = "link";
      a.href = link.href;
      a.textContent = link.label;
      body.append(" See ", a, ".");
    }
    problem.hidden = false;
  };

  const showReport = (text: string, title: string) => {
    $<HTMLTextAreaElement>("install-report").value = text;
    const issue = $<HTMLAnchorElement>("install-issue");
    const url = new URL(issue.href);
    url.searchParams.set("title", title);
    url.searchParams.set("body", "```\n" + text + "\n```\n");
    issue.href = url.toString();
    $("install-copy-status").textContent = "";
    reportBox.hidden = false;
  };
  /** Show the report open after a failure, folded away after a good check. */
  const reportOpen = (open: boolean) => {
    (reportBox as HTMLDetailsElement).open = open;
  };

  /** The check's report for the device the page read last, as text. */
  const deviceText = (found: Checked) =>
    found.kind === "twrp"
      ? formatTwrpReport(found.report, found.assessment, browserName())
      : formatReport(found.report, found.model, found.assessment, browserName());

  const failureReport = (model: ModelId | null, stage: string, step: string, error: unknown, found: Checked | null) =>
    [
      "Pinna install page",
      `model (chosen): ${model ? MODEL_NAMES[model] : "(none)"}`,
      `stopped while: ${stage}${step ? `: ${step}` : ""}`,
      `error: ${error instanceof Error ? `${error.name}: ${error.message}` : String(error)}`,
      ...(error instanceof CommandError && error.output.trim() ? [`output: ${error.output.trim().split("\n").slice(-5).join(" / ")}`] : []),
      ...(found ? ["", deviceText(found)] : [`browser: ${browserName()}`]),
    ].join("\n");

  /** Explain a failure, with a report to paste. */
  const explain = (error: unknown, model: ModelId | null, stage: string, step: string, found: Checked | null) => {
    failedAt = stage === "install" ? "install" : "check";
    $(`install-outcome-${failedAt}`).append(problem, reportBox);
    reportOpen(true);
    const os = platform();
    const twrp = model === "echo-dot-5";
    const gone =
      ((error instanceof FastbootError || error instanceof AdbError) && error.kind === "timeout") ||
      ((error instanceof FastbootError || error instanceof AdbError) && error.kind === "transport" && /disconnect/i.test(error.message));
    if (error instanceof ConnectError && os === "windows" && error.problem !== "not-fastboot" && error.problem !== "not-adb") {
      showProblem("The browser can't use the device", "Windows may have no WinUSB driver for it, or another program uses it.", { href: "#windows", label: "Windows needs the WinUSB driver" });
    } else if (error instanceof ConnectError && error.problem === "permission") {
      showProblem(
        "The browser can't open the device",
        os === "linux" ? "Linux doesn't let your user open it yet." : "The browser isn't allowed to open it.",
        { href: "#cannot-open", label: "The browser can't open the device" },
      );
    } else if (error instanceof ConnectError && error.problem === "busy") {
      showProblem("Another program uses the device", "Close it and connect again.", { href: "#busy", label: "Another program uses the device" });
    } else if (error instanceof ConnectError && error.problem === "not-adb") {
      showProblem("This isn't TWRP", "Start TWRP as described in step 3, and connect again.");
    } else if (error instanceof ConnectError) {
      showProblem("This isn't a device in fastboot", "Start the device in fastboot as described in step 3, and connect again.");
    } else if (gone) {
      showProblem(
        "The device doesn't answer, or went away",
        stage === "install"
          ? `The page stopped${step ? ` at “${step}”` : ""} and sent nothing more. Unplug the device, ${RESTART[twrp ? "twrp" : "fastboot"]}, and connect again (step 4).`
          : "The page stopped and sent nothing more.",
        { href: "#no-reply", label: "The device doesn't answer, or goes away" },
      );
    } else if (error instanceof AdbError && error.kind === "auth") {
      showProblem(
        "TWRP asks for an adb key",
        "This page can't answer it. firebreak's TWRP for the Echo Dot (5th gen) doesn't ask for one, so this may be another build. Install from a terminal instead.",
      );
    } else if (error instanceof AdbError && error.kind === "unsupported") {
      showProblem("TWRP's adb is too old for this page", `${sentence(error.message)} Install from a terminal instead.`);
    } else if (error instanceof FastbootError && error.kind === "fail" && stage === "install") {
      showProblem("The device refused a write", `${step ? `${step}: it` : "It"} answered ${JSON.stringify(error.message)}. Nothing after that step was written.`);
    } else if (error instanceof CommandError) {
      showProblem("The device refused a step", `${error.step}: ${error.message}`);
    } else if (error instanceof InstallError) {
      showProblem("Pinna can't be installed on this device", error.message);
    } else {
      showProblem("The page stopped", sentence(error instanceof Error ? error.message : String(error)));
    }
    showReport(failureReport(model, stage, step, error, found), `Install page: ${stage} failed`);
  };

  const showResult = (found: Checked) => {
    const { assessment, model } = found;
    result.dataset.tone = TONE[assessment.state];
    for (const icon of result.querySelectorAll<HTMLElement>("[data-icon]")) icon.hidden = icon.dataset.icon !== TONE[assessment.state];
    $("install-headline").textContent = assessment.headline;
    $("install-detail").textContent = assessment.detail;
    $("install-next").textContent =
      found.kind === "twrp" && assessment.state === "stop"
        ? "To leave TWRP, unplug the power and plug it in again. If the result looks wrong, open an issue with the report below."
        : NEXT[assessment.state];
    const facts: [string, string][] =
      found.kind === "twrp"
        ? [
            ["Model", MODEL_NAMES[model]],
            ["Serial number", maskSerial(found.report.serial)],
            ["TWRP", found.report.twrp ?? "not reported"],
            ["Device type", found.report.deviceTypeId ?? "not reported"],
            ...["boot_a", "system_a", "misc"].map((name): [string, string] => {
              const v = found.report.volumes.find((x) => x.name === name);
              return [name, v ? `${MiB(v.bytes)} MiB` : "missing"];
            }),
          ]
        : [
            ["Model", MODEL_NAMES[model]],
            ["Product", found.report.product],
            ["Serial number", maskSerial(found.report.serial)],
            ["Bootloader", bootloaderText(found.report.bootloader)],
            ["LK build", found.report.lkBuild ?? "not reported"],
            ...PARTITIONS.map((name): [string, string] => {
              const size = found.report.partitions[name];
              return [name, size === null ? "missing" : `${MiB(size)} MiB`];
            }),
          ];
    $("install-facts").replaceChildren(
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
    result.hidden = false;
    $("install-outcome-check").append(reportBox);
    reportOpen(assessment.state !== "ready");
    showReport(deviceText(found), `Device check: ${found.kind === "twrp" ? "Echo Dot (5th gen)" : found.report.product}, ${assessment.state}`);
    $("install-agree-text").textContent =
      found.kind === "twrp"
        ? `I want to replace Fire OS in slot A of this ${MODEL_NAMES[model]} (${maskSerial(found.report.serial)}) with Pinna.`
        : `I want to erase this ${MODEL_NAMES[model]} (${maskSerial(found.report.serial)}) and install Pinna on it.`;
  };

  form.addEventListener("change", (event) => {
    // The confirmation box sits in the form too; it has its own listener.
    if (busy || event.target === agree) return;
    const name = (event.target as HTMLInputElement).name;
    if (name === "model") {
      // Another model: what the page found no longer applies, nor files for another model.
      clearOutcome();
      device = null;
      status.textContent = "";
      if (release && release.model !== chosenModel()) {
        release = null;
        downloaded = false;
        files.value = "";
        $("install-download-status").replaceChildren();
        $("install-files-status").replaceChildren();
      }
    } else if (name === "channel") {
      showLatest();
      if (downloaded) {
        // The release downloaded is from the other channel.
        release = null;
        downloaded = false;
        $("install-download-status").replaceChildren();
      }
    }
    sync();
  });
  agree.addEventListener("change", sync);
  // Back/forward restores the radios without a change event.
  window.addEventListener("pageshow", sync);

  const chosenChannel = () => (form.querySelector<HTMLInputElement>('input[name="channel"]:checked')?.value ?? "stable") as Channel;

  // The button names the newest release on the chosen channel: the signed
  // manifest only, no image. Without an answer it stays "Download", and a
  // click explains why.
  const label = $("install-download-label");
  let latestAsked = 0;
  const showLatest = async () => {
    const asked = ++latestAsked;
    label.textContent = "Download";
    try {
      const { manifest } = await fetchManifest(chosenChannel(), parseKey(__PINNA_RELEASE_KEY__));
      if (asked === latestAsked) label.textContent = `Download Pinna ${manifest.version}`;
    } catch {
      // Download reports it.
    }
  };

  // The release service (ADR 0022): signature and files checked here.
  download.addEventListener("click", async () => {
    const model = chosenModel();
    if (busy || !model) return;
    const list = $("install-download-status");
    const channel = chosenChannel();
    release = null;
    $("install-files-status").replaceChildren();
    busy = true;
    sync();
    const line = (text: string, tone: "ok" | "danger" | "muted") => {
      const li = document.createElement("li");
      li.className = tone === "ok" ? "text-ink" : tone === "danger" ? "text-danger" : "text-ink-muted";
      li.textContent = `${tone === "ok" ? "✓" : tone === "danger" ? "✗" : "…"} ${text}`;
      return li;
    };
    try {
      const got = await fetchRelease(channel, parseKey(__PINNA_RELEASE_KEY__), {
        model,
        progress: (text) => list.replaceChildren(line(text, "muted")),
      });
      release = got;
      downloaded = true;
      list.replaceChildren(
        line(`Pinna ${got.version}, signed with Pinna's release key`, "ok"),
        ...(got.summary ? [line(got.summary, "muted")] : []),
        line(`${got.root.name} (${MiB(got.root.bytes.length)} MiB), checked`, "ok"),
        line(`${got.boot.name} (${MiB(got.boot.bytes.length)} MiB), checked`, "ok"),
        ...(got.bootctl ? [line(`${got.bootctl.name} (${MiB(got.bootctl.bytes.length)} MiB), checked`, "ok")] : []),
      );
    } catch (error) {
      const text = error instanceof DownloadError ? error.message : `The download failed: ${String(error)}. Try again, or download the files and choose them below.`;
      list.replaceChildren(line(text, "danger"));
    } finally {
      busy = false;
      sync();
    }
  });

  files.addEventListener("change", async () => {
    const list = $("install-files-status");
    release = null;
    $("install-download-status").replaceChildren();
    list.replaceChildren();
    const model = chosenModel();
    if (!files.files || files.files.length === 0 || !model) return sync();
    busy = true;
    sync();
    const item = (text: string, tone: "ok" | "danger" | "muted") => {
      const li = document.createElement("li");
      li.className = tone === "ok" ? "text-ink" : tone === "danger" ? "text-danger" : "text-ink-muted";
      li.textContent = `${tone === "ok" ? "✓" : tone === "danger" ? "✗" : "…"} ${text}`;
      return li;
    };
    try {
      const match = await matchRelease([...files.files], model, (name) => list.replaceChildren(item(`Checking ${name}`, "muted")));
      release = match.release;
      downloaded = false;
      if (release) {
        list.replaceChildren(
          item(`Pinna ${release.version ?? ""}`.trim() + ", checked against SHA256SUMS", "ok"),
          item(`${release.root.name} (${MiB(release.root.bytes.length)} MiB)`, "ok"),
          item(`${release.boot.name} (${MiB(release.boot.bytes.length)} MiB)`, "ok"),
          ...(release.bootctl ? [item(`${release.bootctl.name} (${MiB(release.bootctl.bytes.length)} MiB)`, "ok")] : []),
        );
      } else {
        list.replaceChildren(...match.problems.map((p) => item(p, "danger")));
      }
    } catch (error) {
      list.replaceChildren(item(`The files can't be read: ${String(error)}`, "danger"));
    } finally {
      busy = false;
      sync();
    }
  });

  connect.addEventListener("click", async () => {
    const model = chosenModel();
    if (!model || busy) return;
    clearOutcome();
    busy = true;
    sync();
    status.textContent = "Waiting for you to choose the device…";

    let transport: WebUsbTransport | null = null;
    let step = "";
    try {
      const twrp = model === "echo-dot-5";
      const picked = await requestDevice(usb, twrp ? TWRP_FILTERS : FASTBOOT_FILTERS);
      if (!picked) {
        status.textContent = "No device chosen. If it wasn't in the list, see “The device isn't in the list” below.";
        return;
      }
      device = picked;
      const progress = (s: string) => {
        step = s;
        status.textContent = `${s}…`;
      };
      if (model === "echo-dot-5") {
        transport = await WebUsbTransport.open(picked, "adb");
        const report = await identifyTwrp(new Adb(transport), picked.serialNumber ?? null, progress);
        checked = { kind: "twrp", report, assessment: assessTwrp(report), model };
      } else {
        transport = await WebUsbTransport.open(picked);
        const report = await identify(new Fastboot(transport), progress);
        checked = { kind: "fastboot", report, assessment: assess(report, model), model };
      }
      showResult(checked);
      status.textContent = "Done. Nothing was written to the device.";
    } catch (error) {
      status.textContent = "";
      explain(error, model, "check", step, null);
    } finally {
      await transport?.close();
      busy = false;
      sync();
    }
  });

  start.addEventListener("click", async () => {
    if (busy || !checked || !release || !device || !agree.checked) return;
    const before = checked;
    const model = before.model;
    problem.hidden = true;
    failedAt = null;
    busy = true;
    sync();
    const warn = (e: BeforeUnloadEvent) => e.preventDefault();
    window.addEventListener("beforeunload", warn);
    const bar = $<HTMLProgressElement>("install-progress");
    const text = $("install-progress-text");
    $("install-progress-box").hidden = false;
    bar.value = 0;

    let transport: WebUsbTransport | null = null;
    let step = "Connecting";
    let found: Checked | null = null;
    const onProgress = (p: Progress) => {
      step = p.label;
      bar.value = p.total ? p.sent / p.total : 0;
      text.textContent =
        p.phase === "restarting"
          ? "Restarting the device…"
          : `${p.label}: ${p.phase === "sending" ? `sending, ${MiB(p.sent)} of ${MiB(p.total)} MiB` : "the device writes it"}…`;
    };
    const changed = () => new InstallError("This isn't the device the page checked. Connect again in step 4.");
    try {
      text.textContent = "Connecting…";
      if (release.model !== model) throw new InstallError(`The files in step 2 are for the ${MODEL_NAMES[release.model]}. Get Pinna again for the ${MODEL_NAMES[model]}.`);
      if (before.kind === "twrp") {
        if (!release.bootctl) throw new InstallError("The files in step 2 lack pinna-bootctl, which the install runs in TWRP. Get Pinna again.");
        transport = await WebUsbTransport.open(device, "adb");
        const adb = new Adb(transport);
        // Check again right before writing: the device may have changed since.
        step = "Checking the device again";
        text.textContent = `${step}…`;
        const report = await identifyTwrp(adb, device.serialNumber ?? null);
        found = { kind: "twrp", report, assessment: assessTwrp(report), model: before.model };
        if (report.serial !== before.report.serial || found.assessment.state !== "ready") throw changed();
        await installTwrp(adb, report, { boot: release.boot.bytes, root: release.root.bytes, bootctl: release.bootctl.bytes }, onProgress);
      } else {
        transport = await WebUsbTransport.open(device);
        const fb = new Fastboot(transport);
        // Check again right before writing: the device may have changed since.
        step = "Checking the device again";
        text.textContent = `${step}…`;
        const report = await identify(fb);
        found = { kind: "fastboot", report, assessment: assess(report, before.model), model: before.model };
        if (report.serial !== before.report.serial || found.assessment.state !== "ready") throw changed();
        await install(fb, plan(report, { boot: release.boot.bytes, root: release.root.bytes }), onProgress);
      }
      bar.value = 1;
      text.textContent = "Done.";
      confirm.hidden = true;
      done.hidden = false;
      reportBox.hidden = true;
      checked = null;
      device = null;
      status.textContent = "";
    } catch (error) {
      if (error instanceof FastbootError && error.kind === "fail") {
        text.textContent = "The install stopped. The device stays in fastboot: tick the box and install again, or start again from step 3.";
      } else if (error instanceof CommandError) {
        // The connection is fine; the device said no.
        text.textContent = "The install stopped. TWRP still runs: tick the box and install again, or start again from step 3.";
      } else {
        // The device may still be in the middle of a command: anything sent
        // now could be taken for its data. Only a fresh start is safe.
        text.textContent = `The install stopped. Unplug the device, ${RESTART[before.kind]}, and connect again (step 4).`;
        checked = null;
        device = null;
      }
      explain(error, model, "install", step, found);
    } finally {
      window.removeEventListener("beforeunload", warn);
      await transport?.close();
      busy = false;
      agree.checked = false;
      sync();
    }
  });

  $("install-copy").addEventListener("click", async () => {
    const area = $<HTMLTextAreaElement>("install-report");
    const note = $("install-copy-status");
    try {
      await navigator.clipboard.writeText(area.value);
      note.textContent = "Copied.";
    } catch {
      area.select();
      note.textContent = "Selected: copy it with your keyboard.";
    }
  });

  sync();
  showLatest();
}
