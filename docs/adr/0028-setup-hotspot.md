# ADR 0028: A setup hotspot when no Wi-Fi network is saved

Status: proposed (2026-11-19). Amends ADR 0009, which keeps no access point
mode, and ADR 0008, where USB is the only way back after the last network is
forgotten; decides the setup hotspot that ADR 0012 leaves open. On the
Echo Dot (5th gen), hostapd runs on `ap0` and a join from the Network page
gets an address once `ap0` is removed (2026-11-27); a phone on its hotspot
isn't measured. On the MT8163 the kernel side is compiled only. The rest is
tested on the virtual Echo Dot with simulated radios. Amended 2026-11-28: the
ring shows the hotspot; unit tests only, no device run.

## Context

- A release image carries no Wi-Fi network (ADR 0015). Until one is joined,
  the satellite is reachable over USB only: the dashboard at
  `http://10.42.0.1/` (ADR 0008, ADR 0009).
- The Echo (2nd gen) has its USB on pogo pins in its base. Setting it up, or
  reaching it after its last network was forgotten, needs a pogo-pin cable.
- A phone joins a Wi-Fi network more easily than it reaches a USB network.
- **The MT8163 can be an access point, through its vendor driver's Wi-Fi
  Direct part.** Writing `A` to `/dev/wmtWifi` registers a second interface,
  `ap0`, whose wiphy offers `NL80211_IFTYPE_AP` with the AP SME in the driver
  (`WIPHY_FLAG_HAVE_AP_SME`): hostapd drives it over nl80211 and does only the
  4-way handshake. `wlan0` stays a station.
  - The stock 3.18 build had Wi-Fi Direct on for ARM, with its objects in the
    Makefile; the import Pinna started from had it off and dropped them.
    Kernel patch 0030 ports it to 6.12 behind `CONFIG_MTK_MT8163_WIFI_AP`,
    with Wi-Fi Display, which the vendor code needs with it, as in the stock
    build.
  - Turning it on also compiles Wi-Fi Direct branches into station code that
    every satellite runs, as in the stock build.
  - Amazon's driver has an "OOBE AP mode" (`CONFIG_MTK_WIFI_OOBE_AP_MODE`,
    11b/g rates for an access point). It isn't known whether the Echo builds
    used it. It stays off.
- Not known until an MT8163 runs it:
  - whether `ap0` beacons and takes a WPA2 client;
  - whether `wlan0` can scan while `ap0` runs;
  - whether `wlan0` joins a network with `ap0` registered but stopped.
- The chip has one radio. Nothing shows it can run an access point and a
  station on different channels.

## Decision

- **When:** pinnad runs the hotspot while wpa_supplicant has no saved
  network, no join runs and no network is on trial. That is after the first
  boot of a satellite that was never given a network, after the last network
  is forgotten, after a failed join, and after a trial goes back. A satellite
  with a saved network never runs it, even when none of its networks is in
  range: USB and rescue mode cover that (ADR 0012).
- **Where:** only on a Pinna image, with `wlan0` present and the hotspot unit
  installed. Not on a developer's computer, not in rescue mode (pinnad
  doesn't run there), and not on the virtual Echo Dot unless it is given
  simulated radios.
- **Name:** `Pinna setup <hostname>`, cut to 32 bytes, read when it starts.
- **Password: fixed, `pinnasetup`, and published in the docs.** WPA2-Personal.
  This is a stopgap: anyone nearby who knows the password can join while a
  satellite is unconfigured, and read the home network's password as it is
  typed. A password only the owner can learn is planned; how isn't decided.
- **Radio:** 2.4 GHz channel 6, 802.11n, WPA2-PSK with CCMP.
  - `pinna-hotspot.service` runs hostapd on `ap0`, with the configuration
    pinnad writes to `/run/pinna/hostapd.conf`.
  - Its start step registers `ap0` on the MT8163 once per boot, by writing
    `A`, and leaves it registered until the next boot. It never writes `S`
    or `0`: the chip powers up once per boot (ADR 0008).
  - On the Echo Dot (5th gen)'s MT7663 (mt76, a mac80211 driver), the start
    step adds `ap0` to the radio with `iw`, and the unit's stop step removes
    it again. The virtual Echo Dot's `ap0` is a simulated radio of its own
    and stays. With `ap0` left on, `wlan0` associated and completed WPA, but the
    DHCP server never saw its DISCOVER (measured 2026-11-27).
  - The unit isn't enabled and doesn't restart hostapd; pinnad starts and
    stops it, and stops one an earlier pinnad left running.
- **A hotspot that doesn't work:** hostapd counts as started once it has run
  for 3 seconds. A start that fails, or a hostapd that stops by itself, is
  tried again after the next Wi-Fi change or after 5 minutes. pinnad looks
  every 30 seconds. Until a start succeeds, `ListWifi` says the satellite has
  no hotspot (`has_hotspot`), and the dashboard says it is reachable over USB
  only.
