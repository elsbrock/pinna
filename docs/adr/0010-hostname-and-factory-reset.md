# ADR 0010: A hostname per satellite, and a factory reset

Status: proposed (2026-11-15). Builds on ADR 0005; amends what ADR 0005 says about the hostname.

## Context

- Every satellite has the hostname `pinna` (`Hostname=pinna` in the image). With two on a network, mDNS renames one to `pinna2.local`, and nobody can tell which is which without IP addresses.
- The ESPHome node name is already unique: `pinna-` plus the last three bytes of the MAC.
- `/etc` is on the read-only, signed root, so the static hostname can't change on a device.
- No factory reset exists. The only way back to a clean device is to reflash the userdata partition. Everything the device keeps is on `/var`:
  - `/var/lib/pinna/`: settings, the local `pinnad.toml`, the Noise key, Wi-Fi, WirePlumber state
  - the SSH host key in `/var/lib/ssh/`
  - the journal.

  On first boot, tmpfiles copies the image's seeds back in.

## Decision

- **Hostname:** pinnad sets the transient hostname (systemd-hostnamed) to the node name at start, so a satellite is `pinna-2a914e.local`. `pinna.local` keeps working for the first satellite on a network, through an mDNS alias, because the docs and habits use it. The Host allowlist already accepts every `.local` name.
- **Factory reset**, an RPC that the dashboard puts behind a dialog asking for the satellite's name:
  1. pinnad writes a marker to `/var` and reboots.
  2. Early in the next boot, before pinnad and wpa_supplicant start, a one-shot unit deletes `/var/lib/pinna/` and the SSH host key.
  3. tmpfiles then seeds them again from the image.

  The journal is kept, so a reset that goes wrong can be traced. The satellite comes back as the image built it, including any Wi-Fi network baked into the image. Without one, it comes back over USB only, at the setup flow (ADR 0009).
- Out of scope: SSH keys and the time zone stay baked into the image. Changing them from an unauthenticated page would hand out root access, or make the OS drift into a general-purpose one.

## Consequences

- Two satellites on one network have distinct names in the browser, in Home Assistant and in mDNS.
- Anyone on the LAN can reset a satellite, as anyone can restart it or install an update today (ADR 0005). The way back is the image's own seed, or USB.
