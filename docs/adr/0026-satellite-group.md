# ADR 0026: Satellites in a group pass messages, control each other and answer the wake word once

Status: proposed (2026-11-18). Builds on ADR 0008 (the Wi-Fi trial) and
ADR 0021 (finding the other satellites). Amends ADR 0005 (a second kind of
caller on the dashboard's listener), ADR 0017 (a key that leaves the device,
and the tools), ADR 0020 (metrics) and ADR 0024 (more changes that rewrite
the mDNS record).

## Context

- A household with several satellites in standalone mode runs each one on its
  own. Home Assistant reaches its own satellites, but it takes no part in a
  standalone request (ADR 0017).
- A brain can reach another satellite today only if the owner adds that
  satellite's `/mcp` as one of its MCP servers (ADR 0017): `/mcp` takes
  callers without an Origin header. That works for two satellites. With more
  it doesn't:
  - each pair is set up by hand, on both sides, and again when an address
    changes;
  - every satellite adds about 19 tools, named after the server, so four
    satellites pass the limit of 64;
  - nothing leaves a message for later, and the brain names servers, not
    rooms.
- Satellites in earshot of each other all answer the same wake word. Each
  runs its own request: two replies, and two bills. Home Assistant's rule for
  its satellites, the first detection wins, often picks the far one (forum
  reports, 2026).
- Each satellite already finds the others once a minute (ADR 0021): on the
  same subnet, one entry per MAC, with the friendly name, addresses and web
  port from the `_esphomelib._tcp` TXT record with
  `project_name=pinna.satellite`. A satellite stays listed until it misses
  two searches.
- In standalone mode each satellite serves its tools at `/mcp` to anyone on
  the LAN, without a token (ADR 0017). The dashboard has no login (ADR 0005).
- The wake word detector reports the peak score of the word against a
  threshold (ADR 0013). The capture loop sees each frame before the gain (the
  AGC of ADR 0004, or a fixed gain) is applied. That level is after the
  beamformer and echo cancellation, which differ between the two models.
  Whether the level or the score tells which satellite is nearer hasn't been
  measured.
- The ring's pending notification is Home Assistant's *Notification* switch,
  saved in `settings.toml`. It shows only while idle and no timer arc is
  above it. The Action button checks mute before it.
- goals.md promises that the ring shows whenever the device is listening.
- `ring` 0.17 is already in the build, under rustls (ADR 0017). It has
  HMAC-SHA256 and a secure random generator, but rustls doesn't expose them:
  pinnad has to name `ring` itself. pinnad's own random numbers come from
  fastrand, which is not secure.
- A Connect handler gets the decoded message, not the bytes that were sent: a
  JSON call is re-encoded before the handler sees it.
- Home Assistant switches are handled by the voice state machine and saved in
  `settings.toml`, like the *Assistant* select (ADR 0017). The standalone
  settings are saved in `standalone.toml` by another part of pinnad.
- The mDNS record is written once at boot and again whenever a TXT value
  changes. A change restarts systemd-resolved (ADR 0024).

## Decision

### The group

- **A satellite takes part only when its owner switches it on.** The switch
  *Satellite group* is on the Assistant page, off by default.
- **The group works in standalone mode only.** A member in Home Assistant
  mode stays a member and keeps the key, but sends, receives and arbitrates
  nothing. The other members list it as in Home Assistant mode, and their
  brain can't reach it. A member's mode travels in its signed `GetGroup`
  answer, not in the mDNS record, so a mode change doesn't restart resolved.
- **A satellite is in at most one group.** A group is an id (16 random bytes,
  in lowercase hex), a key (32 random bytes) and its member list, both from
  `ring`'s secure random generator.
- **The mDNS record says it.** With the switch on, pinnad adds `group=<id>` to
  the TXT record (ADR 0021), empty while the satellite is in no group. A key
  with no value (`group`) also means in no group. Without the key, the switch
  is off or the satellite runs an older Pinna. The id isn't secret. It tells
  groups apart and grants nothing.
