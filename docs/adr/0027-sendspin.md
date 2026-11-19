# ADR 0027: Sendspin plays on the server's timeline through pinnad's media lane

Status: proposed (2026-11-19). Experimental while the specification is a
draft: the dashboard and the docs say so. Follows ADR 0011 (media sources and the
media lane), ADR 0004 (pinnad owns the speaker) and ADR 0024 (mDNS records).
Targets the Sendspin specification 1.0.0-rc1 and the player timing rules
added on its main branch up to 2026-11-11 (commit 9d2f15e).

## Context

- Music Assistant plays to its own players over Sendspin, an open protocol
  for synchronized multi-room audio. Music Assistant 2.10.7 and later speak
  1.0.0-rc1; earlier releases speak an older, unencrypted wire format that
  this record does not cover. The specification is still a draft: about 40
  normative changes landed after rc1, many of them on player timing.
- ADR 0011 left Sendspin to its own record. AirPlay and Bluetooth feed a
  stream socket whose latency is kept steady but is only estimated. A
  Sendspin player has to sound every sample at the time the server sets, in
  step with the other speakers of its group: in steady state (from 10 s
  after a new connection, a start from an empty buffer, a full clear, an
  underrun or a filter jump) its error must stay within ±1 ms, its
  corrections must be inaudible, and its speed must stay within ±0.5 %. It
  should correct an error gently whenever that brings it back within 1 ms in
  10 s, rather than jump.
- What the protocol asks of a player:
  - **Connection.** In the recommended direction the player listens
    (port 8928, path `/sendspin`, plain `ws://`), advertises
    `_sendspin._tcp` over mDNS, and the server connects to it. A player that
    advertises must not dial servers itself. It must refuse an upgrade
    request that carries an `Origin` header.
  - **Encryption.** Every connection, paired or not, runs
    `Noise_KKpsk2_25519_{ChaChaPoly|AESGCM}_SHA256` with the server as
    initiator. The player's identity (`client_id`) is a persistent X25519
    public key. Which pre-shared key the server uses is named in its first
    handshake message; a server with no pairing uses a published Sentinel
    key. The server may run the handshake again in-band.
  - **Pairing.** A player must implement the Pairing PSK method: a secret of
    its own that an operator hands to the server as a token, after which the
    two hold a long-term key. It must keep at least five such pairings and
    accept a server's unpairing. Without a pairing, a player that allows
    "unpaired access" can still be told to play; Music Assistant trusts such
    a player automatically. An unpaired session can be intercepted by anyone
    on the network who can sit between the two.
  - **Time.** The player measures the server's clock in bursts of at least
    eight time messages, at most 10 s apart, through a two-dimensional
    Kalman filter (offset and drift) that the specification defines with
    its parameters.
  - **Audio.** Chunks of 15 to 150 ms carry the server time of their first
    sample. Servers must be able to send FLAC and PCM, 16 or 24 bit, mono or
    stereo, 44.1 or 48 kHz; Opus is optional. The player limits how much the
    server sends ahead (`buffer_capacity`) and reports the lead it needs to
    start (`required_lead_time_ms`) and the jitter it needs to cover
    (`min_buffer_ms`). It compensates the latency of its own output path
    itself.
- What Pinna has:
  - **The speaker's timing is already measured.** The speaker session maps
    every frame it writes to the moment it sounds (`Session::time_of`), and
    paces itself at the speaker's measured rate (`PcmClock`, ADR 0004). The
    rate comes from the playback PCM's hardware pointer and the driver's
    timestamp of it (`hw_ptr`, `tstamp` in `/proc/asound/.../status`, on
    the monotonic clock). The start of the mapping is the moment the PCM was
    seen running, so its offset is known only to the polling interval.
  - The speaker chain's limiter delays the output by a fixed 64 frames
    (1.3 ms). The path after the hardware pointer (I2S, codec, amplifier)
    adds a small fixed delay that has not been measured.
  - The media lane already resamples a live source with a variable ratio
    (`VarResampler`) and applies a stream's own volume.
  - Everything the protocol needs is in the build already: WebSockets
    (axum), Noise with X25519 and ChaChaPoly (`snow`, used for the ESPHome
    API), SHA-256 and FLAC decoding (symphonia).
- Rejected:
  - **A bridge process into the stream socket**, like AirPlay: the socket
    hides when a sample will sound, so the player can't keep ±1 ms.
  - **An existing Rust implementation.** The `sendspin` crate speaks the
    pre-rc1 wire format and brings its own audio output (cpal). `syncspin`
    follows rc1 but was four days old with one author, and leaves the
    timing to its caller, which is the hard part here.
  - **Opus.** It would need a C library or a new decoder, and every server
    must offer FLAC.
  - **Dialing the server** (`_sendspin-server._tcp`). Both directions are
    allowed, one at a time; listening is the recommended one and matches how
    AirPlay is found. Music Assistant can also be given the satellite's
    address by hand.
  - **The older unencrypted wire format.** Music Assistant still accepts it,
    marked non-compliant, and may drop it at any time.
  - **Grouping through AirPlay.** Music Assistant can wrap AirPlay players
    in Sendspin groups, so a satellite with AirPlay on may already join
    them. It stays in step only as well as AirPlay's latency estimate
    (0.34 s, ADR 0011, not measured), with no way to correct the estimate,
    and volume flows only one way.

