// "Open an issue" on the Logs page, and the links to Discussions: what a bug
// report needs about the satellite, its log as Markdown, and the GitHub links
// that carry them. The details go in the issue form's link; the log is too
// long for a link, so the page copies it. Issues are public, so the details
// leave out the serial number, MAC and IP addresses and Wi-Fi names, and the
// log loses them too. Built from GetStatus, GetUpdates and the log lines
// alone. No Svelte and no runtime imports, so `node --test` runs it.
import { modelName } from "../../../design/devices/index.ts";
import { MODE } from "./assistant.ts";
import { channelName, lastStart } from "./view.ts";

/** GitHub refuses an issue body over 65,536 characters; leave room for the details and the owner's own words. */
export const REPORT_MAX = 50_000;

const REPO = "https://github.com/elsbrock/pinna";

type StatusLike = {
  version: string;
  /** Its last three bytes are in the default name (pinna-6d484b): they go too. */
  mac?: string;
  model: string;
  virtualDevice: boolean;
  mode: number;
  lastBoot?: { outcome: number; reason: string; summary: string };
  settings?: { updateChannel: number };
};
type UpdatesLike = { slots: { name: string; booted: boolean }[] };

const OCTET = "(?:25[0-5]|2[0-4]\\d|1\\d\\d|[1-9]?\\d)";
// With colons, dashes, or underscores as BlueZ writes them (dev_44_63_B6_7A_85_8B).
const MAC = /(?<![0-9A-Fa-f])[0-9A-Fa-f]{2}([:_-])[0-9A-Fa-f]{2}(?:\1[0-9A-Fa-f]{2}){4}(?![0-9A-Fa-f])/g;
const IPV4 = new RegExp(`(?<![\\d.])${OCTET}(?:\\.${OCTET}){3}(?![\\d.])`, "g");
// Hex groups and colons, not part of a longer word or path; isIpv6 decides.
const IPV6_CANDIDATE = /(?<![\w:.])[0-9A-Fa-f:]{2,39}(?![\w:.])/g;
// How pinnad names a network in its log: Rust's {:?} of the SSID.
const WIFI_QUOTED = /(Wi-Fi: (?:trying|forgot) )"(?:[^"\\]|\\.)*"/g;

function isIpv6(s: string): boolean {
  const groups = s.split(":");
  if (groups.length > 8 || groups.some((g) => g.length > 4)) return false;
  // Every satellite has these: they say nothing about the owner's network.
  if (s === "::" || s === "::1") return false;
  return s.includes("::") ? groups.length >= 3 : groups.length === 8;
}

const isLocal = (ip: string) => ip.startsWith("127.") || ip === "0.0.0.0";

const escape = (s: string) => s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");

/**
 * `text` without MAC addresses, IP addresses (other than loopback), the given
 * Wi-Fi names, and `ownMac`'s last three bytes as the default name has them.
 */
export function scrub(text: string, { wifi = [], ownMac = "" }: { wifi?: readonly string[]; ownMac?: string } = {}): string {
  let out = text.replace(MAC, "[MAC]");
  const tail = ownMac.replace(/[^0-9A-Fa-f]/g, "").slice(6);
  if (tail.length === 6) out = out.replace(new RegExp(`(?<![0-9A-Za-z])${tail}(?![0-9A-Za-z])`, "gi"), "[MAC]");
  out = out
    .replace(IPV4, (ip) => (isLocal(ip) ? ip : "[IP]"))
    .replace(IPV6_CANDIDATE, (s) => (isIpv6(s) ? "[IP]" : s))
    .replace(WIFI_QUOTED, '$1"[Wi-Fi network]"');
  // Longest first, so a name that contains another goes whole. Names shorter
  // than three characters would blank out ordinary words.
  const names = [...new Set(wifi)].filter((n) => n.length >= 3).sort((a, b) => b.length - a.length);
  for (const name of names) {
    out = out.replace(new RegExp(`(?<![\\w])${escape(name)}(?![\\w])`, "g"), "[Wi-Fi network]");
  }
  return out;
}

function deviceName(s: StatusLike): string {
  // The development VM (ADR 0014) passes for an Echo Dot (2nd gen) to draw it.
  if (s.virtualDevice) return "Virtual Echo Dot";
  const known = modelName(s.model);
  if (known) return known;
  return s.model ? `Unknown (${s.model})` : "Unknown";
}

function slotName(u: UpdatesLike | null): string {
  if (!u) return "unknown";
  return u.slots.find((x) => x.booted)?.name ?? "none";
}

