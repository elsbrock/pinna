# ADR 0005: The dashboard is served by pinnad on the LAN, without authentication

Status: accepted (2026-11-15). Amends the "Local control API" part of ADR 0002.
Amended by ADR 0007 (2026-11-16): host names and the satellite's name are set
on the dashboard.

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
  - a name added on the dashboard's Network page (ADR 0007), or in
    `[web] allowed_hosts`.

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
  - settings (wake word, sensitivity, ring brightness, wake sound), through the
    same paths as Home Assistant's entities, and the name and host names
    (ADR 0007)
  - tuning the wake word sensitivity with the device's + and − buttons, until
    the calibration ends
  - the network: Wi-Fi (ADR 0008) and the host names; the rest read-only
  - pinnad's journal at info level and above (debug lines can carry transcripts)
  - the A/B slots
  - installing an update
  - restarting, and a factory reset (ADR 0010)
  - running the ring's test pattern (Identify).
- **Updates run in pinnad.** pinnad runs `pinnactl update -- <manifest>` itself
  (http and https manifests only) and keeps its output, so a browser that goes
  away cannot interrupt an install. Signature and dm-verity checks and the
  rollback stay as in ADR 0003.

## Consequences

- Anyone on the LAN can do all of the following:
  - change the volume, the settings and the software microphone switch (the
    hardware microphone switch stays out of reach)
  - rename the satellite, which Home Assistant's device name follows
  - move the satellite to another Wi-Fi network, which it keeps only once
    someone reaches it there, or forget the network it uses (ADR 0008)
  - add a name the dashboard answers to. It stays until someone removes it,
    and opens the satellite to DNS rebinding through that name. The Network
    page lists every name added.
  - start a conversation, stop an alarm and clear a waiting notification
  - start a wake word calibration, which takes the volume buttons over and
    keeps the wake word from starting requests until it ends, 5 minutes after
    the last + or − press
  - read pinnad's log and the network details
  - run the ring's test pattern
  - restart the satellite, or reset it to how it was installed, which also
    removes the encryption key Home Assistant set (ADR 0010, ADR 0018)
  - install an update.

  Only images signed with the owner's release key install. The satellite falls
  back to the previous slot if the new one fails. An older signed release
  installs too, if someone has a copy.

  The controls match what a plaintext ESPHome API already exposes. Reading the
  log and the network details (SSID, signal, every address), the ring's test
  pattern, restarting, a factory reset, installing, renaming, moving or
  forgetting Wi-Fi and adding a host name go beyond it; a host name widens the DNS rebinding allowlist. So does clearing a
  notification: it fires the `cleared` event, which tells Home Assistant that
  someone at the device acknowledged it. An ESPHome client can only turn the
  switch off. Turning the web listener off (`[web] listen = ""` in
  `/var/lib/pinna/pinnad.toml`) is a developer setting until authentication
  exists (ADR 0007).
- A satellite opened by a custom DNS name (not `.local`) needs that name added
  on the Network page. The 421 response says so.
- Authentication (for example a password seeded from `secrets/device/`) is
  deferred. Adding it would amend this ADR and probably split the listener's
  API from the loopback one.
- `just image` now needs Node to build `dashboard/`.
