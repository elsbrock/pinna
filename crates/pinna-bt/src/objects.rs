//! What BlueZ and BlueALSA list over D-Bus (ObjectManager), as plain values.

use std::collections::HashMap;

use pinna_bridge::Track;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

pub type Props = HashMap<String, OwnedValue>;
pub type Objects = HashMap<OwnedObjectPath, HashMap<String, Props>>;

pub const ADAPTER: &str = "/org/bluez/hci0";

/// S16_LE in BlueALSA's format code: signed, little-endian, 2 bytes, 16 bits.
const S16_LE: u16 = 0x8210;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Adapter {
    pub address: String,
    pub alias: String,
    pub powered: bool,
    pub discoverable: bool,
    pub pairable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    pub path: String,
    pub address: String,
    pub name: String,
    pub paired: bool,
    /// Paired with stored keys. A phone can pair without bonding even while
    /// the adapter isn't pairable; such a pairing is never trusted or listed.
    pub bonded: bool,
    pub trusted: bool,
    pub connected: bool,
}

/// A BlueALSA PCM that a phone plays into (A2DP sink, read side).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pcm {
    pub path: String,
    pub sequence: u32,
    pub format: u16,
    pub channels: u8,
    pub sampling: u32,
    pub volume: u16,
}

fn value<'a>(p: &'a Props, k: &str) -> Option<&'a Value<'static>> {
    p.get(k).map(|v| &**v)
}

fn string(p: &Props, k: &str) -> String {
    value(p, k)
        .and_then(|v| <&str>::try_from(v).ok())
        .unwrap_or_default()
        .to_owned()
}

fn flag(p: &Props, k: &str) -> bool {
    value(p, k)
        .and_then(|v| bool::try_from(v).ok())
        .unwrap_or(false)
}

fn number<T: for<'a> TryFrom<&'a Value<'static>>>(p: &Props, k: &str) -> Option<T> {
    value(p, k).and_then(|v| T::try_from(v).ok())
}

pub fn adapter(objects: &Objects) -> Option<Adapter> {
    let (_, ifaces) = objects.iter().find(|(path, _)| path.as_str() == ADAPTER)?;
    let p = ifaces.get("org.bluez.Adapter1")?;
    Some(Adapter {
        address: string(p, "Address"),
        alias: string(p, "Alias"),
        powered: flag(p, "Powered"),
        discoverable: flag(p, "Discoverable"),
        pairable: flag(p, "Pairable"),
    })
}

/// The adapter's bonded phones, by address.
pub fn devices(objects: &Objects) -> Vec<Device> {
    let prefix = format!("{ADAPTER}/");
    let mut out: Vec<Device> = objects
        .iter()
        .filter(|(path, _)| path.as_str().starts_with(&prefix))
        .filter_map(|(path, ifaces)| {
            let p = ifaces.get("org.bluez.Device1")?;
            let address = string(p, "Address");
            let alias = string(p, "Alias");
            Some(Device {
                path: path.as_str().to_owned(),
                name: if alias.is_empty() {
                    address.clone()
                } else {
                    alias
                },
                address,
                paired: flag(p, "Paired"),
                bonded: flag(p, "Bonded"),
                trusted: flag(p, "Trusted"),
                connected: flag(p, "Connected"),
            })
        })
        .filter(|d| d.paired && d.bonded)
        .collect();
    out.sort_by(|a, b| a.address.cmp(&b.address));
    out
}

/// PCMs that phones play into, oldest connection first.
pub fn pcms(objects: &Objects) -> Vec<Pcm> {
    let mut out: Vec<Pcm> = objects
        .iter()
        .filter_map(|(path, ifaces)| {
            let p = ifaces.get("org.bluealsa.PCM1")?;
            (string(p, "Transport") == "A2DP-sink" && string(p, "Mode") == "source").then(|| Pcm {
                path: path.as_str().to_owned(),
                sequence: number(p, "Sequence").unwrap_or(0),
                format: number(p, "Format").unwrap_or(0),
                channels: number(p, "Channels").unwrap_or(0),
                sampling: number(p, "Sampling").unwrap_or(0),
                volume: number(p, "Volume").unwrap_or(0),
            })
        })
        .collect();
    out.sort_by_key(|p| p.sequence);
    out
}

