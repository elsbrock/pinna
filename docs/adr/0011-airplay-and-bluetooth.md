# ADR 0011: AirPlay and Bluetooth playback feed pinnad's media lane

Status: proposed (2026-11-15).

Follows the speaker-ownership decision of
[ADR 0004](0004-audio-graph.md) and the kernel decisions of
[ADR 0006](0006-kernel.md).

## Context

Pinna should accept audio from phones and computers the way a speaker does: as
an **AirPlay 2 receiver** (iPhones, Macs, multi-room groups) and a **Bluetooth
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
  functions.
- **systemd-resolved owns mDNS today.** It advertises `pinna.local`, and
  `pinnad` publishes `_esphomelib._tcp` for Home Assistant discovery through
  `/run/systemd/dnssd` (`pinnad::identity`, `pinna_esphome::mdns_txt`).
  Debian's shairport-sync depends on `avahi-daemon`, and two mDNS responders
  cannot share port 5353.
- **AirPlay 2 is not in trixie.** trixie's shairport-sync 4.3.7 is built
  without AirPlay 2 (the binary has no nqptp support); `nqptp` is not in
  trixie or trixie-backports. forky ships shairport-sync 5.6.2 + nqptp
  1.2.9, but the forky binaries need newer libc, libconfig and FFmpeg sonames
  (libconfig11 vs 15, libavcodec61 vs 63) that trixie does not have.
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
- **AirPlay 1 only, from the trixie package.** No build work, but no
  multi-room and an older sender path; the owner wants AirPlay 2 from the
  first cut.
- **Forky binaries in a trixie image.** Binary-incompatible (see above).
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
   (`/run/pinna/stream.sock`, mode 0660; raw s16le, 48 kHz stereo, written
   in real time). A connection is one session. Its first 20 ms of data start
   the session (Home Assistant shows `playing`) and replace whatever the lane
   was playing, another connection included; a client that connects and
   sends nothing changes nothing. EOF or a read error ends the session, the
   last audio faded out, and so do 2 s without data: senders pause by going
   quiet, not by closing (shairport-sync never closes its output between
   sessions). `media stop` from Home Assistant closes the connection, and so
   does `media pause` until a later step can pause the sender itself. Once
   the ingest has closed a connection, the client reconnects only after its
   source has gone quiet and starts again — otherwise a stop would undo
   itself. Ducking, volume, mute, the media state and the echo reference
   stay where they are, so they apply to these sources unchanged.

   Live sources run on the sender's clock, the speaker on the Dot's crystal.
   Until the ingest adapts its rate to the speaker clock, it bounds its
   queue: playback starts once 100 ms are queued; when the queue runs dry it
   fades out and waits for 100 ms again; once more than ~250 ms are queued
   it drops the oldest audio back to 100 ms. Neither drift nor the backlog
   while the speaker opens can grow the latency. Rate adaptation — a
   stable, known latency, which AirPlay 2 grouped playback needs — comes
   before AirPlay ships.
2. **AirPlay 2 from two pinned rebuilds.** Pinna rebuilds the trixie
   shairport-sync source package with `--with-airplay-2` (plus the nqptp
   build/runtime dependency) and builds nqptp from pinned source; both
   install into the root image like the kernel `.deb` (a `just` task, sha256
   pins in the repository, assembled by `assemble-rootfs-extra.sh`). Both
   builds are carried only until Debian provides AirPlay 2 packages for
   trixie. shairport-sync plays to the ingest through its `stdout` output
   (set to S16_LE at 48 kHz; AirPlay 2 builds default to S32_LE) and a small
   connector that opens one connection per session. The sender's volume maps
   onto a new media-lane gain (today the volume scales the whole mix, replies
   included) and mirrors into the Home Assistant media player volume while a
   session is active, with the front-panel volume on top as today.
3. **Avahi becomes the one mDNS/DNS-SD stack.** `avahi-daemon` and
   `libnss-mdns` come in with shairport-sync; systemd-resolved turns
   `MulticastDNS=off`; Avahi advertises `pinna.local`, and `pinnad`'s
   `_esphomelib._tcp` publication moves from resolved's `/run/systemd/dnssd`
   to Avahi registration. `nqptp` runs as its own service for AirPlay 2
   timing (UDP 319/320, multicast 224.0.1.129).
4. **Bluetooth: after Wi-Fi, one attempt, failure tolerated.** `wifi-up`
   gains the single BT activation step after Wi-Fi is up; if it fails, the
   device stays a voice satellite. BlueZ and BlueALSA run as services; a
   small `pinna-bt` bridge feeds the BlueALSA PCM to the ingest and watches
   `org.bluez.MediaPlayer1` for status and play/pause (A2DP + AVRCP; no
   HFP/SCO).
5. **Pairing is a window, not a mode.** The adapter is not discoverable by
   default. The dashboard — through the same control API that everything
   else uses — starts a timed pairing window: discoverable and pairable, an
   auto-accept agent, and the ring shows it. Paired keys live in
   `/var/lib/bluetooth` on `userdata` and survive updates. AirPlay stays
   open by default; a password can be configured.
6. **One policy for sources.** Starting a source replaces whatever the media
   lane was playing and reports through the existing media state; a source
   that ends releases the lane. The lane never cuts into a signal: whatever
   it stops mid-signal (replaced, stopped, a stream paused) fades out over
   20 ms before its stream closes, and whatever it joins mid-signal (a
   stream, a resume) fades in. A URL starts and ends as recorded. Replies,
   announcements and timers always win, through the existing ducking.

## Consequences

- The wake word keeps working while AirPlay or Bluetooth plays, through the
  same echo cancellation as Home Assistant media — including the documented
  first seconds before WebRTC converges.
- The image gains shairport-sync, nqptp, BlueZ, BlueALSA and Avahi;
  persistent state stays in `/var`, the read-only root is unchanged.
- The WMT one-shot sequence gains a second function. Bluetooth failure must
  never block networking; Wi-Fi goes first. Only hardware can prove it
  (`just test-device`); the VM cannot.
- AirPlay 2 grouped playback needs PTP multicast to reach the device; APs
  that filter multicast make it fail in ways the dashboard should surface.
  It also needs the ingest's rate adaptation (decision 1): with only the
  bounded queue, the Dot's latency varies by up to ~250 ms against the rest
  of a group.
- Home Assistant discovery now depends on Avahi; the VM boot gate and the
  aioesphomeapi tests cover what can be covered without a device.
- Scope: the device now does more than being a voice satellite. When this
  lands, `goals.md` and `status.md` get a short "media sources" wording
  rather than leaving the goals silent.
