# ADR 0011: AirPlay and Bluetooth playback feed pinnad's media lane

Status: proposed (2026-11-15), accepted (2026-11-16), amended 2026-11-17:
the dashboard also sets the media volume and mutes the music (`ControlMedia`,
**Now playing** on Overview), replies have their own volume (`reply_volume`),
so the volume no longer has to scale replies with the music, and track
metadata is not shown anywhere yet: the dashboard says only "From AirPlay or
Bluetooth". AirPlay's 0.34 s pipeline latency is still an estimate.

Follows the speaker-ownership decision of
[ADR 0004](0004-audio-graph.md) and the kernel decisions of
[ADR 0006](0006-kernel.md).

## Context

Pinna should accept audio from phones and computers the way a speaker does: as
an **AirPlay receiver** (iPhones, Macs, Music Assistant) and a **Bluetooth
A2DP sink** (any phone). Receive-only: playing to an external speaker or
headset stays out of scope.

What the repository has today, and what it does not:

- **The speaker has one owner.** `pinnad` drives it through ALSA at 48 kHz
  (the `EchoDotPipeWire` backend), "so that no other program can switch the
  amplifier on or off at the wrong moment", and every chunk it plays is also
  sent as 16 kHz mono to the echo canceller's reference sink
  (`pinna_audio::refsink`). The wake word depends on that reference: without
  it, the device hears its own music.
- **Bluetooth is configured but off, and unproven.** The kernel has the
  CONSYS vendor driver with BlueZ HCI hooks (`CONFIG_MTK_MT8163_BLUEZ_HCI`,
  BR/EDR, RFCOMM, no SCO), the Wi-Fi/BT firmware ships in `firmware/`, and
  `wmt_bt_on` — a single pinned ioctl behind an explicit
  `--execute-bt-only-once` gate — is built into the image. But `wifi-up`
  powers on only the Wi-Fi function, and no BlueZ package is in the image.
  The chip tolerates one activation attempt per boot, shared by both
  functions. `pinna-wifi.service` runs before `network-pre.target`, so
  networking, `pinnad` and the boot deadline wait for everything `wifi-up`
  does. The kernel's HCI bridge also switches the Bluetooth function on
  itself when BlueZ powers the adapter.
- **systemd-resolved owns mDNS today.** It advertises the hostname
  (`pinna-2a914e.local`, ADR 0010), and `pinnad` publishes `_esphomelib._tcp`
  for Home Assistant discovery through `/run/systemd/dnssd`
  (`pinnad::identity`, `pinna_esphome::mdns_txt`). Debian's shairport-sync
  depends on `avahi-daemon`, and two mDNS responders cannot share port 5353.
  shairport-sync connects to Avahi without failing when it is absent
  (`AVAHI_CLIENT_NO_FAIL`); it then runs but never advertises. Its AirPlay 1
  record is fixed: `<hardware address>@<name>` on `_raop._tcp`, with a
  constant TXT list.
- **AirPlay 2 is not in trixie.** trixie's shairport-sync 4.3.7 is built
  without AirPlay 2 (the binary has no nqptp support); `nqptp` is not in
  trixie or trixie-backports. forky ships shairport-sync 5.6.2 + nqptp
  1.2.9, but the forky binaries need newer libc, libconfig and FFmpeg sonames
  (libconfig11 vs 15, libavcodec61 vs 63) that trixie does not have.
- **What shairport-sync 4.3.7 can do.** Its `stdout` output has no settings:
  it always writes S16_LE at 44.1 kHz (the format and rate settings belong to
  the ALSA output). Its password works only for AirPlay 1. An AirPlay 2
  build talks to nqptp through shared memory, version 10, which nqptp 1.2.4
  to 1.2.8 provide; nqptp 1.2.9 moved to version 11, for shairport-sync 5.6.
