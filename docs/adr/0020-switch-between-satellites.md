# ADR 0020: Switch between satellites, and one dashboard address

Status: proposed (2026-11-16). Amends ADR 0005 and ADR 0007 (the host
allowlist and the dashboard's `allowed_hosts`). Builds on ADR 0010 (hostname).

## Context

- A household with several satellites opens each dashboard by hand: its
  `.local` name, its IP address or a name the owner set up. Home Assistant's
  device page links to each one ("Visit", ADR 0005), but only from Home
  Assistant, which may become optional, and not from one dashboard to the next
  when updating several satellites in one sitting.
- Every satellite announces itself over mDNS as `_esphomelib._tcp`, named by
  its node name, with TXT `project_name=pinna.satellite`. That record carries
  the ESPHome port, 6053, not the web listener's.
- The Network page has an "Other names" list (`allowed_hosts`, ADR 0007). It
  only widens the Host allowlist against DNS rebinding (ADR 0005). Owners read
  it as "the satellite's address", which it isn't: nothing else uses it.
- systemd-resolved 257 can't browse DNS-SD: a PTR query through it returns
  only the first device that answers (tested on a Dot, 2026-11-16). It does
  answer one-shot mDNS queries sent from any port, by unicast (RFC 6762,
  section 6.7; `resolved-mdns.c`).
- The dashboard talks only to its own satellite: cross-origin requests are
  blocked on purpose (ADR 0005). Switching therefore means opening the other
  satellite's own dashboard, not driving it from this one.

## Decision

- **One dashboard address.** The Network page's "Other names" list becomes one
  optional setting, the dashboard address: `url` in `settings.toml`, for
  example `https://dot.example.com/`.
  - It is `http` or `https`, a host and an optional port, path `/`. No user,
    query or fragment, and no host other satellites can't reach: loopback,
    the USB link's 10.42.0.x, link-local IPv6. Stored normalised to end in
    `/`.
  - The Host allowlist becomes: IP addresses, `localhost`, every `.local`
    name, the hostname, the dashboard address's host, and `[web]
    allowed_hosts` (developers).
  - Not now: also accepting the hostname under the router's DHCP domain
    (`kitchen.fritz.box`). systemd-networkd only exposes that domain with
    `UseDomains=` on, which changes name resolution; the lease file is private.
  - `allowed_hosts` in `settings.toml` (only builds from 2026-11-16 have it)
    is no longer read. The key stays in the file, so a rollback keeps it.
  - No Home Assistant entity: like the host names before, it guards the
    dashboard, not the satellite.
- **The ESPHome record carries the dashboard.** pinnad adds `web_port=<port>`
  and, when set, `url=<dashboard address>` to the `_esphomelib._tcp` TXT
  record. Home Assistant ignores keys it doesn't know. With the web listener
  off, `web_port` is left out.
- **pinnad asks when the list opens.** A unary RPC, `ListSatellites`, sends
  a one-shot mDNS query for `_esphomelib._tcp.local` from its own UDP port and
  collects answers for a second. For instances whose SRV, TXT or address
  records didn't come with the answers, it asks once more and waits another
  half second. It keeps those with
  `project_name=pinna.satellite`, leaving out itself (by MAC). pinnad never
  binds port 5353: resolved stays the only mDNS socket. Only satellites that
  are on answer, so there is nothing to expire. Satellites on firmware without
  `web_port` are listed with port 80.
- **The sidebar's device box opens the list.** On phones, the name in the app
  bar does. The list shows this satellite first, marked as current, then the
  others: drawing, friendly name, and where the link goes (host). Choosing one
  opens that satellite's dashboard in the same tab.
  - The link is the other satellite's dashboard address when it has one.
  - Otherwise: if this page was opened by a `.local` name, the other's
    `.local` name. Opened any other way, the other's IP address of the same
    family: IPv4, or a global or unique-local IPv6 address in brackets, never
    a link-local one. Someone who opened this page by IP or by a router name
    may not resolve `.local` (many Android browsers can't). Without an address
    of that family, the `.local` name is still better than no link.
  - Never 10.42.0.1: every satellite has that address on its USB link.
  - The list states: looking (while the query runs); no other satellites on
    this network; this satellite isn't on a network yet (USB only); the query
    failed (the error, this satellite still shown).
  - A satellite whose dashboard address is set but doesn't open: the list
    shows its `.local` name and IP address under the link as a way around.

## Consequences

- An owner with several satellites moves between them from any one of them.
- A satellite on older firmware is listed but has no list of its own, so the
  switch back is by hand.
- Only satellites on the same subnet are listed: resolved answers a one-shot
  query only from its own subnet, so satellites on different VLANs behind an
  mDNS reflector don't see each other.
- The Network page shows one address instead of a list of names. A second
  custom name, such as the router's name next to a reverse proxy's, needs
  `[web] allowed_hosts`, a developer setting.
- Anyone on the LAN can announce a fake satellite whose link goes elsewhere;
  the list shows each link's host. Anyone on the LAN can already control the
  satellites (ADR 0005), so this widens nothing that matters while there is no
  authentication.
- pinnad gets a small mDNS client: one-shot queries only, no responder, no
  background traffic.
- To check when building: switching from an `https` dashboard address to an
  `http` `.local` one in Chrome, which now upgrades to HTTPS by default.
