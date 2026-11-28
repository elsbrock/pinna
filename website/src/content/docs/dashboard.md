---
title: Set up the satellite
description: Open the satellite's dashboard in a browser, set up a new satellite, and find your way around the dashboard's pages.
---

The dashboard is a web page that the device serves on your network. It is
where you set up and control the satellite: its name, Wi-Fi, the wake words,
who answers requests, your services in standalone mode, music sources and
updates. You need no app and no account, and you don't need Home Assistant. It
works without an internet connection: the page and its fonts come from the
device itself.

If you use Home Assistant, it shows many of the same settings, and a change in
one shows in the other. A few things belong to Home Assistant:

- In Home Assistant mode, while Home Assistant is connected, it chooses the
  wake words.
- Only Home Assistant raises a [notification](/docs/ring-and-buttons/#notifications);
  the dashboard clears it.
- Only Home Assistant plays announcements.

## Open the dashboard

Open one of these addresses in a browser on the same network as the device:

- `http://pinna-2a914e.local/`, with the device's name on the network: until
  you [change it](/docs/settings/#change-the-hostname), `pinna-` and the last six characters
  of its MAC address. Your router lists
  it, Home Assistant shows it when it discovers the device, and the dashboard
  shows it under **System** as **Node name**. This works if your computer
  resolves `.local` names; macOS, iOS, Windows 10 and later, and most Linux
  desktops do. On Android, use the IP address if the name doesn't work.
- `http://<address>/`, with the device's IP address from your router.
- In Home Assistant, open the device under **Settings > Devices & services >
  ESPHome** and select **Visit**.
- Over the USB cable: `http://10.42.0.1/`. This works when the Wi-Fi doesn't.
- Over its setup hotspot, while the device has no saved Wi-Fi network:
  `http://10.42.1.1/`. See [Connect to Wi-Fi](/docs/flash/#connect-to-wi-fi).

Each device starts with its own name on the network. If you change it, give
each device a different one.

## Set up a new satellite

1. [Open the dashboard](#open-the-dashboard). Over the USB cable or the setup
   hotspot, first [connect the device to Wi-Fi](/docs/flash/#connect-to-wi-fi)
   on **Network**.
2. The dashboard offers a password before anything else. Select **Set a
   password** and press **Action** on the device when the ring circles white,
   or select **Leave it open** if you trust everyone on your network. See
   [Who can use the dashboard](#who-can-use-the-dashboard).
3. [Name the device](/docs/settings/#name-the-device) on **System**, for example `Kitchen`.
   Do this before you add it to Home Assistant, which keeps the first name in
   its entity IDs.
4. [Set the time zone](/docs/settings/#set-the-time-zone) on **System**, so that the
   satellite tells your time in standalone mode, and its own clock and log
   use your zone.
5. Choose who answers requests, on **Assistant**:
   [connect the device to Home Assistant](/docs/home-assistant/), or
   [set up standalone mode](/docs/standalone/). Until Home Assistant connects
   or you switch to standalone mode, **Overview** shows how to do either.

## What the dashboard shows

On a computer the sections are in a sidebar, with the device at its foot. The
open section lists its pages under it, such as **Services**, **Conversation**,
**Tools** and **Satellite group** under **Assistant**, and **Updates**,
**Password and tokens** and **Logs** under **System**. On a phone the
sections are in a bar at the bottom, and a section's pages are rows on it.

The button next to the logo changes the theme. From **System**, it switches
first to the opposite of what your system shows, then to the other one, then
back to **System**; its tooltip says which comes next. On a phone, **Theme**
is on the **System** page, under **This browser**. The browser keeps the choice for each
dashboard address, so another satellite starts with its own.

| Section | What you can do |
| --- | --- |
| **Overview** | See the device with its light ring lit as it is right now, what it is doing, and its connection to Home Assistant (in standalone mode, whether its services are ready), Wi-Fi, version and uptime at a glance. Set the volume, turn the microphones and the wake word on or off, start or stop a request, clear a waiting [notification](/docs/ring-and-buttons/#notifications), and see the timers and how many messages from the [satellite group](/docs/satellite-groups/#pass-messages-between-satellites) wait to be said. **Now playing** shows what plays and where it comes from, with pause, resume and stop, and **Mute music**; with nothing playing, it says how to play something. **Identify** runs the ring's test pattern, to find the device among others |
| **Voice & sound** (**Voice** in the phone's bar) | Turn the wake word on or off, choose the **Wake word** and **Second wake word** while Home Assistant isn't connected or in standalone mode, turn the microphones on or off, set the volume and the [reply volume](/docs/audio/#volume) (turn **Replies follow Volume** off first), and turn the **Wake sound** on or off. **Ring** sets the ring brightness, turns **Listening direction** on or off, [uses the ring as a light](/docs/ring-and-buttons/#use-the-ring-as-a-light), runs the ring's test pattern with **Identify**, and shows what each ring state means. **Media sources** turns [AirPlay](/docs/airplay/), [Bluetooth](/docs/bluetooth/) and [Sendspin](/docs/sendspin/) on or off, shows whether they run, pairs phones and forgets them, shows which Music Assistant Sendspin is connected to and whether it plays in step, and [plays music from a URL](/docs/music/#play-music-from-a-url). **Wake words** lists the words on offer and [adds your own](/docs/wake-words/#add-your-own-wake-word). **Sensitivity** opens the **Wake word sensitivity** page, where you [tune the wake word](/docs/wake-words/#tune-the-wake-word). Home Assistant shows the same settings, and a change in one shows in the other |
| **Assistant** | Choose who answers requests: Home Assistant or the satellite itself, in [standalone mode](/docs/standalone/). In Home Assistant mode, see whether Home Assistant is connected and has a voice pipeline for the device, and whether the connection is encrypted, and see the device's name and copy the host and port to add the device by hand. **Let Home Assistant add this satellite** [opens the window](/docs/home-assistant/#add-a-device-that-has-a-dashboard-password) in which Home Assistant can add a satellite that has a dashboard password: it is on this page in standalone mode, and on the **Home Assistant** page in Home Assistant mode. While a password is set, the satellite has no key from Home Assistant and Home Assistant isn't connected, **Overview** says that Home Assistant can't add the satellite until you let it, with the same button, and the **Home Assistant** page says that the satellite refuses Home Assistant until the window is open. **Services**, **Conversation** and **Tools** set up standalone mode: the speech and language services and their keys, the language, place and prompt, a box to type a question, what the brain may do, radio stations and MCP. **Satellite group**, a page of its own, puts satellites in a group, so that they [pass messages](/docs/satellite-groups/#pass-messages-between-satellites) to each other and [control each other](/docs/satellite-groups/#control-another-satellite): turn it on, add and remove members, and turn **Group messages** and **Control from the group** on or off. **Allow adding to a group** [lets another satellite add this one](/docs/satellite-groups/#add-a-satellite-that-has-a-dashboard-password) when it has a dashboard password |
| **Network** | See the Wi-Fi network, the signal strength and the addresses. [Change the hostname](/docs/settings/#change-the-hostname). [Change the Wi-Fi network](/docs/settings/#change-the-wi-fi-network) and forget saved ones. If the Wi-Fi doesn't work, the page says which of the [troubleshooting](/docs/troubleshooting/#the-device-has-no-wi-fi) cases applies. [Set a dashboard address](/docs/settings/#set-a-dashboard-address), where else the dashboard opens |
| **System** | [Name the device](/docs/settings/#name-the-device), [set its time zone](/docs/settings/#set-the-time-zone), restart it, [reset it](/docs/settings/#reset-the-device), and see its details. The rows for its pages say where each stands, such as a release that is ready or a failed install, and open them. **Updates**: choose where the device looks for updates (the **Updates** setting: Stable, Beta or Off), choose whether updates install by themselves and in which window (**Install updates**), choose whether the satellite is counted in the [public statistics](/docs/update/#include-in-the-statistics) (**Include in the statistics**), check for one and see what the last check found, see which version runs from which slot, install an update and follow its steps, and switch to the version in the other slot. See [Install an update from the dashboard](/docs/update/#install-an-update-from-the-dashboard) and [Install updates automatically](/docs/update/#install-updates-automatically). **Password and tokens**: under **Dashboard password**, [set, change and remove the password](#set-change-and-remove-the-password) and sign out; under **Tokens**, [create the tokens for Prometheus and agents](#tokens-for-prometheus-and-agents). **Logs** shows the log of `pinnad`, and of the AirPlay and Bluetooth services, as it is written, and its row says how the last start ended. Debug lines are left out, because they can contain what was said. While the log is scrolled to the bottom, it follows the newest line. Scroll up to read older lines, and select **Newest** to go back to the bottom. Select **All** or **Warnings and errors**, and type in **Filter** to show only the lines that match. **Copy** and **Download**, the two icon buttons, copy the lines shown or save them as a text file. A line names its level only when it is a warning or an error, and those lines are tinted. **Open an issue** opens the issue form on GitHub with the satellite's details filled in, and copies its log: see [Report a problem](/docs/troubleshooting/#report-a-problem). **Record audio** records up to 30 seconds of what the satellite hears, after you press **Action** on it, then lists the audio as files that you save from your browser. It needs a dashboard password. Its row says when a recording runs. See [Record what the satellite hears](/docs/troubleshooting/#record-what-the-satellite-hears). **Timeline** turns debugging on for 15 minutes, 1 hour or 8 hours and draws what the satellite does on one time axis: its phases and events, every request it sends to a service, and, with a dashboard password, the audio levels and the text of the requests. The text also needs a press of **Action**. Its row says whether debugging runs. See [Follow a request on the timeline](/docs/troubleshooting/#follow-a-request-on-the-timeline). Under **Help**, **Report a problem** opens **Logs**; **Ask a question** and **Share feedback** start a discussion on GitHub, with the version, the device and the mode below your words; **Read the docs** opens these pages. **Feedback** at the foot of the sidebar does the same as **Share feedback**. |

## Switch between satellites

With more than one device, you can move from one dashboard to the next. On a
computer, select the device at the foot of the sidebar: it opens into a list
of the other devices on the same subnet, above this one. On a phone, select
the name at the top: the list opens below it, with this device first. Each
device shows its picture and name. Devices on another subnet or VLAN aren't
listed, even if your router passes mDNS between them. Select one to open its
dashboard in the same tab.

Anyone on your network can announce a device, so a fake entry could lead to a
page that looks like a login form. Check the address in the address bar
before you enter a password on a page you opened from the list. On a
computer, your browser shows where an entry leads when you point at it.

Each device looks for the others once a minute, so the list opens at once. A
device you just turned on can take a minute to show up. One that you turn off,
or whose dashboard stops answering, leaves the list within two minutes.

The link goes to the other device's dashboard address if it has one. If not, it
goes to its `.local` name when you opened this dashboard by a `.local` name, and
to its IP address otherwise (an IPv6 address if you opened this one by IPv6),
because some browsers, such as many on Android, don't find `.local` names. If
a device's dashboard address doesn't open, type its `.local` name or its IP
address in the address bar.

A device on an older release shows in the list, but its dashboard has no list
to go back with: open it from the address bar, or update it. A device that
isn't on a network yet, only on USB, shows the list without others.

## Who can use the dashboard

Until you set a password, **anyone on your network can use the dashboard**:
change any setting, restart or reset the device, install updates, see what
plays and pause or stop the Music Assistant group it plays in over Sendspin.
In [standalone mode](/docs/standalone/) that includes your services, and
pointing a speech-to-text service at a server of someone else's, which then
receives what the microphones hear after the wake word. `/mcp` and the
[metrics](/docs/metrics/) are open the same way.

The first time you open the dashboard, it offers a password before anything
else. Choose **Set a password**, or **Leave it open** if you trust everyone on
your network. The choice is stored on the device, so other browsers don't ask
again, and **Overview** keeps saying "Anyone on your network can control this
satellite" until you set one. An update never locks you out: a device that
updates into this version asks too, and you can leave it open.

You can't turn the dashboard off from the dashboard. Developers can turn it off
in the [developer settings](/docs/configure/#web).

### What the password protects

With a password, every request to the device needs you to be signed in, except
the page's own files and the sign-in itself. That covers the dashboard, the
control API, and the log. The metrics and `/mcp` need
[their own tokens](#tokens-for-prometheus-and-agents) instead. The password
also:

- closes the ESPHome API to Home Assistant until you
  [open the window](/docs/home-assistant/#add-a-device-that-has-a-dashboard-password)
  for a satellite that has no key from Home Assistant yet;
- refuses to let another satellite add this one to a group until you
  [allow it](/docs/satellite-groups/#add-a-satellite-that-has-a-dashboard-password);
- refuses [messages and calls from members without a password](/docs/satellite-groups/#passwords-between-members);
- is needed to [record what the satellite hears](/docs/troubleshooting/#record-what-the-satellite-hears),
  and to record the audio levels and the text of requests on the
  [timeline](/docs/troubleshooting/#follow-a-request-on-the-timeline).

The password doesn't cover everything:

- **It travels unencrypted.** The device serves plain HTTP: it has no
  certificate that a browser trusts. Someone who can read the traffic on your
  network, for example another device on your Wi-Fi, can read the password and
  your session. Don't use a password that you use anywhere else, and don't
  use one password for every satellite. A reverse proxy with TLS in front of
  the dashboard address protects the path from your browser to the proxy. The
  hop from the proxy to the satellite stays plain HTTP over your network. See
  [Set a dashboard address](/docs/settings/#set-a-dashboard-address).
- **It isn't Home Assistant's key.** Home Assistant connects with its own
  encryption key, which you can't see or change on the dashboard.
- **It doesn't stop someone in the room.** The device's buttons work without
  it.
- **It can't stop someone from slowing sign-ins down.** After 10 wrong
  passwords in a row, the sign-in waits a minute between tries, for everyone.
  Browsers that are signed in keep working.
- **A Home Assistant iframe panel can't hold the session.** Use the **Visit**
  link on the device in Home Assistant, which opens the dashboard in its own
  tab.

An older version of Pinna in the other slot may not ask for a password. If you
[go back to it](/docs/update/#go-back-to-an-older-version), the dashboard,
`/mcp` and the metrics are open until the next update.

### Set, change and remove the password

Open **System > Password and tokens**. Under **Dashboard password**:

- **Set a password.** Select **Set a password…**. The dashboard says "Press
  the Action button (the one with the dot) on the satellite", and the ring
  circles white: press the button on the device within 60 seconds, as in
  [Confirm a change at the device](/docs/ring-and-buttons/#confirm-a-change-at-the-device).
  Press it only if you started this yourself. Select **Cancel** to stop the
  ring at once. Then enter the password, 8 to 128 characters, with no other
  rules. A password that starts or ends with a space is allowed, and the form
  says so. The confirmation lasts 5 minutes: after that the form says "The
  confirmation expired." and **Confirm again** asks for another press, and
  keeps what you typed. If an agent or Prometheus called the
  device in the last 24 hours, the form says so: they need a token once the
  password is set. You can
  [create the tokens first](#tokens-for-prometheus-and-agents).
- **Change it.** Select **Change…** and enter the current password and the
  new one. Or select **Confirm at the satellite instead** and press **Action**.
- **Remove it.** Select **Remove…** and enter the current password, or confirm
  at the satellite. Removing it opens the dashboard, the control API, `/mcp` and
  the metrics to anyone on your network again.

Changing the password signs every browser out. The dashboard signs this one in
again with the new password, and says "Password set." or "Password changed.
Every other browser is signed out."

### A confirmation that someone else started

While a confirmation waits for a press of **Action**, **Overview** shows "A
browser at 192.168.1.23 is waiting for a press of Action, 41 s left. Don't
press it unless that was you." with the address of the browser that asked.
Only a signed-in browser sees it. If you didn't start it, select **Stop it**:
the ring stops at once. The browser that asked sees that the confirmation was
stopped.

A confirmation for a [recording](/docs/troubleshooting/#record-what-the-satellite-hears)
says so: "… is waiting for a press of Action to record from the
microphones …", and the ring circles in the Listening colour instead of
white. While a recording runs, **Overview** on the other signed-in browsers
shows "A browser at 192.168.1.23 is recording from the microphones: 18 s
left." with **Stop it**, which ends it at once. The browser that records
keeps what arrived and says that the recording was stopped.

After a confirmation is canceled, with **Cancel** or **Stop it**, the next one
can start 10 seconds later. After one that was confirmed or ran out, it can
start a minute later. Anyone who asks while one waits is told "A confirmation
from another browser is waiting. Press Action only if you started it." and how
many seconds to wait. The **Try again** button stays disabled while the wait
counts down.

### Sign in

On another computer, open the dashboard. The sign-in page shows the address
of the page and the satellite's name, as this browser last saw it. Enter the
password and select **Sign in**. If a sign-in fails too often, the page counts
down the seconds until you can try again.

If this browser was signed in and the password was changed or every session was
ended, the sign-in page says "You were signed out: the password was changed, or
every session was ended." If someone removes the password while the sign-in
page is open, the page opens the dashboard within 10 seconds.

### Sign out

Open **System > Password and tokens**. Under **Dashboard password**:

- **Sign out** ends the session of this browser. A session lasts 30 days, and
  survives a restart of the device.
- **Sign out everywhere** ends every session, in every browser, and closes the
  pages they had open. Use it when you signed in on a computer that isn't
  yours. Tokens keep working.
- **Failed sign-ins** shows how many wrong passwords were tried since the last
  sign-in, and the address and time of the last one.

### If you forget the password

You need to be at the device. On the sign-in page, select **Forgot the
password?** and press **Action** on the device when the ring circles white,
then set a new password. You need no cable and no reset. See
[I forgot the dashboard password](/docs/troubleshooting/#i-forgot-the-dashboard-password).

### Tokens for Prometheus and agents

With a password set, the metrics and `/mcp` don't take a session. Each has a
token that opens it and nothing else. Open **System > Password and tokens**. Under **Tokens**, select
**Create a token** next to **Metrics token** or **Agent token**. The dashboard shows the
token once, with a **Copy** button, and afterwards only its last four
characters. The token stays on the page until you select **Dismiss**, so you
can create both before you copy them. **Create a new token** asks you to
confirm, and then replaces the old one, which stops working at once. A factory
reset removes the password and the tokens.

You can create the tokens before you set a password. Give them to Prometheus
and your agents first, and they keep working once the password is set.

- Prometheus takes the metrics token as `authorization` credentials. See
  [Prometheus metrics](/docs/metrics/#who-can-read-them).
- An MCP client sends the agent token in an `Authorization: Bearer` header. See
  [Connect an agent over MCP](/docs/ai-agents/#who-can-use-the-mcp-server).

## What's next

- [Connect to Home Assistant](/docs/home-assistant/), or
  [set up standalone mode](/docs/standalone/).
- Change the satellite's name, Wi-Fi network and other
  [settings](/docs/settings/).
- [Update Pinna](/docs/update/) from the dashboard.
