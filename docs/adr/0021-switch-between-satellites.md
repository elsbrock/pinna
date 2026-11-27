# ADR 0021: Switch between satellites, and one dashboard address

Status: accepted (2026-11-16), amended 2026-11-17: pinnad looks in the
background, and lists a satellite only when its dashboard answers;
asks over IPv4 and IPv6, and takes the `.local` name from the SRV record
(with ADR 0024); amended 2026-11-27: each query goes out three times, a
satellite the search misses stays listed while its dashboard answers, and the
list opens out of the device box with each satellite's drawing and name only.
Amends ADR
0005 and ADR 0007 (the host allowlist and the dashboard's `allowed_hosts`).
Builds on ADR 0010 (hostname).

## Context

- A household with several satellites opens each dashboard by hand: its
  `.local` name, its IP address or a name the owner set up. Home Assistant's
  device page links to each one ("Visit", ADR 0005), but only from Home
  Assistant, which may become optional, and not from one dashboard to the next
  when updating several satellites in one sitting.
- Every satellite announces itself over mDNS as `_esphomelib._tcp`, named by
  the host name resolved publishes (`%H`, ADR 0024), which is its node name
  unless resolved renamed it after a conflict, with TXT
  `project_name=pinna.satellite`. That record carries
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
- **pinnad looks in the background.** Once at start and then every minute,
  pinnad sends a one-shot mDNS query for `_esphomelib._tcp.local` from its own
  UDP ports: to 224.0.0.251, out of the interface the kernel picks, and
  to ff02::fb on each interface besides the USB link that is up and has an
  address (link-local IPv6 is enough). It collects answers for 1.2 seconds.
  Records heard over either family are merged: on 2026-11-17 an Echo (2nd
  gen) whose resolved was in the state ADR 0024 describes answered PTR
  queries over IPv6 but not over IPv4. For instances whose SRV, TXT or
  address records didn't come with the answers, it asks again for what is
  still missing, to the same addresses, up to three times, a second each:
  a responder may answer only the question asked, so the addresses come
  only after the SRV record names the host. resolved does: it answers the
  PTR question with the PTR record alone, so a search takes three rounds.
  **Every query goes out three times, 300 ms apart**, each time with a new
  id, and the answers to all three are merged. On 2026-11-27, an Echo Dot
  (2nd gen) heard the Echo Dot (5th gen)'s PTR answer to 8 of 15 single
  queries, and to 14 of 15 queries sent three times; resolved answers each
  copy. With one copy per round, that satellite was listed after about one
  search in six, and the Echo Dot (2nd gen) didn't list it for minutes on
  end. It keeps those with
  `project_name=pinna.satellite`, leaving out itself (by MAC), and lists
  each MAC once: a reload of resolved could leave an old instance of the
  same satellite published (ADR 0024). Of those it keeps the one whose host
  has addresses, then the one named like its host, else the first heard;
  entries without a MAC aren't merged. pinnad never binds port 5353:
  resolved stays the only mDNS socket. A satellite stays listed until it
  misses two searches in a row, so one lost packet doesn't drop it. After
  that, it stays listed while its dashboard still accepts a connection at
  the addresses it was last listed with (checked as below), until it has
  missed ten searches in a row: a satellite that is on but whose answers
  keep getting lost isn't dropped, and one whose address passed to another
  device with a web server leaves after about ten minutes. A failed
  search counts as a miss for every satellite, so the list doesn't freeze
  while searches fail. A search fails only when no query went out on either
  family; a family whose socket won't open or whose send fails is skipped. Satellites
  on firmware without `web_port` are listed with port 80.
  - While the satellite has no address beyond the USB link, pinnad sends
    nothing and checks for one every ten seconds: pinnad starts before Wi-Fi
    is up.
  - A unary RPC, `ListSatellites`, returns what the last search found, at
    once. Opening the list doesn't send a query: the one-second wait made the
    list slow to open.
- **pinnad checks that each dashboard answers.** In the same search, pinnad
  opens a TCP connection to each satellite's web port at its host name's
  addresses: the first IPv4 and the first IPv6 address it heard, other than
  the USB link's and link-local ones, three seconds each. A satellite whose
  dashboard doesn't accept the connection counts as missed: its web listener
  is off, or pinnad isn't running there. So does one with no address heard.
  The dashboard address isn't checked. It may lead through a reverse proxy, a
  login or a VPN that the satellite can't pass, and the browser opens it, not
  the satellite.
- **The sidebar's device box opens into the list.** The box keeps its place
  and width and grows upward: the other satellites stack above this
  satellite's row, which stays where the box was. On phones, the name in the
  app bar opens the list below it, with this satellite first, marked as
  current. Each other satellite shows its drawing and friendly name, nothing
  else. Choosing one opens that satellite's dashboard in the same tab. The
  browser's status bar shows where a link goes when the pointer is on it.
  Amended 2026-11-27: the list was a separate box above the device box, and
  each entry also named the link's host, the group mark of ADR 0029 and the
  ways around below; the owner found it too much text to read.
  - The link is the other satellite's dashboard address when it has one.
  - Otherwise: if this page was opened by a `.local` name, the other's
    `.local` name: the host its SRV record names, not its instance name,
    since resolved may have renamed either after a conflict (ADR 0024). Opened any other way, the other's IP address of the same
    family: IPv4, or a global or unique-local IPv6 address in brackets, never
    a link-local one. Someone who opened this page by IP or by a router name
    may not resolve `.local` (many Android browsers can't). Without an address
    of that family, the `.local` name is still better than no link.
  - Never 10.42.0.1: every satellite has that address on its USB link.
  - The list states: looking (before the first search has finished); no
    other satellites on this network; this satellite isn't on a network yet
    (USB only); the search failed (the error, this satellite still shown).
  - A satellite whose dashboard address is set but doesn't open has no way
    around in the list (amended 2026-11-27; it showed the `.local` name and
    IP address under the link): the owner opens one of those by hand.

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
- ADR 0029 marked the members of the satellite's group in the list, and said
  that the other entries were only what the network announced. Since
  2026-11-27 the list marks neither (ADR 0029).
- Anyone on the LAN can announce a fake satellite whose link goes elsewhere.
  The list doesn't show the link's host; the browser's status bar and then
  its address bar do. Anyone on the LAN can already control the
  satellites (ADR 0005), so this widens nothing that matters while there is no
  authentication.
- pinnad gets a small mDNS client: one-shot queries only, no responder. Each
  satellite searches once a minute (a query per family and link, then up to
  three follow-ups to the same addresses, each sent three times), and opens
  a connection a minute to each other satellite's dashboard. A search's
  queries take up to 4.2 seconds. Its dashboard checks run at once for every
  satellite and take up to 6 seconds, then up to 6 more for those the
  queries missed.
- A satellite that comes on, leaves or changes its address shows the change
  within a minute; one that goes off stays listed for up to two.
- To check when building: switching from an `https` dashboard address to an
  `http` `.local` one in Chrome, which now upgrades to HTTPS by default.
