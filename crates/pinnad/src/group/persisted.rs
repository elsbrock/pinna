//! The group as this satellite keeps it across restarts (ADR 0026): the
//! switches, the id and the member list in `standalone.toml`, the key in
//! `secrets.toml`. The standalone service owns both files and publishes this
//! on `AssistLive` after every commit.
//!
//! A group is only a group with all of its parts: the switch on, an id, and
//! a key stored for that id. The key is written before the id and removed
//! before it, so a crash between the two writes leaves a key without its
//! group or an id without its key, and either reads as in no group.

use pinna_assist::config::{GroupConfig, GroupMember};
use pinna_assist::secrets::Secrets;
use pinna_http::Secret;

use super::model::{Member, merge};
use super::seal::{self, Key};

/// The group's part of the standalone configuration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Persisted {
    /// The *Satellite group* switch.
    pub on: bool,
    /// The *Control from the group* switch.
    pub control: bool,
    /// The group this satellite is in; `None` in no group, and always while
    /// the switch is off.
    pub joined: Option<Joined>,
}

impl Default for Persisted {
    fn default() -> Self {
        Self {
            on: false,
            control: true,
            joined: None,
        }
    }
}

/// A group this satellite is in. Its Debug output names the key only as
/// redacted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Joined {
    /// 32 lowercase hex digits.
    pub id: String,
    pub key: Key,
    /// Sorted by MAC, one entry per MAC, this satellite's included. Removed
    /// entries stay, so a removal reaches the members that missed it.
    pub members: Vec<Member>,
}

impl Joined {
    /// A new group of this satellite and `other`: a new id and key from the
    /// secure random generator, and both entries at their first version.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "Add starts a group (ADR 0026), in a later change")
    )]
    pub fn start(own: Member, other: Member) -> anyhow::Result<Self> {
        let first = |m: Member| Member {
            version: 1,
            removed: false,
            ..m
        };
        Ok(Self {
            id: seal::new_id()?,
            key: Key::new(seal::new_key()?),
            members: merge(&[], &[first(own), first(other)]).members,
        })
    }
}

impl Persisted {
    /// What the mDNS record says (ADR 0026): `None` without the `group` key
    /// while the switch is off, `Some("")` while it is on in no group, and
    /// the group's id otherwise.
    pub fn record(&self) -> Option<String> {
        self.on.then(|| {
            self.joined
                .as_ref()
                .map(|joined| joined.id.clone())
                .unwrap_or_default()
        })
    }

    /// The group as the two files hold it. A member list from disk goes
    /// through [`merge`], so it is sorted and has no entry without a MAC.
    pub fn read(config: &GroupConfig, members: &[GroupMember], secrets: &Secrets) -> Self {
        let id = config.id.as_str();
        let key = secrets
            .group_key(id)
            .and_then(|key| Key::from_base64(key.expose()));
        let joined = match key {
            Some(key) if config.on && seal::is_id(id) => Some(Joined {
                id: id.to_owned(),
                key,
                members: merge(&[], &members.iter().map(from_file).collect::<Vec<_>>()).members,
            }),
            _ => None,
        };
        Self {
            on: config.on,
            control: config.control,
            joined,
        }
    }

    /// Write the group into the two files' forms. A group that ended takes
    /// its id, its members and its key along.
    pub fn write(
        &self,
        config: &mut GroupConfig,
        members: &mut Vec<GroupMember>,
        secrets: &mut Secrets,
    ) {
        config.on = self.on;
        config.control = self.control;
        match &self.joined {
            Some(joined) => {
                config.id.clone_from(&joined.id);
                *members = joined.members.iter().map(to_file).collect();
                let key = joined.key.to_base64();
                // Unchanged, it keeps what a newer image stored beside it.
                if secrets.group_key(&joined.id).map(Secret::expose) != Some(key.as_str()) {
                    secrets.set_group_key(&joined.id, Secret::new(key));
                }
            }
            None => {
                config.id.clear();
                members.clear();
                secrets.clear_group_key();
            }
        }
    }
}

fn from_file(m: &GroupMember) -> Member {
    Member {
        mac: m.mac.clone(),
        name: m.name.clone(),
        version: m.version,
        removed: m.removed,
    }
}

