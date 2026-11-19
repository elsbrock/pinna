# ADR 0022: Update channels, served from updates.getpinna.dev

Status: proposed (2026-11-17), accepted (2026-11-19): the owner confirmed the
points that were marked *default*. Amends ADR 0003 (the manifest is signed,
describes the whole release, and a channel install never goes back in
version), ADR 0015 (where satellites find releases; prereleases; the version
scheme) and ADR 0016 (the installer downloads the release itself). Built:
the channels, the release service, the scheduled check, installing a
release, and automatic installs (2026-11-20), which no satellite has run
yet.

## Context

- A satellite installs an update only when someone hands it a manifest URL:
  `pinnactl update <url>`, or the dashboard's Updates page, which suggests
  port 8000 on the browser's own host (ADR 0003). Nothing tells an owner that
  a new release exists.
- Releases are GitHub releases, created as drafts and published by hand
  (ADR 0015). The repository stays private until the first public release, so
  its release files need a GitHub token. The URL in ADR 0015
  (`…/releases/latest/download/manifest.json`) doesn't work for satellites
  yet. A URL compiled into an image stays in use for as long as that image
  runs.
- GitHub's release downloads send no `Access-Control-Allow-Origin` header
  (ADR 0016), so the browser installer asks the user to download the files and
  pick them.
- Only the root image is signed (its footer, ADR 0003). The manifest is plain
  JSON: whoever serves it chooses which signed image a satellite installs,
  an older one included. Nothing checks the boot image's origin; the installer
  compares it with `SHA256SUMS`, which shows the download is intact, not who
  made it.
- An update writes the boot image and the root into the other slot together
  (ADR 0019), but only where the bootloader has shown that it loads the
  slot's kernel. Elsewhere (a Kaeru from before that fix, or slot a before
  the proof) the slot keeps the kernel it has, and a root that needs
  something new from pinna-init or the kernel fails its trial boot and rolls
  back. pinna-init records the root's boot protocol (ADR 0012), but nothing
  records what a boot image provides.
- The ESPHome native API has an update entity: `ListEntitiesUpdateResponse`,
  `UpdateStateResponse`, and `UpdateCommandRequest` with `UPDATE` and `CHECK`.
  They are in the `api.proto` that pinna-esphome vendors. Home Assistant shows
  such an entity as an available update, with a summary, a link to the release
  notes, Install, Skip and progress. pinna-esphome doesn't offer one yet.
- A build signed with a developer's key (`just keygen`, ADR 0015) accepts only
  images signed with that key. A release would download in full and then fail
  the signature check. Development builds are versioned
  `<date>.<time>-<commit>` (`scripts/build-root.sh`); releases take the tag's
  version. Nothing checks the format of a release tag, and no tag exists yet.
- The project owns the domain `getpinna.dev`.

## Decision

### The release service

- `https://updates.getpinna.dev` is the one address of the project that
  satellites and the installer use. It is a Cloudflare Worker in
  `services/updates/`, TypeScript without runtime dependencies.
- GitHub releases stay the source. The Worker reads the release list through
  GitHub's API and caches it for 5 minutes. The Worker holds a
  fine-grained, read-only token as a Worker secret: while the repository is
  private, for access; once it is public, for GitHub's rate limit. Without a
  token the API allows 60 requests an hour per address, and a Worker shares
  its addresses with others.
- It answers:

  | Request | Answer |
  | --- | --- |
  | `GET /v1/channels/<channel>` | A redirect to the manifest of the newest release on `stable` or `beta` |
  | `GET /v1/releases/<tag>/<file>` | A file of that release, streamed from GitHub: `manifest.json`, `manifest.json.sig`, the images, `SHA256SUMS` |

  Every answer carries `Access-Control-Allow-Origin: *`. File URLs in a
  manifest are relative, so they resolve next to the manifest.
- Only published releases are served. A bad release is withdrawn by adding
  its tag to `services/updates/withdrawn.json` and deploying the Worker; the
  channels then skip it, and its files are no longer served, so an install
  that a check found earlier fails at the download. GitHub can't always turn a published release back
  into a draft: its web interface can't, and nothing can for an immutable
  release. Satellites that already installed a withdrawn release stay on it,
  because channels never go back in version; a newer release fixes them.
