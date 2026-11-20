# ADR 0028: A dashboard password, confirmed at the device

Status: proposed (2026-11-19). Amends ADR 0005 (no authentication on the
listener), ADR 0017 (who may call `/mcp`), ADR 0018 (who may connect to the
ESPHome API before Home Assistant sets a key), ADR 0020 (who may read
`/metrics`), ADR 0021 (the list of satellites) and ADR 0026 (`Join`, and
calls from members without a password). Points marked *default* are not
confirmed by the owner yet.

## Context

- The dashboard has no login (ADR 0005). Anyone on the LAN can restart the
  satellite, reset it (which also removes the key Home Assistant set, ADR
  0018), install an older signed release, move it to another Wi-Fi network,
  read its log and network details, and play any URL on it. In standalone
  mode they can also point a speech-to-text slot at their own server and
  receive what the microphones hear after the wake word (ADR 0017), which
  runs against the first goal: your voice goes only where you send it.
- Release images carry no SSH key (ADR 0015), so the dashboard is the only
  way an owner manages a satellite. Whoever reaches it can do everything the
  owner can, short of reflashing.
- Three records left work for "when the dashboard has a login": SSH keys
  from the dashboard (ADR 0010), a provisioning window for the ESPHome key
  (ADR 0018), and the group key (ADR 0026).
- The listener's callers: browsers (the dashboard), MCP clients (agents),
  Prometheus (`/metrics`) and satellites of a group (`PeerService`). Beside
  it, the ESPHome API on port 6053 accepts anyone until Home Assistant sets
  a key (ADR 0018). Its entities switch the voice mode, the wake word,
  Bluetooth pairing and the update channel, install updates and play URLs.
  A standalone satellite without Home Assistant never gets a key.
- `PeerService` signs every call except `Join`, which anyone on the LAN can
  send to a satellite whose *Satellite group* switch is on and that is in no
  group (ADR 0026). The group key never changes, and it crosses the LAN
  unencrypted at Add.
- The satellite serves plain HTTP. It has no certificate a browser trusts,
  and can't get one for a `.local` name. A password sent to it crosses the
  LAN in the clear. Wi-Fi encryption keeps it from outsiders, not from others
  who know the Wi-Fi password. A reverse proxy with TLS in front of the
  dashboard address (ADR 0021) avoids that.
- The USB link isn't proof that someone is at the device. The Echo Dot
  (2nd gen) takes its power through the same cable, so a computer, NAS or
  hub that powers it keeps the link up, and every process there could reach
  `10.42.0.1`. A DHCP server on the Wi-Fi network can push a route for
  `10.42.0.0/24` that wins over the USB link's. On the Echo (2nd gen) the
  USB link is test pads, not a socket.
- Every model has an Action button. pinnad reads it from the input device,
  and already takes buttons over for a while: the wake word calibration
  takes + and − (ADR 0005).
- What comparable devices do: ESPHome's web server and Tasmota are open
  until their YAML or file sets a password; WLED's PIN covers its settings
  pages only; Home Assistant makes the first visitor create the owner
  account; Frigate generates an admin password at first start. Devices that
  hold paid keys or route private data ask for a login.
- pinnad already links `ring` 0.17 (ADR 0026): PBKDF2, HMAC-SHA256, a system
  random source and constant-time comparison. No new dependency is needed.
- Home Assistant is optional. Boot progress, the health check, rescue mode
  and support tools never wait for it, nor for a login.

## Decision

### Presence: a press of the Action button

- **Some changes need someone at the device.** The dashboard asks for it,
  and for the next 60 seconds the ring shows the confirm pattern, a white
  light that circles slowly *(default)*, until Action is pressed. The press
  is pinnad's alone while it waits: it doesn't stop an alarm, start a
  request, or reach Home Assistant as an event, and it works while muted.
  Only one confirmation waits at a time, and a new one can start a minute
  after the last ended, so nobody on the LAN can keep the ring busy.
- **A press confirms only the request that asked for it.** The dashboard
  that asked gets a one-time grant, valid for 5 minutes, that the next call
  presents.
- **What needs a press:**
  - setting the first password;
  - setting or removing the password without the current one, which is how
    an owner who forgot it gets back in;
  - opening the ESPHome window and the group window (below) on a satellite
    without a password. With a password, the session is enough.
- The USB link gets no exemption. Rescue mode and recovery still answer
  only there, without a password (ADR 0012, ADR 0019).

### A password, offered first and kept optional

- **The dashboard offers a password before anything else**, on every
  satellite that has none, until one is set or someone chooses **Leave it
  open**. The choice is stored on the satellite, so other browsers don't ask
  again. A satellite that updates into this asks too, and its owner can
  leave it open; nobody is locked out by an update. *(default: offered, not
  required)*
- **While it is open,** the Overview says so: "Anyone on your network can
  control this satellite." In standalone mode it adds: "That includes where
  your voice goes."