## Decision

### Where it runs

1. **The player is part of `pinnad`.** A new library crate,
   `pinna-sendspin`, holds the protocol: messages and their range checks,
   binary frames and the reassembly of fragments, the Noise responder,
   the Kalman filter, and pairing records. It does no audio. `pinnad`'s
   `sendspin` module runs the connection and feeds the media lane through an
   in-process interface, not a socket.

### Finding and connecting

2. **A switch, off by default.** "Sendspin" sits next to AirPlay and
   Bluetooth on the dashboard's Media sources page and is a Home Assistant
   switch entity (ADR 0011 §5). While it is on, `pinnad` listens on TCP
   8928 at `/sendspin` and publishes a `_sendspin._tcp` record (TXT
   `path=/sendspin` and `name=` the satellite's name) through
   `/run/systemd/dnssd/pinna-sendspin.dnssd`, written and removed as
   ADR 0024 says. Switched off, the record goes first, then the connection
   ends with `client/goodbye` (`shutdown`) and the listener closes. A
   rename re-publishes the record and ends the connection with `restart`,
   so the server reconnects and reads the new name. A device without a
   speaker refuses the switch. `Status` reports Off, Ready (listening),
   Connected (with the server's name), Playing, Failed (with the reason,
   such as the port being taken) or Unavailable, and whether this speaker
   can play in exact step (§12). A server that opens with the older
   unencrypted handshake is answered with a close, and the status says that
   Music Assistant must be 2.10.7 or later.
3. **One connection at a time.** A second server is admitted or turned away
   by the specification's rules for several servers (playback ranks above
   pairing, pairing above nothing); the server that last played is
   remembered across restarts.

### Security

4. **Unpaired access is on.** The player advertises
   `unpaired_access.enabled`, so Music Assistant plays to it with no setup.
   Anyone on the network can then play to the satellite, as with AirPlay
   (ADR 0011 §5), and someone who can intercept its traffic can read or
   change the audio. The dashboard and the docs say so.
5. **Pairing PSK is built, not yet offered.** The player keeps its own
   pairing key, accepts the pairing flow, stores up to eight pairings
   (evicting the oldest that no open connection uses, provisional or
   admitted; fewer than eight paired connections are open at once, so one
   can always go), and accepts unpairing. It lists `pairing_psk`
   without `locations`: nothing gives the token to an owner yet. Handing it
   to Music Assistant through Home Assistant, which can ask an ESPHome
   device for it over an encrypted API connection, is planned.
6. **Bounded input.** Everything a server sends is parsed in `pinnad`,
   which also holds the microphone and, in standalone mode, the owner's
   keys. So the player enforces its own `buffer_capacity` and closes a
   connection that exceeds it; it reassembles at most one fragmented
   message, up to that capacity; it drops audio timestamped more than 30 s
   ahead; it takes JSON only within the specification's limits (16 KiB,
   depth 10); a handshake step that takes longer than 30 s, or a
   connection that is not activated within 30 s, is closed; and so is a
   server whose clock the filter finds drifting more than 1 % from the
   player's, twice what the lane follows (§11). The server's time replies
   alone set the filter's drift, and at -100 % the conversion to local time
   runs backward, so that audio due later counts as already played.
7. **What is kept, and where.** The X25519 identity, the pairing key, the
   pairings, the last server, and the volume and mute live in
   `/var/lib/pinna/sendspin.toml`, mode 0600, written atomically. A factory
   reset (ADR 0010) removes them; the next start creates a new identity.
   Only ChaChaPoly is offered: the processor has no AES instructions.

### Playing on time

8. **Roles and formats.** `player@v1`, `controller@v1` (for pause and
   play, §14) and `metadata@v1`. The player lists
   FLAC, then PCM, each 16-bit stereo at 48 kHz, then at 44.1 kHz, and
   prefers 48 kHz FLAC: half the airtime of PCM on a radio shared with
   Bluetooth (ADR 0011). Each chunk is decoded when it arrives; 44.1 kHz
   is converted to 48 kHz. `buffer_capacity` is 1 MiB (about 5 s of PCM,
   10 s of FLAC).
9. **Server time to local time.** Bursts of eight time messages every 9 s
   (the next request goes out after 500 ms without a reply) feed the
   specification's filter, unchanged. The specification allows at most 10 s
   between the starts of two bursts; the second to spare covers a timer that
   fires late. The first `client/state` waits for
   the first burst, as the specification requires. Local time is the
   monotonic clock, the one the PCM's timestamps use.
10. **When a frame sounds.** The speaker session corrects the offset of its
   frame-to-time mapping, not only its rate, from the hardware pointer and
   its timestamp. The time a frame is heard is that mapping, plus the
   limiter's 64 frames, plus a fixed delay per model for the path after the
   pointer, measured on each device and recorded with its evidence. Until
   measured it is 0.
