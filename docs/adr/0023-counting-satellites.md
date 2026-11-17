# ADR 0023: Counting satellites in the update check

Status: proposed (2026-11-17). Builds on ADR 0022 (the daily update check)
and ADR 0020 (what a satellite reports about itself). Points marked *default*
are not confirmed by the owner yet.

## Context

- The project knows nothing about the satellites in use: how many there are,
  which model, which versions. That decides which devices and versions get
  attention, and how many satellites a bad release reaches.
- ADR 0020's metrics stay on the LAN. That record also shows why activity is
  sensitive: wake counts stored over time are a record of who was home.
- The owners are Home Assistant users who chose Pinna to keep their home
  local (PRODUCT.md, the goals page). Home Assistant's own analytics are
  opt-in, asked once during onboarding.
- In the EU, reading information from a device and sending it needs the
  user's consent unless it is strictly necessary for a service the user asked
  for (the ePrivacy directive, Art. 5(3); in Germany §25 TDDDG). That holds
  whether or not the information identifies anyone (EDPB Guidelines 2/2023).
  The update check is such a service. Whether its model, version and channel
  are strictly necessary for it is for a legal review to confirm.
- Fedora counts its installations this way ("countme"): one repository request
  a week carries a few coarse fields and no identifier.
- The project's analytics go to PostHog's EU region (`eu.posthog.com`).

## Decision

### Counts

- The scheduled update check (ADR 0022), and no other check, carries three
  fields as query parameters: the model, the version and the channel. Other
  checks carry none, so each satellite counts once a day.
- For each such check the Worker sends PostHog one event: no identifier (a new
  random `distinct_id` for each event, no person profile), no IP address, no
  location. The event is dated to the day, without the time, so the time at
  which a satellite checks can't link its events across days. Satellites per
  day, by model, version and channel, are the number of events per day.
- The Worker sees the address a check comes from, as any server does. It
  doesn't pass it on, and the Worker's logs (Workers Logs, Logpush) stay off.
  Cloudflare itself still sees each request's address, as for any site it
  serves.
- The check carries nothing else: no identifier, no serial number, no names
  (the satellite's, Wi-Fi networks, Home Assistant's address), and nothing
  about what the satellite heard or did.
- Development builds (ADR 0025) never send the fields.

### The switch

- Counting is on by default. The setting **Include in the satellite count**
  is a setting like the others (ADR 0007): a field of `UpdateSettings`, a
  control on the dashboard's Updates page, and a `switch` entity in Home
  Assistant (category config).
- With it off, the check carries no fields. With Updates off (ADR 0022) there
  is no check, so nothing is counted either.

### Published

- A privacy page on the website lists the fields with an example, and says
  how long PostHog keeps them. A new field needs a change to this record and
  a line in the release notes.
- On the goals page, "Cloud accounts and remote access" says that Pinna has no
  account, and that it contacts one service of the project's to check for
  updates, which also counts satellites without identifying them. The page
  changes when this ships.

### Not decided here

- Telemetry that the owner turns on (an install ID, boot and update outcomes,
  crash reports) gets its own record, later.

## Consequences

- The project sees how many satellites check in each day, by model, version
  and channel, from every satellite that keeps both Updates and counting on.
  Satellites with Updates off don't appear.
- Questions about single satellites (rollbacks, crashes, Home Assistant use)
  stay with GitHub issues until telemetry is decided.
- PostHog processes the counts for the project. A data processing agreement
  with PostHog, and a review of the consent question by someone who knows EU
  law, come before the first public release. If the counts need consent, they
  become opt-in.