- **Once a password is set, every request to the listener needs a
  session**, except:
  - the page's own files (`/`, `/assets/`), which hold no secret;
  - `/auth/login`, and starting and following a confirmation;
  - `/mcp` and `/metrics` with their tokens (below);
  - `PeerService`, which checks the group key (ADR 0026, and below).

  The check wraps the whole router, the fallback included, and the
  exceptions are exact paths, so a route added later is protected without
  anyone remembering to. `DeviceService` and `AssistantService` refuse with
  Connect's `unauthenticated` code, and the page shows the login form. The
  virtual Echo Dot's socket in the development VM (ADR 0014) needs a session
  like the API.
- **The password is 8 to 128 characters**, with no other rules. The form
  says not to use it anywhere else: it crosses the network unencrypted.
  *(default)*
- **It is stored as a hash:** PBKDF2-HMAC-SHA256 with a 16-byte random salt,
  in `/var/lib/pinna/secrets.toml` (mode 0600, written durably, ADR 0017),
  with its iteration count, so the count can change. One check should take
  about a quarter of a second on the Echo Dot (2nd gen); the count is
  measured there when this is built. Checks run on a blocking thread, away
  from the voice path.
- **Changing or removing the password needs the current one, or a press.**
  Changes go through one lock, so of two changes made at once, the second
  fails as a wrong current password. A wrong current password counts as a
  failed login. A change ends every session.
- **The Wi-Fi trial is confirmed before the password is checked.** Opening
  the login page on the new network keeps it (ADR 0008), so a password never
  makes a trial lapse.

### Sessions

- **`POST /auth/login`** takes `{"password": "..."}` as JSON and sets a
  cookie: `HttpOnly`, `SameSite=Strict`, `Path=/`, kept for 30 days
  *(default)*. Its name ends with the last six hex digits of the MAC
  (`pinna_session_e32a7f`), because cookies ignore ports and two satellites
  behind one proxy host would otherwise overwrite each other's.
  `POST /auth/logout` clears it. They are plain HTTP routes, not Connect,
  because they set cookies. The Host allowlist, an exact JSON content type
  and the Origin check of `/mcp` apply to both.
- The cookie has no `Secure` flag: the satellite serves HTTP. Behind a
  reverse proxy with TLS, the proxy can add it. A Home Assistant iframe panel
  can't hold the session (`SameSite=Strict`); the "Visit" link opens the
  dashboard in its own tab, which can.
- **A session survives restarts.** The cookie holds its expiry and an
  HMAC-SHA256 tag under a session key in `secrets.toml`; pinnad keeps no
  list of sessions. A satellite restarts after every update, and a login
  after each would be a nuisance. A new password replaces the session key.
  So does **Sign out everywhere** on the System page.
- **Ending sessions ends their streams.** A new session key closes open
  `WatchStatus`, `WatchRing` and the VM socket, so a session that ended
  doesn't keep receiving.
- **Failed logins slow down for the whole satellite**, not per address,
  because addresses on a LAN cost nothing: at most one attempt a second, and
  after 10 failures in a row, one a minute until a login succeeds or the
  password is reset with a press. *(default)* A refused attempt doesn't run
  PBKDF2. The form says how long to wait, and the System page shows the
  failures since the last login and the address of the last one. Sessions
  that already exist keep working.

### Tokens, for machines

- **Two tokens** *(default)*, each created on the System page: 32 random
  bytes, base64url, shown in full once and by their last four characters
  after that. **Create a new token** replaces one, which revokes the old.
  `secrets.toml` keeps their SHA-256, not the tokens.
  - **The metrics token** reads `/metrics`, nothing else.
  - **The agent token** opens `/mcp` and its tools, which can speak, play a
    URL, mute and switch the voice mode (ADR 0017). It doesn't open
    `DeviceService` or `AssistantService`.
- **They go in `Authorization: Bearer <token>`.** Not in the address: query
  strings end up in proxy logs.
- **A refusal names the fix.** `/mcp` answers 401 with
  `WWW-Authenticate: Bearer realm="pinna"` and a JSON-RPC error: "This
  satellite needs its agent token. Create one on its System page." It offers
  no OAuth metadata, so clients don't start an OAuth sign-in that can't
  succeed. `/metrics` answers 401 with a one-line text body.
- **Without a password, `/mcp` and `/metrics` stay open**, as today.
- **Before a first password is saved,** the form lists the MCP clients that
  called in the last 24 hours (ADR 0017 keeps the last 8) and whether
  `/metrics` was read in that time, and says that they will need a token.

### Home Assistant: the ESPHome API opens in a window

- **With a password set, a satellite without an ESPHome key refuses
  connections to the ESPHome API**, except for 15 minutes after the owner
  chooses **Let Home Assistant add this satellite** on the Home Assistant
  page. Inside the window it is provisionable as before (ADR 0018); once Home
  Assistant has set its key, only the key works, as before, and the window
  no longer matters.
- Without a password, nothing changes: a satellite without a key is
  provisionable, as ADR 0018 says.
- A satellite with a key keeps it when a password is set. The Home Assistant
  page explains why Home Assistant can't connect while the window is closed.