11. **A timed source in the media lane.** For each 20 ms chunk it mixes, the
    speaker tells the lane when the chunk's first frame will be heard. The
    Sendspin source reads its decoded audio from the matching server time:
    - **Drift** is followed by the resampler: its ratio is the product of
      the server clock's drift (from the filter) and the speaker clock's
      (from the pointer), and a proportional correction on the remaining
      error, within a dead band of 0.1 ms. The speed never leaves ±0.5 %.
    - **Jumps.** At the start, after a full clear, after an underrun, or
      when the error is too large to correct within 10 s at that speed, the
      source starts at the right sample: it skips audio or waits in
      silence, and fades in over 20 ms.
    - **Late audio** (a chunk whose end has already been heard) is dropped.
    - `stream/clear` with `from_timestamp` drops only queued audio from that
      time on; without it, everything queued. `stream/end` fades out and
      empties the queue. A format change in place applies from its first
      chunk.
    - The audio already handed to `aplay` can't be recalled, so
      `required_lead_time_ms` covers it: the speaker's 100 ms lead, aplay's
      128 ms buffer and decoding, 300 ms in all until measured.
      `min_buffer_ms` is the 99th percentile of how late chunks arrive over
      the last minute, reported only when it changes by more than 20 ms.
    - The sync error and the filter's error are metrics (ADR 0020), so a
      device can show that it keeps ±1 ms.
12. **Other backends.** On PipeWire (the virtual Echo Dot, laptops) there is
    no hardware pointer to read: the source plays at the pacing clock, in
    time to within the PipeWire graph's latency, not to 1 ms. The status
    and the docs say so.

### Sharing the lane

13. **The lane's rules hold (ADR 0011 §6).** A Sendspin stream replaces
    whatever the lane plays once its first audio is due, and fades in.
    Replies, announcements and timers duck it, and its timeline runs on
    underneath: when the duck lifts, it plays from where the group is.
    The media state names the source `sendspin`; the dashboard shows "From
    Music Assistant" and the metadata role's title, artist and album.
14. **Pause and play act on the group.** Pause, play and stop from Home
    Assistant's media player, a voice request or the dashboard, while the
    satellite is in a Sendspin group, go to the server as controller
    commands, so the whole group pauses, plays or stops, as grouped
    speakers and Music Assistant's own controls do. The server ends the
    stream (`stream/end`), and the media state shows Paused with the
    source `sendspin` until the group plays again or the connection ends;
    Play then sends `play`, which resumes the group's last media. Another
    source taking the lane (a URL, AirPlay, Bluetooth) makes the satellite
    leave its group instead (`client/leave`): the rest of the group plays
    on, and Music Assistant can add it back. A role the server removes, a
    `stream/end` or a lost connection stops it with a fade.
15. **Volume.** The server's volume and mute set the Sendspin stream's own
    level, with the specification's curve (`(v/100)^1.5`), ramped, and are
    kept across restarts. While Sendspin plays, Home Assistant's media
    player volume and the dashboard's set the same level and are reported
    back in `client/state`, so Music Assistant's slider follows. The
    front-panel buttons set the satellite's overall volume, as for every
    source. The player does not offer `set_output_delay`: the speaker is
    built in.

## Consequences

- A satellite joins Music Assistant groups with other Sendspin speakers,
  and the wake word keeps working while they play, through the same echo
  cancellation (ADR 0011).
- Release gate, on the Echo Dot (2nd gen) and the Echo (2nd gen): a
  recording of a click track played by the satellite and a reference player
  (a computer running `sendspin-cli`) in one group, cross-correlated, shows
  them within 2 ms of each other in steady state, the reference's own
  output latency allowed for. The per-model delay of §10 is measured the
  same way. The metric only shows that the satellite follows its own
  model; it can't see a wrong constant. ADR 0011's hours-of-music wake word
  test passes with Sendspin as the source.
- Other members of a group that play in the same room are not in the echo
  canceller's reference: the satellite hears them as it hears any other
  speaker in the room. The wake word and a request with a second member
  playing nearby are part of the gate; the docs say what to expect.
- No new dependencies. The size of `pinnad` and of the root image before and
  after is recorded when the code lands.
- A test suite runs Music Assistant's own server library (`aiosendspin`),
  at the versions in the oldest and the newest Music Assistant release that
  speak rc1, against a `pinnad` on the developer's computer: handshake,
  PCM and FLAC played sample for sample, volume both ways, metadata, pause
  and play through the controller, leaving, and reconnecting
  (`just sendspin-conformance`). The docs claim only the versions it
  passes with.
- With AirPlay on as well, Music Assistant may list the satellite more than
  once (AirPlay, AirPlay wrapped in Sendspin, and Sendspin). The docs say
  which one to use.
- The specification is a draft. A change to the wire or to the timing rules
  means another look at this record; Music Assistant older than 2.10.7 can't
  use the satellite.
- Unpaired access stays on until pairing through Home Assistant is built.
- Not built: pairing through Home Assistant, the buttons as group controls,
  Opus, 24-bit formats, and `set_output_delay`.
