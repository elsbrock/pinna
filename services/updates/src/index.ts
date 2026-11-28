// The public channel and release file routes (ADR 0022), the count of update
// checks and the statistics (ADR 0039).
import withdrawn from "../withdrawn.json" with { type: "json" };
import { download, GitHubError, releases, type Asset, type Deps, type Env, type Release } from './github.ts';
import { compare, parse } from './semver.ts';
import { statistics } from './statistics.ts';
export { resetCache } from './github.ts';

const methods = 'GET, HEAD, OPTIONS';
const cors = { 'Access-Control-Allow-Origin': '*' };
const plain = 'text/plain; charset=utf-8';

function eligible(r: Release, withdrawn: string[]) {
  return !r.draft && /^v(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(-beta\.(0|[1-9]\d*))?$/.test(r.tag_name)
    && !withdrawn.includes(r.tag_name)
    && ['manifest.json', 'manifest.json.sig'].every(name => r.assets.some(asset => asset.name === name));
}

function newest(list: Release[], channel: string) {
  let best: { tag: string; version: NonNullable<ReturnType<typeof parse>> } | undefined;
  for (const release of list) {
    const version = parse(release.tag_name.slice(1))!;
    if (channel === 'stable' && (release.prerelease !== false || version.pre.length)) continue;
    if (!best || compare(version, best.version) > 0) best = { tag: release.tag_name, version };
  }
  return best?.tag;
}

// A new board's codename gets added here; any other codename is counted as `other`.
const codenames = ['biscuit', 'radar', 'cannoli'];
const agent = /^pinna-update\/([0-9a-z.-]+) \(([a-z0-9-]+)\)$/;

// Counts a satellite's daily check. A browser always sends an Origin header on
// a cross-origin fetch, so a web page can't count its visitors. Writing never
// changes or fails the answer.
function count(request: Request, channel: string, all: Release[], deps: Deps) {
  if (!deps.checks || request.method !== 'GET' || request.headers.has('Origin')) return;
  const match = agent.exec(request.headers.get('User-Agent') ?? '');
  if (!match || !all.some(r => !r.draft && r.tag_name === `v${match[1]}`)) return;
  const codename = codenames.includes(match[2]) ? match[2] : 'other';
  const country = (request as { cf?: { country?: unknown } }).cf?.country;
  try {
    deps.checks.writeDataPoint({ indexes: [codename], blobs: [codename, match[1], channel,
      typeof country === 'string' && /^[A-Z]{2}$/.test(country) ? country : 'XX'] });
  } catch (error) {
    console.error('Counting an update check failed:', error instanceof Error ? error.message : error);
  }
}

function contentType(name: string) {
  if (name.endsWith('.json')) return 'application/json';
  if (name === 'SHA256SUMS' || name.endsWith('.sh')) return plain;
  return 'application/octet-stream';
}

// A second download fills the cache. Teeing the client's stream instead would
// hold whatever the slower reader hasn't read yet in the Worker's 128 MB.
async function store(env: Env, deps: Deps, asset: Asset, key: Request, headers: Headers) {
  const stored = new Headers(headers);
  const upstream = await download(env, deps, asset);
  stored.set('Cache-Control', 'public, max-age=31536000');
  stored.set('Content-Length', upstream.headers.get('Content-Length') ?? String(asset.size));
  await deps.cache!.put(key, new Response(upstream.body, { headers: stored }));
}

// Workers' fetch must be called as a plain function: detached as `deps.fetch`
// it throws "Illegal invocation". Node's doesn't, so the tests can't show it.
const live: Deps = { fetch: (input, init) => fetch(input, init), now: Date.now, withdrawn };

export async function handle(request: Request, env: Env, deps: Deps = live) {
  const { method } = request;
  const text = (status: number, message: string, headers = {}) => new Response(method === 'HEAD' ? null : message, {
    status, headers: { ...cors, 'Content-Type': plain, ...headers },
  });
  if (method === 'OPTIONS') return new Response(null, { status: 204, headers: {
    ...cors, 'Access-Control-Allow-Methods': methods, 'Access-Control-Allow-Headers': '*', 'Access-Control-Max-Age': '86400',
  } });
  if (method !== 'GET' && method !== 'HEAD') return text(405, 'Method not allowed.', { Allow: methods });

  const { pathname, origin } = new URL(request.url);
  if (pathname === '/') return text(200, "Pinna's release service. See https://getpinna.dev/docs/update/");
  if (pathname === '/v1/statistics') return statistics(env, deps, origin, method);
  const channel = /^\/v1\/channels\/([^/]+)$/.exec(pathname)?.[1];
  const file = /^\/v1\/releases\/([^/]+)\/([^/]+)$/.exec(pathname);
  if (!channel && !file) return text(404, 'Not found.');
  if (channel && channel !== 'stable' && channel !== 'beta') {
    return text(404, `There is no channel ${channel}. The channels are stable and beta.`);
  }

  try {
    const all = await releases(env, deps);
    const list = all.filter(r => eligible(r, deps.withdrawn));
    if (channel) {
      const tag = newest(list, channel);
      if (!tag) return text(404, `No release on ${channel} yet.`);
      count(request, channel, all, deps);
      return new Response(null, { status: 302, headers: {
        ...cors, Location: `${origin}/v1/releases/${tag}/manifest.json`, 'Cache-Control': 'public, max-age=60',
      } });
    }
    const [, tag, name] = file!;
    const release = list.find(r => r.tag_name === tag);
    if (!release) return text(404, `There is no release ${tag}.`);
    const asset = release.assets.find(a => a.name === name);
    if (!asset) return text(404, `${tag} has no file ${name}.`);
    const headers = new Headers({ ...cors, 'Content-Type': contentType(name),
      'Cache-Control': 'public, max-age=3600', 'Content-Length': String(asset.size),
    });
    if (name.endsWith('.img')) headers.set('Content-Disposition', `attachment; filename="${name}"`);
    if (method === 'HEAD') return new Response(null, { headers });
    // Cloudflare's cache doesn't see what a Worker returns, so the Worker keeps
    // files in the data centre's cache itself, after the checks above. The key
    // holds GitHub's asset id, which changes when a file is uploaded again.
    const key = new Request(`${origin}/v1/assets/${asset.id}`);
    const cached = await deps.cache?.match(key);
    const source = cached ?? await download(env, deps, asset);
    if (!cached && deps.cache && deps.waitUntil) deps.waitUntil(store(env, deps, asset, key, headers));
    headers.set('Content-Length', source.headers.get('Content-Length') ?? String(asset.size));
    return new Response(source.body, { headers });
  } catch (error) {
    if (error instanceof GitHubError) return text(502, error.message);
    throw error;
  }
}

export default {
  fetch(request: Request, env: Env, ctx: { waitUntil(promise: Promise<unknown>): void }) {
    return handle(request, env, { ...live, checks: env.CHECKS, cache: caches.default, waitUntil: p => ctx.waitUntil(p) });
  },
};
