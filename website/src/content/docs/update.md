---
title: Update Pinna
description: Install a new version of Pinna on a running device, and how the device falls back if the new version doesn't work.
---

Pinna keeps two copies of its system, called slots A and B. Each holds a
kernel and a root filesystem. An update goes into the slot that isn't running.
The device then tries the new version, and if the new version doesn't work, it
goes back to the old one on its own, kernel included.

You install updates from the [dashboard](#install-an-update-from-the-dashboard),
from [Home Assistant](#install-an-update-from-home-assistant), or in standalone
mode [by asking the satellite](#ask-the-satellite-to-update). The satellite
can also [install updates by itself](#install-updates-automatically), in a
window that you choose.

On the Echo Dot (5th gen), the first update installs into slot B, so it
replaces TWRP. See [The way back](/docs/flash/#the-way-back) before you
install it.

## How an update works

1. The satellite checks the signature of the release, then downloads the new
   boot image and root filesystem and writes them to the slot that isn't
   running. It checks each download against its checksum. After writing, it
   reads the boot image back, and checks the root filesystem's signature and
   its hash tree, which lets the device detect any changed block.
2. On the next boot, the bootloader starts the new slot **on trial**, with
   three attempts. The bootloader counts them itself, so a kernel that doesn't
   start uses up attempts too.
3. When the new system is up, Pinna checks itself: its services must answer
   within five minutes. If the check passes, the slot is marked **good** and
   stays in use.
4. If the check fails or doesn't finish in time, the device restarts. Each
   boot on trial uses one attempt. When no attempts are left, the bootloader
   starts the previous slot, its kernel and its root filesystem, and the new
   slot counts as **unbootable**. A version that fails before its system
   runs, for example a root filesystem that Pinna can't verify or mount,
   doesn't wait for its attempts: the device marks its slot unbootable and
   restarts into the other one straight away.

Pinna only installs releases that are signed with the key built into the
device: the project's key in a release, your own key in a build of your own.
Build your updates with the same signing key that you used for the first
installation.

## How the satellite finds updates

The **Updates** setting chooses where the satellite looks for new releases:

| Setting | What the satellite does |
| --- | --- |
| **Stable** | Looks for finished releases. Release images start with this setting |
| **Beta** | Also looks for betas, releases before they are finished, and moves on to a finished release when that is newer. A beta from the browser installer starts with this setting |
| **Off** | Doesn't look. Builds of your own start with this setting |

With **Stable** or **Beta**, the satellite asks `updates.getpinna.dev` for
the newest release on its channel: once after it starts, as soon as its clock
is set, then once a day at a time it picked at random, when you change the
setting, when you select **Check now**, and when you ask the satellite in
standalone mode. It fetches the release's manifest
and its signature, nothing else, and checks the signature. Its request carries
no identifier. Nothing is downloaded or installed until you install it, or
until your install window, if you chose
[automatic installs](#install-updates-automatically).

A newer release then shows on the dashboard's **System > Updates** page and in Home
Assistant. The satellite never goes back to an older version on its own, and a
satellite on **Stable** never installs a beta.

## Before you begin

- The device runs Pinna, and its current slot is marked **Good** on the
  dashboard's **System > Updates** page. Pinna refuses to install an update while the
  running slot is still on trial.
- The dashboard has an **Updates** setting. If it doesn't, the satellite runs
  a build from before update channels: install a current build first, with
  its **Release manifest** field, as in
  [Install a build of your own](#install-a-build-of-your-own). On such a
  build the field is on **System** itself, not on an **Updates** page.
- The device runs a Pinna built on or after 17 November 2026. An older one
  keeps its own slot record, which the new version doesn't read: a developer
  moves it over once, as in
  [Move a device from an older Pinna](/docs/releases/#move-a-device-from-an-older-pinna).

## Install an update from the dashboard

1. In the [dashboard](/docs/dashboard/), open **System > Updates**. Set
   **Updates** to **Stable**, or to **Beta** for betas.
2. Select **Check now** to look at once. A newer release shows below it, with
   a summary and a link to its release notes.
3. Select **Install**. The satellite installs the release that the page shows,
   and no other: if a newer one appeared since, the page asks you to look
   again.

   The page shows each step. The device keeps running the update if you close
   the page; open **System > Updates** again to see where it is. If the update fails,
   the page says **The update was not installed** and gives the reason, and
   **Output on the satellite** shows every line. The device keeps running the
   current version.
4. When the update is installed, select **Restart now**. The page reconnects
   when the device is back, and shows the new version on trial.

   To start it later instead, select **Start it later**. The update stays
   in its slot as **Not started yet**, and **System > Updates** offers **Start on next
   restart**, which makes it start on the next restart without downloading it again.
5. Wait until the new slot is marked **Good**, within 5 minutes of the restart.
   You can install the next update after that.

If the new version doesn't start, the device goes back to the previous one.
**System > Updates** then says, for example, `0.4.1 failed to start and was rolled
back.`, shows the new slot as **Can't boot**, and doesn't offer that version
again. A newer release is offered as usual. See
[Recover a device](/docs/recovery/) if the device doesn't come back at all.

**System > Updates** says why **Install** isn't there when an update can't be
installed:

- **Needs a new boot image: install it over USB.** The release needs a newer
  kernel or `pinna-init` than the one that would start it. See
  [Update the boot image](#update-the-boot-image).
- **The running version is still on trial.** Wait until it is marked **Good**.
- **A development overlay is active.** Programs that a developer put on the
  device with `just push` would hide the new release's programs. Remove them
  first, as in [Try a change on a device](/docs/development/#try-a-change-on-a-device).

A release published for another model isn't offered at all: **System > Updates** says
which models it is for, and Home Assistant shows no update.

## Ask the satellite to update

In [standalone mode](/docs/standalone/), you can ask the satellite which
version it runs and whether there is an update, such as "is there an update?".
If the check takes more than a few seconds, the satellite says that it goes on:
ask again a minute later, such as "what did the update check find?". While
**Updates** is **Off**, the satellite still says which version runs, but it
doesn't check: choose **Stable** or **Beta** under **System > Updates** first.

The satellite can also install the release that the last check found. This is
off until you turn it on:

1. In the dashboard, select **Assistant**, then **Tools**.
2. Under **Pinna's tools**, turn on **Install update**.

Then ask, such as "install the update". The satellite installs the release into
the other slot, as **Install** on **System > Updates** does, and restarts the next time
it is Idle, with no timer set and nothing playing. If the new version doesn't
work, the satellite goes back to the previous one on its own. To go back by
hand, see [Go back to an older version](#go-back-to-an-older-version).

- The brain is told to install only when you ask, but nothing else stops it
  while **Install update** is on. Turn it off again if you don't want that.
- It never installs an older version, and where **System > Updates** can't install the
  release, the satellite refuses and says why. Switching to the other slot by
  hand and going back stay on the dashboard.
- With Hermes or OpenClaw as the brain, the agent can say the version and check
  for updates only if it uses
  [Pinna's MCP address](/docs/ai-agents/#connect-an-agent-over-mcp). It
  can't install an update: install it from the dashboard.

## Install an update from Home Assistant

The **Firmware** update entity shows the running version and the newest
release on the satellite's channel, with the release's summary and a link to
its notes. **Install** installs that release, and Home Assistant shows its
progress.

- Home Assistant has no restart button for the satellite, so after an install
  from Home Assistant the satellite restarts on its own once it is Idle, with
  no timer set and nothing playing. If it is Idle when the install finishes,
  it restarts at once. To choose when it restarts, install from the
  dashboard.
- **Skip** belongs to Home Assistant; the satellite doesn't store it.
- Until a check has succeeded since the satellite started, and while
  **Updates** is off, the entity's state is unknown: the satellite doesn't
  know whether it is up to date. A failed check doesn't change the entity.
- When the update can't be installed, the entity's summary says why, as on
  the dashboard. **Install** then does nothing.
- The **Updates** select in Home Assistant is the same setting as on the
  dashboard.

## Install updates automatically

With **Install updates** set to **Automatically**, the satellite installs a
new release by itself, at a time inside a window that you choose. The setting
is **Manually** until you change it.

1. In the [dashboard](/docs/dashboard/), open **System > Updates**. In the **Updates**
   group, check that **Updates** is **Stable** or **Beta**: nothing installs by
   itself while it is **Off**.
2. In the same group, set **Install updates** to **Automatically**.
3. Set the **Install window**: a start and an end time. The times are local
   time in the satellite's time zone ([Set the time zone](/docs/settings/#set-the-time-zone)).
   The window is 03:00 to 05:00 until you change it, and it may cross
   midnight, for example 23:00 to 01:00. The satellite doesn't accept a window
   whose start and end are the same time.

Home Assistant has the same settings: the **Install updates** select, and the
**Install window start** and **Install window end** times. See
[Home Assistant entities](/docs/entities/#controls).

When a check has found a release, the satellite:

1. Picks a random minute inside the window, so that satellites don't all
   install at once. It uses the window that is open now, if the minute is still
   ahead, and the next window otherwise.
2. Checks again at that minute that the release can still be installed, then
   installs it into the other slot, as **Install** does.
3. Restarts when it is Idle, with no timer set and nothing playing, and only
   inside the window. If the window ends first, the restart waits for the next
   window. **Restart now** on the dashboard works at any time.

The new version starts on trial, and the satellite goes back to the previous
one if it doesn't start, as for any update.

### What never installs automatically

- A release that needs a new boot image. **System > Updates** shows it with **Needs a
  new boot image: install it over USB**, and it waits for you.
- A version that failed to start and was rolled back.
- A version that is already in the other slot, for example one you kept back
  with **Start it later** or left there when you went back to an older
  version. That was your choice. A newer release installs over it in its
  window.
- A version whose automatic install failed three times in a row, for example
  because the download failed. The satellite logs that it skips the version.
  It tries again when you install that version by hand, or when a newer
  release appears.

Nothing installs automatically while the satellite's clock isn't set, while the
running version is still on trial, while another install runs, while
**Updates** is **Off**, or while a developer's own update address replaces the
channel ([Offer your own builds as updates](/docs/releases/#offer-your-own-builds-as-updates)).
The version number doesn't decide: a new major version, or before 1.0 a new
minor version, installs like any other.

If an automatic download fails, **System > Updates** shows the failure under **Last
install**, as for an install that you started. The next window tries again.

An install that you start yourself ignores the window: from the dashboard,
with Home Assistant's **Install** button, from a Home Assistant automation, or
by asking the satellite.

### Turn automatic installs off

Set **Install updates** to **Manually**, on the dashboard or in Home
Assistant. An install that was planned and hasn't started is cancelled. An
install that is already running finishes, and then waits for you to select
**Restart now**, as an install from the dashboard does.

An automatic install that has already finished, and waits for the window to
restart, then waits for you too: select **Restart now**, or **Start it
later** to keep it back.

## Go back to an older version

Going back is your choice; the satellite never does it on its own. Your
settings stay: the older version keeps the settings it doesn't know, and they
apply again after the next update.

- When the other slot holds the older version, switch to it, as in
  [Switch to the other version](#switch-to-the-other-version). When that
  version is also the channel's release, **System > Updates** offers **Start …** next
  to it, which does the same.
- When the channel's release is older than the running version, for example
  after you changed **Updates** from **Beta** to **Stable**, **System > Updates** offers
  **Install … (older)**. It asks before it installs. Then select **Restart
  now**.

While a [dashboard password](/docs/dashboard/) is set, the dashboard warns you
before it starts an older version: "The version in the other slot is older, and
may not ask for a password." An older Pinna keeps the password's entries but
doesn't check them, so the dashboard, `/mcp` and the metrics are open until the
next update. Pinna can't tell which older versions check the password, so it
warns for each one. The dashboard shows this warning wherever it would start an
older version.

After you go back, the newer version is still in the other slot, and the next
check offers it again: **System > Updates** shows **Start on next restart**, and Home Assistant shows
it as an update. Home Assistant's **Install** would start it and restart the
satellite, and so would the satellite in standalone mode if you ask it to
install while **Install update** is on. To stay on the older version, select
**Skip** in Home Assistant and turn off **Install update**, or set **Updates**
to **Off**. Automatic installs don't start the version in the other slot, but
the next newer release installs over it in its window. To stay on the older
version, set **Install updates** to **Manually** too.

## Switch to the other version

The slot that isn't running holds the previous version, or an update that you
kept back (**Not started yet**). To run it:

1. In the dashboard, open **System > Updates**. If it says **This version is on trial**,
   wait until the running slot is marked **Good**, within 5 minutes of the
   last restart.
2. Under **Software**, select **Switch…** next to **Switch to slot A** or
   **Switch to slot B**. There is no **Switch…** when the other slot is empty
   or marked **Can't boot**.
3. Select **Switch and restart**. The page reconnects when the device is back,
   and shows that version on trial.

The other version starts on trial, like an update. If it doesn't start, the
device goes back to the version that ran before.

If a dashboard password is set and the other version is older, the page warns
you that it may not ask for a password; see
[Go back to an older version](#go-back-to-an-older-version).

Versions built before the switch was added don't have **Switch…**. If you
switch to one of them, install an update to leave it.

## Install a build of your own

You install builds of your own this way. A satellite without internet access
installs releases the same way. You serve the release
folder from your computer, and the satellite downloads it. Before you begin,
the device must reach your computer over Wi-Fi or over the USB cable.

1. On your computer, build the update files:

   ```sh
   just release
   ```

   The files are in `build/release/`: the root image, the boot image,
   `manifest.json` and its signature `manifest.json.sig`. `just release`
   replaces them each time, so copy a release that works to a safe place.
   A release from GitHub has the same files.

2. Serve the folder over HTTP, on your computer:

   ```sh
   python3 -m http.server 8000 --directory build/release
   ```

3. In the dashboard, open **System > Updates**, open **Install from a manifest URL**,
   check the address, and select **Install**. The address is filled in with
   port 8000 on the computer or phone that you opened the dashboard on, or
   with the last address you used, such as
   `http://192.168.1.20:8000/manifest.json`. Over the USB cable, your
   computer's address is in the range `10.42.0.10` to `10.42.0.29`.

   An install from an address installs what it is given after the signature
   checks, an older version too, and says when it is older. Someone who can
   use an open dashboard and has a copy of an older signed release can
   downgrade the device this way.
4. When it is installed, select **Restart now**.
5. Wait until **System > Updates** shows the new slot as **Good**, within 5 minutes of
   the restart.

A build of your own accepts only images signed with your key, so releases from
`updates.getpinna.dev` don't install on it. To have the satellite find your
builds on its own, the way it finds releases on a channel, see
[Offer your own builds as updates](/docs/releases/#offer-your-own-builds-as-updates).
To install from the satellite's command line instead, see
[Install an update over SSH](/docs/releases/#install-an-update-over-ssh).

## Update the boot image

An update writes the boot image, and with it the kernel, into the slot it
installs, so you don't need fastboot for it. The exception is an older Kaeru,
the bootloader addition that the unlock installs (see
[What you need](/docs/requirements/)): Kaeru builds from before 22 October 2026
load the kernel from `boot_a` even when they start slot B.

Pinna finds out which kind it has. The first update after an installation
goes into slot B. When slot B starts and passes its health check, Pinna knows
whether the kernel came from `boot_b`:

- If it did, later updates into slot A write `boot_a` too.
- If it didn't, updates leave the kernel alone and say so: `the kernel stays:
  this bootloader loads boot_a whichever slot it starts`.

Until Pinna knows, an update into slot A keeps the kernel in `boot_a`, because
a new kernel there might have no other kernel to fall back to.

With an older Kaeru, install new boot images from fastboot, on your computer:

```sh
scripts/flash-gen2.sh boot
```

This writes the boot image to `boot_a` and `boot_b` and doesn't change which
slot starts. See [Install Pinna](/docs/flash/#install-the-boot-image).

A release that needs something new from the kernel or `pinna-init` says so in
its manifest (`requires.boot_level`). When the boot image that would start it
is older, the satellite shows the release with **Needs a new boot image:
install it over USB** and doesn't install it. Download the release's files,
start the satellite in fastboot, and run `./flash-gen2.sh boot` in that
folder, on your computer, as in [Install Pinna](/docs/flash/). Then install
the update.

## What's next

- If an update doesn't come up, see
  [Recover from a failed update](/docs/recovery/#recover-from-a-failed-update).
- For the technical details of slots and verification, see
  [Architecture](/docs/architecture/).
- For the update manifest and installing over SSH, see
  [Releases and updates](/docs/releases/).
