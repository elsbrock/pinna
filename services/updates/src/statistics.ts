// GET /v1/statistics: weekly satellite counts from the update checks (ADR 0039).
import type { Deps, Env } from './github.ts';

const day = 86_400_000;
const weeks = 12;
// Per day, so the weeks are added up here. 91 days reach the start of the oldest week.
const sql = `SELECT toStartOfInterval(timestamp, INTERVAL '1' DAY) AS day, blob1 AS codename, blob2 AS version, `
  + `blob3 AS channel, blob4 AS country, SUM(_sample_interval) AS n FROM pinna_checks `
  + `WHERE timestamp >= NOW() - INTERVAL '91' DAY GROUP BY day, codename, version, channel, country FORMAT JSON`;
const dimensions = { codenames: 'codename', versions: 'version', channels: 'channel', countries: 'country' } as const;
const json = { 'Access-Control-Allow-Origin': '*', 'Content-Type': 'application/json', 'Cache-Control': 'public, max-age=3600' };

type Row = { day: number; codename: string; version: string; channel: string; country: string; n: number };
type Item = { name: string | null; satellites: number | null };

// A number under 5 never leaves the Worker; 0 stays 0.
const hide = (n: number) => n > 0 && n < 5 ? null : n;
const date = (ms: number) => new Date(ms).toISOString().slice(0, 10);
const monday = (ms: number) => ms - ms % day - (new Date(ms).getUTCDay() + 6) % 7 * day;

// The ISO week belongs to the year of its Thursday.
function label(start: number) {
  const thursday = start + 3 * day;
  const year = new Date(thursday).getUTCFullYear();
  const week = Math.floor((thursday - Date.UTC(year, 0, 1)) / day / 7) + 1;
  return `${year}-W${String(week).padStart(2, '0')}`;
}

async function query(env: Env, deps: Deps) {
  if (!env.CF_ACCOUNT_ID || !env.CF_ANALYTICS_TOKEN) throw new Error('CF_ACCOUNT_ID or CF_ANALYTICS_TOKEN is not set.');
  const response = await deps.fetch(
    `https://api.cloudflare.com/client/v4/accounts/${env.CF_ACCOUNT_ID}/analytics_engine/sql`,
    { method: 'POST', headers: { Authorization: `Bearer ${env.CF_ANALYTICS_TOKEN}` }, body: sql });
  if (!response.ok) throw new Error(`The SQL API answered ${response.status}.`);
  const { data } = await response.json() as { data?: Record<string, unknown>[] };
  if (!Array.isArray(data)) throw new Error('The SQL API answered without data.');
  return data.map((r): Row => {
    // The day arrives as "2026-11-28 00:00:00", in UTC; `n` may be a string.
    const at = Date.parse(`${String(r.day).slice(0, 10)}T00:00:00Z`), n = Number(r.n);
    if (Number.isNaN(at) || !Number.isFinite(n)) throw new Error('The SQL API answered with an unexpected row.');
    return { day: at, codename: String(r.codename), version: String(r.version),
      channel: String(r.channel), country: String(r.country), n };
  });
}

function breakdown(rows: Row[], key: keyof typeof dimensions): Item[] {
  const sums = new Map<string, number>();
  for (const row of rows) sums.set(row[dimensions[key]], (sums.get(row[dimensions[key]]) ?? 0) + row.n);
  const named: { name: string; satellites: number }[] = [];
  let rest = 0;
  for (const [name, sum] of sums) {
    const satellites = Math.round(sum / 7);
    if (satellites < 5 || key === 'codenames' && name === 'other') rest += sum;
    else named.push({ name, satellites });
  }
  named.sort((a, b) => b.satellites - a.satellites || (a.name < b.name ? -1 : 1));
  const merged = Math.round(rest / 7);
  return rest > 0 ? [...named, { name: null, satellites: merged < 5 ? null : merged }] : named;
}

async function compute(env: Env, deps: Deps) {
  const rows = await query(env, deps);
  const first = monday(deps.now()) - weeks * 7 * day;
  const starts = Array.from({ length: weeks }, (_, i) => first + i * 7 * day);
  const inWeek = (start: number) => rows.filter(r => r.day >= start && r.day < start + 7 * day);
  const totals = starts.map(start => Math.round(inWeek(start).reduce((sum, r) => sum + r.n, 0) / 7));
  const latest = starts[weeks - 1];
  const lists = Object.fromEntries(Object.keys(dimensions).map(key =>
    [key, totals[weeks - 1] < 5 ? [] : breakdown(inWeek(latest), key as keyof typeof dimensions)]));
  return {
    generated: new Date(deps.now()).toISOString(),
    weeks: starts.map((start, i) => ({ week: label(start), start: date(start), satellites: hide(totals[i]) })),
    latest: { week: label(latest), ...lists },
  };
}

export async function statistics(env: Env, deps: Deps, origin: string, method: string) {
  const key = new Request(`${origin}/v1/statistics`);
  let body = await (await deps.cache?.match(key))?.text();
  if (body === undefined) {
    try {
      body = JSON.stringify(await compute(env, deps));
    } catch (error) {
      // The message never holds the token.
      console.error('Statistics failed:', error instanceof Error ? error.message : error);
      return new Response(method === 'HEAD' ? null : 'Statistics are unavailable. Try again later.', {
        status: 503, headers: { 'Access-Control-Allow-Origin': '*', 'Content-Type': 'text/plain; charset=utf-8' },
      });
    }
    if (deps.cache) {
      const put = deps.cache.put(key, new Response(body, { headers: json }));
      if (deps.waitUntil) deps.waitUntil(put); else await put;
    }
  }
  return new Response(method === 'HEAD' ? null : body, { headers: json });
}