/// What the phone whose PCM is `pcm` plays, as its AVRCP player names it
/// (BlueZ's `MediaPlayer1` `Track`): the player the phone addresses
/// (`MediaControl1` `Player`), otherwise its first. Empty when the phone
/// shares none.
pub fn track(objects: &Objects, pcm: &str) -> Track {
    // `/org/bluealsa/hci0/dev_XX/a2dpsnk/source` -> `/org/bluez/hci0/dev_XX`.
    let Some(dev) = pcm.split('/').find(|s| s.starts_with("dev_")) else {
        return Track::default();
    };
    let device = format!("{ADAPTER}/{dev}");
    let addressed = objects
        .iter()
        .find(|(path, _)| path.as_str() == device)
        .and_then(|(_, ifaces)| ifaces.get("org.bluez.MediaControl1"))
        .and_then(|p| match value(p, "Player")? {
            Value::ObjectPath(op) => Some(op.as_str().to_owned()),
            _ => None,
        });
    let prefix = format!("{device}/");
    let mut players: Vec<(&str, &Props)> = objects
        .iter()
        .filter(|(path, _)| path.as_str().starts_with(&prefix))
        .filter_map(|(path, ifaces)| Some((path.as_str(), ifaces.get("org.bluez.MediaPlayer1")?)))
        .collect();
    players.sort_by_key(|(path, _)| *path);
    let player = players
        .iter()
        .find(|(path, _)| Some(*path) == addressed.as_deref())
        .or(players.first());
    let Some(fields) = player
        .and_then(|(_, p)| value(p, "Track"))
        .and_then(|v| v.try_clone().ok())
        .and_then(|v| HashMap::<String, OwnedValue>::try_from(v).ok())
    else {
        return Track::default();
    };
    Track {
        title: string(&fields, "Title"),
        artist: string(&fields, "Artist"),
        album: string(&fields, "Album"),
    }
}

/// The rate to announce to pinnad, if pinnad takes this PCM's format.
pub fn stream_rate(p: &Pcm) -> Option<u32> {
    (p.format == S16_LE && p.channels == 2 && matches!(p.sampling, 44100 | 48000))
        .then_some(p.sampling)
}

