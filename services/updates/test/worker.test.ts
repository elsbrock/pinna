import assert from 'node:assert/strict';
import { beforeEach, test } from 'node:test';
import fixture from './fixtures/releases.json' with { type: 'json' };
import { handle, resetCache } from '../src/index.ts';
import type { Deps, Env } from '../src/github.ts';

beforeEach(resetCache);

const origin = 'https://local.example:8443';
const api = 'https://api.example/repos/elsbrock/pinna/releases';
const storage = 'https://storage.example/object?signature=123';
const stable = '/v1/channels/stable', beta = '/v1/channels/beta';
const files = '/v1/releases/v0.1.1/';
const allowed = 'GET, HEAD, OPTIONS';
const sqlApi = 'https://api.cloudflare.com/client/v4/accounts/acct/analytics_engine/sql';

function service(token?: string) {
  const calls: Request[] = [];
  const state = {
    time: 0, list: structuredClone(fixture), withdrawn: [] as string[], points: [] as unknown[],
    write: (point: unknown) => { state.points.push(point); }, sql: (): Response => Response.json({ data: [] }),
    cache: undefined as Deps['cache'], pending: [] as Promise<unknown>[],
    listResponse: (): Response => Response.json(state.list),
    assetResponse: (): Response => Response.redirect(storage),
    storageResponse: (): Response => new Response('file', { headers: { 'Content-Length': '4' } }),
  };
  const env: Env = { GITHUB_REPO: 'elsbrock/pinna', GITHUB_API: 'https://api.example', GITHUB_TOKEN: token };
  const mock: typeof fetch = async (input, init) => {
    const req = new Request(input, init);
    calls.push(req);
    if (req.url === `${env.GITHUB_API}/repos/${env.GITHUB_REPO}/releases?per_page=100`) return state.listResponse();
    if (req.url.startsWith(`${api}/assets/`)) return state.assetResponse();
    if (req.url === storage) return state.storageResponse();
    if (req.url === sqlApi) return state.sql();
    throw new Error(`Unexpected URL: ${req.url}`);
  };
  async function request(path: string, method = 'GET', headers = {}, cf: object = { country: 'DE' }) {
    const req = new Request(origin + path, { method, headers });
    Object.defineProperty(req, 'cf', { value: cf });
    const response = await handle(req, env, { fetch: mock, now: () => state.time, withdrawn: state.withdrawn,
      cache: state.cache, waitUntil: p => state.pending.push(p), checks: { writeDataPoint: p => state.write(p) } });
    assert.equal(response.headers.get('Access-Control-Allow-Origin'), '*');
    return response;
  }
  return { calls, state, request, env };
}

async function error(response: Response, status: number, message: string) {
  assert.equal(response.status, status);
  assert.equal(response.headers.get('Content-Type'), 'text/plain; charset=utf-8');
  assert.equal(await response.text(), message);
}

function redirect(response: Response, tag: string) {
  assert.equal(response.status, 302);
  assert.equal(response.headers.get('Location'), `${origin}/v1/releases/${tag}/manifest.json`);
  assert.equal(response.headers.get('Cache-Control'), 'public, max-age=60');
}

test('channels pick the highest eligible version on the request origin', async () => {
  const { request, state } = service();
  redirect(await request(stable), 'v0.1.1');
  redirect(await request(beta, 'HEAD'), 'v0.2.0-beta.2');
  state.withdrawn.push('v0.2.0-beta.2', 'v0.1.1');
  redirect(await request(beta), 'v0.2.0-beta.1');
  redirect(await request(stable), 'v0.1.0');
  state.withdrawn.push('v0.1.0');
  await error(await request(stable), 404, 'No release on stable yet.');
});

test('beta advances to a newer full release; stable checks both the flag and the tag', async () => {
  const { request, state } = service();
  state.list.push({ ...state.list[0], tag_name: 'v0.4.0' },
    { ...state.list[0], tag_name: 'v0.5.0', prerelease: true },
    { ...state.list[0], tag_name: 'v0.6.0-beta.1', prerelease: false });
  redirect(await request(stable), 'v0.4.0');
  state.withdrawn.push('v0.5.0', 'v0.6.0-beta.1');
  redirect(await request(beta), 'v0.4.0');
});

