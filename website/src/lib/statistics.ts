// The public statistics: the release service's answer to
// GET /v1/statistics, checked and turned into labels for the page.

import { SERVICE } from "./installer/download";

export const STATISTICS_URL = `${SERVICE}/v1/statistics`;

/** `satellites: null` means fewer than 5; `name: null` is the Other row. */
export type Row = { name: string | null; satellites: number | null };
export type Week = { week: string; start: string; satellites: number | null };
export type Statistics = {
  generated: string;
  weeks: Week[];
  latest: {
    week: string;
    codenames: Row[];
    versions: Row[];
    channels: Row[];
    countries: Row[];
  };
};

const HARDWARE: Record<string, string> = {
  biscuit: "Echo Dot (2nd gen)",
  radar: "Echo (2nd gen)",
  cannoli: "Echo Dot (5th gen)",
};

const count = (value: unknown): value is number | null =>
  value === null || (typeof value === "number" && Number.isFinite(value) && value >= 0);

const isRows = (value: unknown): value is Row[] =>
  Array.isArray(value) &&
  value.every(
    (row) =>
      typeof row === "object" &&
      row !== null &&
      (typeof row.name === "string" || row.name === null) &&
      count(row.satellites),
  );

/** Returns the answer if it has the documented shape, otherwise null. */
export function parseStatistics(value: unknown): Statistics | null {
  const stats = value as Statistics | null;
  if (typeof stats !== "object" || stats === null) return null;
  if (typeof stats.generated !== "string") return null;
  if (
    !Array.isArray(stats.weeks) ||
    stats.weeks.length === 0 ||
    !stats.weeks.every(
      (week) =>
        typeof week === "object" &&
        week !== null &&
        typeof week.week === "string" &&
        typeof week.start === "string" &&
        /^\d{4}-\d{2}-\d{2}$/.test(week.start) &&
        count(week.satellites),
    )
  ) {
    return null;
  }
  const latest = stats.latest;
  if (typeof latest !== "object" || latest === null) return null;
  if (
    !isRows(latest.codenames) ||
    !isRows(latest.versions) ||
    !isRows(latest.channels) ||
    !isRows(latest.countries)
  ) {
    return null;
  }
  return stats;
}

/** True when no week has a single satellite: nothing is counted yet. */
export function isEmpty(stats: Statistics): boolean {
  return stats.weeks.every((week) => week.satellites === 0);
}

/** "12" or "Fewer than 5". */
export function countLabel(satellites: number | null): string {
  return satellites === null ? "Fewer than 5" : String(satellites);
}

export function hardwareLabel(name: string): string {
  return HARDWARE[name] ?? (name === "other" ? "Other" : name);
}

export function channelLabel(name: string): string {
  return name === "stable" ? "Stable" : name === "beta" ? "Beta" : name;
}

export function countryLabel(code: string): string {
  if (code === "XX") return "Unknown";
  try {
    return new Intl.DisplayNames(["en"], { type: "region" }).of(code) ?? code;
  } catch {
    return code;
  }
}

/** "Nov 16" for the week that starts on `start` (a Monday, UTC): the chart's labels. */
export function weekStart(start: string): string {
  return new Intl.DateTimeFormat("en", { day: "numeric", month: "short", timeZone: "UTC" }).format(
    new Date(`${start}T00:00:00Z`),
  );
}

/** "16 Nov to 22 Nov 2026" for the week that starts on `start` (a Monday, UTC). */
export function weekRange(start: string): string {
  const first = new Date(`${start}T00:00:00Z`);
  const last = new Date(first.getTime() + 6 * 86_400_000);
  const day = new Intl.DateTimeFormat("en", { day: "numeric", month: "short", timeZone: "UTC" });
  const full = new Intl.DateTimeFormat("en", {
    day: "numeric",
    month: "short",
    year: "numeric",
    timeZone: "UTC",
  });
  return `${day.format(first)} to ${full.format(last)}`;
}
