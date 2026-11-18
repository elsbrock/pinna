//! The group's member list and its rules (ADR 0026), without I/O.
//!
//! Members share the list entry by entry, one entry per MAC, and merge each
//! other's lists once a minute. Any two members that have merged the same
//! entries hold the same MACs, versions and removals, whichever order the
//! lists arrived in. Names are the exception: each member keeps the last
//! friendly name its own search heard.

use std::collections::BTreeMap;
use std::fmt;

use crate::satellites::mac_digits;
use crate::tools::radio;

/// The name `send_message` takes for every other member, so no member may
/// have it.
pub const ALL: &str = "all";

/// One entry of the member list. The own satellite has one too.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    /// In lowercase colon form, as `DeviceConfig::mac_string()` writes it.
    pub mac: String,
    /// The last friendly name the search heard.
    pub name: String,
    /// Raised by every change of the entry: an Add or a removal. A rename
    /// doesn't raise it: each member hears a rename in its own search, and
    /// versions raised by every member for the same rename would never
    /// settle.
    pub version: u64,
    /// Removed from the group. The entry stays, so the removal reaches the
    /// members that missed it.
    pub removed: bool,
}

/// The result of a merge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Merged {
    /// Sorted by MAC, one entry per MAC.
    pub members: Vec<Member>,
    /// It differs from the local list, so it needs saving. A minute's
    /// exchange that brings nothing new rewrites nothing.
    pub changed: bool,
}

/// A MAC in lowercase colon form, from any form the search or a member
/// sends. Empty when it has no hex digits.
pub fn mac_form(mac: &str) -> String {
    let digits = mac_digits(mac);
    let mut out = String::with_capacity(digits.len() * 3 / 2);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && i % 2 == 0 {
            out.push(':');
        }
        out.push(c);
    }
    out
}

/// Whether two MACs name the same satellite. An empty MAC never matches.
pub fn same_mac(a: &str, b: &str) -> bool {
    let a = mac_digits(a);
    !a.is_empty() && a == mac_digits(b)
}

/// Merge another member's list into the local one, entry by entry: for each
/// MAC the higher version wins, and at equal versions a removal wins. At
/// equal versions and the same removal, the local entry stays, with the name
/// this satellite's search heard. An entry on one side only is kept: a list
/// without an entry means its sender hasn't heard of it yet, not that it was
/// removed.
///
/// An entry without a MAC is dropped: it could never match a satellite.
pub fn merge(local: &[Member], remote: &[Member]) -> Merged {
    let mut by_mac: BTreeMap<String, Member> = BTreeMap::new();
    for entry in local.iter().chain(remote) {
        let mac = mac_form(&entry.mac);
        if mac.is_empty() {
            continue;
        }
        let entry = Member {
            mac: mac.clone(),
            ..entry.clone()
        };
        match by_mac.get_mut(&mac) {
            Some(kept) if wins(&entry, kept) => *kept = entry,
            Some(_) => {}
            None => {
                by_mac.insert(mac, entry);
            }
        }
    }
    let members: Vec<Member> = by_mac.into_values().collect();
    let changed = members != local;
    Merged { members, changed }
}

/// Whether `new` replaces `kept`, an entry for the same MAC.
fn wins(new: &Member, kept: &Member) -> bool {
    new.version > kept.version || (new.version == kept.version && new.removed && !kept.removed)
}

/// Take the friendly name the search heard for a member, without raising its
/// version. Returns whether the list changed. An empty name changes nothing:
/// a record without one says nothing about a rename.
pub fn heard_name(members: &mut [Member], mac: &str, name: &str) -> bool {
    let name = name.trim();
    if name.is_empty() {
        return false;
    }
    match members.iter_mut().find(|m| same_mac(&m.mac, mac)) {
        Some(member) if member.name != name => {
            member.name = name.to_string();
            true
        }
        _ => false,
    }
}

/// Whether no other member is left: the group ends.
pub fn alone(members: &[Member], own_mac: &str) -> bool {
    !members
        .iter()
        .any(|m| !m.removed && !same_mac(&m.mac, own_mac))
}

/// Whether the list marks this satellite's own entry removed: the only thing
/// that makes it forget the key. A list without its entry doesn't.
pub fn own_removed(members: &[Member], own_mac: &str) -> bool {
    members
        .iter()
        .any(|m| m.removed && same_mac(&m.mac, own_mac))
}

/// A friendly name as names are compared: as a station's name is (lower
/// case, without accents, one space between words), since the brain matches
/// both to what was said.
pub fn name_key(name: &str) -> String {
    radio::key(name)
}