const capital = (s: string) => s.charAt(0).toUpperCase() + s.slice(1);

const modeName = (s: StatusLike) => (s.mode === MODE.standalone ? "Standalone" : "Home Assistant");

/** "2026-11-20 21:44, UTC+01:00": `now` in the zone `offset` minutes east of UTC. */
function when(now: Date, offset: number): string {
  const local = new Date(now.getTime() + offset * 60_000).toISOString();
  const abs = Math.abs(offset);
  const zone = `${offset < 0 ? "-" : "+"}${String(Math.floor(abs / 60)).padStart(2, "0")}:${String(abs % 60).padStart(2, "0")}`;
  return `${local.slice(0, 10)} ${local.slice(11, 16)}, UTC${zone}`;
}

type When = { now?: Date; offset?: number };

/**
 * The satellite's details for the issue form's Satellite field, as a Markdown
 * list. The time is in the zone `offset` minutes east of UTC: the browser's,
 * like the log's times.
 */
export function satelliteDetails(
  s: StatusLike,
  updates: UpdatesLike | null,
  { now = new Date(), offset = -now.getTimezoneOffset() }: When = {},
): string {
  return [
    `- Pinna: ${s.version}`,
    `- Device: ${deviceName(s)}`,
    `- Running slot: ${slotName(updates)}`,
    `- ${s.lastBoot ? lastStart(s.lastBoot).hint : "Last start: unknown"}`,
    `- Mode: ${modeName(s)}`,
    `- Update channel: ${capital(channelName(s.settings?.updateChannel))}`,
    // The log's times are the browser's, without a date.
    `- Copied: ${when(now, offset)}, the zone of the log's times`,
  ].join("\n");
}

export type LogBlock = { markdown: string; kept: number };

/**
 * `log` (oldest first) as a collapsed section for the issue form's Log field,
 * scrubbed. When the whole log doesn't fit in REPORT_MAX, the newest lines
 * that do; `kept` says how many. Empty when there are no lines.
 */
export function logBlock(
  log: readonly string[],
  { wifi = [], ownMac = "" }: { wifi?: readonly string[]; ownMac?: string } = {},
): LogBlock {
  if (!log.length) return { markdown: "", kept: 0 };
  const lines = log.map((l) => scrub(l, { wifi, ownMac }));
  // A fence longer than any run of backticks in the log, so no line ends it.
  const longest = Math.max(0, ...lines.map((l) => Math.max(0, ...(l.match(/`+/g) ?? []).map((r) => r.length))));
  const fence = "`".repeat(Math.max(3, longest + 1));

  const n = (k: number) => k.toLocaleString("en-US");
  const head = (summary: string) => `<details>\n<summary>${summary}</summary>\n\n${fence}text\n`;
  const tail = `${fence}\n\n</details>\n`;
  // Measured with the longest summary it can have, so what fits is decided once.
  let room = REPORT_MAX - head(`Log, the newest ${n(lines.length)} of ${n(lines.length)} lines`).length - tail.length;
  let kept = 0;
  for (let i = lines.length - 1; i >= 0 && room >= lines[i].length + 1; i--, kept++) room -= lines[i].length + 1;

  const summary =
    kept === lines.length
      ? `Log, ${n(kept)} ${kept === 1 ? "line" : "lines"}`
      : `Log, the newest ${n(kept)} of ${n(lines.length)} lines`;
  const body = lines.slice(lines.length - kept).map((l) => `${l}\n`).join("");
  return { markdown: head(summary) + body + tail, kept };
}

/** The issue form (.github/ISSUE_TEMPLATE/problem.yml) with the Satellite field filled in. */
export function issueUrl(details: string): string {
  return `${REPO}/issues/new?template=problem.yml&satellite=${encodeURIComponent(details)}`;
}

/** The Discussions categories the dashboard links to, by their slugs. */
export type Category = "q-a" | "general";

/**
 * A new discussion in `category`, starting with one line about the satellite
 * below the owner's words: the version, the device and the mode, nothing that
 * names the owner or their network.
 */
export function discussionUrl(category: Category, s: StatusLike | null): string {
  const url = `${REPO}/discussions/new?category=${category}`;
  if (!s) return url;
  const line = `Pinna ${s.version} · ${deviceName(s)} · ${modeName(s)}`;
  return `${url}&body=${encodeURIComponent(`\n\n---\n${line}`)}`;
}
