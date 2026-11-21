# Pinna release service

`GET /v1/channels/stable` redirects to the newest full release's manifest.
`GET /v1/channels/beta` redirects to the newest manifest, including prereleases.
`GET /v1/releases/<tag>/<file>` streams a file from that GitHub release, or
from the Cloudflare data centre's cache once a download has filled it.
Both routes support `HEAD`. Only published releases with a manifest and its
signature are available. See [ADR 0022](../../docs/adr/0022-update-channels.md).

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

To withdraw a release, add its tag (for example, `"v0.4.0"`) to the array in
`withdrawn.json`. Deploy again. Both channels skip the release, and the Worker
no longer serves its files.
