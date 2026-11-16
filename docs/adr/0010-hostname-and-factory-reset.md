# ADR 0010: A hostname per satellite, and a factory reset

Status: proposed (2026-11-15), amended 2026-11-16: `pinna.local` goes, and the reset checks the typed name. Builds on ADR 0005; amends what ADR 0005 says about the hostname.

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

- **Hostname:** the kernel hostname is the node name, so a satellite is `pinna-2a914e.local` in mDNS, in the router's DHCP list and on the dashboard. A one-shot unit (`pinna-hostname.service`, `pinnad --set-hostname`) sets it early, before systemd-networkd and systemd-resolved start; pinnad sets it again at start, for a board whose MAC only appears with wlan0. The static hostname in `/etc` stays `pinna`, as the fallback before that.
- **No `pinna.local`.** systemd-resolved announces only the machine's hostname, and a second mDNS responder for an alias isn't worth it. The docs and the dashboard name the satellite by its node name, which the dashboard shows and Home Assistant's discovery lists; over USB it stays `http://10.42.0.1/`. The Host allowlist already accepts every `.local` name.
- **Factory reset**, an RPC that the dashboard puts behind a dialog asking for the satellite's name; pinnad checks the typed name too, so no stray call resets it:
  1. pinnad writes a marker to `/var` and reboots.
  2. Early in the next boot, before pinnad and wpa_supplicant start, a one-shot unit deletes `/var/lib/pinna/` and the SSH host key.
  3. tmpfiles then seeds them again from the image, and sshd makes a new host key.

  The journal is kept, so a reset that goes wrong can be traced. The satellite comes back as the image built it, including any Wi-Fi network baked into the image. Without one, it comes back over USB only, at `http://10.42.0.1/`. A key or Wi-Fi network the image carries comes back with it.
- Out of scope: SSH keys and the time zone stay baked into the image. Changing them from an unauthenticated page would hand out root access, or make the OS drift into a general-purpose one.

## Consequences

- Two satellites on one network have distinct names in the browser, in Home Assistant and in mDNS.
- `http://pinna.local/` stops working. Bookmarks and habits move to the node name.
- Anyone on the LAN can reset a satellite, as anyone can restart it or install an update today (ADR 0005). The way back is the image's own seed, or USB.
