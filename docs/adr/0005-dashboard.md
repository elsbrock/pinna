# ADR 0005: The dashboard is served by pinnad on the LAN, without authentication

Status: accepted (2026-11-15). Amends the "Local control API" part of ADR 0002.

## Context

The satellite has no screen. The control API (`pinna.v1.DeviceService`) has
been reachable only on loopback, through `pinnactl` over SSH. A dashboard in a
browser on a phone or laptop needs two things: something on the satellite
that serves its files, and an API the browser can reach from the LAN.

Constraints:

- About 480 MB of RAM and a read-only, signed root. No Node runtime and no
  second web server on the device.
- The dashboard has to work offline (no CDN fonts or scripts).
- The ESPHome API on port 6053 already faces the LAN. It is plaintext unless a
  Noise key is set, and it already exposes mute, volume and the action button
  as Home Assistant entities.

## Decision

- **Static build, served by pinnad.** The dashboard (`dashboard/`: Svelte,
  connect-es) is compiled to static files at image build time and installed at
  `/usr/share/pinna/web`. Node is needed only on the build machine.
- **A second listener in pinnad.** `[web] listen` (`[::]:80` in the image,
  off by default in code) serves those files. It also serves the same
  `DeviceService` under `/pinna.v1.DeviceService/`, so the page and the API
  share one origin and need no CORS. It uses axum, through connectrpc's `axum`
  feature, and tower-http's `ServeDir`.
- **No authentication.** Anyone who can reach the satellite on the LAN can use
  the dashboard.
- **Host allowlist against DNS rebinding.** The web listener answers only
  requests whose Host is:
  - an IP address
  - `localhost`
  - any `*.local` name (which only mDNS on the local network resolves)
  - the system hostname
  - a name in `[web] allowed_hosts`.

  Any other Host gets `421 Misdirected Request`. Cross-site requests are
  already blocked by the Connect protocol: JSON and proto bodies need a CORS
  preflight, and the server never grants one.
- **Loopback stays as it is.** `[api] listen = "127.0.0.1:7470"` keeps serving
  `pinnactl` and the boot health check, independent of the web listener.
- **The dashboard is not essential.** If the web listener fails to bind, pinnad
  logs the error and keeps running. It never stops the voice path or fails the
  boot health check.
- **Caching.** Hashed files under `/assets/` are `immutable`. Everything else,
  including `index.html`, is `no-store`, because image builds fix file times
  and a conditional request would keep a stale page after an A/B update.
- **Discovery.** pinnad reports the web port as ESPHome
  `DeviceInfo.webserver_port`, so Home Assistant shows a "Visit" link on the
  device page.
- **What the dashboard can do.** Everything is in `DeviceService`:
  - status and controls
  - settings (wake word, sensitivity, ring brightness), through the same paths
    as Home Assistant's entities
  - the network (read-only)
  - pinnad's journal at info level and above (debug lines can carry transcripts)
  - the A/B slots
  - installing an update
  - restarting
  - running the ring's test pattern (Identify).
- **Updates run in pinnad.** pinnad runs `pinnactl update -- <manifest>` itself
  (http and https manifests only) and keeps its output, so a browser that goes
  away cannot interrupt an install. Signature and dm-verity checks and the
  rollback stay as in ADR 0003.

## Consequences

- Anyone on the LAN can do all of the following:
  - change the volume, the settings and the software microphone switch (the
    hardware microphone switch stays out of reach)
  - start a conversation and stop an alarm
  - read pinnad's log and the network details
  - run the ring's test pattern
  - restart the satellite
  - install an update.

  Only images signed with the owner's release key install. The satellite falls
  back to the previous slot if the new one fails. An older signed release
  installs too, if someone has a copy.

  The controls match what a plaintext ESPHome API already exposes. Reading the
  log and the network details (SSID, signal, every address), the ring's test
  pattern, restarting and installing go beyond it. An owner who doesn't want any of it sets
  `[web] listen = ""` in `/var/lib/pinna/pinnad.toml`.
- A satellite opened by a custom DNS name (not `.local`) needs that name in
  `[web] allowed_hosts`. The 421 response says so.
- Authentication (for example a password seeded from `secrets/device/`) is
  deferred. Adding it would amend this ADR and probably split the listener's
  API from the loopback one.
- `just image` now needs Node to build `dashboard/`.