### Satellite groups

- **With a password, `Join` needs the owner's say on the satellite being
  added.** It accepts a `Join` only for 10 minutes after **Allow adding to a
  group** on its Satellite group page; the switch alone no longer does. Without
  a password, the switch is consent, as ADR 0026 says.
- **Members tell each other whether they have a password**, in the signed
  facts each call carries (`PeerSender`). The member list shows a member
  without one.
- **A satellite with a password refuses `Call` and `Deliver` from a member
  without one,** with "That satellite has no password; set one there
  first." A member with an older Pinna counts as one without. Otherwise
  anyone could reach a protected satellite through the brain of an
  unprotected member.
- **The key doesn't change** (ADR 0026). A removed member that kept it can
  still call the others. To shut it out, end the group and form a new one.

### Switching between satellites

- Each satellite has its own password and its own session. Choosing a
  satellite with a password opens its login form.
- **Anyone on the LAN can announce a satellite** (ADR 0021), and with
  passwords a fake entry could lead to a look-alike login form. The list
  marks the members of this satellite's group, which their key confirms;
  every other entry is only what the network announced, and the list says
  so. The docs don't suggest one password for every satellite.

### What doesn't change

- The loopback API (`pinnactl`, the boot health check) needs no password:
  root on the satellite can already do everything.
- The Host allowlist, the Origin checks and Connect's preflight stay.
- A factory reset removes the password, the session key and the tokens with
  the rest of `/var/lib/pinna` (ADR 0010).
- **No Home Assistant entity.** The password protects the dashboard; Home
  Assistant has its own key. This is the exception ADR 0007 allows for a
  setting that makes sense in one place only.

## Consequences

- With a password set, someone on the LAN without it can't change settings,
  restart, reset, install, switch slots, move the Wi-Fi, read the log, call
  `/mcp`, use the ESPHome API before Home Assistant has a key, add the
  satellite to a group, or reach it through an unprotected member. They can
  still:
  - press the device's buttons, if they are in the room;
  - read the password, a session cookie or a token off the network, if they
    can read the Wi-Fi traffic. A reverse proxy with TLS prevents that for
    the dashboard;
  - hold the login at one attempt a minute by failing on purpose. Existing
    sessions keep working, and a press resets the password;
  - guess about 1,440 passwords a day. A password that isn't a common word
    outlasts that.
- An owner who forgot the password presses Action; no cable and no reset.
- Agents and Prometheus need a token once a password is set, and the form
  says so before it's saved.
- Adding a protected satellite to Home Assistant or to a group takes one
  more click on its own dashboard.
- **A rollback removes the protection while it lasts.** An image from before
  this keeps the password's entries in `secrets.toml` but doesn't check
  them, so the dashboard is open until the next update. Before switching to
  such a slot, or keeping an update back in favour of it, the Updates page
  says: "The version in the other slot doesn't ask for a password."
- New in `DeviceService`: the security state (password set, tokens set,
  failures, windows); setting, changing and removing the password; the
  confirmation; creating tokens; signing out everywhere; opening the
  ESPHome and group windows.
- New in the dashboard: the login form, the first-run offer, the password,
  tokens and sign-out on the System page, the notice on the Overview, the
  Home Assistant page's window, the Satellite group page's window and its
  members' password state, and the warning on the Updates page.
- New on the ring: the confirm pattern, in `pinna-hal`'s animations and
  `design/ring.ts` together.
- `[web] listen = ""` stays a developer setting (ADR 0007).
- Left out: SSH keys from the dashboard (ADR 0010), rotating the group key,
  per-client tokens, and TLS on the satellite.

## Alternatives not taken

- **The USB link as physical access:** a computer that powers the Dot keeps
  the link, a DHCP route can win over it, and the Echo (2nd gen) has pads,
  not a socket. A button press works on every model, over any network.
- **A password required before anything works:** with the press, nobody on
  the LAN can claim a satellite first, so this would be safe. But an update
  would stop owners from using satellites they use today until they set one.
  The offer asks every owner once instead. *(default; the owner may prefer
  required)*
- **A password from `secrets/device/` at build time** (ADR 0005's example):
  release images carry no owner data (ADR 0015).
- **HTTP Basic authentication:** browsers keep it with no way to sign out,
  and its dialog can't follow the design system.
- **Sessions kept in memory:** every restart, and so every update, would end
  them.
- **TLS with a self-signed certificate:** every browser warns, and owners
  learn to click past the warning.
- **A limit per address:** addresses on a LAN cost nothing.
- **One token for agents and Prometheus:** a token kept in a Prometheus
  configuration on another machine would also speak through the satellite.
- **A token in `/mcp`'s address,** for ADR 0017's MCP client, which sends no
  headers: its use, one satellite's brain calling another's `/mcp`, is what
  satellite groups replaced (ADR 0026).
- **Showing the ESPHome key on the dashboard** instead of a window: ADR 0018
  keeps Home Assistant as the one place that sets it.