- **Network:** `ap0` has `10.42.1.1/24`, next to USB's `10.42.0.1/24`.
  systemd-networkd's DHCP server hands out addresses with `10.42.1.1` as
  router and DNS server. No captive portal option (RFC 8910): it names a
  captive portal API (RFC 8908), which Pinna doesn't have, and the redirect
  below does the job. mDNS answers on `ap0` too.
- **Captive portal:** while the hotspot runs, pinnad answers every DNS query
  that reaches `10.42.1.1:53` with `10.42.1.1` for A, and with no records for
  other types. A web request that reaches `10.42.1.1` under another name is
  redirected to the Network page, `http://10.42.1.1/#/network`. A phone's connectivity check gets that
  redirect and opens the dashboard by itself. Every other address keeps the
  page for a name the satellite doesn't answer to.
- **Joining from the hotspot is ADR 0008's join**, on the Network page.
  - Scans leave out setup hotspots, the satellite's own and other
    satellites': nobody can open a satellite that joined one.
  - pinnad scans before it starts the hotspot, and remembers the last scan
    that found networks. While the hotspot runs, a scan that fails or finds
    nothing lists that one, so the list has networks even if the chip can't
    scan while `ap0` runs.
  - When a join starts, pinnad first sends the page a step saying the hotspot
    goes off, waits a second, and then stops it; the phone drops off it. A
    page that heard a step knows it lost the satellite, rather than that the
    join was refused.
  - A join that fails brings the hotspot back, and the page shows why once
    the phone is on it again (`ListWifi`'s last outcome).
  - A trial is kept when the dashboard is opened at the new address, as
    before. Otherwise it goes back after 5 minutes, and the hotspot returns;
    the failure names the address that would have kept it, for a phone that
    couldn't open the `.local` name.
  - A failed join says the hotspot comes back, not that the satellite is
    reachable over USB only.
  - Before the switch the page says what happens: the phone leaves the
    hotspot; rejoin your network and open the satellite's `.local` name, or
    join the hotspot again if it comes back.
- **The dashboard shows it:** `ListWifi` carries the hotspot's name while it
  runs, and whether the satellite has one. The Network page says it is on,
  and a page opened on the hotspot shows how the last join ended.
- **The ring shows it:** while the hotspot runs, a white arc of four pixels
  at the front of the ring slowly gets brighter and dimmer, once every 4
  seconds. Without it, an unconfigured satellite's ring is dark, as when it
  is idle, and only a phone's list of networks shows that it waits for one.
  - It shows where the dark idle ring would. A timer's arc, a notification,
    the light and music show above it, and so do the phases, the muted
    microphones, Bluetooth pairing and a confirmation (ADR 0029). The boot
    stages' frame (ADR 0012) shows above it too: the hotspot is up a few
    seconds after pinnad starts, and the start's arc, its fill and fade, and
    a failed stage's flashes keep their meaning. Once the start has faded,
    the arc shows.
  - It holds still, so it reads differently from the white light that
    circles for a confirmation and from the boot stages' white flashes.
    Orange stays out: it is the stock firmware's setup look.
  - It ends when the hotspot stops, also while a join from it runs, and
    comes back with the hotspot. When hostapd stops by itself, pinnad
    notices within 30 seconds, and the arc ends then.
- Home Assistant plays no part (ADR 0017).

## Consequences

- Once Pinna is installed, an Echo (2nd gen) can be set up and recovered
  without the pogo-pin cable. Installing still needs USB (ADR 0016).
- Forgetting the last network no longer strands a satellite without USB:
  the hotspot comes back.
- An unconfigured satellite announces itself to everyone in range, with a
  password anyone can look up.
- Someone next to an unconfigured satellite sees on its ring that it waits
  to be set up, while its hotspot runs. A satellite whose hotspot didn't
  start keeps a dark ring.
- The image gains hostapd. pinnad gains a small DNS responder.
- Wi-Fi Direct changes station code on every Echo. The device test covers a
  configured satellite too, not only the hotspot. If its Wi-Fi regresses,
  turning `CONFIG_MTK_MT8163_WIFI_AP` off restores the old driver, and
  `pinna-hotspot.service` then fails to start, so no satellite offers the
  hotspot.
- To run on a device, without writing it until the owner asks:
  1. A configured satellite: Wi-Fi joins, holds and streams as before.
  2. An unconfigured one: `ap0` appears, the hotspot is visible, a phone
     joins with the password, gets an address, and opens the dashboard by
     itself.
  3. A scan from the hotspot lists networks.
  4. A join from the hotspot works and is kept from the new address; a wrong
     password brings the hotspot back.
