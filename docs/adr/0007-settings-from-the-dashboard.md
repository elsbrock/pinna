# ADR 0007: Settings the dashboard writes live in settings.toml

Status: proposed (2026-11-15). Builds on ADR 0005.

## Context

- pinnad reads three files:
  - `/etc/pinna/pinnad.toml`, from the image
  - `/var/lib/pinna/pinnad.toml`, the owner's overrides, seeded from `secrets/device/`
  - `/var/lib/pinna/settings.toml`, the runtime settings.
- The two `pinnad.toml` files are merged at start. Nothing in them reloads: every change needs `systemctl restart pinnad`.
- Both are parsed with `deny_unknown_fields`. A bad write stops pinnad from starting, which fails `pinna-boot-ok` and reboots the device. With the fastboot guard armed, it reboots into fastboot.
- `settings.toml` holds volume, reply volume, wake sensitivity, LED brightness, the wake word switch and the software mute. Those apply at once and are Home Assistant entities too. pinnad writes the file to a temporary file and renames it, without an fsync.
- The dashboard can change only those settings today. Everything else needs SSH. For example, every satellite shows up in Home Assistant as "Echo Dot".
- goals.md: everything is available in Home Assistant.

## Decision

- **The dashboard never writes `pinnad.toml`.** It stays the place for the owner's and developers' overrides, where a mistake is made by hand, over SSH, with USB as the way back.
- **Settings the dashboard manages go into `settings.toml`.** A value there wins over `pinnad.toml`'s for the same thing. `pinnad.toml` gives the first-boot default, as it already does for volume and sensitivity.
- **Typed fields, not key/value.**
  - Each setting is an optional field on `UpdateSettingsRequest` and a field on `Settings`, documented with its range.
  - pinnad checks the value and rejects a bad one with `InvalidArgument` and a sentence the dashboard can show.
- **Every setting is also a Home Assistant entity**, through the same code path, unless Home Assistant has its own way to do the same thing.
- **Writes are durable:** write a temporary file, fsync it, rename it, then fsync the directory.
- **`settings.toml` ignores keys it doesn't know.** After a rollback to an older image, a key that only the newer pinnad writes must not reset the other settings, mute included. The two `pinnad.toml` files keep refusing unknown keys.
- **First settings:**
  - `friendly_name`, the name Home Assistant shows. It is read when Home Assistant connects, so pinnad closes the API connection after a change and Home Assistant reconnects with the new name.
  - `wake_sound`, the chime when listening starts. It takes effect on the next request.
- **Added later:** the active wake words, chosen from the dashboard or from Home Assistant's own dropdowns ([ADR 0013](0013-wake-words.md)).
- **Not settings:**
  - The speaker profile, which follows the hardware (`Status.model`).
  - Backends and ports.

## Consequences

- A satellite can be named from the dashboard or from Home Assistant, without SSH.
- Deleting a key from `settings.toml` brings back the `pinnad.toml` default. The docs about `[device] friendly_name` and `[audio] wake_sound` say the dashboard is the usual way.
- Anyone on the LAN can rename the satellite, as ADR 0005 already allows for the other settings.
