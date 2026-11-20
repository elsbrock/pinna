# ADR 0018: Home Assistant sets the encryption key

Status: accepted (2026-11-16). Amends ADR 0007 and ADR 0015 (the key was
file-only) and the key handling of pinna-esphome. Builds on ADR 0005 and ADR
0010.

## Context

- The ESPHome API can be encrypted with a Noise key, 32 bytes. Today a
  satellite gets one only from a file: `[esphome] encryption_key`, or
  `secrets/device/esphome-key` baked in by a builder. A release image (ADR
  0015) carries none, and its owner has no SSH key, so its connection to Home
  Assistant is plaintext: anyone on the network can listen to the microphone
  stream and send commands.
- pinnad takes a new key from Home Assistant only over a connection that is
  already encrypted, to rotate it.
- The dashboard has no login (ADR 0005). Showing or entering the key there
  would hand it to anyone on the network; the owner ruled that out while there
  is no authentication.
- Home Assistant provisions a key itself (its ESPHome integration,
  `_handle_dynamic_encryption_key`): when a device it has no key for reports
  `api_encryption_supported`, it generates one and sends
  `NoiseEncryptionSetKeyRequest`:
  - if the device reports `api_encryption_provisionable`, over a short Noise
    connection with the well-known all-zero key, so the key never travels in
    plaintext (this protects against listening, not against an active
    attacker in the middle);
  - otherwise over the plaintext connection (deprecated).

  ESPHome firmware with an `encryption:` block but no key does the same: it
  accepts both until a key is set, then only the key; only a factory reset
  removes it.

## Decision

- **Until it has a key, a satellite is provisionable.** It accepts plaintext
  connections, as today, and Noise connections with the all-zero key. Its
  device info reports `api_encryption_provisionable`. Proposed in ADR 0029:
  once the dashboard has a password, only for 15 minutes after the owner
  allows it there ([ADR 0029](0029-dashboard-password.md)).
- **The first key comes from whoever sends it first,** over either kind of
  connection. pinnad stores it (mode 0600, written durably) in
  `/var/lib/pinna/esphome-key`, uses it from then on, and closes every
  plaintext and all-zero connection, so Home Assistant reconnects with the key.
- **With a key, only the key works**, as before: no plaintext, no all-zero key.
  Home Assistant can still rotate it over a connection encrypted with the
  current one.
- **The way back is a factory reset** (ADR 0010), which removes the key file. A
  key set by mistake, or by someone else first, is undone there.
- **The dashboard never shows or takes the key.** It shows whether the
  connection is encrypted, and while it isn't, that Home Assistant turns
  encryption on when it adds the satellite (or reconnects to one it added
  without a key).
- `[esphome] encryption_key` and `secrets/device/esphome-key` stay, for
  developers and builders: a satellite born with a key is never provisionable.

## Consequences

- A release-image satellite added to a current Home Assistant is encrypted with
  no step by its owner. One added before this change becomes encrypted the next
  time Home Assistant connects.
- Anyone on the network can set the first key before Home Assistant does, and
  since the factory reset (ADR 0010) is as open as the rest of the dashboard,
  they can remove Home Assistant's key and set their own at any time. That
  locks Home Assistant out until the owner resets the satellite; it gives them
  nothing a plaintext satellite didn't already give them, and less than the
  dashboard does. A time window after power-up,
  as ESPHome's provisioning component has, would narrow it; it is left out
  while the dashboard itself has no login.
- An older Home Assistant that sends no key keeps a plaintext connection, as
  today.
- The docs stop telling owners to write a key file over SSH.