- The Worker is not trusted. Whoever controls it can withhold updates, but
  can't make a satellite install anything the release key didn't sign, or go
  back in version.
- If GitHub limits downloads through the Worker, the release workflow also
  uploads the files to Cloudflare R2 and the Worker serves them from there,
  under the same URLs. This is built only when it is needed.

### Versions

- Releases use semantic versions (SemVer 2.0.0). A release tag is
  `v<major>.<minor>.<patch>`, a beta `v<major>.<minor>.<patch>-beta.<n>`,
  for example `v0.4.0` and `v0.4.0-beta.1`. The version is the tag without
  the `v`. The release workflow refuses any other tag.
- What the numbers mean for an image: a patch release only fixes; a minor
  release adds features; a major release needs the owner to do something by
  hand, such as installing a new boot image over USB or changing the key.
  Before 1.0, the minor number takes the major number's role.
- Development builds are versioned
  `<next patch>-dev.<YYYYMMDDHHMM>.g<commit>`, for example
  `0.4.1-dev.202611171405.gabc1234`. The next patch is the newest `v*` tag
  reachable from the commit with its patch number raised by one; after a beta
  tag it is that tag's version without the prerelease part, so
  `0.5.0-dev.…` comes after every `0.5.0-beta.<n>` and before `0.5.0`; with no
  such tag it is `0.0.1`. Such a version sorts after the release it builds on and
  before the next release. The `g` keeps a commit hash that is all digits a
  valid part of a semantic version. Manual dry runs of the release workflow
  are versioned `0.0.0-dryrun.<run number>.g<commit>`.
- Versions contain only lowercase letters, digits, `.` and `-`, never `+`:
  `IMAGE_VERSION` in os-release allows nothing else.
- Every comparison uses semantic version precedence: in the updater, in the
  release service's channels and in Home Assistant's update entity. Cargo's
  crate versions are unrelated; the image version is the version owners see.
  The binaries report the version the build sets in `PINNA_VERSION`, as the
  release workflow does, in their logs, `--version` and the names they give
  other services; a build without it reports the crate version.
- A booted version that isn't a semantic version, a development build from
  before this change (`2026.11.17.1405-abc1234`), comes before every release.

### Channels

- A release published as a full release is on `stable`; one published as a
  prerelease is on `beta`. A tag with a prerelease part (`v0.4.0-beta.1`)
  makes the release workflow create a prerelease. `0.4.0-beta.1` comes
  before `0.4.0` (Versions).
- `beta` offers the newest release of either kind, so a satellite on beta
  moves on to a stable release that is newer.
- Each satellite has the setting **Updates**: `stable`, `beta` or `off`.
  `off` checks nothing. Release images start on `stable`; development builds
  (ADR 0025) start on `off`. Until ADR 0025 is built, a
  development build is one that compiles in `secrets/release.pub`
  (ADR 0015). A release image whose version has a prerelease part, a beta
  from the browser installer, starts on `beta`, so it stays on the track it
  was installed from.
- It is a setting like the others (ADR 0007): a field of `UpdateSettings`, a
  control on the dashboard's Updates page, and a `select` entity in Home
  Assistant (category config).
- Switching channels is allowed at any time. From `stable` to `beta`, the
  next check offers the newest beta. From `beta` to `stable`, the satellite
  keeps its beta build until a stable release is newer, normally the next
  one: `0.5.0` comes after `0.5.0-beta.2`. Installs from a channel never go
  back in version.
- Going back at once is the owner's choice, on the dashboard only. When the
  other slot holds the older version, the dashboard offers to start it
  (`SetActiveSlot`). Otherwise it offers "Install 0.4.0 (older)" after a
  confirmation. The rule against older versions guards against the release
  service, not against the owner. Settings survive the step back: pinnad
  keeps settings keys it doesn't know (ADR 0007).

### Checking

