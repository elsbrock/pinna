// /install/ (ADR 0016): check a device in fastboot, then install Pinna on it.
import { assess, formatReport, identify, maskSerial, MODELS, PARTITIONS, type Assessment, type Bootloader, type DeviceReport, type ModelId } from "./device.ts";
import { Fastboot, FastbootError } from "./fastboot.ts";
import { install, InstallError, plan, type Progress } from "./install.ts";
import { DownloadError, fetchManifest, fetchRelease, parseKey, type Channel } from "./download.ts";
import { matchRelease, type Release } from "./release.ts";

/** keys/release.pub, put in at build time (astro.config.mjs). */
declare const __PINNA_RELEASE_KEY__: string;
import { ConnectError, requestFastbootDevice, webUsb, WebUsbTransport, type UsbDevice } from "./webusb.ts";

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
  let checked: { report: DeviceReport; assessment: Assessment; model: ModelId } | null = null;
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
    return value === "echo-dot-2" || value === "echo-2" ? value : null;
  };

  const sync = () => {
    const model = chosenModel();
    for (const input of form.querySelectorAll("input")) input.disabled = busy;
    connect.disabled = busy || model === null;
    download.disabled = busy;
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

  const failureReport = (model: ModelId | null, stage: string, step: string, error: unknown, report: DeviceReport | null) =>
    [
      "Pinna install page",
      `model (chosen): ${model ? MODELS[model].name : "(none)"}`,
      `stopped while: ${stage}${step ? `: ${step}` : ""}`,
      `error: ${error instanceof Error ? `${error.name}: ${error.message}` : String(error)}`,
      ...(report && model ? ["", formatReport(report, model, assess(report, model), browserName())] : [`browser: ${browserName()}`]),
    ].join("\n");

  /** Explain a failure, with a report to paste. */
  const explain = (error: unknown, model: ModelId | null, stage: string, step: string, report: DeviceReport | null) => {
    failedAt = stage === "install" ? "install" : "check";
    $(`install-outcome-${failedAt}`).append(problem, reportBox);
    reportOpen(true);
    const os = platform();
    const gone = error instanceof FastbootError && (error.kind === "timeout" || (error.kind === "transport" && /disconnect/i.test(error.message)));
    if (error instanceof ConnectError && (os === "windows" && error.problem !== "not-fastboot")) {
      showProblem("The browser can't use the device", "Windows may have no WinUSB driver for it, or another program uses it.", { href: "#windows", label: "Windows needs the WinUSB driver" });
    } else if (error instanceof ConnectError && error.problem === "permission") {
      showProblem(
        "The browser can't open the device",
        os === "linux" ? "Linux doesn't let your user open it yet." : "The browser isn't allowed to open it.",
        { href: "#cannot-open", label: "The browser can't open the device" },
      );
    } else if (error instanceof ConnectError && error.problem === "busy") {
      showProblem("Another program uses the device", "Close it and connect again.", { href: "#busy", label: "Another program uses the device" });
    } else if (error instanceof ConnectError) {
      showProblem("This isn't a device in fastboot", "Start the device in fastboot as described in step 3, and connect again.");
    } else if (gone) {
      showProblem(
        "The device doesn't answer, or went away",
        stage === "install"
          ? `The page stopped${step ? ` at “${step}”` : ""} and sent nothing more. Unplug the device, start it in fastboot again (step 3), and connect again (step 4).`
          : "The page stopped and sent nothing more.",
        { href: "#no-reply", label: "The device doesn't answer, or goes away" },
      );
    } else if (error instanceof FastbootError && error.kind === "fail" && stage === "install") {
      showProblem("The device refused a write", `${step ? `${step}: it` : "It"} answered ${JSON.stringify(error.message)}. Nothing after that step was written.`);
    } else if (error instanceof InstallError) {
      showProblem("Pinna can't be installed on this device", error.message);
    } else {
      showProblem("The page stopped", sentence(error instanceof Error ? error.message : String(error)));
    }
    showReport(failureReport(model, stage, step, error, report), `Install page: ${stage} failed`);
  };

  const showResult = (report: DeviceReport, assessment: Assessment, model: ModelId) => {
    result.dataset.tone = TONE[assessment.state];
    for (const icon of result.querySelectorAll<HTMLElement>("[data-icon]")) icon.hidden = icon.dataset.icon !== TONE[assessment.state];
    $("install-headline").textContent = assessment.headline;
    $("install-detail").textContent = assessment.detail;
    $("install-next").textContent = NEXT[assessment.state];
    const facts: [string, string][] = [
      ["Model", MODELS[model].name],
      ["Product", report.product],
      ["Serial number", maskSerial(report.serial)],
      ["Bootloader", bootloaderText(report.bootloader)],
      ["LK build", report.lkBuild ?? "not reported"],
      ...PARTITIONS.map((name): [string, string] => {
        const size = report.partitions[name];
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
    showReport(formatReport(report, model, assessment, browserName()), `Device check: ${report.product}, ${assessment.state}`);
    $("install-agree-text").textContent = `I want to erase this ${MODELS[model].name} (${maskSerial(report.serial)}) and install Pinna on it.`;
  };

  form.addEventListener("change", (event) => {
    // The confirmation box sits in the form too; it has its own listener.
    if (busy || event.target === agree) return;
    const name = (event.target as HTMLInputElement).name;
    if (name === "model") {
      // Another model: what the page found no longer applies.
      clearOutcome();
      device = null;
      status.textContent = "";
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
    if (busy) return;
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
        progress: (text) => list.replaceChildren(line(text, "muted")),
      });
      release = got;
      downloaded = true;
      list.replaceChildren(
        line(`Pinna ${got.version}, signed with Pinna's release key`, "ok"),
        ...(got.summary ? [line(got.summary, "muted")] : []),
        line(`${got.root.name} (${MiB(got.root.bytes.length)} MiB), checked`, "ok"),
        line(`${got.boot.name} (${MiB(got.boot.bytes.length)} MiB), checked`, "ok"),
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
    if (!files.files || files.files.length === 0) return sync();
    busy = true;
    sync();
    const item = (text: string, tone: "ok" | "danger" | "muted") => {
      const li = document.createElement("li");
      li.className = tone === "ok" ? "text-ink" : tone === "danger" ? "text-danger" : "text-ink-muted";
      li.textContent = `${tone === "ok" ? "✓" : tone === "danger" ? "✗" : "…"} ${text}`;
      return li;
    };
    try {
      const match = await matchRelease([...files.files], (name) => list.replaceChildren(item(`Checking ${name}`, "muted")));
      release = match.release;
      downloaded = false;
      if (release) {
        list.replaceChildren(
          item(`Pinna ${release.version ?? ""}`.trim() + ", checked against SHA256SUMS", "ok"),
          item(`${release.root.name} (${MiB(release.root.bytes.length)} MiB)`, "ok"),
          item(`${release.boot.name} (${MiB(release.boot.bytes.length)} MiB)`, "ok"),
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
      const picked = await requestFastbootDevice(usb);
      if (!picked) {
        status.textContent = "No device chosen. If it wasn't in the list, see “The device isn't in the list” below.";
        return;
      }
      device = picked;
      transport = await WebUsbTransport.open(picked);
      const report = await identify(new Fastboot(transport), (s) => {
        step = s;
        status.textContent = `${s}…`;
      });
      const assessment = assess(report, model);
      checked = { report, assessment, model };
      showResult(report, assessment, model);
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
    const { model, report: before } = checked;
    const images = { boot: release.boot.bytes, root: release.root.bytes };
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
    let report: DeviceReport | null = null;
    try {
      text.textContent = "Connecting…";
      transport = await WebUsbTransport.open(device);
      const fb = new Fastboot(transport);
      // Check again right before writing: the device may have changed since.
      step = "Checking the device again";
      text.textContent = `${step}…`;
      report = await identify(fb);
      if (report.serial !== before.serial || assess(report, model).state !== "ready") {
        throw new InstallError("This isn't the device the page checked. Connect again in step 4.");
      }
      const writes = plan(report, images);
      await install(fb, writes, (p: Progress) => {
        step = p.label;
        bar.value = p.total ? p.sent / p.total : 0;
        text.textContent =
          p.phase === "restarting"
            ? "Restarting the device…"
            : `${p.label}: ${p.phase === "sending" ? `sending, ${MiB(p.sent)} of ${MiB(p.total)} MiB` : "the device writes it"}…`;
      });
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
      } else {
        // The device may still be in the middle of a command: anything sent
        // now could be taken for its data. Only a fresh start is safe.
        text.textContent = "The install stopped. Unplug the device, start it in fastboot again (step 3), and connect again (step 4).";
        checked = null;
        device = null;
      }
      explain(error, model, "install", step, report);
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