/// BlueALSA's volume (left channel in the upper byte: mute bit, then
/// 0..=127, the phone's slider) as pinnad's 0..=100.
pub fn volume_percent(volume: u16) -> u8 {
    let left = (volume >> 8) as u8;
    if left & 0x80 != 0 {
        return 0;
    }
    ((u32::from(left & 0x7f) * 100 + 63) / 127) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owned(v: Value<'static>) -> OwnedValue {
        v.try_to_owned().unwrap()
    }

    fn props(pairs: Vec<(&str, Value<'static>)>) -> Props {
        pairs
            .into_iter()
            .map(|(k, v)| (k.to_owned(), owned(v)))
            .collect()
    }

    fn object(objects: &mut Objects, path: &str, iface: &str, p: Props) {
        objects.insert(
            OwnedObjectPath::try_from(path).unwrap(),
            HashMap::from([(iface.to_owned(), p)]),
        );
    }

    fn sample() -> Objects {
        let mut o = Objects::new();
        object(
            &mut o,
            ADAPTER,
            "org.bluez.Adapter1",
            props(vec![
                ("Address", Value::from("68:37:E9:41:FF:4F")),
                ("Alias", Value::from("pinna")),
                ("Powered", Value::from(true)),
            ]),
        );
        object(
            &mut o,
            "/org/bluez/hci0/dev_44_63_B6_7A_85_8B",
            "org.bluez.Device1",
            props(vec![
                ("Address", Value::from("44:63:B6:7A:85:8B")),
                ("Alias", Value::from("iPhone")),
                ("Paired", Value::from(true)),
                ("Bonded", Value::from(true)),
                ("Connected", Value::from(true)),
            ]),
        );
        object(
            &mut o,
            "/org/bluez/hci0/dev_11_22_33_44_55_66",
            "org.bluez.Device1",
            props(vec![
                ("Address", Value::from("11:22:33:44:55:66")),
                ("Paired", Value::from(false)),
            ]),
        );
        // Paired without bonding, as the kernel allows while not pairable.
        object(
            &mut o,
            "/org/bluez/hci0/dev_AA_BB_CC_DD_EE_FF",
            "org.bluez.Device1",
            props(vec![
                ("Address", Value::from("AA:BB:CC:DD:EE:FF")),
                ("Paired", Value::from(true)),
                ("Bonded", Value::from(false)),
            ]),
        );
        object(
            &mut o,
            "/org/bluealsa/hci0/dev_44_63_B6_7A_85_8B/a2dpsnk/source",
            "org.bluealsa.PCM1",
            props(vec![
                ("Transport", Value::from("A2DP-sink")),
                ("Mode", Value::from("source")),
                ("Sequence", Value::from(3u32)),
                ("Format", Value::from(0x8210u16)),
                ("Channels", Value::from(2u8)),
                ("Sampling", Value::from(44100u32)),
                ("Volume", Value::from(0x7f7fu16)),
            ]),
        );
        o
    }

    #[test]
    fn reads_the_adapter_and_paired_phones() {
        let o = sample();
        let a = adapter(&o).unwrap();
        assert_eq!(
            (a.address.as_str(), a.alias.as_str()),
            ("68:37:E9:41:FF:4F", "pinna")
        );
        assert!(a.powered && !a.pairable);
        let d = devices(&o);
        assert_eq!(d.len(), 1, "unpaired and unbonded devices are left out");
        assert_eq!(
            (d[0].name.as_str(), d[0].connected, d[0].trusted),
            ("iPhone", true, false)
        );
    }

    #[test]
    fn finds_the_sink_pcm_and_its_rate() {
        let p = pcms(&sample());
        assert_eq!(p.len(), 1);
        assert_eq!(stream_rate(&p[0]), Some(44100));
        let mono = Pcm {
            channels: 1,
            ..p[0].clone()
        };
        assert_eq!(stream_rate(&mono), None);
        let s24 = Pcm {
            format: 0x8418,
            ..p[0].clone()
        };
        assert_eq!(stream_rate(&s24), None);
    }

    fn player(objects: &mut Objects, path: &str, title: &str) {
        let track: HashMap<String, Value<'static>> = HashMap::from([
            ("Title".to_owned(), Value::from(title.to_owned())),
            ("Artist".to_owned(), Value::from("Artist")),
            ("Duration".to_owned(), Value::from(1000u32)),
        ]);
        object(
            objects,
            path,
            "org.bluez.MediaPlayer1",
            props(vec![("Track", Value::from(track))]),
        );
    }

    #[test]
    fn the_track_is_the_playing_phones_addressed_player() {
        const PCM: &str = "/org/bluealsa/hci0/dev_44_63_B6_7A_85_8B/a2dpsnk/source";
        let mut o = sample();
        assert_eq!(track(&o, PCM), Track::default(), "no player: no track");

        player(
            &mut o,
            "/org/bluez/hci0/dev_44_63_B6_7A_85_8B/player0",
            "First",
        );
        player(
            &mut o,
            "/org/bluez/hci0/dev_44_63_B6_7A_85_8B/player1",
            "Second",
        );
        player(
            &mut o,
            "/org/bluez/hci0/dev_11_22_33_44_55_66/player0",
            "Other phone",
        );
        assert_eq!(
            track(&o, PCM),
            Track {
                title: "First".into(),
                artist: "Artist".into(),
                album: String::new(),
            },
            "without an addressed player, the first"
        );

        let device = OwnedObjectPath::try_from("/org/bluez/hci0/dev_44_63_B6_7A_85_8B").unwrap();
        let addressed =
            zbus::zvariant::ObjectPath::try_from("/org/bluez/hci0/dev_44_63_B6_7A_85_8B/player1")
                .unwrap();
        o.get_mut(&device).unwrap().insert(
            "org.bluez.MediaControl1".into(),
            props(vec![("Player", Value::from(addressed))]),
        );
        assert_eq!(track(&o, PCM).title, "Second");
        assert_eq!(
            track(
                &o,
                "/org/bluealsa/hci0/dev_11_22_33_44_55_66/a2dpsnk/source"
            )
            .title,
            "Other phone"
        );
    }

    #[test]
    fn volume_follows_the_left_channel_and_mute() {
        assert_eq!(volume_percent(0x7f7f), 100);
        assert_eq!(volume_percent(0x0000), 0);
        assert_eq!(volume_percent(0x4040), 50);
        assert_eq!(volume_percent(0xff7f), 0, "muted");
    }
}
