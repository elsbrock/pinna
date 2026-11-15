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

On a computer the sections are in a sidebar under the device's name; on a
phone they are in a bar at the bottom.

| Section | What you can do |
| --- | --- |
| **Overview** | See the device with its light ring, what it is doing, and its connection to Home Assistant, Wi-Fi, version and uptime at a glance. Set the volume, turn the microphones and the wake word on or off, start or stop a request, clear a waiting [notification](/docs/ring-and-buttons/#notifications), and see the timers. **Identify** runs the ring's test pattern, to find the device among others |
| **Voice & sound** (**Voice** in the phone's bar) | Turn the wake word on or off and set its **Sensitivity**, turn the microphones on or off, and set the volume and the ring brightness. Home Assistant shows the same settings, and a change in one shows in the other. The page also shows what each ring state means |
| **Home Assistant** | See whether Home Assistant is connected and has a voice pipeline for the device. Copy the host and port to add the device by hand |
| **Network** | See the Wi-Fi network, the signal strength, the addresses and the hostname. If the Wi-Fi doesn't work, the page says which of the [troubleshooting](/docs/troubleshooting/#the-device-has-no-wi-fi) cases applies |
| **System** | See which version runs from which slot, install an update and follow its steps, restart the device, and see its details. See [Install an update from the dashboard](/docs/update/#install-an-update-from-the-dashboard). **Logs**, under System, shows the log of `pinnad` as it is written. Debug lines are left out, because they can contain what was said |

You change the Wi-Fi network and the wake word model over SSH, not in the
dashboard. See [Configure Pinna](/docs/configure/).

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
