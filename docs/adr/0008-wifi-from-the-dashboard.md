# ADR 0008: Wi-Fi from the dashboard, joined with confirm-or-revert

Status: proposed (2026-11-15). Builds on ADR 0005.

## Context

- `wpa_supplicantwlan0` reads `/var/lib/pinna/wifi/wpa_supplicant-wlan0.conf`.
  - On first boot, tmpfiles copies the file from the image (`secrets/wifi/`) if it doesn't exist yet. Edits on the device survive updates.
  - `/etc/wpa_supplicant/` links to it.
- Nothing guarantees a `ctrl_interface`, and pinnad never touches the file. Changing networks is the one routine task that still needs SSH: edit the file, then restart `wpa_supplicantwlan0`.
- The MT8163 Wi-Fi chip powers up once per boot (`pinna-wifi.service`). Restarting `wpa_supplicantwlan0` is fine; restarting `pinna-wifi` is not.
- systemd-networkd runs DHCP on wlan0.
- The USB link (NCM, `10.42.0.1`, with a DHCP server for the computer) is always there, and the dashboard answers on it.

## Decision

- **pinnad owns `/var/lib/pinna/wifi/`.**
  - At start, pinnad adds `ctrl_interface=/run/wpa_supplicant` and `update_config=1` if they are missing, and keeps every network block as written.
  - The image's seed gets both lines too.
- **The control socket, not the file.** pinnad scans, lists saved networks, joins and forgets them over wpa_supplicant's control socket. `save_config` writes the file only after a successful join.
- **Joining is confirm-or-revert:**
  1. pinnad adds the network, selects it, and waits up to 45 s for an IPv4 address on wlan0.
  2. If none comes, it selects the previous network again and says why: the password was refused (the 4-way handshake failed), the network wasn't found, or there was no DHCP answer.
  3. Only a network that worked is saved.
- **The page says where it went.** Before the switch, the dashboard says where to find the satellite afterwards: `http://pinna.local/` on the new network, or `http://10.42.0.1/` over USB. Over USB, the page stays connected throughout.
- **No secrets out.**
  - Passphrases go in and are never returned.
  - Saved networks list name, security and order only.
  - Logs never contain a passphrase.
- **RPCs:** `ScanWifi`, `ListWifi`, `JoinWifi` (streams its progress), `ForgetWifi`.

## Consequences

- Moving a satellite to another network no longer needs SSH. The SSH note on the Network page and in the docs goes away.
- Anyone on the LAN can move the satellite to another network. The previous network and USB are the way back. Authentication is parked by the owner; this ADR doesn't change ADR 0005's trust model.
- A file edited by hand still works: pinnad only adds the control lines.
