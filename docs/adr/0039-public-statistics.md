# ADR 0039: Public statistics from the daily update check

Status: proposed (2026-11-28). Builds on ADR 0022 (the release service and the
daily update check), ADR 0007 (settings from the dashboard) and ADR 0032
(board descriptions). Takes up the question that ADR 0023 left when it was
withdrawn on 2026-11-19: the owner now wants satellites counted. Points
marked *default* are not confirmed by the owner yet.

## Context

- The project knows nothing about the satellites in use: how many there are,
  which hardware, which versions. That decides which devices and releases get
  attention, and how many satellites a bad release reaches. Home Assistant
  publishes such numbers at analytics.home-assistant.io; the owner wants the
  same for Pinna, by hardware codename (`biscuit`, `radar`, `cannoli`), version
  and country.
- Satellites already ask `updates.getpinna.dev` for the newest release: once
  after each start, once a day at a minute each satellite picked at random,
  and when someone changes the setting, selects Check now, or asks in
  standalone mode (`crates/pinnad/src/updates.rs`, `run_checks`). Every
  request already carries `User-Agent: pinna-update/<version>`
  (`crates/pinna-http/src/client.rs`, since 0ac32b3). The goals page, the
  Update page and ADR 0022 say the service learns the address, the time and
  the channel, "nothing else": the version is missing there today.
- Counting every check would count restarts, Check now and setting changes.
  The daily check runs once a day per satellite, so it counts satellites.
- The codename isn't in the code yet, only in comments on `Model`. The model
  id (`echo-dot-2`) names the product; a codename names the hardware, and one
  product can come as two boards (the Echo Dot (3rd gen): `donut` and
  `crumpet`).
- There are tens of satellites, a handful of them Echo Dot (5th gen). A count
  per codename and country is often one satellite, one owner.
- In the EU, reading information from a device and sending it needs the
  user's consent unless it is strictly necessary for a service the user asked
  for (ePrivacy directive, Art. 5(3); in Germany §25 TDDDG). The update check
  is such a service; the codename isn't necessary for it, because a satellite
  checks a release's `requires.models` itself. An opt-out switch is not
  consent.
- Home Assistant's analytics are opt-in. Fedora's "countme" counts with one
  request a week that carries a few coarse fields and no identifier.

## Decision

### What the satellite sends

- The daily check, and no other check, carries the codename:
  `User-Agent: pinna-update/<version> (<codename>)`, for example
  `pinna-update/0.7.1 (cannoli)`. The check after a start, Check now, a
  changed setting and Home Assistant's check keep `pinna-update/<version>`.
  pinnad runs the daily check as `pinnactl update --check --count`;
  `--count` adds the codename of the detected board.
- Each board description gets a `codename`: `biscuit`, `radar`, `cannoli`. A
  satellite without a board (the virtual Echo Dot) sends none.
- Nothing else is added: no identifier, no serial number, no names, nothing
  about what the satellite heard or did.

### The switch

- **Include in the statistics** is a setting like the others (ADR 0007): a
  field of `Status` and `UpdateSettingsRequest`, a switch on the dashboard's
  Updates page, and a `switch` entity in Home Assistant (category config). It
  is on by default.
- Off, the daily check carries no codename and isn't counted. With Updates
  off there is no check at all.

### What the release service counts

- The Worker counts a request when all of these hold: it is a `GET` of
  `/v1/channels/stable` or `/v1/channels/beta`; its User-Agent is exactly
  `pinna-update/<version> (<codename>)`; it has no `Origin` header (a browser
  always sends one, so a web page can't count its visitors); the version is
  the tag of a release on GitHub, withdrawn ones included. A codename the
  Worker doesn't know counts as `other`.
- It writes one data point to a Workers Analytics Engine dataset bound to the
  Worker, `pinna_checks`: the codename, the version, the channel and the
  country, Cloudflare's two-letter code for the address (`XX` when unknown).
  The address and the User-Agent itself are not stored.
- Analytics Engine stores each data point with the time it was written and
  keeps it for three months. A satellite checks at the same minute every day,
  so within the project's Cloudflare account its points can be told apart
  from day to day. They are never published one by one.
- Writing doesn't wait for Analytics Engine, so it neither delays nor
  changes the answer.
- Analytics Engine over D1: it is made for counting events, and there is no
  database, schema or migration to run. D1 would have kept only weekly counts,
  without times, and for longer; the owner chose Analytics Engine.

### What is published

- `GET /v1/statistics` answers with JSON for the last 12 completed ISO
  weeks (UTC). The Worker reads them from Analytics Engine's SQL API with a
  read-only API token (Account Analytics: Read, a Worker secret, beside
  `GITHUB_TOKEN`), counting with `_sample_interval`, and keeps the answer for
  an hour in the data centre's cache:
  - each week's satellites: the week's checks divided by 7, the average
    number of satellites a day, rounded;
  - for the last completed week, satellites by codename, by version, by
    channel and by country, each on its own, never combined.
- A number under 5 doesn't leave the Worker. The groups under 5 are merged
  into one "Other" row, without their names; if that row is under 5 too, it
  reads "fewer than 5". A week under 5 reads "fewer than 5" as a whole.
- `getpinna.dev/statistics/` shows these numbers, says what is counted and
  what isn't, and states the limits below. The page reads the JSON in the
  browser; without it the page says that the statistics are unavailable.
- The goals page's paragraph on the release service, the Update page and
  ADR 0022 say what a check carries (the version; once a day the codename,
  unless the switch is off) and that weekly counts are published. Release
  notes announce the switch in the release that ships it.

### Not decided here

- Telemetry about single satellites (an install ID, boot and update outcomes,
  crash reports) stays out.

## Consequences

- The project sees how many satellites check each day, by codename, version,
  channel and country, from satellites on the release that ships this or
  later, with Updates and the switch on, and running at their check minute.
  Older releases are not counted, so the page starts empty.
- A satellite that is off at its check minute isn't counted that day; a week
  of release service outages shows as fewer satellites.
- History reaches back three months, Analytics Engine's limit. A longer one
  needs weekly totals kept elsewhere, later.
- Anyone can send counted requests and inflate the numbers. Filling the
  Analytics Engine quota (100,000 data points a day on the free plan) stops
  counting until the next day; updates keep working.
- If the SQL API or the token fails, `/v1/statistics` answers 503 and the
  page says that the statistics are unavailable; counting and updates go on.
- Workers Logs (ADR 0022) also hold the codename now, with the address, for a
  few days.
- The switch is opt-out, so the codename is sent without consent. Someone who
  knows EU law reviews this before the release that ships it; if consent is
  needed, the switch becomes off by default and the dashboard asks.
