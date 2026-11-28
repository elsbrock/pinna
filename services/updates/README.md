# Pinna release service

`GET /v1/channels/stable` redirects to the newest full release's manifest.
`GET /v1/channels/beta` redirects to the newest manifest, including prereleases.
`GET /v1/releases/<tag>/<file>` streams a file from that GitHub release, or
from the Cloudflare data centre's cache once a download has filled it.
Both routes support `HEAD`. Only published releases with a manifest and its
signature are available. See [ADR 0022](../../docs/adr/0022-update-channels.md).

`GET /v1/statistics` answers with the number of satellites per week for the
last 12 completed weeks, and the last week by codename, version, channel and
country. It supports `HEAD`. The answer is cached for an hour, and it is
`503` while Analytics Engine can't be read. A number under 5 is never
included. See [ADR 0039](../../docs/adr/0039-public-statistics.md).

## Update check count

A `GET` of a channel counts when its `User-Agent` is
`pinna-update/<version> (<codename>)`, it has no `Origin` header, and
`<version>` is a published release. The Worker writes the codename, version,
channel and country to the Workers Analytics Engine dataset `pinna_checks`,
bound as `CHECKS` in `wrangler.toml`. It stores no address. Cloudflare creates
the dataset on the first write. A codename that isn't in the list in
`src/index.ts` counts as `other`: add a new board's codename there.

## Test and run locally

Run the tests on your computer from the repository root. They need Node 24 or later.
They use a local fixture and need no account.

```sh
just updates-test
```

For a private repository, create `services/updates/.dev.vars` on your computer.
Set `GITHUB_TOKEN=…` to a fine-grained token with read access to repository contents.
From `services/updates/` on your computer, start the Worker:

```sh
npx wrangler dev
```

## Deploy and withdraw releases

From `services/updates/` on your computer, set the Worker secret and deploy:

```sh
npx wrangler secret put GITHUB_TOKEN
npx wrangler deploy
```

`/v1/statistics` reads the dataset through Cloudflare's SQL API. Set two more
secrets from `services/updates/` on your computer. `CF_ACCOUNT_ID` is your
account ID. `CF_ANALYTICS_TOKEN` is an API token with the permission
Account Analytics: Read.

```sh
npx wrangler secret put CF_ACCOUNT_ID
npx wrangler secret put CF_ANALYTICS_TOKEN
```

Without them, `/v1/statistics` answers `503`. Counting and updates still work.

To withdraw a release, add its tag (for example, `"v0.4.0"`) to the array in
`withdrawn.json`. Deploy again. Both channels skip the release, and the Worker
no longer serves its files.