- **What AirPlay 2 would cost.** It decodes through FFmpeg. On trixie armhf,
  on top of the image's packages, the AirPlay 2 runtime adds 100 packages,
  about 87 MiB installed (codec2, video codecs, librsvg, fonts), to a root of
  about 153 MiB; trixie's stock shairport-sync adds 10 packages, 2.6 MiB.
- **Bluetooth audio has a standard trixie stack:** BlueZ 5.82 and BlueALSA
  4.3.1, both armhf.
- The ESPHome media player carries state, volume and mute only; the protocol
  has no title/artist fields.

What was considered and rejected:

- **Receivers play directly to ALSA or PipeWire** (shairport-sync, bluealsa,
  or PipeWire's bluez nodes). The echo reference would be silence — the wake
  word would fire on the music — and the amp sequencing would leave `pinnad`.
  That is the setup ADR 0004 forbids; music through the 16 kHz PipeWire graph
  was rejected there as well (8 kHz bandwidth).
- **AirPlay 2 from rebuilt packages.** Rebuilding trixie's shairport-sync
  with `--with-airplay-2` and building nqptp 1.2.8, in a pinned trixie
  container. What it adds over AirPlay 1 is grouping with Apple speakers
  from an iPhone or Mac. What it costs is two packages to carry and either
  Debian's FFmpeg (about 87 MiB, see above) or a third rebuilt package, a
  minimal FFmpeg with only the AAC and ALAC decoders, to keep patched.
  Music Assistant gains nothing from it: it cannot play AirPlay 2 to
  shairport-sync receivers and falls back to AirPlay 1. Its own multi-room
  protocol, Sendspin, plays on a timeline the player must follow exactly,
  which needs the speaker's real timing inside `pinnad`, not this socket;
  it gets its own decision record.
- **Forky binaries in a trixie image.** Binary-incompatible (see above).
- **Avahi as the one mDNS stack.** shairport-sync would advertise itself,
  but Home Assistant discovery, `.local` lookups (nss-mdns) and the image's
  network files would all move for the sake of one record, and `/etc` being
  read-only needs a link to `/run` for Avahi's service files.
- **shairport-sync's `pipe` output straight into the ingest.** It only
  writes to a FIFO, opened non-blocking, which fails silently on a socket;
  it never closes between sessions and ignores a closed reader (EPIPE), so a
  FIFO ingest could not end a session either. Neither `pipe` nor `stdout`
  reports its delay, so shairport-sync cannot correct drift through them.
- **Sender volume ignored.** Phone volume buttons would do nothing.
- **Always-discoverable Bluetooth.** Any phone nearby could pair at any time,
  with no screen to show what is happening.
- **Track metadata in Home Assistant.** Not expressible in the ESPHome media
  player; metadata shows on the dashboard instead.

## Decision

1. **One ingest, `pinnad`'s media lane.** `pinnad` gains a PCM stream input
   next to the URL decoder in the media lane: a Unix socket under `/run`
   (`/run/pinna/stream.sock`, mode 0660, group `pinna-stream`). A client
   first writes one line, `s16le <rate> 2 [<source>]` (rate 44100 or 48000;
   source, the bridge's name: `airplay` or `bluetooth`, up to 16 of `a-z`,
   `0-9` and `-`), then raw stereo PCM in real time; any other first line
   closes the connection without touching the lane. A connection is one session. Its first 20 ms
   of data start the session (Home Assistant shows `playing`) and replace
   whatever the lane was playing, another connection included; a client that
   connects and sends nothing changes nothing. EOF or a read error ends the
   session, the last audio faded out, and so do 2 s without data: senders
   pause by going quiet, not by closing (shairport-sync never closes its
   output between sessions). `media stop` from Home Assistant closes the
   connection, and so does `media pause` until a later step can pause the
   sender itself. Once the ingest has closed a connection, the client
   reconnects only after its source has gone quiet (2 s without audio, or
   a pause, seek or end the sender announces) and starts again — otherwise
   a stop would undo itself. Ducking, volume, mute, the media
   state and the echo reference stay where they are, so they apply to these
   sources unchanged.

   Live sources run on the sender's clock, the speaker on the Dot's crystal.
   The ingest resamples each stream to the speaker's 48 kHz with a ratio that
   follows its queue: the queue holds 100 ms, so the latency is stable and
   known, which shairport-sync needs to play on the sender's time. The ratio
   stays within ±1000 ppm of nominal. The bounded queue stays as the safety
   net: playback starts once 100 ms are queued; when the queue runs dry it
   fades out and waits for 100 ms again; once more than ~250 ms are queued it
   drops the oldest audio back to 100 ms. Neither drift nor the backlog while
   the speaker opens can grow the latency. Rate adaptation comes before
   AirPlay ships.
2. **AirPlay 1 from trixie's shairport-sync.** The image installs the stock
   package, built without AirPlay 2: no rebuilt packages, no nqptp, 2.6 MiB.
   iPhones, Macs and Music Assistant play to one device at a time; grouping
   with Apple speakers is out of scope until Debian ships AirPlay 2 for the
   release Pinna uses. shairport-sync plays to the ingest through its `stdout`
   output (S16_LE at 44.1 kHz) and a small connector that opens one connection
   per session. shairport-sync writes early by the whole fixed path from its
   output to the speaker (the 100 ms queue, the speaker's lead and aplay's
   buffer: about 0.34 s, to be measured on the device before release). The
   sender's
   volume maps onto a new media-lane gain (today the volume scales the whole
   mix, replies included) and mirrors into the Home Assistant media player
   volume while a session is active, with the front-panel volume on top as
   today. Bridges set it through a `SetStreamVolume` call that `pinnad`
   accepts only from loopback, each before it connects and on every change; it
   is one value, kept across sessions, and a stream that fades out keeps its
   own. AirPlay's -30..0 dB map onto 1..100 and its mute onto 0. While a
   stream plays, Home Assistant's media player volume and its up/down set it
   too, until the bridge connects again with the sender's own volume; nothing
   tells the sender, so a phone's slider stays where it was.
3. **systemd-resolved stays the one mDNS responder.** `avahi-daemon`, which
   comes in with shairport-sync, is masked, so Home Assistant discovery and
   `.local` names stay as they are. `pinnad` publishes shairport-sync's
   `_raop._tcp` record itself, through a second `/run/systemd/dnssd` file
   like its `_esphomelib._tcp` one, while AirPlay is on (written and
   removed as ADR 0024 says: a changed or removed record restarts resolved
   instead of reloading it): the name carries
   the hardware address shairport-sync answers challenges with, and the TXT
   list is the stock build's (its `fv` is the package version). The record
   follows the trixie package; a new shairport-sync version means checking
   it again. Switched off, the record goes first and the bridge after, given
   3 s to stop, so senders stop listing the device at once. A device without
   a speaker or stream socket refuses the switch.
4. **Bluetooth: after Wi-Fi, only while switched on, failure tolerated.**
   `pinnad`, which starts after `pinna-wifi.service`, starts
   `pinna-bluetooth.target` (bluetoothd, BlueALSA as an A2DP sink, and
   `pinna-bt`, each `PartOf` it) while the switch is on and stops it when
   it goes off; neither udev nor D-Bus activation starts them. So Bluetooth
   never delays networking; if it fails, the device stays a voice
   satellite. Tested on the Echo Dot (2nd gen): powering the
   adapter through the management interface is the whole sequence (the kernel
   bridge's first open switches the Bluetooth function on itself; `wmt_bt_on`
   is not needed), it works long after boot, and off/on cycles don't upset the
   chip or the Wi-Fi. The controller reports a placeholder address
   (`00:00:46:81:63:01`); the device's own is idme's `bt_mac_addr`, which the
   kernel bridge sets with MediaTek's vendor command on every power-up. The
   controller stays in setup, unseen by bluetoothd, until the management
   interface powers it once after boot; bluetoothd's unit does that when it
   starts. The adapter's name is the satellite's name, as for AirPlay. The
   `pinna-bt` bridge (the D-Bus client; `pinnad` links no D-Bus library and
   only calls `busctl` to forget a phone) feeds the newest phone's BlueALSA PCM to the ingest at its own rate, and
   BlueALSA's native volume (`--a2dp-volume`) carries the phone's volume to
   `SetStreamVolume`, as AirPlay's does. A2DP only: no HFP/SCO, so no
   microphone ever leaves over Bluetooth, and no AVRCP control from Home
   Assistant yet (its stop and pause end the session on the device, as for
   AirPlay). Bluetooth shares the chip with the Wi-Fi that reaches the
   device, so it must never cost the network: `pinnad` writes a marker with
   the boot's ID (synced to disk; without it, Bluetooth doesn't start)
   before it starts Bluetooth, and removes it once the bridge has been
   ready for 30 s with `wlan0` up. A marker from an earlier boot means that
   start never got there, so Bluetooth stays off on this boot, and the
   dashboard says why until it is switched off and on; a marker from this
   boot only means `pinnad` restarted. Bluetooth runs wherever the
   kernel registers a controller. Tested with Pinna's units and bridge on
   the Echo (2nd gen) on 2026-11-16 (pairing in the window, A2DP at
   44.1 kHz, the phone's volume, reconnecting, forgetting, off and on, and
   a boot with it on); the Echo Dot (2nd gen) passed the same steps by hand.
