---
title: Use the dashboard
description: Open the device's dashboard in a browser to see its state, change settings, install updates, and read its network details and log.
---

The device serves a small web page, the dashboard, on port 80. It shows what
the device is doing and lets you change what Home Assistant can change, and
more. It works without an internet connection: the page and its fonts come
from the device itself.

## Open the dashboard

Open one of these addresses in a browser on the same network as the device:

- `http://pinna.local/`, if your computer resolves `.local` names (macOS,
  most Linux desktops, Windows 10 and later, iOS and Android).
- `http://<address>/`, with the device's IP address from your router.
- In Home Assistant, open the device under **Settings > Devices & services >
  ESPHome** and select **Visit**.
- Over the USB cable: `http://10.42.0.1/`. This works when the Wi-Fi doesn't.

If two devices are both called `pinna`, mDNS renames one of them, for example
to `pinna-2.local`.

## What the dashboard shows

| Section | What you can do |
| --- | --- |
| **Overview** | See the light ring, the phase and the connection to Home Assistant. Set the volume, turn the microphones and the wake word on or off, start or stop a request, and see the timers and the device details |
| **Settings** | Set the wake word sensitivity and the ring brightness. Home Assistant shows the same settings, and a change in one shows in the other |
| **Updates** | See which version runs from which slot. Install an update and restart the device. See [Install an update from the dashboard](/docs/update/#install-an-update-from-the-dashboard) |
| **Network** | See the Wi-Fi network, the signal strength and the addresses. If the Wi-Fi doesn't work, the page says which of the [troubleshooting](/docs/troubleshooting/#the-device-has-no-wi-fi) cases applies |
| **Logs** | Read the log of `pinnad` as it is written. Debug lines are left out, because they can contain what was said |

You change the Wi-Fi network and the wake word over SSH, not in the dashboard.
See [Configure Pinna](/docs/configure/).

## Who can use the dashboard

The dashboard has no login. **Anyone on your network can use it.** That
includes restarting the device and installing an update. Only an update that
is signed with your release key installs, and the device goes back to the
previous version if the new one doesn't start.

To turn the dashboard off, add this to `/var/lib/pinna/pinnad.toml` on the
device and restart it:

```toml
[web]
listen = ""
```

`pinnactl` and the boot health check keep working, because they use the
control API on `127.0.0.1:7470`.

## Open the dashboard by another name

The dashboard answers to IP addresses, to `localhost`, to names that end in
`.local` and to the device's hostname. That stops other web sites from
reaching it through your browser. If you open it by another name, for example
one that your router's DNS gives the device, the dashboard shows
`This satellite does not answer to the name`. Add the name to
`/var/lib/pinna/pinnad.toml`:

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
