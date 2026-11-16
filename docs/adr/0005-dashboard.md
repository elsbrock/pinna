# ADR 0005: The dashboard is served by pinnad on the LAN, without authentication

Status: accepted (2026-11-15). Amends the "Local control API" part of ADR 0002.
Amended by ADR 0007 (2026-11-16): host names and the satellite's name are set
on the dashboard. Amended 2026-11-16: the dashboard draws the ring's own frames
(WatchRing). Amended 2026-11-16: the dashboard switches to the other slot and
keeps an installed update back (SetActiveSlot). Amended by ADR 0020
(2026-11-16): the listener also serves `/metrics`. Amended by ADR 0021: one
dashboard address replaces the host names.

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
  - the hostname: the one the satellite started with, and its current one
    after a rename (ADR 0010)
  - the host of the dashboard address set on the Network page (ADR 0021),
    or a name in `[web] allowed_hosts`.

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
    same paths as Home Assistant's entities, the name (ADR 0007), the
    hostname (ADR 0010) and the dashboard address (ADR 0021)
  - tuning the wake word sensitivity with the device's + and − buttons, until
    the calibration ends
  - the network: Wi-Fi (ADR 0008), the hostname (ADR 0010) and the
    dashboard address (ADR 0021); the rest read-only
  - pinnad's journal at info level and above (debug lines can carry transcripts)
  - the A/B slots
  - installing an update, switching to the other slot, or keeping an installed
    update back
  - restarting, and a factory reset (ADR 0010)
  - running the ring's test pattern (Identify).
- **The ring on the page is the device's.** `WatchRing` streams the frames
  pinnad shows: the current one, then the latest after each 40 ms in which it
  changed, and nothing while nobody watches. Every ring that stands for the
  satellite draws them: the logo's, the app bar's, the sidebar's and the
  Overview's drawing. A ring drawn from `Status` could not show what
  `Status` does not carry (the Home Assistant light, music, the volume and
  error flashes, the talker's direction) or the device's timing, and it left
  out a timer's arc and the brightness. While no frames come (before the
  first, while the satellite is unreachable, for a few seconds after the
  stream ends, from a pinnad without WatchRing), the page draws from
  `Status` again. Amended 2026-11-16; until then it always did.
- **Updates run in pinnad.** pinnad runs `pinnactl update -- <manifest>` itself
  (http and https manifests only) and keeps its output, so a browser that goes
  away cannot interrupt an install. Signature and dm-verity checks and the
  rollback stay as in ADR 0003. Switching to the other slot is pinnad's too
  (`SetActiveSlot`): it checks the slot's image as pinna-init does, makes it
  active as a trial and restarts. If the restart fails, it puts the slot state
  back, so a switch doesn't wait for the next power cut. Only a slot that is
  good or hasn't been started yet; not while an install runs or the running
  slot is on trial. The running slot can be made active again, which keeps an
  installed update back without a restart. Amended 2026-11-16.

## Consequences

- Anyone on the LAN can do all of the following:
  - change the volume, the settings and the software microphone switch (the
    hardware microphone switch stays out of reach)
  - rename the satellite, which Home Assistant's device name follows
  - move the satellite to another Wi-Fi network, which it keeps only once
    someone reaches it there, or forget the network it uses (ADR 0008)
  - set the dashboard address. Its host stays allowed until someone changes
    or removes the address, and opens the satellite to DNS rebinding through
    that name. The Network page shows it.
  - start a conversation, stop an alarm and clear a waiting notification
  - start a wake word calibration, which takes the volume buttons over and
    keeps the wake word from starting requests until it ends, 5 minutes after
    the last + or − press
  - read pinnad's log and the network details
  - read the metrics: the host's load, memory, traffic, temperatures and
    services, and pinnad's counters (ADR 0020)
  - run the ring's test pattern
  - restart the satellite, or reset it to how it was installed, which also
    removes the encryption key Home Assistant set (ADR 0010, ADR 0018)
  - install an update, switch to the version in the other slot, or keep an
    installed update from starting.

  Only images signed with the owner's release key install. The satellite falls
  back to the previous slot if the new one fails. An older signed release
  installs too, if someone has a copy.

  The controls match what a plaintext ESPHome API already exposes. Reading the
  log, the network details (SSID, signal, every address) and the metrics, the
  ring's test pattern, restarting, a factory reset, installing, switching slots, renaming, moving or
  forgetting Wi-Fi, renaming the host and setting the dashboard address go beyond it; the address widens the DNS rebinding allowlist. So does clearing a
  notification: it fires the `cleared` event, which tells Home Assistant that
  someone at the device acknowledged it. An ESPHome client can only turn the
  switch off. Turning the web listener off (`[web] listen = ""` in
  `/var/lib/pinna/pinnad.toml`) is a developer setting until authentication
  exists (ADR 0007).
- A satellite opened by a custom DNS name (not `.local`) needs that name as its
  dashboard address on the Network page. The 421 response says so.
- Authentication (for example a password seeded from `secrets/device/`) is
  deferred. Adding it would amend this ADR and probably split the listener's
  API from the loopback one.
- `just image` now needs Node to build `dashboard/`.
