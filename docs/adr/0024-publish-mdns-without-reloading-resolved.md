# ADR 0024: Publish mDNS records without reloading systemd-resolved

Status: proposed (2026-11-17). Amends ADR 0010 (a rename) and ADR 0011 (how
the `_raop._tcp` record is published). Builds on ADR 0021.

## Context

- pinnad publishes `_esphomelib._tcp` and, while AirPlay is on, `_raop._tcp`
  through files in `/run/systemd/dnssd` (ADR 0002, ADR 0011). After writing or
  removing one it ran `systemctl reload-or-restart systemd-resolved`, which
  reloads. A rename set the hostname and, in the same second, rewrote the
  ESPHome file with the new `Name=` and reloaded.
- The image runs systemd-resolved 257.13 from Debian trixie. Debian has
  nothing newer for trixie: no backport, and forky's 262 can't go into a
  trixie image (ADR 0011).
- **A reload leaves the old records published.** resolved frees the services
  it read from the files without removing their records
  (`dnssd_service_clear_on_reload`; upstream PR #42983 is open). The old TXT
  record and the old instance name stay next to the new ones until resolved
  restarts. Measured on the virtual Echo Dot with no other mDNS device:
  - after two TXT changes, three TXT records;
  - after a `Name=` change, both instances;
  - after a restart, only the current records.

  On the Echo (2nd gen) `echo2`, the instance from before a rename stayed,
  with its SRV record pointing at a host name that was gone, and with two TXT
  records: one with the dashboard address and one from before it was set.
  Other satellites saw either at random.
- **resolved 257.11 to 257.13 lose to their own answers.** Stable commit
  `fcd87869d9` keeps resolved from ignoring its own multicast replies, which
  loop back, so a record that is still being announced for the first time can
  lose against itself (upstream issue #42072). It was reverted in v258 and
  later, never in v257. A loss renames: on 2026-11-17 a rename of `echo2`
  ended as host `echo8.local` with the ESPHome instance `echo10`, within one
  second. A second rename with debug logging (same day) showed resolved
  processing eight of its own replies; that time nothing was lost. The risk
  comes with every announcement of a new record: at boot, on a rename, and on
  every restart of resolved.
- **A hostname change is enough.** With `Name=%H` in a `.dnssd` file, resolved
  moves the instance name and the SRV target to the new hostname by itself
  and withdraws the old ones (measured on the virtual Echo Dot). `%H` is the
  name resolved publishes, which a conflict may have changed.
- resolved's D-Bus API (`RegisterService`) would avoid the files, but in 257
  a registration outlives the program that made it, every registration
  announces the hostname again, and unregistering sends goodbye packets for
  the host's addresses too. v262 fixes the first; the others remain.

## Decision

- **The ESPHome record is named `%H`** when the node name is the hostname,
  which is always the case on a satellite image (ADR 0010); pinnad on another
  machine keeps the node name. pinnad writes the file at start and when its
  TXT record changes (friendly name, dashboard address, encryption). A rename
  only sets the hostname; resolved moves the record.
- **pinnad restarts resolved instead of reloading it** when it changes or
  removes a `.dnssd` file that already existed. A file that is new (every
  boot, since `/run` starts empty, and AirPlay turned on) only needs a
  reload: nothing old can be left behind, and a reload doesn't announce the
  host name again. Requests less than a second apart share one reload or
  restart, which runs a second after the last of them, and a batch with any
  change restarts, so a change that touches both records (the friendly name)
  restarts once. Turning AirPlay off restarts at once instead, so the record
  goes before the bridge stops (ADR 0011). A restart costs a pause in name
  resolution of well under a second, and resolved announces the host name
  and every record again.
- **pinnad puts a renamed host back.** On a satellite image, while pinnad
  publishes its ESPHome record, it reads the name resolved publishes
  (`LLMNRHostname`, through busctl) once a minute, starting a minute after it
  starts. When it differs from the hostname's first label on two checks in a
  row (a check that can't read it resets the count), pinnad logs both names and restarts resolved, which
  reads the hostname and the files again (it brought `echo2` back on
  2026-11-17).
  Setting the hostname again would restore the host but not a service
  instance that resolved renamed, which only a restart resets. pinnad does
  this at most once every ten minutes, so a network that keeps answering for
  the name doesn't make the satellite announce itself without end. On
  another machine the hostname isn't meant to match, and restarting its
  resolved would never end. The Network page shows the name the satellite
  answers to while it differs; `GetNetwork` reports it only where pinnad
  watches it.
- **Not chosen:**
  - Patching resolved in the image: Pinna would build systemd. Ask Debian to
    ship the revert in a trixie point release instead; drop the repair once
    the image has a fixed resolved and renames stop.
  - The D-Bus API, for the reasons above. Look again with systemd 262.
  - Avahi (ADR 0011).

## Consequences

- A rename no longer leaves an old ESPHome instance behind, and a changed TXT
  record replaces the old one.
- Turning AirPlay off withdraws its record; with a reload it stayed
  published until the next restart of resolved.
- Each restart of resolved can still lose to its own answers until Debian
  ships the revert. The repair notices a renamed host. A service instance
  renamed while the host kept its name goes unnoticed until resolved next
  restarts; its SRV record still names the right host, and Home Assistant
  matches the satellite by MAC.
- Discovery can't rely on an instance name matching the host: ADR 0021 builds
  `.local` links from the SRV record's host.
