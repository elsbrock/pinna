// GitHub release metadata and streamed downloads for the release service (ADR 0022).
export type Checks = { writeDataPoint(point: { indexes: string[]; blobs: string[] }): void };
export type Env = {
  GITHUB_REPO: string; GITHUB_API?: string; GITHUB_TOKEN?: string;
  CHECKS?: Checks; CF_ACCOUNT_ID?: string; CF_ANALYTICS_TOKEN?: string;
};
export type Deps = {
  fetch: typeof fetch; now: () => number; withdrawn: string[]; checks?: Checks;
  cache?: Pick<Cache, 'match' | 'put'>; waitUntil?: (promise: Promise<unknown>) => void;
};
export type Asset = { id: number; name: string; size: number };
export type Release = { tag_name: string; draft: boolean; prerelease: boolean; assets: Asset[] };

const cache = new Map<string, { at: number; list: Release[] }>();
export function resetCache() { cache.clear(); }

function endpoint(env: Env) {
  return `${env.GITHUB_API ?? 'https://api.github.com'}/repos/${env.GITHUB_REPO}/releases`;
}

function headers(env: Env, accept: string) {
  const h = new Headers({ Accept: accept, 'User-Agent': 'pinna-updates', 'X-GitHub-Api-Version': '2022-11-28' });
  if (env.GITHUB_TOKEN) h.set('Authorization', `Bearer ${env.GITHUB_TOKEN}`);
  return h;
}

export class GitHubError extends Error {}

export async function releases(env: Env, deps: Deps): Promise<Release[]> {
  const url = endpoint(env);
  const entry = cache.get(url);
  if (entry && deps.now() - entry.at < 300_000) return entry.list;
  try {
    const response = await deps.fetch(`${url}?per_page=100`, { headers: headers(env, 'application/vnd.github+json') });
    if (!response.ok) throw new GitHubError(`GitHub answered ${response.status}.`);
    const list: Release[] = await response.json();
    cache.set(url, { at: deps.now(), list });
    return list;
  } catch (error) {
    if (entry) return entry.list;
    throw error instanceof GitHubError ? error : new GitHubError('GitHub did not answer.');
  }
}

export async function download(env: Env, deps: Deps, asset: Asset) {
  const url = `${endpoint(env)}/assets/${asset.id}`;
  let response: Response;
  try {
    response = await deps.fetch(url, { headers: headers(env, 'application/octet-stream'), redirect: 'manual' });
    const location = response.headers.get('Location');
    if (response.status >= 300 && response.status < 400 && location) {
      // Start a fresh request: the signed storage URL needs no GitHub credentials.
      response = await deps.fetch(new URL(location, url));
    }
  } catch {
    throw new GitHubError('GitHub did not answer.');
  }
  if (response.status !== 200) throw new GitHubError(`GitHub answered ${response.status} for ${asset.name}.`);
  return response;
}