5. **Pairing is a window, not a mode.** The adapter is not discoverable by
   default. The dashboard — through the same control API that everything
   else uses — starts a two-minute pairing window: discoverable and
   pairable, pairing without a code, and the ring shows it. `pinnad` keeps
   the window and `pinna-bt` applies it to the adapter, closing it at its
   time even if `pinnad` stops answering. The agent is NoInputNoOutput, so
   the kernel accepts a Just Works pairing without asking it: the gates are
   the adapter's pairable flag, and trust. `pinna-bt` trusts (and lists) only
   phones that bonded while the window was open; a phone that paired
   without bonding, which the kernel allows even when not pairable, stays
   untrusted, and the agent refuses its connections outside the window. A
   phone that bonds closes the window and reconnects later without asking.
   Paired keys live in `/var/lib/bluetooth` on `userdata` and survive
   updates. AirPlay and Bluetooth each have a switch, both off by default,
   on a "Media sources" page under Voice & sound in the dashboard and as
   Home Assistant switch entities ("Bluetooth audio", so it isn't taken for
   ESPHome's Bluetooth proxy), with the pairing window as a Home Assistant
   switch that turns itself off when the window closes; switching a source
   off ends its session and stops it from being found. Pinna sets
   no AirPlay password for now (shairport-sync would take one for AirPlay
   1), so while AirPlay is on, anyone on the network can play to the
   device; its units run sandboxed. The switch shows what is saved;
   `Status` shows what runs (off, starting, ready under its name, failed
   with systemd's reason, or unavailable without a speaker), and the page
   says which name to pick. AirPlay uses the satellite's name (ADR 0007), so
   a rename restarts the receiver under the new one.
6. **One policy for sources.** A source replaces whatever the media lane
   was playing once it has audio, and reports through the existing media
   state; a source that ends releases the lane. A URL that is still opening
   leaves the current source playing, and one that fails to open leaves it
   playing too: the media state reports the failure and the URL that failed
   beside what still plays, until the next play or stop. A stream has audio
   when it connects (its first 20 ms). The lane never cuts into a signal: whatever
   it stops mid-signal (replaced, stopped, a stream paused) fades out over
   20 ms before its stream closes, and whatever it joins mid-signal (a
   stream, a resume) fades in. A URL starts and ends as recorded. Replies,
   announcements and timers always win, through the existing ducking.
7. **What plays shows on the dashboard.** The media state carries the
   track as its source names it (title, artist, album, and an internet
   radio station's name) and the source of a stream. Now playing shows
   them; Home Assistant's media player can't (see above).
   - **Streams.** Each bridge sends its track through `SetStreamMetadata`
     under the name its header gives, loopback only like `SetStreamVolume`.
     `pinnad` keeps the latest track of each source and shows the one whose
     stream plays, so a sender that changes track while another source
     plays never shows on that session. A bridge sends its track before
     each session connects and on every change, so a pause that ends the
     session (2 s without audio) doesn't lose it. `pinna-airplay` takes
     `minm`, `asar` and `asal` from each bundle on shairport-sync's metadata
     pipe (`mdst` to `mden`) and forgets them when play ends (`pend`).
     `pinna-bt` reads BlueZ's `MediaPlayer1` `Track` (AVRCP) from the
     player that the phone whose audio it plays addresses (`MediaControl1`
     `Player`), or from that phone's first player; a phone that shares none
     shows none.
   - **URLs.** The lane reads a file's tags (ID3v2, Vorbis comments, FLAC,
     RIFF INFO). Every URL played as music asks for ICY metadata
     (`Icy-MetaData: 1`), which servers without it ignore. An internet radio
     server that sends it interleaves blocks every `icy-metaint` bytes: the
     lane removes them before decoding, and shows `StreamTitle` as the
     title, as sent (stations order artist and title differently, so it
     isn't split), and `icy-name` as the station. ICY text that isn't UTF-8
     is read as Latin-1. Only addresses played from the dashboard carry them:
     Home Assistant plays through its ffmpeg proxy, which strips tags and
     ICY metadata, so the dashboard says such media comes from Home
     Assistant instead of showing the proxy's address.
   - Each field is capped at 256 bytes. Cover art stays off: shairport-sync
     runs without it, and nothing else reads it.

## Consequences

- The wake word keeps working while AirPlay or Bluetooth plays, through the
  same echo cancellation as Home Assistant media — including the documented
  first seconds before WebRTC converges. Hours of music become normal, so
  a release gate counts false and missed wakes over hours of music on both
  devices; if music makes the satellite wake by itself or go deaf, these
  sources are not worth shipping.
- The image gains shairport-sync (with a masked Avahi), BlueZ and BlueALSA,
  all from trixie (about 12 MiB installed); persistent state stays in
  `/var`, the read-only root is unchanged.
- Wi-Fi and Bluetooth share one antenna on 2.4 GHz (the stock
  `coex_wmt_ant_mode=1`). Measured on the Dot while an iPhone streamed A2DP
  on 2.4 GHz Wi-Fi: about 30 ms more Wi-Fi latency and no loss for small
  traffic (a voice-sized stream arrives as without Bluetooth), but bulk
  transfers such as an update slow down and stall. Paused, the network is
  back at once. Lowering the volume would not help (the airtime is the
  same), so music is not paused for voice requests; the docs recommend
  5 GHz Wi-Fi, which the chip supports.
- No WMT step is added: the kernel bridge switches the Bluetooth function on
  when the adapter powers up. Bluetooth failure must never block
  networking; Wi-Fi goes first. Only hardware can prove it; the VM has no
  Bluetooth (the owner chose not to give it a virtual controller).
- No AirPlay grouping with Apple speakers. Multi-room with Music Assistant
  is Sendspin's job, decided in its own record.
- The dashboard has no authentication (ADR 0005): anyone on the network who can open
  it sees the title of what plays, from a phone over Bluetooth too, as
  they already see a URL that plays.
- Scope: the device now does more than being a voice satellite. When this
  lands, `goals.md` and `status.md` get a short "media sources" wording
  rather than leaving the goals silent.
