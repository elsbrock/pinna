// "Copy for an issue" on the Logs page: what a bug report needs about the
// satellite, then its log, as Markdown for a GitHub issue. Issues are public,
// so the block leaves out the serial number, MAC and IP addresses and Wi-Fi
// names, and scrubs them from the log too. Built from GetStatus, GetUpdates and
// the log lines alone. No Svelte and no runtime imports, so `node --test` runs it.
import { drawingFor } from "../../../design/devices/index.ts";
import { MODE } from "./assistant.ts";
import { channelName, lastStart } from "./view.ts";

/** GitHub refuses an issue body over 65,536 characters; leave room for the owner's own words. */
export const REPORT_MAX = 50_000;

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
  const known = drawingFor(s.model)?.name;
  if (known) return known;
  return s.model ? `Unknown (${s.model})` : "Unknown";
}

function slotName(u: UpdatesLike | null): string {
  if (!u) return "unknown";
  return u.slots.find((x) => x.booted)?.name ?? "none";
}

const capital = (s: string) => s.charAt(0).toUpperCase() + s.slice(1);

/** "2026-11-20 21:44, UTC+01:00": `now` in the zone `offset` minutes east of UTC. */
function when(now: Date, offset: number): string {
  const local = new Date(now.getTime() + offset * 60_000).toISOString();
  const abs = Math.abs(offset);
  const zone = `${offset < 0 ? "-" : "+"}${String(Math.floor(abs / 60)).padStart(2, "0")}:${String(abs % 60).padStart(2, "0")}`;
  return `${local.slice(0, 10)} ${local.slice(11, 16)}, UTC${zone}`;
}

export type IssueReport = { markdown: string; kept: number };

/**
 * The Markdown for an issue: the satellite's facts, then `log` (oldest first,
 * in the browser's time zone) in a collapsed section, scrubbed. When the whole
 * log doesn't fit in REPORT_MAX, the newest lines that do; `kept` says how many.
 */
export function issueReport(
  s: StatusLike,
  updates: UpdatesLike | null,
  log: readonly string[],
  { wifi = [], now = new Date(), offset = -now.getTimezoneOffset() }: { wifi?: readonly string[]; now?: Date; offset?: number } = {},
): IssueReport {
  const facts = [
    `- Pinna: ${s.version}`,
    `- Device: ${deviceName(s)}`,
    `- Running slot: ${slotName(updates)}`,
    `- ${s.lastBoot ? lastStart(s.lastBoot).hint : "Last start: unknown"}`,
    `- Mode: ${s.mode === MODE.standalone ? "Standalone" : "Home Assistant"}`,
    `- Update channel: ${capital(channelName(s.settings?.updateChannel))}`,
    // The log's times are the browser's, without a date.
    `- Copied: ${when(now, offset)}, the zone of the log's times`,
  ].join("\n");
  if (!log.length) return { markdown: `${facts}\n- Log: no lines on the page\n`, kept: 0 };

  const lines = log.map((l) => scrub(l, { wifi, ownMac: s.mac }));
  // A fence longer than any run of backticks in the log, so no line ends it.
  const longest = Math.max(0, ...lines.map((l) => Math.max(0, ...(l.match(/`+/g) ?? []).map((r) => r.length))));
  const fence = "`".repeat(Math.max(3, longest + 1));

  const n = (k: number) => k.toLocaleString("en-US");
  const head = (summary: string) => `${facts}\n\n<details>\n<summary>${summary}</summary>\n\n${fence}text\n`;
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
