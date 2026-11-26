---
title: Satellite settings
description: Rename the satellite, set its time zone, change its hostname or Wi-Fi network, set a dashboard address, and reset it.
---

You change these settings on the satellite's [dashboard](/docs/dashboard/):
the name, the time zone and a reset under **System**, and the hostname, the
Wi-Fi network and the dashboard address under **Network**.

## Name the device

Home Assistant shows the device by its name. Until you choose one, the name is
the model and the last six characters of the MAC address, for example
`Echo Dot 2a914e`. To rename the device, open **System**, change **Name** and
select **Save**. The dashboard shows the new name at once. The **Node name**
under **System**, such as `pinna-2a914e`, stays: it is the device's ID for
Home Assistant and mDNS, not the name that people see. To change it too,
[change the hostname](#change-the-hostname).

Name the device before you add it to Home Assistant. Home Assistant takes a
new name the next time it connects, and shows it on the device page and in the
device list. The device asks it to reconnect as soon as no request, reply,
announcement, music or timer alarm is playing. The entity
IDs, such as `switch.echo_dot_2a914e_wake_sound`, and the entry under
**Settings > Devices & services > ESPHome** keep the name that the device had
when you added it. A name that you give the device in Home Assistant itself
wins over the device's.

In a [satellite group](/docs/satellite-groups/#pass-messages-between-satellites),
the brain calls the satellite by this name, so choose one that is easy to
say. The other members use a new name after their next search, within about
a minute.

## Set the time zone

The device keeps UTC until you choose a zone. Open **System**, then pick one
under **Time zone**. It is the zone of the device's own clock and logs, and
takes effect at once. If your browser is in another zone than the device, the page offers
**Use** and your browser's zone. Home Assistant keeps its own time zone.

## Change the hostname

The hostname is the device's name on your network, such as `pinna-2a914e`. It is
also its node name in ESPHome, which Home Assistant uses to find it. To change
it, open **Network**, enter a name under **Hostname** and select **Save**. A
hostname is one word of letters, digits and hyphens, for example `kitchen-dot`.
Give each device a different one.

The change takes effect at once, and the device's `.local` address changes with
it: `http://kitchen-dot.local/` replaces `http://pinna-2a914e.local/`, so
bookmarks to the old address stop working. If you opened the dashboard by the
old `.local` address, the browser moves to the new one by itself. Home
Assistant follows on its own, as soon as no request, reply, announcement, music
or timer alarm is playing. Your router's device list shows the new name after
the device next joins the Wi-Fi, for example after a restart. A reverse proxy
that reaches the device by its old `.local` name stops working: point it at the
new name, or better at the device's IP address.

After a name conflict on the network, the device can answer by another
`.local` name for a few minutes, such as `kitchen-dot3.local`. **Network** then
says which name it answers to. The device takes its own name back by itself,
at most once every ten minutes, so if the conflict comes back, it keeps the
other name for a while.

## Change the Wi-Fi network

Open **Network**. **Saved networks** lists the networks that the device
joins: the first one it finds, from the top. **Forget** removes one. Forgetting
the network in use disconnects the device. If it was the only one, the device
runs its [setup hotspot](/docs/flash/#over-the-setup-hotspot), with the
dashboard at `http://10.42.1.1/`, and stays reachable over USB at
`http://10.42.0.1/`.

To join another network, select **Scan** under **Join a network**, select the
network and enter its password. For a network that doesn't show its name,
select **Join a hidden network** and type the name exactly as the router has
it. The device joins open and WPA2 networks, WPA2/WPA3 networks included. The
Wi-Fi chip of the Echo (2nd gen) and the Echo Dot (2nd gen) can't join networks
that allow only WPA3, enterprise networks or WEP networks; whether the Echo Dot
(5th gen)'s chip can is untested.

Joining is a trial, so that a mistake can't strand the device on a network you
can't reach, such as a guest network that keeps its devices apart:

1. The device tries the network for up to a minute. If the password is wrong,
   or the network gives it no address, it goes back to the network it was on,
   or, with no saved network, runs its setup hotspot again. The page says why.
2. Once it is on the new network, it keeps the network as soon as someone
   opens the dashboard at its new address: connect a phone or computer to that
   network and open the device's `.local` address, which the page shows before
   you join, or the device's address from your
   router. You can also select **Keep it** on the page, for example over
   USB.
3. Until then, every page shows the network on trial and the time left. After
   5 minutes, on **Go back**, or when the device restarts, it goes back to its
   saved networks, or runs its setup hotspot again if it has none.

If you open the dashboard over Wi-Fi or over the setup hotspot, the page loses
the device when it switches networks. Before you select **Join**, the page says
where to find it, including its MAC address to look for in your router.

## Set a dashboard address

The dashboard answers to IP addresses, to `localhost`, to names that end in
`.local` and to the device's hostname. That stops other web sites from
reaching it through your browser. If you open it by another name, for example
`https://dot.example.com/` that a reverse proxy serves, the browser shows a
short message from the device, such as
`This satellite does not answer to the name "dot.example.com".` To make the
device answer to that name:

1. Open the dashboard by the device's IP address or its `.local` name.
2. Open **Network**. Under **Dashboard address**, enter the full address, for
   example `https://dot.example.com/`, and select **Save**.

The address starts with `http://` or `https://`, has a host name or an IP
address and optionally a port, and has nothing after that: no user name,
password, path, query or fragment. The host must be one that other devices on
your network can reach, so `localhost`, loopback addresses, the USB link's
`10.42.0.x` addresses and link-local IPv6 addresses are refused. The device
stores it with a `/` at the end.
It works at once. **Remove** clears it. While the dashboard is open by the
address's own host name, it won't remove the address or change it to another
host, because the page would stop answering: open it by the IP address or the
`.local` name first.

Point the reverse proxy at the device's IP address, ideally one that your
router always gives it, rather than its `.local` name, which changes when you
change the hostname.

Other devices link to this address when you [switch between
devices](/docs/dashboard/#switch-between-satellites). Without one, they use the device's
`.local` name or its IP address. Only one address fits; more names, such as
your router's name next to a reverse proxy's, are a
[developer setting](/docs/configure/#web). Anyone who can use the dashboard
can set the address, so check it now and then.

## Reset the device

To start over, as if the device were just installed, open **System** and
select **Reset…** next to **Factory reset**. Type the device's name to confirm.
The device forgets:

- its name and every setting made on the dashboard or in Home Assistant
- its saved Wi-Fi networks
- the encryption key that Home Assistant set; a key that came with the image
  stays
- the dashboard password, its sessions, and the tokens for the metrics and
  agents
- SSH keys added on the device, and its SSH host key.

It then restarts as it was installed. Its log stays, and so does the version
of Pinna it runs. If its image doesn't set up Wi-Fi, which a prebuilt release
doesn't, the device runs its setup hotspot afterwards, with the dashboard at
`http://10.42.1.1/`, and stays reachable over USB at `http://10.42.0.1/`:
[connect it to Wi-Fi](/docs/flash/#connect-to-wi-fi) again. If Home Assistant had set its key, Home Assistant shows it as
unavailable until you add it again.

## What's next

- Choose and tune the [wake words](/docs/wake-words/).
- [Update Pinna](/docs/update/) from **System > Updates**.