- On `stable` or `beta`, pinnad checks once a day at a time of day it picks at
  random and keeps, which spreads the load. It also checks once after each
  start, once the clock is synchronized, and when the owner asks: "Check now"
  on the dashboard, Home Assistant's check (`UPDATE_COMMAND_CHECK`), or the
  satellite itself in standalone mode (ADR 0017).
- A check fetches the channel's manifest and its signature, nothing else.
  pinnad runs it as `pinnactl update --check`, as it runs installs through
  pinnactl: the signature code is the updater's, and pinnad has no HTTP
  client of its own. A
  newer release with a valid signature shows as available on the dashboard and
  in Home Assistant. Nothing is downloaded until the owner installs it, or
  until the install window when installs are automatic (below).
- A failed check shows its cause on the Updates page ("updates.getpinna.dev
  did not answer", "the clock is not set") and is retried at the next
  scheduled check. In Home Assistant it isn't an error: the update entity
  keeps its last state. Until a check has succeeded since pinnad started,
  and while Updates is `off`, the entity reports no state, which Home
  Assistant shows as unknown: the satellite doesn't know whether it is up to
  date. A satellite without internet access is updated from a
  release copied to the LAN, through the manifest URL on the dashboard.
- Nothing else waits for the release service: the voice path, the health
  check and rescue mode never contact it.
- A check carries nothing but its request for the channel. Counting
  satellites through the check was proposed in ADR 0023 and withdrawn
  (2026-11-19).

### Installing

- The owner installs from the dashboard, with Home Assistant's Install
  button, or in standalone mode by asking the satellite, once they have
  turned that tool on (ADR 0017). pinnad runs the install it runs today (`InstallUpdate`, ADR 0003)
  on the release it showed: the `/v1/releases/<tag>/manifest.json` that the
  check found, not the channel again, so a release published in between is
  never installed unseen. pinnactl applies the channel's rules (below). The
  update entity's progress follows the install's stages (`InstallStage`).
- Until the restart, the update entity stays in progress, and the version
  waiting in the other slot is not installed again.
- When the other slot already holds the release, verified and not marked
  unbootable (an update the owner held back with "Start it later"), Install
  makes that slot boot next instead of downloading it again.
- A version that failed its trial boot (the other slot holds it, marked
  unbootable) is not offered again. The Updates page and the entity's summary
  say so: "0.4.0 failed to start and was rolled back." This needs the slot's
  version to be the release the check found: an install that stopped part
  way also leaves the slot unbootable, with the previous version still in
  it. A newer release is offered as usual.
- Where Install would be refused (the boot level, a `just push` overlay, a
  running slot on trial), the entity's summary and the Updates
  page say why before anyone presses it. Home Assistant's Install sends a
  command that has no answer.
- After an install started from Home Assistant or by asking the satellite,
  pinnad restarts the satellite the next time it is Idle with no timer set
  and nothing playing: Home Assistant has no restart button, and nobody is at
  one when the satellite was asked. After an install from the dashboard,
  the dashboard offers "Restart now" and "Start it later", as before. pinnad
  keeps a pending restart in `/run`: a restart of pinnad keeps it, a restart
  of the satellite ends it.
- The update entity: `current_version` is the booted version,
  `latest_version` the channel's, `title` is "Pinna <version>",
  `release_summary` the manifest's `summary`, `release_url` its `notes_url`.
  Skip is Home Assistant's own; the satellite doesn't store it.
- Install is refused while a `just push` overlay is active
  (`/var/lib/extensions/pinna-dev`, ADR 0025): after the restart it would hide
  the new release's binaries. The message names the command that removes it,
  `scripts/push-dev.sh --remove`, which runs on the developer's computer.

### Automatic installs

Built. Its tests run against a simulated release service; no satellite has
installed a release this way yet.

- The setting **Install updates** is `manually` or `automatically`. With
  `automatically` it has a window: a start and an end time in the satellite's
  time zone (ADR 0007), 03:00 to 05:00 unless the owner changes it. A window
  may cross midnight. Satellites start on `manually` until 1.0.
- It is a setting like the others (ADR 0007): fields of `UpdateSettings`,
  controls on the dashboard's Updates page, and in Home Assistant a `select`
  entity for the mode and two `time` entities for the window (category
  config), in both voice modes. The settings are `install_updates`,
  `install_window_start` and `install_window_end` in settings.toml, each
  optional. A window whose start equals its end is refused, with a sentence
  that says why.
- With `automatically`, when a check has found a release (the scheduled check,
  the one after start, or one the owner asks for), pinnad picks a random
  minute inside the window: in the window that is open now if the minute is
  still ahead, otherwise in the next one. The window is local wall-clock time
  in the satellite's time zone, so a daylight saving change moves it with the
  clock. pinnad starts the install at that minute, as an install with its own
  origin, after it checks again that the release may install.
  Switching to `manually` cancels an install that has not started; one that
  runs finishes like the owner's, and the owner restarts. When the install has
  succeeded, pinnad restarts the satellite when it is Idle with no timer set
  and nothing playing, inside the window. If the window ends first, the
  restart waits for the next window. The dashboard's Restart now works at
  any time. pinnad keeps the pending restart in `/run`, marked as one that
  waits for the window.
- Never installed automatically: a release that needs a higher boot level, and
  a version that failed its trial boot. Nothing installs automatically while
  the clock isn't synchronized, while the running slot is still on trial,
  while an install runs, while Updates is `off`, or while pinnad.toml's
  `[update] manifest` replaces the channel. Such releases are shown and wait
  for the owner.
- After an unattended download fails, the failure is logged and shown as the
  last install's failure, as for any install. The next window tries again, at
  a new random minute. After 3 failed automatic attempts of one version in a
  row, automatic installs skip that version, and log that they do, until the
  owner installs it by hand or a newer version is found. The count lives in
  memory: a restart of pinnad begins again.
- The version number doesn't decide: a major or, before 1.0, a minor version
  installs like a patch. What a release needs by hand is in its manifest's
  `requires`, and a release signed with another key fails verification on any
  satellite.
- An install the owner starts ignores the window: from the dashboard, with
  Home Assistant's Install button, from a Home Assistant automation, or by
  asking the satellite.

### The manifest

- The manifest describes the whole release:

  ```json
  {
    "version": "0.4.0",
    "channel": "stable",
    "summary": "Up to 255 characters, for Home Assistant's update card.",
    "notes_url": "https://github.com/elsbrock/pinna/releases/tag/v0.4.0",
    "requires": { "boot_level": 1, "models": ["echo-dot-2", "echo-2"] },
    "root": { "url": "pinna-0.4.0.root.img", "size": 160432128, "sha256": "…" },
    "boot": { "url": "pinna-boot.img", "size": 9437184, "sha256": "…" }
  }
  ```

- `manifest.json.sig`, next to it, is an ed25519 signature over the file's
  exact bytes, made with the release key (ADR 0015): the raw 64 bytes,
  nothing else. A signature over the bytes needs no canonical form of JSON. `pinna-image` writes both, for
  `just release` and the release workflow.
- The updater checks the signature before it reads anything from the
  manifest, and refuses a manifest without one. No release was published
  without one, so nothing depends on unsigned manifests.
- Relative file URLs resolve against the manifest's URL after redirects.
- A satellite on `stable` refuses a manifest whose `channel` is `beta`.
- An install from a channel refuses a version that isn't newer than the
  booted one. `pinnactl update <url>` and a manifest URL entered on the
  dashboard install what they are given, after the same signature checks, and
  say when it is older. Going back to the other slot stays `SetActiveSlot`.
- **Boot level.** A boot image carries the level of what it provides as
  `pinna.level=<n>` in its header's command line, next to `pinna.boot=`
  (ADR 0019); a boot image without it is level 1. A change that makes a root
  depend on something new in pinna-init or the kernel raises the level, and
  the root declares the lowest level it needs, which the manifest copies
  into `requires.boot_level`. Both are constants in the source tree and
  start at 1. The updater refuses a release when the boot image that will
  start the target slot provides less: the release's own when the updater
  writes it; the running one when the release has none and the updater
  copies it; otherwise the kernel the bootloader will load for that slot,
  which is `boot_a` while the slot keeps its kernel. The
  release is still shown, with the summary "Needs a new boot image: install
  it over USB", and Install is refused.
- **Models.** `requires.models`, when present, lists the models a release is
  for, as `board_model()` names them: `echo-dot-2`, `echo-2`. Without it a
  release is for every model. The updater refuses a release for another
  model, and the check doesn't offer it: Home Assistant shows no update, and
  the Updates page says the release is for another model. A release that hurts one model can
  then be replaced by one for the other model only; withdrawing it reaches
  every model.
- The `boot` entry lets the installer check the boot image against a signed
  list.
- Each release has notes in `releases/<version>.md`, in the style of Home
  Assistant's release notes: a summary paragraph, then what to do before
  updating, highlights, fixes and the full list of changes. The project's
  `release-notes` skill drafts them from the commits since the previous
  release (for a stable release, the previous stable one); the owner reviews
  and commits them before tagging. The release workflow refuses a tag
  without them. Their first paragraph, at most 255 characters, is the
  manifest's `summary`; the file is the GitHub release's text, which
  `notes_url` points to. `just release` on a developer's computer leaves
  `notes_url` out.

### The browser installer

- The installer fetches the newest release from the release service
  (`stable`, or `beta` if the user chooses it). It checks the manifest's
  signature with WebCrypto and each file's size and SHA-256 against the
  manifest before writing it. The boot image is then checked against a signed
  list for the first time.
- Picking downloaded files stays as ADR 0016 describes it, for builds of
  one's own.

### Development

- Contributors don't have the project key (ADR 0015). What they change decides
  how it reaches a satellite:

  | You change | How it reaches the satellite |
  | --- | --- |
  | pinnad and the other daemons | `just push` over SSH: an overlay in `/var`, also on a satellite installed from a release. No key needed |
  | The dashboard | Nothing: during development it runs on the developer's computer, against a satellite's API (`PINNA_URL`) |
  | The root image: packages, units, configuration | Signed with a development key the satellite trusts (ADR 0025), then installed over the network as below |
  | The kernel or pinna-init | USB, as for everyone (ADR 0003) |

- `[update] manifest = "<url>"` in `pinnad.toml` replaces the channel. The
  Updates setting then shows that URL instead of a channel (in Home
  Assistant, as the select's only option), and the satellite checks it on
  the same schedule. It offers the manifest when its version is
  newer than the booted one; development versions sort like releases
  (Versions).
- `just release` writes `manifest.json` and its signature to
  `build/release/`. Served with `python3 -m http.server`, as today, they give
  a developer's VM or satellite the whole flow, the update entity in Home
  Assistant included, signed with the developer's key.
- The Worker runs locally with `npx wrangler dev`. Its tests run with
  `node --test` against recorded GitHub answers, without a Cloudflare or
  GitHub account.

## Consequences

- Satellites contact a service of the project by default, once a day and
  after each start. The service learns what any web server learns: the
  address a request comes from, when, and the channel it asks for. The
  Worker's logs keep these for a few days at Cloudflare (`observability` in
  `wrangler.toml`), and the goals page says so.
- Owners see a new release on the dashboard and in Home Assistant and install
  it with one button. Nothing installs without them.
- The project runs a Worker and keeps a Cloudflare account and the domain. If
  the Worker is down, satellites report a failed check and nothing else
  changes. A GitHub download URL doesn't replace it: GitHub redirects its
  downloads to storage addresses without the file names, so the signature
  and the images can't be found next to the manifest. A copy of a release on
  any other web server works, as `just release` output does.
- The release key now signs manifests too. A leaked key is as bad as before.
- Raising the boot level stops network updates for satellites whose
  bootloader keeps the slot's kernel and whose kernel is older, for that
  release and every one after it, until they get a new boot image over USB.
  It is raised only when there is no other way.
- The installer no longer needs the user to download files first.
- This ships before the first release. A satellite installed from a release
  without it never checks, and its updater accepts unsigned manifests.
- Release tags must be semantic versions. The website's examples with date
  versions change when this ships.
