---
title: Use the dashboard
description: Open the device's dashboard in a browser to see its state, change settings, install updates, and read its network details and log.
---

The device serves a small web page, the dashboard, on port 80. It shows what
the device is doing and lets you change most of what Home Assistant can
change. It doesn't change the **Listening direction** switch, the ring's colour
and effect, or the **Reply volume**. It also does what Home Assistant can't:
it installs updates, restarts the device, shows the network details and the
log, and runs the ring's test pattern. It works without an internet
connection: the page and its fonts come from the device itself.

## Open the dashboard

Open one of these addresses in a browser on the same network as the device:

- `http://pinna.local/`, if your computer resolves `.local` names. macOS, iOS,
  Windows 10 and later, and most Linux desktops do. On Android, use the IP
  address if the name doesn't work.
- `http://<address>/`, with the device's IP address from your router.
- In Home Assistant, open the device under **Settings > Devices & services >
  ESPHome** and select **Visit**.
- Over the USB cable: `http://10.42.0.1/`. This works when the Wi-Fi doesn't.

Every Pinna device has the hostname `pinna`. With two devices on the network,
mDNS renames one of them by adding a number, for example to `pinna2.local`. To
tell them apart, use their IP addresses or the **Visit** link in Home
Assistant.

## What the dashboard shows

On a computer the sections are in a sidebar, with the device at its foot; on a
phone they are in a bar at the bottom.

| Section | What you can do |
| --- | --- |
| **Overview** | See the device with its light ring, what it is doing, and its connection to Home Assistant, Wi-Fi, version and uptime at a glance. Set the volume, turn the microphones and the wake word on or off, start or stop a request, clear a waiting [notification](/docs/ring-and-buttons/#notifications), and see the timers. **Identify** runs the ring's test pattern, to find the device among others |
| **Voice & sound** (**Voice** in the phone's bar) | Turn the wake word on or off, choose the **Wake word** and **Second wake word** while Home Assistant isn't connected, turn the microphones on or off, and set the volume and the ring brightness. **Sensitivity** opens the **Wake word** page, where you [tune the wake word](#tune-the-wake-word). Home Assistant shows the same settings, and a change in one shows in the other. The page also shows what each ring state means |
| **Home Assistant** | See whether Home Assistant is connected and has a voice pipeline for the device. Copy the host and port to add the device by hand |
| **Network** | See the Wi-Fi network, the signal strength, the addresses and the hostname. If the Wi-Fi doesn't work, the page says which of the [troubleshooting](/docs/troubleshooting/#the-device-has-no-wi-fi) cases applies |
| **System** | See which version runs from which slot, install an update and follow its steps, restart the device, and see its details. See [Install an update from the dashboard](/docs/update/#install-an-update-from-the-dashboard). **Logs**, under System, shows the log of `pinnad` as it is written. Debug lines are left out, because they can contain what was said |

You change the Wi-Fi network over SSH, not in the dashboard, and the dashboard
only offers the wake words whose models the image ships. See
[Configure Pinna](/docs/configure/).

## Tune the wake word

**Voice & sound > Sensitivity** opens the **Wake word** page. The slider sets
how easily the device wakes. Its hint shows the score that the setting needs.
The score is how sure the device is that it heard the wake word, from 0 to 1.
While you move the slider, the hint and the entries show what the new value
does.

**Recent wakes** lists the last 20 wakes and near misses. A near miss is a
sound that scored at least 0.20 but didn't wake the device; it may have
reached the setting and been rejected for another reason. Each entry shows
when it happened, its score, and what that score means at the current setting:

- **Woke**: the entry says which sensitivity ignores that score. If it wasn't
  you, select **Set to**.
- **Almost woke**: the entry says which sensitivity catches that score. If it
  was you, select **Set to**.

After the change, the same button says **Back to** and the old value. Select
it to undo the change.

The entry also says what else the new setting changes, for example "But it
also ignores 2 other wakes listed here". Check the times of those entries. If
ignoring a false wake also ignores your own wakes, or catching your near miss
also catches the TV, no setting fixes both. Move the device away from the TV or
the radio instead.

An entry without a button says why:

- **Ignored at this setting** or **Wakes at this setting**: the current
  setting already does what you want.
- **Too faint for any setting**: even 100 doesn't catch it. Speak closer, or
  with less noise around.
- **High enough at the time, but too quiet against the background or too short
  to count**: the device didn't accept it. Speak closer or louder, with less
  noise around.
- **Every setting wakes on this**: the score is 0.85 or more. If it wasn't
  you, move the device away from the TV or the radio, or turn off the wake word
  while it plays.

The list holds scores only, not what was said. It records only while the
device listens for the wake word. It starts again when `pinnad` or the device
restarts.

### Calibrate at the device

Select **Calibrate**, next to the slider, to tune the sensitivity from where
you usually stand, using the buttons on the device. The volume buttons become
feedback buttons while calibration runs: the ring shows the setting as a green
arc.

1. Say the wake word. When the device hears it, the ring flashes green and the
   wake sound plays, if you turned it on. Nothing else happens: no request
   starts, and a false wake costs nothing.
2. If it missed you, press **+** within 15 seconds. The sensitivity rises just
   enough to catch what it almost heard.
3. If it woke on something that wasn't you, such as the TV, press **−**, also
   within 15 seconds. The sensitivity drops just enough to ignore that.

The panel under the slider lists what each press did, for example
`+ raised the sensitivity 68 → 90, to catch a near miss at 0.48`. If a **+**
and a **−** would undo each other, because your wake and the false one score
about the same, the second press changes nothing: the ring flashes red at the
device, and the panel says why. No setting settles both, so move the device
away from the sound instead.

Select **Done**, press the Action button, or leave it 5 minutes without a
press to finish. Muting the microphones or turning the wake word off ends it
too. **Back to 68** returns the sensitivity to the value it had when you
started. Home Assistant has the same switch, **Wake word calibration**.
Calibration won't start while the microphones or the wake word are off, or
while a request or a timer alarm is active. It also works before Home
Assistant connects, because a calibration wake starts no request.

## Who can use the dashboard

The dashboard has no login. **Anyone on your network can use it.** That
includes restarting the device and installing an update. Only an update that
is signed with your release key installs, and the device goes back to the
previous version if the new one doesn't start. An older release that you
signed installs too, so someone with a copy of it can downgrade the device.

To turn the dashboard off, set `listen` to an empty string under `[web]` in
`/var/lib/pinna/pinnad.toml`. If the file has no `[web]` section yet, add one:

```toml
[web]
listen = ""
```

Then restart `pinnad`. See [Change a setting](/docs/configure/#change-a-setting).

```sh
systemctl restart pinnad
```

`pinnactl` and the boot health check keep working, because they use the
control API on `127.0.0.1:7470`.

## Open the dashboard by another name

The dashboard answers to IP addresses, to `localhost`, to names that end in
`.local` and to the hostname `pinna`. That stops other web sites from reaching
it through your browser. If you open it by another name, for example one that
your router's DNS gives the device, the browser shows a short message from the
device, such as
`This satellite does not answer to the name "kitchen.home.arpa".` Add the name
under `[web]` in `/var/lib/pinna/pinnad.toml`:

```toml
[web]
allowed_hosts = ["kitchen.home.arpa"]
```

Then restart `pinnad`:

```sh
systemctl restart pinnad
```

## What's next

- [Update Pinna](/docs/update/)
- [pinnactl and the control API](/docs/control-api/)