fn to_file(m: &Member) -> GroupMember {
    GroupMember {
        mac: m.mac.clone(),
        name: m.name.clone(),
        version: m.version,
        removed: m.removed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "00112233445566778899aabbccddeeff";
    const OWN: &str = "02:00:00:00:00:01";
    const B: &str = "02:00:00:00:00:02";

    fn m(mac: &str, name: &str, version: u64, removed: bool) -> Member {
        Member {
            mac: mac.into(),
            name: name.into(),
            version,
            removed,
        }
    }

    fn key() -> Key {
        Key::new(std::array::from_fn(|i| i as u8))
    }

    fn joined() -> Joined {
        Joined {
            id: ID.into(),
            key: key(),
            members: vec![m(OWN, "Hall", 1, false), m(B, "Kitchen", 2, false)],
        }
    }

    fn written(group: &Persisted) -> (GroupConfig, Vec<GroupMember>, Secrets) {
        let mut files = (GroupConfig::default(), Vec::new(), Secrets::default());
        group.write(&mut files.0, &mut files.1, &mut files.2);
        files
    }

    #[test]
    fn a_group_round_trips_through_its_files() {
        let group = Persisted {
            on: true,
            control: false,
            joined: Some(joined()),
        };
        let (config, members, secrets) = written(&group);
        assert_eq!(config.id, ID);
        assert!(config.on);
        assert!(!config.control);
        assert_eq!(members.len(), 2);
        assert!(secrets.group_key(ID).is_some());
        assert_eq!(Persisted::read(&config, &members, &secrets), group);
    }

    #[test]
    fn the_record_says_off_in_no_group_or_the_id() {
        let mut group = Persisted::default();
        assert_eq!(group.record(), None);
        group.on = true;
        assert_eq!(group.record(), Some(String::new()));
        group.joined = Some(joined());
        assert_eq!(group.record(), Some(ID.to_owned()));
    }

    #[test]
    fn a_group_missing_any_part_reads_as_no_group() {
        let on = Persisted {
            on: true,
            control: true,
            joined: Some(joined()),
        };
        let (config, members, secrets) = written(&on);

        // The key without its id: a crash after writing secrets.toml.
        let without_id = GroupConfig {
            id: String::new(),
            ..config.clone()
        };
        let read = Persisted::read(&without_id, &members, &secrets);
        assert!(read.on && read.joined.is_none());
        assert_eq!(read.record(), Some(String::new()));

        // The id without its key: a crash after removing the key.
        let read = Persisted::read(&config, &members, &Secrets::default());
        assert!(read.joined.is_none());

        // Another group's key.
        let mut other = Secrets::default();
        other.set_group_key(
            "ffeeddccbbaa99887766554433221100",
            secrets.group_key(ID).unwrap().clone(),
        );
        assert!(Persisted::read(&config, &members, &other).joined.is_none());

        // A key that isn't 32 bytes.
        let mut short = Secrets::default();
        short.set_group_key(ID, Secret::new("AAECAw=="));
        assert!(Persisted::read(&config, &members, &short).joined.is_none());

        // An id that isn't one.
        let mut upper = secrets.clone();
        upper.set_group_key(ID.to_uppercase(), secrets.group_key(ID).unwrap().clone());
        let config_upper = GroupConfig {
            id: ID.to_uppercase(),
            ..config.clone()
        };
        assert!(
            Persisted::read(&config_upper, &members, &upper)
                .joined
                .is_none()
        );

        // The switch off.
        let off = GroupConfig {
            on: false,
            ..config.clone()
        };
        let read = Persisted::read(&off, &members, &secrets);
        assert!(read.joined.is_none());
        assert_eq!(read.record(), None);
    }

    #[test]
    fn members_from_disk_are_sorted_and_need_a_mac() {
        let (config, _, secrets) = written(&Persisted {
            on: true,
            control: true,
            joined: Some(joined()),
        });
        let members = [
            to_file(&m(B, "Kitchen", 2, false)),
            to_file(&m("", "Nobody", 1, false)),
            to_file(&m("02-00-00-00-00-01", "Hall", 1, false)),
        ];
        let read = Persisted::read(&config, &members, &secrets);
        assert_eq!(read.joined.unwrap().members, joined().members);
    }

    #[test]
    fn a_group_that_ended_takes_its_id_members_and_key_along() {
        let (mut config, mut members, mut secrets) = written(&Persisted {
            on: true,
            control: true,
            joined: Some(joined()),
        });
        Persisted {
            on: true,
            control: true,
            joined: None,
        }
        .write(&mut config, &mut members, &mut secrets);
        assert!(config.id.is_empty());
        assert!(members.is_empty());
        assert!(secrets.to_file().group.is_none());
        assert!(config.on);
    }

    #[test]
    fn a_new_group_has_a_fresh_id_and_key_and_both_entries_at_version_1() {
        let group = Joined::start(m(B, "Kitchen", 0, false), m(OWN, "Hall", 7, true)).unwrap();
        assert!(seal::is_id(&group.id));
        assert_eq!(
            group.members,
            [m(OWN, "Hall", 1, false), m(B, "Kitchen", 1, false)]
        );
        let other = Joined::start(m(B, "Kitchen", 0, false), m(OWN, "Hall", 0, false)).unwrap();
        assert_ne!(group.id, other.id);
        assert_ne!(group.key, other.key);
    }

    #[test]
    fn debug_output_never_shows_the_key() {
        let group = Persisted {
            on: true,
            control: true,
            joined: Some(joined()),
        };
        let debug = format!("{group:?}");
        assert!(debug.contains(ID), "{debug}");
        assert!(!debug.contains(&key().to_base64()), "{debug}");
        let bytes = format!("{:?}", key().bytes());
        assert!(!debug.contains(&bytes[1..bytes.len() - 1]), "{debug}");
    }
}