test('download follows storage redirect without the token or Range', async () => {
  const { request, calls, state } = service('read-only');
  const name = 'pinna-boot.img';
  const asset = state.list.find(r => r.tag_name === 'v0.1.1')!.assets.find(a => a.name === name)!;
  const r = await request(files + name, 'GET', { Range: 'bytes=1-2' });
  assert.equal(r.status, 200);
  assert.equal(await r.text(), 'file');
  assert.equal(r.headers.get('Content-Length'), '4');
  assert.equal(r.headers.get('Content-Disposition'), `attachment; filename="${name}"`);
  assert.equal(r.headers.get('Content-Type'), 'application/octet-stream');
  assert.equal(r.headers.get('Cache-Control'), 'public, max-age=3600');
  assert.deepEqual(calls.map(c => c.url), [`${api}?per_page=100`, `${api}/assets/${asset.id}`, storage]);
  for (const c of calls.slice(0, 2)) {
    assert.equal(c.headers.get('Authorization'), 'Bearer read-only');
    assert.equal(c.headers.get('User-Agent'), 'pinna-updates');
  }
  assert.equal(calls[0].headers.get('X-GitHub-Api-Version'), '2022-11-28');
  assert.equal(calls[0].headers.get('Accept'), 'application/vnd.github+json');
  assert.equal(calls[1].headers.get('Accept'), 'application/octet-stream');
  assert.equal(calls[1].redirect, 'manual');
  assert.equal(calls[2].headers.get('Authorization'), null);
  assert.ok(calls.every(c => !c.headers.has('Range')));
});

test('a direct API response streams before the download finishes', async () => {
  const { request, state, calls } = service();
  const { readable, writable } = new TransformStream();
  state.assetResponse = () => new Response(readable);
  const r = await request(files + 'manifest.json');
  assert.equal(r.status, 200);
  assert.equal(r.headers.get('Content-Length'), '512');
  assert.equal(r.headers.get('Content-Type'), 'application/json');
  assert.equal(r.headers.get('Content-Disposition'), null);
  const body = r.text();
  const writer = writable.getWriter();
  await writer.write(new TextEncoder().encode('manifest'));
  await writer.close();
  assert.equal(await body, 'manifest');
  assert.equal(calls.length, 2);
});

test('a download fills the cache in the background; later ones come from it', async () => {
  const { request, calls, state } = service();
  const stored = new Map<string, { body: string; headers: Headers }>();
  state.cache = {
    async match(key) { const s = stored.get(new Request(key).url); return s && new Response(s.body, { headers: s.headers }); },
    async put(key, r) { stored.set(new Request(key).url, { body: await r.text(), headers: r.headers }); },
  };
  const name = 'pinna-boot.img';
  const asset = state.list.find(r => r.tag_name === 'v0.1.1')!.assets.find(a => a.name === name)!;
  const fetched = () => calls.filter(c => c.url === storage).length;

  assert.equal(await (await request(files + name)).text(), 'file');
  await Promise.all(state.pending);
  assert.equal(fetched(), 2);
  const entry = stored.get(`${origin}/v1/assets/${asset.id}`)!;
  assert.equal(entry.headers.get('Cache-Control'), 'public, max-age=31536000');
  assert.equal(entry.headers.get('Content-Length'), '4');

  const hit = await request(files + name);
  assert.equal(await hit.text(), 'file');
  assert.equal(hit.headers.get('Cache-Control'), 'public, max-age=3600');
  assert.equal(hit.headers.get('Content-Length'), '4');
  assert.equal(hit.headers.get('Content-Disposition'), `attachment; filename="${name}"`);
  assert.equal(fetched(), 2);
  assert.equal((await request(files + name, 'HEAD')).body, null);
  assert.equal(fetched(), 2);

  // A withdrawn release stays unavailable although its files are cached.
  state.withdrawn.push('v0.1.1');
  await error(await request(files + name), 404, 'There is no release v0.1.1.');
  state.withdrawn.length = 0;

  // A file uploaded again under the same tag has a new asset id.
  asset.id += 1000;
  state.time = 300_000;
  assert.equal(await (await request(files + name)).text(), 'file');
  await Promise.all(state.pending);
  assert.equal(fetched(), 4);
  assert.ok(stored.has(`${origin}/v1/assets/${asset.id}`));
});

test('HEAD uses asset metadata without fetching any bytes', async () => {
  const { request, calls, state } = service();
  for (const asset of state.list.find(r => r.tag_name === 'v0.1.1')!.assets) {
    const r = await request(files + asset.name, 'HEAD');
    assert.equal(r.status, 200);
    assert.equal(r.body, null);
    assert.equal(r.headers.get('Content-Length'), String(asset.size));
    const type = asset.content_type === 'text/plain' ? 'text/plain; charset=utf-8' : asset.content_type;
    assert.equal(r.headers.get('Content-Type'), type);
  }
  assert.equal(calls.length, 1);
});

