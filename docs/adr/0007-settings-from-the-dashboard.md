# ADR 0007: Settings the dashboard writes live in settings.toml

Status: proposed (2026-11-15), amended 2026-11-16: `pinnad.toml` is for
developers only, and owners need neither a file nor SSH. Builds on ADR 0005.

## Context

- pinnad reads three files:
  - `/etc/pinna/pinnad.toml`, from the image
  - `/var/lib/pinna/pinnad.toml`, the local overrides, seeded from `secrets/device/` in the owner's own builds
  - `/var/lib/pinna/settings.toml`, the runtime settings.
- The two `pinnad.toml` files are merged at start. Nothing in them reloads: every change needs `systemctl restart pinnad`.
- Both are parsed with `deny_unknown_fields`. A bad write stops pinnad from starting, which fails `pinna-boot-ok` and reboots the device. With the fastboot guard armed, it reboots into fastboot.
- `settings.toml` holds volume, reply volume, wake sensitivity, LED brightness, the wake word switch and the software mute. Those apply at once and are Home Assistant entities too. pinnad writes the file to a temporary file and renames it, without an fsync.
- The dashboard can change only those settings today. Everything else needs a file:
  - Every satellite shows up in Home Assistant as "Echo Dot", the Echo (2nd gen) included.
  - The Echo (2nd gen) needs `speaker_profile = "Radar"` set by hand. Without it, it plays with the Echo Dot's speaker tuning.
  - The dashboard's 421 page sends people to `[web] allowed_hosts`.
- Release images carry no SSH key (ADR 0015). For their owners, "edit a file" means the USB serial console.
- goals.md: everything is available in Home Assistant.

## Decision

- **Owners never edit a file.** Every setting an owner needs is on the dashboard. `pinnad.toml`, the image's and the local one, is for developers: backends, ports, capture gain, LED driver limits and first-boot defaults. The docs stop sending owners there. The dashboard never writes it.
- **Settings the dashboard manages go into `settings.toml`.** A value there wins over `pinnad.toml`'s for the same thing. The settings this ADR adds, and the wake words, are optional in the file: a missing key means the `pinnad.toml` value, so a `settings.toml` written by an older image keeps the developer's choice. The older ones (volume, sensitivity, brightness, switches) are always written; `pinnad.toml` gives them only when no `settings.toml` exists.
- **Typed fields, not key/value.**
  - Each setting is an optional field on `UpdateSettingsRequest` and a field on `Settings`, documented with its range.
  - pinnad checks the value and rejects a bad one with `InvalidArgument` and a sentence the dashboard can show.
- **Every setting is also a Home Assistant entity**, through the same code path, unless Home Assistant has its own way to do the same thing.
- **Writes are durable:** write a temporary file, fsync it, rename it, then fsync the directory. `UpdateSettings` fails if the write does. The change still applies until the satellite restarts, and the error says it will be lost then, so the dashboard never claims a change is saved when it isn't.
- **`settings.toml` keeps keys it doesn't know.** After a rollback to an older image, a key that only the newer pinnad writes must not reset the other settings, mute included, and must still be there after the next update. pinnad reads unknown keys and writes them back unchanged. pinnad images from before this amendment drop them on their next save. The two `pinnad.toml` files keep refusing unknown keys.
- **First settings:**
  - `friendly_name`, the name Home Assistant shows: 1 to 64 characters, at most 200 bytes, no control characters. It is read when Home Assistant connects. pinnad updates its device info and mDNS record at once. It closes the API connection once the satellite is idle, never during a request, an announcement, media or a ringing timer, and Home Assistant reconnects with the new name. No entity: Home Assistant renames devices itself.
  - `wake_sound`, the chime when listening starts, also the "Wake sound" switch. It takes effect on the next request.
  - `allowed_hosts`, the names the dashboard answers to besides IP addresses, `localhost`, `.local` names and the hostname (ADR 0005): DNS names, at most 16. They add to `[web] allowed_hosts` and apply at once. No entity: they guard the dashboard, not the satellite, so they belong where the dashboard is, not in Home Assistant. Replaced by one dashboard address, `url` ([ADR 0020](0020-switch-between-satellites.md)); the key is no longer read.
- **Added later:** the active wake words, chosen from the dashboard or from Home Assistant's own dropdowns ([ADR 0013](0013-wake-words.md)); the hostname, which is also the node name ([ADR 0010](0010-hostname-and-factory-reset.md)).
- **The time zone** (added later): `time_zone` in `settings.toml`, an IANA name such as `Europe/Berlin`, chosen on the System page from the zones the satellite has (`zone1970.tab`). The page offers the browser's own zone when the satellite has it. Unset is UTC. The image ships no fixed zone:
  - It installs tzdata. `/etc/localtime`, on the read-only root, links to `/var/lib/pinna/localtime`, and pinnad points that link at `/usr/share/zoneinfo/<zone>`. glibc, and so journald and `date`, follow both links. `timedatectl` follows only one, so it shows no zone. systemd-timedated can't set it: it writes `/etc/localtime`.
  - Home Assistant sends its time zone only as a POSIX rule (`CET-1CEST,M3.5.0,M10.5.0/3`), which names no zone, so the satellite doesn't follow it, and it needs no Home Assistant to have one.
  - No entity: a list of about 300 zones is no use as a Home Assistant select, and Home Assistant keeps its own time zone.
  - A factory reset brings back UTC.
- **The default name follows the hardware.** Without `friendly_name` anywhere, a satellite is called by its model and the last three bytes of its MAC, the way Home Assistant Voice PE is: "Echo Dot 2a914e", "Echo 2a914e". The device image no longer sets one; the development VM's layer names it "Virtual Echo Dot" (ADR 0014).
- **Not settings:**
  - The model in the device info and the speaker profile follow the hardware (`board_model()`): Radar tuning on the Echo (2nd gen), Flat otherwise. Home Assistant shows the project name's model (`pinna.satellite`) instead. `[device] model` and `[audio] speaker_profile` remain developer overrides; the image sets neither.
  - The suggested area: Home Assistant only reads it when it first adds the device, and assigns areas itself.
  - Turning the dashboard off (`[web] listen = ""`) is a developer setting until authentication exists. A switch on a page with no login protects nothing.
  - Backends and ports.
- **Still file-only, for their own ADRs:** SSH keys (ADR 0010). The ESPHome encryption key is set by Home Assistant itself ([ADR 0018](0018-home-assistant-sets-the-encryption-key.md)); the dashboard shows whether encryption is on.

## Consequences

- A satellite can be named from the dashboard, without SSH. Home Assistant's device name follows. Its entity IDs and the integration's title keep the name the device had when it was added, so while the default name stands, the dashboard says to name the device before adding it.
- Changing the default name renames, once, every satellite that has no `friendly_name` and wasn't renamed in Home Assistant.
- The Echo (2nd gen) gets its own speaker tuning without configuration.
- Deleting a key from `settings.toml` brings back the `pinnad.toml` value. The docs about `[device] friendly_name`, `[audio] wake_sound` and `[web] allowed_hosts` say the dashboard is the usual way.
- Anyone on the LAN can rename the satellite and set its dashboard address (ADR 0020), as ADR 0005 already allows for the other settings.