- **The record is right from boot.** pinnad reads the group before it writes
  the record at boot, so a satellite in a group doesn't rewrite it (and
  restart resolved) on every start. Turning the switch on or off, joining and
  leaving change the record, and restart resolved (amends ADR 0024).
- **A satellite joins by a click on a member's dashboard.** With the switch
  on, the Assistant page lists the satellites the search found (ADR 0021):
  - members of this group: name, mode, and when it was last seen (kept in
    RAM, not written to disk every minute);
  - a member that answers the search in no group: "Left the group", with
    **Add** while its switch is on;
  - satellites with the switch on and in no group: **Add**;
  - the others, without an action, with the reason: "In another group" (and
    a link to one of its members' dashboards), or "Satellite group is off
    there, or its Pinna is older".

  **Add** on a satellite while this one is in no group starts a group of the
  two. The satellite added accepts only while its switch is on and it is in
  no group: its switch is its owner's consent to be added, and the click is
  the group's. A satellite never adds itself to a group from its own
  dashboard: the group decides who gets its key.
- **Names must be speakable and unique.** The brain names members by their
  friendly name (ADR 0007). Add is refused for a satellite whose friendly name
  is a member's already, or is "all" (which `send_message` uses for everyone),
  with "Rename it first on its dashboard". A satellite still on its default
  name can be added, with a note that it's hard to say.
  The friendly name comes from the search, which is how a rename reaches the
  others; the member list keeps the last one heard. A rename can still make
  two members share a name: a tool call naming them is refused, and the
  result names both by host name.
- **Add sends the key unencrypted** (amends ADR 0017, whose keys never leave
  the device), over the dashboard's listener, as plain HTTP on the LAN.
  Anyone who can read the LAN's traffic can read it. They can already read
  the keys an owner types into the dashboard.
- **Two Adds at once.** A satellite with an Add of its own under way refuses
  a `Join`. Two satellites that add each other at the same moment both fail,
  and each dashboard says to try again.
- **Members share the member list, entry by entry.** Each entry has a MAC, the
  last friendly name heard, a version and a removed flag. A change raises its
  entry's version and goes to every member that can be reached. Once a
  minute, after the search, each member asks the others for their list and
  merges it: for each MAC, the higher version wins; at equal versions, a
  removal wins. Two Adds on two dashboards in the same minute both stay.
- **Remove and leave.**
  - Any member's dashboard removes a member.
  - A satellite forgets the key only when a list signed with the key marks
    its own entry removed. A list without its entry means the sender hasn't
    heard of it yet: members merge entry by entry, so a member that missed
    an Add sends lists without the newcomer. A call with a valid tag from a
    MAC the receiver doesn't know makes it fetch and merge the caller's list
    first, and refuse only if the caller is still not a member. The search
    may not list a newcomer for a minute, so every call names the caller's
    web port, and the receiver calls back at the address the call came
    from. `GetGroup`
    needs only a valid tag, and a merge never starts another merge, so two
    newcomers calling each other don't loop.
  - A refused call never makes a satellite forget: a member that was
    factory reset refuses every call, and that must not break the group for
    whoever called it.
  - A removed member that was off learns it at its next exchange: members
    answer `GetGroup` with a signed list.
  - A member leaves by itself when every other member has answered two
    searches in a row outside the group, and refuses its `GetGroup` with
    "not in this group". This ends a group whose other members all left or
    were reset while it was off, and which nobody can send a signed removal
    to. The record alone isn't enough: a reload can leave an old one
    published (ADR 0021), and an older image publishes no `group` key.
  - Switching the group off on a satellite leaves the group and tells the
    others.
  - A group left with one member ends.
  - The key doesn't change. A satellite that kept it could still act as a
    member. It could already make any standalone satellite speak and act
    through `/mcp`, so a new key buys nothing while the LAN is trusted.

### Calls between members

- **`PeerService` on the dashboard's listener** (amends ADR 0005): Connect RPC
  from `proto/pinna/v1/peer.proto`, beside `DeviceService` and
  `AssistantService`. Its callers are satellites, not browsers. The Host
  allowlist applies; members call each other by IP address.
- **Its calls:** `Join` (the group id, key and member list), `GetGroup` (the
  signed list), `PutGroup` (list entries), `Deliver` (a message) and `Call` (a
  device tool).
- **Every call and answer except `Join` is signed.** A Connect handler never
  sees the bytes that were sent, so each call and answer is an envelope: the
  group id, the sender's MAC and web port, the inner message encoded as
  protobuf bytes, and an HMAC-SHA256 over the group id, the MAC and those
  bytes under the group key. The receiver checks the tag on the bytes as
  they arrived, then decodes them. A call with a wrong tag, another group, or
  a sender that isn't a member is refused, and the refusal says which, except
  as above: `GetGroup`, and a sender the receiver doesn't know yet. A refusal
  is an unsigned Connect error, which is why it never makes a satellite
  forget. There is no replay protection: the LAN is trusted (ADR 0005) and
  can already call `/mcp`.
- **A member's call doesn't confirm a Wi-Fi trial.** During a trial
  (ADR 0008), only the owner opening the dashboard keeps the new network;
  a call to `PeerService` doesn't count.
- **Members call each other at the addresses the search heard**: the first
  IPv4 address, then the first IPv6 address, 1 s each to connect. A member
  the search no longer lists (two missed searches, ADR 0021) is unreachable
  and isn't called.
- **A call ends inside the caller's tool limit** of 10 s (ADR 0017): the
  caller gives up after 9.5 s, the member runs the tool with 7 s (`/mcp` has
  no limit of its own, so `Call` adds it), and a call to several members goes
  to all of them at once. A tool the member already started may still finish
  after the caller gave up.

### Messages

- **A device tool, `send_message`,** is offered to the brain while the
  satellite is in a group with at least one other member in standalone mode.
  Only a brain whose tools pinnad runs gets it. An agent such as Hermes or
  OpenClaw gets no device tools (ADR 0017), and `/mcp` doesn't offer it.
  - `to` is a member's name, or `all` for every other member;
  - `text` is at most 500 characters, as for `speak` (ADR 0017).
- **The receiver says it in its own voice**: "Message from Kitchen: dinner is
  ready." The opening comes from a table of phrases, one per language, as the
  acknowledgment does (ADR 0017), falling back to English.
- **`Deliver` answers once the receiver has taken the message**, before it
  plays: played now, held, or refused with the reason. A receiver whose
  text-to-speech slot hasn't passed its check refuses; a voice that is set
  up but unchecked isn't enough, unlike for `speak`. The tool's result names
  each member's answer, or unreachable, and the brain can say it. A message
  whose speech fails on the receiver is held again once, and tried again no
  sooner than 30 s later; if it fails again, it is dropped, with a log line.
- **The brain knows the members.** The context pinnad adds to each request
  (ADR 0017) lists the members in standalone mode by name, and names those in
  Home Assistant mode as out of reach, so the brain can say why.
- **It plays now when it can, and is held when it can't.**
  - Idle, or with only music playing: it plays at once on the voice lane and
    ducks the music, as `speak` does.
  - While a request runs (the follow-up window included), anything else plays
    on the voice lane (a reply, `speak`, a Home Assistant announcement,
    another message or a cue), an alarm rings, the calibration runs, or the
    satellite is muted: it is held. Muted is the owner saying "not now"
    (ADR 0017).
  - Held messages play by themselves when the satellite is idle again and
    not muted, oldest first.
  - Anything else that cuts a playing message (an alarm, a Home Assistant
    announcement, the calibration) holds it again, without counting a
    failure.
- **Held messages are their own state**, not Home Assistant's *Notification*
  switch, and Home Assistant gets no `cleared` event for them. They need no
  ring of their own: they play as soon as the satellite is idle and not
  muted, and while it's muted the ring shows Muted, as it does today. The
  dashboard's Overview shows how many are held.
- **Held messages stay in RAM**: at most 5, each forgotten after 12 hours and
  on a restart, an update's included. A message held again goes back to the
  front; when a sixth arrives, the oldest of the others is dropped. Switching
  to Home Assistant mode or turning *Satellite group* off drops them.
- **The receiver chooses.** The switch *Group messages*, on by default, which
  is also a Home Assistant switch entity, so an automation can turn messages
  off at night. While it's off, `Deliver` refuses; messages already held
  still play.
- **The wake word and the Action button stop a message** as they stop a
  reply. The wake word puts it back among the held messages, since the
  person wanted to talk, not to dismiss it. The Action button drops it.

### Control

- **Device tools that act on the satellite take an optional `satellite`**: a
  member's name. Empty means this satellite. The brain sees one tool for each
  action, not one for each member, so the tool limit of 64 (ADR 0017) holds.
  - The tools: volume (set and change), reply volume, the ring light, the
    media player (play a URL, pause, resume, stop), radio (play a station,
    list the stations) and timers (set, list, cancel).
  - `cancel_request`, which the brain doesn't get for its own satellite
    (ADR 0017), is offered for other members: it stops their alarm, their
    request, or a message or `speak` that plays. A message stopped this way
    is dropped, as by the Action button.
  - Not: the microphone, speak (that's `send_message`), the notification,
    switching the assistant, the time and the weather.
  - Only the brain sees `satellite`. `/mcp` stays as it is.
- **The member runs a call as its own `/mcp` would** (ADR 0017): its tool
  switches and its busy rule apply, with the 7 s limit above, and the result
  comes back as text. It refuses a tool that isn't on the list above, and a
  call that names a `satellite`, so a call is never passed on.
- **Both satellites' switches apply.** A tool switched off on the calling
  satellite isn't offered to its brain at all, for itself or for another
  member.
- **Metrics** (amends ADR 0020): a member's call counts as a tool call with
  `via="peer"`, beside `brain` and `mcp`.
- **The member chooses.** The switch *Control from the group*, on by default.
- **A timer set on another member rings there**, and lives there.

### One answer to the wake word

- **When several members hear the wake word, the one that heard it loudest
  answers.**
- **Only starting a request is arbitrated.** A member whose alarm rings
  stops it when it detects the wake word, as today, and sends no claim:
  stopping an alarm starts nothing, so it can't cause a second answer.
- **Each detection that would start a request is a claim.** A member that
  detects the wake word sends a claim by UDP, port 6054, to each other
  member in standalone mode, at the addresses the search heard. A member that
  is muted or whose slots aren't ready sends none. The claim holds the group
  id, its MAC, the wake word, the level, the score and whether it is busy,
  and an HMAC-SHA256 under the group key. Claims compete only for the same
  wake word. A claim with a valid tag counts even from a MAC not yet in the
  list: there's no time to merge first.
- **A member being calibrated** (ADR 0013) sends a busy claim. The owner is
  probably saying the word to it, but a calibration lasts minutes and anyone
  can start one, so it doesn't win a word said next to another member.
- **Better means:** louder, then a higher score, then the higher MAC. The
  level is the speech level of the wake word's blocks before the AGC, in
  dBFS, plus an offset for each model's microphones. A busy member (Thinking,
  Speaking or playing a message) gets a bonus on its level, so the person
  talking to it can interrupt it from a little further away, but a far
  satellite doesn't cut its reply off for a wake word said next to another
  one.
- **Every member waits for the others.**
  - It lights the Listening ring at once, as today, and holds the wake sound
    for a short window (200 ms, to be measured). A member with no other
    member that could send a claim (in standalone mode, *Answer once in the
    group* on, and listed by the search) doesn't wait. The microphone audio is
    kept from the wake word on, so nothing said is lost. A busy member
    lowers what it's saying for the window, instead of stopping it.
  - If a better claim arrives in the window, or arrived up to 1 s before its
    own detection, it stands down: the ring goes out, and there's no sound,
    no request and nothing uploaded. A busy member that stands down carries
    on with what it was doing, at its volume again. A log line names the
    member that answered, and so does the dashboard's Overview.
  - Otherwise it goes on as it does today.
- **Missing claims fail open.** A member that gets no claim answers, so a
  lost packet means two answers, as without a group.
- **The member chooses.** The switch *Answer once in the group*, on by
  default. While it's off, the member sends no claims and ignores others'.
- **Metrics** (amends ADR 0020): claims sent and received, and wakes won and
  stood down.
- **Measured before it's built.** First, the capture loop logs each wake
  word it passes on with its level before the gain: the mean level in dBFS
  of the speech frames among the 1.28 s the detector's classifier saw, and
  the detection's time to the millisecond. Clocks are synced by
  systemd-timesyncd. None is logged while muted or listening. A wake that
  only stops an alarm or counts for a calibration is logged too; the state
  machine's next line tells it apart. On the owner's Echo Dot (2nd gen) and Echo (2nd
  gen) in two rooms, this measures:
  - whether the level, or the score, picks the nearer satellite;
  - the offset between the two models, and the busy bonus;
  - how far apart two satellites detect the same word, and how long a claim
    takes over Wi-Fi: together they set the window.

  The test is at least 30 wakes from at least 3 places. A week of ordinary
  use also counts how many wakes both satellites hear. The section is
  dropped from the ADR before it's built if neither the level nor the score
  picks the nearer satellite at least 9 times in 10, or if fewer than 1 wake
  in 10 is heard by both.

### Settings

- **The group's settings are on the dashboard**, like the other standalone
  settings (ADR 0017): *Satellite group*, *Control from the group*, the
  member list, add and remove. They are RPCs in `AssistantService`.
  *Answer once in the group* comes with the arbitration, not before.
- ***Group messages* is also a Home Assistant entity**, since it's how the
  satellite behaves at a given time, not how it's set up. Like the
  *Assistant* select (ADR 0017), it is a field of `settings.toml`, owned by
  the voice state machine and set through `DeviceService`'s settings, and
  Home Assistant lists it in both modes. It does nothing in Home Assistant
  mode or while *Satellite group* is off. The other switches and the member
  list are dashboard only.
- **In `standalone.toml`**: `[group]` holds the switches and the id, and each
  member is an entry of its own (`[[group_member]]`), so an entry this image
  can't read costs that entry, not the list (ADR 0017). The key is in
  `secrets.toml`, mode 0600 (ADR 0017). The dashboard never shows it.

## Consequences

- A household with several standalone satellites can leave messages from
  room to room, act on another room's satellite by its name, and gets one
  answer to a wake word heard in two rooms.
- Only satellites on the same subnet can be in a group: the search finds no
  others (ADR 0021).
- The key keeps other groups' satellites out of a group's calls, and is ready
  for a dashboard login. Until there is one, it secures nothing that the LAN
  can't already do through `/mcp`.
- In a group, the wake sound comes up to one window later. A satellite that
  stands down shows its ring for that window, which tells the owner it heard
  the wake word.
- A claim that takes longer than the window over Wi-Fi arrives too late, and
  two satellites answer.
- A message to `all` costs a text-to-speech request on each member that plays
  it.
- A member that is off while the list changes takes the change at its next
  minute's exchange.
- `PeerService` is the first part of Pinna that checks a key on the LAN.
- With an agent as the brain, the group's messages and control are out of
  reach by voice: the agent gets no device tools. Members still receive
  messages and calls from other members.
- Every change of the switch or the group restarts resolved on that
  satellite, and an Add on both.
- The name "Satellite group" should not be confused with a speaker group for
  music in step (Music Assistant, planned). The UI wording is settled when
  that one is built.
- Left out for now:
  - a reply to a message: the receiver's follow-up going back to the sender;
  - an intercom, which opens the microphone without a wake word (ADR 0017
    refuses that);
  - groups across subnets;
  - several satellites sharing one conversation (ADR 0017);
  - Home Assistant mode, where Home Assistant runs the request. Whether its
    satellites should answer the wake word once too is a later decision.