test('missing files and ineligible releases are unavailable', async () => {
  const { request, state, calls } = service();
  state.withdrawn.push('v0.1.0');
  const unsigned = { ...state.list[0], tag_name: 'v0.7.0', assets: state.list[0].assets.filter(a => a.name !== 'manifest.json') };
  state.list.push(unsigned);
  for (const tag of ['v0.1.0', 'v0.2.0', 'v0.3.0', 'nightly', 'v0.7.0', 'v99.0.0']) {
    await error(await request(`/v1/releases/${tag}/pinna-boot.img`), 404, `There is no release ${tag}.`);
  }
  await error(await request(files + 'absent'), 404, 'v0.1.1 has no file absent.');
  assert.equal(calls.length, 1);
});

test('failed asset requests report the upstream status', async () => {
  const { request, state } = service();
  state.storageResponse = () => new Response(null, { status: 403 });
  await error(await request(files + 'manifest.json'), 502, 'GitHub answered 403 for manifest.json.');
  for (const status of [302, 500]) {
    state.assetResponse = () => new Response(null, { status });
    await error(await request(files + 'manifest.json'), 502, `GitHub answered ${status} for manifest.json.`);
  }
});

test('public repository requests need no authorization', async () => {
  const { request, calls } = service();
  assert.equal((await request(files + 'manifest.json')).status, 200);
  assert.equal(calls.length, 3);
  assert.ok(calls.every(r => !r.headers.has('Authorization')));
});

test('list expires after five minutes and survives failed refreshes', async () => {
  const { request, calls, state, env } = service();
  redirect(await request(stable), 'v0.1.1');
  state.list = state.list.filter(r => r.tag_name !== 'v0.1.1');
  state.time = 299_999;
  redirect(await request(stable), 'v0.1.1');
  assert.equal(calls.length, 1);
  state.time++;
  redirect(await request(stable), 'v0.1.0');
  assert.equal(calls.length, 2);
  state.time += 300_000;
  state.listResponse = () => new Response(null, { status: 503 });
  redirect(await request(stable), 'v0.1.0');
  state.listResponse = () => { throw new Error('offline'); };
  redirect(await request(stable), 'v0.1.0');
  assert.equal(calls.length, 4);
  env.GITHUB_REPO = 'other/repo';
  await error(await request(stable), 502, 'GitHub did not answer.');
  state.listResponse = () => new Response(null, { status: 429 });
  await error(await request(stable), 502, 'GitHub answered 429.');
});

test('preflight, help and routing errors need no release list', async () => {
  const { request, calls } = service();
  const options = await request('/any', 'OPTIONS');
  assert.equal(options.status, 204);
  assert.equal(options.headers.get('Access-Control-Allow-Methods'), allowed);
  assert.equal(options.headers.get('Access-Control-Allow-Headers'), '*');
  assert.equal(options.headers.get('Access-Control-Max-Age'), '86400');
  const help = await request('/');
  assert.equal(help.status, 200);
  assert.equal(await help.text(), "Pinna's release service. See https://getpinna.dev/docs/update/");
  await error(await request('/v1/channels/dev'), 404, 'There is no channel dev. The channels are stable and beta.');
  await error(await request('/other'), 404, 'Not found.');
  const post = await request(stable, 'POST');
  await error(post, 405, 'Method not allowed.');
  assert.equal(post.headers.get('Allow'), allowed);
  assert.equal(calls.length, 0);
});

const agent = (version: string, codename?: string) => ({ 'User-Agent': `pinna-update/${version}${codename ? ` (${codename})` : ''}` });

function published() {
  const s = service();
  s.state.list.push({ ...s.state.list[0], tag_name: 'v0.7.1' });
  return s;
}

test('a daily check that names the hardware is counted', async () => {
  const { request, state } = published();
  redirect(await request(stable, 'GET', agent('0.7.1', 'cannoli')), 'v0.7.1');
  assert.deepEqual(state.points, [{ indexes: ['cannoli'], blobs: ['cannoli', '0.7.1', 'stable', 'DE'] }]);
  redirect(await request(beta, 'GET', agent('0.7.1', 'radar'), { country: 'FR' }), 'v0.7.1');
  assert.deepEqual(state.points[1], { indexes: ['radar'], blobs: ['radar', '0.7.1', 'beta', 'FR'] });
});

