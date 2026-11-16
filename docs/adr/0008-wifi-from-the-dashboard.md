# ADR 0008: Wi-Fi from the dashboard, joined with confirm-or-revert

Status: proposed (2026-11-15), amended 2026-11-16: hidden networks, the
security types, who prepares the file, a join that outlives the page, and a
trial that is kept only once the satellite is reached on the new network.
Builds on ADR 0005 and ADR 0007.

## Context

- `wpa_supplicant@wlan0` reads `/var/lib/pinna/wifi/wpa_supplicant-wlan0.conf`.
  - On first boot, tmpfiles copies the file from the image (`secrets/wifi/`) if it doesn't exist yet. Edits on the device survive updates.
  - `/etc/wpa_supplicant/` links to it.
  - Without the file, `wpa_supplicant@wlan0` doesn't start at all. Release images carry none (ADR 0015).
- Nothing guarantees a `ctrl_interface`, and pinnad never touches the file. Changing networks is the one routine task that still needs SSH: edit the file, then restart `wpa_supplicant@wlan0`.
- The MT8163 Wi-Fi chip powers up once per boot (`pinna-wifi.service`). Restarting `wpa_supplicant@wlan0` is fine; restarting `pinna-wifi` is not.
- Its vendor driver connects with WPA/WPA2-Personal and 802.1X only: no SAE (WPA3), and management frame protection off, so it can't join a network that requires either.
- Debian's unit starts wpa_supplicant on the `/etc` link. wpa_supplicant saves by writing a temporary file next to that path and renaming it, which fails on the read-only root: saving never worked on the device.
- The Echo (2nd gen) has its USB on pogo pins in its base: USB is no everyday way back for it.
- systemd-networkd runs DHCP on wlan0.
- The USB link (NCM, `10.42.0.1`, with a DHCP server for the computer) is always there, and the dashboard answers on it.

## Decision

- **wpa_supplicant runs on the file in `/var`**, so it can save it. A drop-in for `wpa_supplicant@wlan0` points `-c` there, restarts it on failure, and runs a start step (`ExecStartPre`, whose failure never stops Wi-Fi) that creates the file when it is missing, with no network in it, adds `ctrl_interface=/run/wpa_supplicant` when it is missing, and sets `update_config=1`. It keeps every network block as written. So wpa_supplicant runs, and the dashboard can scan, on a device that was never given a network.
- **The control socket, not the file.** pinnad scans, lists saved networks, joins and forgets them over wpa_supplicant's control socket. `SAVE_CONFIG` writes the file only after a successful join or a forget.
- **Networks it joins:** open and WPA/WPA2-Personal (WPA2/WPA3 mixed networks as WPA2), picked from the scan's flags. WPA3-only, Enterprise (802.1X) and WEP networks are listed but can't be joined.
- **Hidden networks** are joined by name: the owner types the SSID and picks open or WPA2. They are saved with `scan_ssid=1`, so wpa_supplicant probes for them by name.
- **The scan walks wpa_supplicant's list one entry at a time** (`BSS FIRST`, `BSS NEXT-<id>`): one reply holds about 4 KB, which a crowded building fills.
- **Joining is a trial, kept only once the satellite is reached on the new network.** An address isn't enough: a guest network that isolates its clients, or a VLAN that mDNS doesn't cross, gives one and still strands the satellite.
  1. pinnad adds the network, selects it, and waits up to 45 s for an IPv4 address on wlan0. If none comes, it goes back and says why: the password was refused (wpa_supplicant disabled the network after failed authentication), the network didn't let it connect, or no address came.
  2. With an address, the network is on trial, in memory only: not saved, not first in line.
  3. It is kept when the dashboard is opened at the new address, or when someone presses **Keep** (over USB, say). Kept, it gets the highest priority, replaces an older entry with the same name, the other saved networks stay as fallbacks, and the file is saved.
  4. Otherwise, after 5 minutes, or on **Go back**, pinnad goes back. A power cut or a restart of pinnad goes back too, since nothing was saved.
- **Going back is `RECONFIGURE`:** wpa_supplicant rereads the saved file, which a trial never touches. pinnad marks a trial in `/run` and goes back at start when it finds the mark, so a pinnad restart mid-join leaves nothing behind.
- **One Wi-Fi operation at a time.** A scan, a forget or a join fails with a sentence when another runs; keeping a trial or going back waits its turn; forgetting and joining are refused during a trial. Every save first re-enables every network (`ENABLE_NETWORK all`), so a selection never saves the others as disabled.
- **The join runs in pinnad**, like an update (ADR 0005): a browser that goes away, because the satellite left its network, doesn't stop it. `JoinWifi` streams its steps until the trial or the failure; `ListWifi` shows the trial and the last outcome, so the page can show them after it reconnects.
- **The page says where it went.** Before the switch, the dashboard says where to find the satellite afterwards: its `.local` name on the new network (ADR 0010), its MAC to look for in the router, or `http://10.42.0.1/` over USB, and that it comes back after 5 minutes unless it is reached there. Over USB, the page stays connected throughout.
- **No wlan0** on an Echo means the chip didn't power up this boot: the dashboard says so and offers a restart. On a machine without Wi-Fi (the VM), the RPCs say it has none.
- **Forgetting** removes a saved network and saves the file. Forgetting the one in use disconnects the satellite; the dashboard asks first, and says when it was the last one (USB only, then).
- **No secrets out.**
  - Passphrases go in and are never returned.
  - Saved networks list name, security and whether they are hidden or in use.
  - Logs never contain a passphrase.
- **RPCs:** `ScanWifi`, `ListWifi`, `JoinWifi` (streams its progress), `KeepWifi` (keep the trial, or go back now), `ForgetWifi`.

## Consequences

- Moving a satellite to another network no longer needs SSH. The SSH note on the Network page and in the docs goes away.
- A release image joins Wi-Fi from the dashboard over USB, with no file baked in.
- Anyone on the LAN can move the satellite to another network or forget the one it uses. A move nobody can follow undoes itself; a forget doesn't, and USB is the way back. Authentication is parked by the owner; this ADR doesn't change ADR 0005's trust model.
- A satellite moved over USB must be kept by pressing Keep, since USB proves nothing about the new network.
- A file edited by hand still works: the start step only adds the control lines, and wpa_supplicant rewrites the file in its own format on the next save.