/// Why a satellite can't be added under its friendly name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NameRefusal {
    /// The name has no letter or digit to say.
    Unsayable,
    /// The name is "all", which `send_message` uses for every member.
    All,
    /// This satellite has the name.
    Own(String),
    /// Another member has the name.
    Taken(String),
}

impl fmt::Display for NameRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsayable => f.write_str("Its name has no letters or digits to say."),
            Self::All => f.write_str("Its name is \"all\", which stands for every member."),
            Self::Own(name) => write!(f, "It has the same name as this satellite, {name}."),
            Self::Taken(name) => write!(f, "A member is called {name} already."),
        }?;
        f.write_str(" Rename it first on its dashboard.")
    }
}

/// Check the friendly name of the satellite with `mac` before it is added:
/// the brain names members by their friendly names, so each must be
/// speakable and unique. Removed members' names are free again, and the
/// satellite's own entry (when it is added back) doesn't count against it.
///
/// This satellite's name is `own_name`, not its entry's: the search never
/// lists this satellite, so its entry may hold an older name.
pub fn check_name(
    members: &[Member],
    own_mac: &str,
    own_name: &str,
    mac: &str,
    name: &str,
) -> Result<(), NameRefusal> {
    let key = name_key(name);
    if key.is_empty() {
        return Err(NameRefusal::Unsayable);
    }
    if key == ALL {
        return Err(NameRefusal::All);
    }
    if name_key(own_name) == key {
        return Err(NameRefusal::Own(own_name.trim().to_string()));
    }
    match members.iter().find(|m| {
        !m.removed
            && !same_mac(&m.mac, own_mac)
            && !same_mac(&m.mac, mac)
            && name_key(&m.name) == key
    }) {
        Some(member) => Err(NameRefusal::Taken(member.name.clone())),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OWN: &str = "02:00:00:00:00:01";
    const B: &str = "02:00:00:00:00:02";
    const C: &str = "02:00:00:00:00:03";

    fn m(mac: &str, name: &str, version: u64, removed: bool) -> Member {
        Member {
            mac: mac.into(),
            name: name.into(),
            version,
            removed,
        }
    }

    /// What every member must agree on after merging the same entries: the
    /// names are each member's own.
    fn agreed(members: &[Member]) -> Vec<(String, u64, bool)> {
        members
            .iter()
            .map(|m| (m.mac.clone(), m.version, m.removed))
            .collect()
    }

    #[test]
    fn the_higher_version_wins() {
        let local = [m(B, "Kitchen", 1, false)];
        let remote = [m(B, "Hall", 2, true)];
        let merged = merge(&local, &remote);
        assert_eq!(merged.members, [m(B, "Hall", 2, true)]);
        assert!(merged.changed);

        let merged = merge(&remote, &local);
        assert_eq!(merged.members, [m(B, "Hall", 2, true)]);
        assert!(!merged.changed);

        // A higher version brings a member back after its removal.
        let merged = merge(&[m(B, "Kitchen", 2, true)], &[m(B, "Kitchen", 3, false)]);
        assert_eq!(merged.members, [m(B, "Kitchen", 3, false)]);
    }

    #[test]
    fn a_removal_wins_at_equal_versions() {
        let kept = [m(B, "Kitchen", 4, false)];
        let removed = [m(B, "Kitchen", 4, true)];
        assert_eq!(merge(&kept, &removed).members, removed);
        assert!(merge(&kept, &removed).changed);
        assert_eq!(merge(&removed, &kept).members, removed);
        assert!(!merge(&removed, &kept).changed);
    }

    #[test]
    fn the_local_name_stays_at_equal_versions() {
        let local = [m(B, "Kitchen", 2, false)];
        let remote = [m(B, "Cuisine", 2, false)];
        let merged = merge(&local, &remote);
        assert_eq!(merged.members, local);
        assert!(!merged.changed);

        let local = [m(B, "Kitchen", 2, true)];
        let remote = [m(B, "Cuisine", 2, true)];
        assert_eq!(merge(&local, &remote).members, local);
    }

    #[test]
    fn an_entry_on_one_side_only_is_kept() {
        let local = [m(OWN, "Hall", 1, false), m(B, "Kitchen", 1, false)];
        let remote = [m(B, "Kitchen", 1, false), m(C, "Office", 1, false)];
        let merged = merge(&local, &remote);
        assert_eq!(
            merged.members,
            [
                m(OWN, "Hall", 1, false),
                m(B, "Kitchen", 1, false),
                m(C, "Office", 1, false),
            ]
        );
        assert!(merged.changed);
        assert_eq!(merge(&local, &[]).members, local);
        assert!(!merge(&local, &[]).changed);
        assert_eq!(merge(&[], &remote).members, remote);
    }

    #[test]
    fn a_list_without_the_own_entry_keeps_it() {
        // The sender missed this satellite's Add: it hasn't heard of it yet.
        let local = [m(OWN, "Hall", 1, false), m(B, "Kitchen", 1, false)];
        let merged = merge(&local, &[m(B, "Kitchen", 1, false)]);
        assert_eq!(merged.members, local);
        assert!(!own_removed(&merged.members, OWN));
    }

    #[test]
    fn a_list_that_removes_the_own_entry_removes_it() {
        let local = [m(OWN, "Hall", 1, false), m(B, "Kitchen", 1, false)];
        // At a higher version, and at the same version, where a removal wins.
        for version in [1, 2] {
            let remote = [m(OWN, "Hall", version, true), m(B, "Kitchen", 1, false)];
            let merged = merge(&local, &remote);
            assert!(own_removed(&merged.members, OWN), "version {version}");
        }
        // Not an older removal: this satellite was added back since.
        let local = [m(OWN, "Hall", 3, false)];
        let merged = merge(&local, &[m(OWN, "Hall", 2, true)]);
        assert!(!own_removed(&merged.members, OWN));
        assert!(!merged.changed);
    }

    #[test]
    fn merging_twice_changes_nothing() {
        let local = [m(OWN, "Hall", 1, false), m(B, "Kitchen", 2, false)];
        let remote = [m(B, "Cuisine", 2, true), m(C, "Office", 1, false)];
        let once = merge(&local, &remote);
        let twice = merge(&once.members, &remote);
        assert_eq!(twice.members, once.members);
        assert!(!twice.changed);
        let itself = merge(&once.members, &once.members);
        assert_eq!(itself.members, once.members);
        assert!(!itself.changed);
    }

    #[test]
    fn merges_agree_in_any_order() {
        fn list(rng: &mut fastrand::Rng) -> Vec<Member> {
            let names = ["Hall", "Kitchen", "Office"];
            let mut out = Vec::new();
            for mac in [OWN, B, C, "02:00:00:00:00:04"] {
                if rng.bool() {
                    let name = names[rng.usize(..names.len())];
                    out.push(m(mac, name, rng.u64(0..4), rng.bool()));
                }
            }
            rng.shuffle(&mut out);
            out
        }
        let mut rng = fastrand::Rng::with_seed(26);
        for _ in 0..2000 {
            let (a, b, c) = (list(&mut rng), list(&mut rng), list(&mut rng));
            let ab = merge(&a, &b).members;
            let ba = merge(&b, &a).members;
            assert_eq!(agreed(&ab), agreed(&ba), "{a:?} {b:?}");
            // Idempotent, names included.
            assert_eq!(merge(&ab, &b).members, ab);
            assert_eq!(merge(&ab, &a).members, ab);
            assert!(!merge(&ab, &ab).changed);
            // Associative: three members converge whoever merges first.
            let ab_c = merge(&ab, &c).members;
            let a_bc = merge(&a, &merge(&b, &c).members).members;
            assert_eq!(agreed(&ab_c), agreed(&a_bc), "{a:?} {b:?} {c:?}");
            // Sorted by MAC, one entry each.
            assert!(ab.windows(2).all(|w| w[0].mac < w[1].mac), "{ab:?}");
        }
    }

    #[test]
    fn macs_match_in_any_form_and_are_kept_in_lowercase_colon_form() {
        let local = [m("02:00:00:AA:BB:CC", "Kitchen", 1, false)];
        let remote = [m("02-00-00-aa-bb-cc", "Kitchen", 2, true)];
        let merged = merge(&local, &remote);
        assert_eq!(merged.members, [m("02:00:00:aa:bb:cc", "Kitchen", 2, true)]);
        // An upper-case local MAC is rewritten in the stored form.
        assert!(merge(&local, &[]).changed);
        assert_eq!(mac_form("0200.00AA.BBCC"), "02:00:00:aa:bb:cc");
        assert_eq!(mac_form(""), "");
    }

    #[test]
    fn an_entry_without_a_mac_is_dropped() {
        let local = [m(B, "Kitchen", 1, false)];
        let remote = [m("", "Nobody", 9, false), m("zz", "Nobody", 9, true)];
        let merged = merge(&local, &remote);
        assert_eq!(merged.members, local);
        assert!(!merged.changed);
        assert!(!same_mac("", ""));
        assert!(!same_mac("zz", "zz"));
    }

    #[test]
    fn the_search_renames_a_member_without_raising_its_version() {
        let mut members = vec![m(OWN, "Hall", 1, false), m(B, "Kitchen", 3, false)];
        assert!(heard_name(&mut members, "02-00-00-00-00-02", "Cuisine"));
        assert_eq!(members[1], m(B, "Cuisine", 3, false));
        // Nothing new, an empty name, or a satellite that isn't a member.
        assert!(!heard_name(&mut members, B, "Cuisine"));
        assert!(!heard_name(&mut members, B, "  "));
        assert!(!heard_name(&mut members, C, "Office"));
        assert_eq!(members[1].name, "Cuisine");
    }

    #[test]
    fn a_satellite_is_alone_with_no_other_member_left() {
        assert!(alone(&[], OWN));
        assert!(alone(&[m(OWN, "Hall", 1, false)], OWN));
        assert!(alone(
            &[m(OWN, "Hall", 1, false), m(B, "Kitchen", 2, true)],
            OWN
        ));
        assert!(!alone(
            &[m(OWN, "Hall", 1, false), m(B, "Kitchen", 1, false)],
            OWN
        ));
        // Its own MAC in another form is still its own.
        assert!(alone(&[m(OWN, "Hall", 1, false)], "02-00-00-00-00-01"));
    }

    #[test]
    fn only_a_removed_own_entry_counts_as_removed() {
        let own_removed_list = [m(OWN, "Hall", 2, true), m(B, "Kitchen", 1, false)];
        assert!(own_removed(&own_removed_list, OWN));
        assert!(own_removed(&own_removed_list, "02-00-00-00-00-01"));
        assert!(!own_removed(
            &[m(OWN, "Hall", 2, false), m(B, "Kitchen", 2, true)],
            OWN
        ));
        assert!(!own_removed(&[m(B, "Kitchen", 1, false)], OWN));
        // An empty MAC never matches, not even a removed entry without one.
        assert!(!own_removed(&[m("", "Hall", 2, true)], ""));
    }

    #[test]
    fn names_compare_without_case_accents_or_punctuation() {
        assert_eq!(name_key("  Küche "), name_key("kuche"));
        assert_eq!(name_key("Living-Room"), name_key("living room"));
        assert_eq!(name_key("ALL!"), ALL);
    }

    #[test]
    fn a_name_must_be_speakable_and_not_all() {
        let members = [m(OWN, "Hall", 1, false)];
        assert_eq!(
            check_name(&members, OWN, "Hall", B, "?!"),
            Err(NameRefusal::Unsayable)
        );
        assert_eq!(
            check_name(&members, OWN, "Hall", B, ""),
            Err(NameRefusal::Unsayable)
        );
        for name in ["all", "All", " ALL "] {
            assert_eq!(
                check_name(&members, OWN, "Hall", B, name),
                Err(NameRefusal::All),
                "{name}"
            );
        }
        // "All rooms" is a name of its own.
        assert_eq!(check_name(&members, OWN, "Hall", B, "All rooms"), Ok(()));
    }

    #[test]
    fn a_name_must_not_be_a_members_or_this_satellites() {
        let members = [
            m(OWN, "Old hall", 1, false),
            m(B, "Kitchen", 1, false),
            m(C, "Office", 2, true),
        ];
        let new = "02:00:00:00:00:04";
        assert_eq!(
            check_name(&members, OWN, "Hall", new, "kitchen "),
            Err(NameRefusal::Taken("Kitchen".into()))
        );
        assert_eq!(
            check_name(&members, OWN, "Hall", new, "HALL"),
            Err(NameRefusal::Own("Hall".into()))
        );
        // A removed member's name is free again.
        assert_eq!(check_name(&members, OWN, "Hall", new, "Office"), Ok(()));
        // The own entry's older name isn't this satellite's name any more.
        assert_eq!(check_name(&members, OWN, "Hall", new, "Old hall"), Ok(()));
        // A member added back keeps its own name.
        assert_eq!(check_name(&members, OWN, "Hall", B, "Kitchen"), Ok(()));
        // A satellite still on its default name can be added.
        assert_eq!(
            check_name(&members, OWN, "Hall", new, "Pinna 0a1b2c"),
            Ok(())
        );
    }

    #[test]
    fn a_refused_name_says_to_rename_the_satellite_first() {
        for refusal in [
            NameRefusal::Unsayable,
            NameRefusal::All,
            NameRefusal::Own("Hall".into()),
            NameRefusal::Taken("Kitchen".into()),
        ] {
            let sentence = refusal.to_string();
            assert!(
                sentence.ends_with(". Rename it first on its dashboard."),
                "{sentence}"
            );
        }
        assert_eq!(
            NameRefusal::Taken("Kitchen".into()).to_string(),
            "A member is called Kitchen already. Rename it first on its dashboard."
        );
    }
}