test('other requests are not counted', async () => {
  const { request, state } = published();
  const good = agent('0.7.1', 'cannoli');
  await request(stable, 'HEAD', good);
  await request(stable, 'GET', { 'User-Agent': 'Mozilla/5.0 (X11; Linux x86_64)' });
  await request(stable, 'GET', agent('0.7.1'));
  await request(stable, 'GET', { 'User-Agent': 'pinna-update/0.7.1 (cannoli) extra' });
  await request(stable, 'GET', { ...good, Origin: 'https://example.com' });
  await request(stable, 'GET', agent('0.7.2-dev.202611281200.gabc1234', 'cannoli'));
  await request(stable, 'GET', agent('0.7.2', 'cannoli'));
  await request('/v1/channels/dev', 'GET', good);
  await request(files + 'manifest.json', 'GET', good);
  state.list.find(r => r.tag_name === 'v0.7.1')!.draft = true;
  state.time += 300_000;
  await request(stable, 'GET', good);
  assert.deepEqual(state.points, []);
});

test('a withdrawn version is counted; unknown hardware is other; no country is XX', async () => {
  const { request, state } = published();
  state.withdrawn.push('v0.7.1');
  redirect(await request(stable, 'GET', agent('0.7.1', 'cannoli'), {}), 'v0.1.1');
  await request(stable, 'GET', agent('0.7.1', 'mystery'), { country: 'Germany' });
  assert.deepEqual(state.points, [
    { indexes: ['cannoli'], blobs: ['cannoli', '0.7.1', 'stable', 'XX'] },
    { indexes: ['other'], blobs: ['other', '0.7.1', 'stable', 'XX'] },
  ]);
});

test('a failing write still gives the redirect', async () => {
  const { request, state } = published();
  state.write = () => { throw new Error('quota'); };
  redirect(await request(stable, 'GET', agent('0.7.1', 'cannoli')), 'v0.7.1');
});

const secrets = { CF_ACCOUNT_ID: 'acct', CF_ANALYTICS_TOKEN: 'secret-token' };
const sampled = (day: string, codename: string, version: string, channel: string, country: string, n: number | string) =>
  ({ day: `${day} 00:00:00`, codename, version, channel, country, n });
const statistics = '/v1/statistics';
const memory = () => {
  const stored = new Map<string, { body: string; headers: Headers }>();
  return { stored, cache: {
    async match(key: Request) { const s = stored.get(new Request(key).url); return s && new Response(s.body, { headers: s.headers }); },
    async put(key: Request, r: Response) { stored.set(new Request(key).url, { body: await r.text(), headers: r.headers }); },
  } as Deps['cache'] };
};

function statsService(data: unknown[]) {
  const s = service();
  Object.assign(s.env, secrets);
  s.state.time = Date.UTC(2026, 10, 28, 10);
  s.state.sql = () => Response.json({ data });
  return s;
}

test('statistics add the days into 12 completed ISO weeks', async () => {
  const { request, calls } = statsService([
    sampled('2026-11-23', 'biscuit', '0.7.1', 'stable', 'DE', 700),
    sampled('2026-11-16', 'biscuit', '0.7.1', 'stable', 'DE', '30'),
    sampled('2026-11-22', 'biscuit', '0.7.1', 'stable', 'DE', 40),
    sampled('2026-11-09', 'biscuit', '0.7.1', 'stable', 'DE', 14),
    sampled('2026-11-02', 'biscuit', '0.7.1', 'stable', 'DE', 3),
    sampled('2026-09-01', 'biscuit', '0.7.1', 'stable', 'DE', 35),
    sampled('2026-08-30', 'biscuit', '0.7.1', 'stable', 'DE', 700),
  ]);
  const r = await request(statistics);
  assert.equal(r.status, 200);
  assert.equal(r.headers.get('Content-Type'), 'application/json');
  assert.equal(r.headers.get('Cache-Control'), 'public, max-age=3600');
  const body = await r.json();
  assert.deepEqual(Object.keys(body), ['generated', 'weeks', 'latest']);
  assert.equal(body.generated, '2026-11-28T10:00:00.000Z');
  assert.equal(body.weeks.length, 12);
  assert.deepEqual(body.weeks[0], { week: '2026-W36', start: '2026-08-31', satellites: 5 });
  assert.deepEqual(body.weeks[8], { week: '2026-W44', start: '2026-10-26', satellites: 0 });
  assert.deepEqual(body.weeks[9], { week: '2026-W45', start: '2026-11-02', satellites: 0 });
  assert.deepEqual(body.weeks[10], { week: '2026-W46', start: '2026-11-09', satellites: null });
  assert.deepEqual(body.weeks[11], { week: '2026-W47', start: '2026-11-16', satellites: 10 });
  assert.deepEqual(body.latest, { week: '2026-W47', codenames: [{ name: 'biscuit', satellites: 10 }],
    versions: [{ name: '0.7.1', satellites: 10 }], channels: [{ name: 'stable', satellites: 10 }],
    countries: [{ name: 'DE', satellites: 10 }] });
  const sql = calls.find(c => c.url === sqlApi)!;
  assert.equal(sql.method, 'POST');
  assert.equal(sql.headers.get('Authorization'), 'Bearer secret-token');
  assert.match(await sql.text(), /FROM pinna_checks/);
});

test('statistics number the weeks across a year boundary', async () => {
  const { request, state } = statsService([]);
  state.time = Date.UTC(2027, 0, 12);
  const { weeks, latest } = await (await request(statistics)).json();
  assert.deepEqual(weeks.slice(10).map((w: { week: string; start: string }) => [w.week, w.start]),
    [['2026-W53', '2026-12-28'], ['2027-W01', '2027-01-04']]);
  assert.equal(latest.week, '2027-W01');
});

test('statistics hide numbers under 5, merge small groups and sort', async () => {
  const day = '2026-11-16';
  const { request } = statsService([
    sampled(day, 'radar', '0.7.0', 'stable', 'FR', 14),
    sampled(day, 'cannoli', '0.7.1', 'beta', 'DE', 21),
    sampled(day, 'biscuit', '0.7.1', 'stable', 'DE', 70),
    sampled(day, 'other', '0.7.0', 'stable', 'XX', 70),
  ]);
  const { weeks, latest } = await (await request(statistics)).json();
  assert.equal(weeks[11].satellites, 25);
  assert.deepEqual(latest.codenames, [{ name: 'biscuit', satellites: 10 }, { name: null, satellites: 15 }]);
  assert.deepEqual(latest.versions, [{ name: '0.7.1', satellites: 13 }, { name: '0.7.0', satellites: 12 }]);
  assert.deepEqual(latest.channels, [{ name: 'stable', satellites: 22 }, { name: null, satellites: null }]);
  assert.deepEqual(latest.countries, [{ name: 'DE', satellites: 13 }, { name: 'XX', satellites: 10 },
    { name: null, satellites: null }]);
});

test('statistics show no breakdown when the latest week is under 5', async () => {
  const small = await (await statsService([sampled('2026-11-16', 'biscuit', '0.7.1', 'stable', 'DE', 14)])
    .request(statistics)).json();
  assert.equal(small.weeks[11].satellites, null);
  assert.deepEqual(small.latest, { week: '2026-W47', codenames: [], versions: [], channels: [], countries: [] });
  const none = await (await statsService([]).request(statistics)).json();
  assert.equal(none.weeks[11].satellites, 0);
  assert.deepEqual(none.latest.codenames, []);
});

test('statistics are unavailable without secrets or when the SQL API fails, and failures are not cached', async () => {
  const { request, state, env } = statsService([]);
  const { stored, cache } = memory();
  state.cache = cache;
  const message = 'Statistics are unavailable. Try again later.';
  delete env.CF_ANALYTICS_TOKEN;
  await error(await request(statistics), 503, message);
  env.CF_ANALYTICS_TOKEN = 'secret-token';
  state.sql = () => new Response('nope', { status: 500 });
  await error(await request(statistics), 503, message);
  state.sql = () => new Response('<html>', { status: 200 });
  await error(await request(statistics), 503, message);
  state.sql = () => Response.json({ errors: [] });
  await error(await request(statistics), 503, message);
  await Promise.all(state.pending);
  assert.equal(stored.size, 0);
  state.sql = () => Response.json({ data: [] });
  assert.equal((await request(statistics)).status, 200);
});

test('statistics come from the cache for an hour', async () => {
  const { request, state, calls } = statsService([sampled('2026-11-16', 'biscuit', '0.7.1', 'stable', 'DE', 70)]);
  const { stored, cache } = memory();
  state.cache = cache;
  const first = await (await request(statistics)).text();
  await Promise.all(state.pending);
  assert.equal(stored.get(`${origin}${statistics}`)!.headers.get('Cache-Control'), 'public, max-age=3600');
  const hit = await request(statistics);
  assert.equal(await hit.text(), first);
  assert.equal(hit.headers.get('Cache-Control'), 'public, max-age=3600');
  assert.equal((await request(statistics, 'HEAD')).body, null);
  assert.equal(calls.filter(c => c.url === sqlApi).length, 1);
});
