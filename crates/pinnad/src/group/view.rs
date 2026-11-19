//! What the Assistant page shows of the satellite group (ADR 0026): the
//! switches, the members, and the satellites the search found, each with
//! whether it can be added and why not. Built from the saved group and the
//! search's latest result, without I/O. Never the key.

use crate::identity::looks_default;
use crate::satellites::{Found, Seen};

use super::model::{check_name, same_mac};
use super::persisted::Persisted;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GroupView {
    pub on: bool,
    pub in_group: bool,
    pub control: bool,
    /// Not removed, by name.
    pub members: Vec<MemberView>,
    /// Every satellite the search found except the members of this group,
    /// as the search lists them. A member that left the group is in
    /// `members` too.
    pub candidates: Vec<Candidate>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemberView {
    pub mac: String,
    pub name: String,
    pub this_satellite: bool,
}

/// Where a satellite the search found stands towards this group.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CandidateState {
    /// Its switch is on and it is in no group.
    Addable,
    InAnotherGroup,
    /// Its record has no `group` key: the switch is off there, or its Pinna
    /// is older.
    OffOrOlder,
    /// A member of this group whose record says it is in no group, and
    /// that refused a call as not in this group.
    LeftTheGroup,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub found: Found,
    pub state: CandidateState,
    /// Add is offered: its switch is on and it is in no group.
    pub addable: bool,
    /// Its friendly name is still the default one, which is hard to say.
    pub default_name: bool,
    /// Why Add would be refused for its name; `None` when it can be added
    /// under it, and when Add isn't offered.
    pub name_refusal: Option<String>,
}

/// The view of `group` for this satellite (`own_mac`, called `own_name`),
/// with the satellites in `seen`. `answered_outside` says whether a member,
/// by MAC, refused this satellite's `GetGroup` as not in this group after
/// the latest search listed it outside the group.
pub fn view(
    group: &Persisted,
    own_mac: &str,
    own_name: &str,
    seen: &Seen,
    answered_outside: impl Fn(&str) -> bool,
) -> GroupView {
    let joined = group.joined.as_ref();
    let members = joined.map_or(&[][..], |joined| &joined.members[..]);
    let mut listed: Vec<MemberView> = members
        .iter()
        .filter(|member| !member.removed)
        .map(|member| {
            let this_satellite = same_mac(&member.mac, own_mac);
            MemberView {
                mac: member.mac.clone(),
                // The search never lists this satellite, so its entry may
                // hold an older name than its own.
                name: if this_satellite {
                    own_name.to_owned()
                } else {
                    member.name.clone()
                },
                this_satellite,
            }
        })
        .collect();
    listed.sort_by_key(|member| (member.name.to_lowercase(), member.mac.clone()));

    // The page lists the search only with the switch on.
    let found = if group.on { &seen.satellites[..] } else { &[] };
    let candidates = found
        .iter()
        .filter_map(|found| {
            let member = members
                .iter()
                .any(|m| !m.removed && same_mac(&m.mac, &found.mac));
            let state = match found.group.as_deref() {
                // A member in this group, or a newcomer whose Add this
                // satellite hasn't merged yet: a member either way.
                Some(id) if joined.is_some_and(|joined| joined.id == id) => return None,
                // A member is still one while only its record says
                // otherwise: the search can list a record from before its
                // Add, or one resolved kept (ADR 0021). Its refusal of a
                // call makes it a candidate again.
                _ if member && !answered_outside(&found.mac) => return None,
                Some(id) if !id.is_empty() => CandidateState::InAnotherGroup,
                _ if member => CandidateState::LeftTheGroup,
                Some(_) => CandidateState::Addable,
                None => CandidateState::OffOrOlder,
            };
            let addable = found.group.as_deref() == Some("");
            let name_refusal = addable
                .then(|| check_name(members, own_mac, own_name, &found.mac, &found.friendly_name))
                .and_then(Result::err)
                .map(|refusal| refusal.to_string());
            Some(Candidate {
                found: found.clone(),
                state,
                addable,
                default_name: looks_default(&found.friendly_name, &found.mac),
                name_refusal,
            })
        })
        .collect();

    GroupView {
        on: group.on,
        in_group: joined.is_some(),
        control: group.control,
        members: listed,
        candidates,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::group::model::Member;
    use crate::group::persisted::Joined;
    use crate::group::seal::Key;

    const ID: &str = "00112233445566778899aabbccddeeff";
    const OTHER_ID: &str = "ffeeddccbbaa99887766554433221100";
    const OWN: &str = "02:00:00:00:00:01";

    fn member(mac: &str, name: &str, removed: bool) -> Member {
        Member {
            mac: mac.into(),
            name: name.into(),
            version: 1,
            removed,
        }
    }

    fn found(mac: &str, friendly_name: &str, group: Option<&str>) -> Found {
        Found {
            name: friendly_name.to_lowercase(),
            host: friendly_name.to_lowercase(),
            friendly_name: friendly_name.into(),
            board: "Echo Dot (2nd Gen)".into(),
            mac: mac.to_uppercase(),
            version: "0.4.0".into(),
            web_port: 80,
            url: String::new(),
            ipv4: vec!["192.168.1.20".parse().unwrap()],
            ipv6: Vec::new(),
            group: group.map(str::to_owned),
            missed: 0,
        }
    }

    fn seen(satellites: Vec<Found>) -> Seen {
        Seen {
            searched: true,
            on_network: true,
            satellites,
            error: None,
            search: 1,
        }
    }

    fn in_group(members: Vec<Member>) -> Persisted {
        Persisted {
            on: true,
            control: true,
            joined: Some(Joined {
                id: ID.into(),
                key: Key::new([7; 32]),
                members,
            }),
        }
    }

    fn states(view: &GroupView) -> Vec<(&str, CandidateState, bool)> {
        view.candidates
            .iter()
            .map(|c| (c.found.friendly_name.as_str(), c.state, c.addable))
            .collect()
    }

    #[test]
    fn in_no_group_the_search_says_who_can_be_added() {
        let group = Persisted {
            on: true,
            ..Persisted::default()
        };
        let seen = seen(vec![
            found("02:00:00:00:00:02", "Kitchen", Some("")),
            found("02:00:00:00:00:03", "Office", Some(OTHER_ID)),
            found("02:00:00:00:00:04", "Porch", None),
        ]);
        let view = view(&group, OWN, "Hall", &seen, |_| false);
        assert!(view.on && !view.in_group && view.control);
        assert!(view.members.is_empty());
        assert_eq!(
            states(&view),
            [
                ("Kitchen", CandidateState::Addable, true),
                ("Office", CandidateState::InAnotherGroup, false),
                ("Porch", CandidateState::OffOrOlder, false),
            ]
        );
        assert!(view.candidates.iter().all(|c| c.name_refusal.is_none()));
        // The candidate carries what the search found, for its link.
        assert_eq!(view.candidates[0].found, seen.satellites[0]);
    }

    #[test]
    fn with_the_switch_off_nothing_is_listed() {
        let seen = seen(vec![found("02:00:00:00:00:02", "Kitchen", Some(""))]);
        let view = view(&Persisted::default(), OWN, "Hall", &seen, |_| false);
        assert!(!view.on && !view.in_group);
        assert!(view.candidates.is_empty());
    }

    #[test]
    fn members_are_listed_by_name_and_members_that_left_are_candidates() {
        let group = in_group(vec![
            member(OWN, "Old hall", false),
            member("02:00:00:00:00:02", "Kitchen", false),
            member("02:00:00:00:00:03", "Office", false),
            member("02:00:00:00:00:04", "Attic", false),
            member("02:00:00:00:00:05", "Cellar", true),
            member("02:00:00:00:00:06", "Garage", false),
            member("02:00:00:00:00:08", "Loft", false),
            member("02:00:00:00:00:09", "Study", false),
        ]);
        let seen = seen(vec![
            // In this group: shown as a member, not a candidate.
            found("02:00:00:00:00:02", "Kitchen", Some(ID)),
            // Members whose records say they are in no group, and that
            // refused a call as not in this group.
            found("02:00:00:00:00:03", "Office", Some("")),
            found("02:00:00:00:00:04", "Attic", None),
            // A removed member is a satellite like any other.
            found("02:00:00:00:00:05", "Cellar", Some("")),
            // A member now in another group, by its record and its refusal.
            found("02:00:00:00:00:06", "Garage", Some(OTHER_ID)),
            // A newcomer this satellite hasn't merged yet.
            found("02:00:00:00:00:07", "Den", Some(ID)),
            // Members whose records say otherwise, but that never refused a
            // call: records from before their Add.
            found("02:00:00:00:00:08", "Loft", Some("")),
            found("02:00:00:00:00:09", "Study", None),
        ]);
        let refused = [
            "02:00:00:00:00:03",
            "02:00:00:00:00:04",
            "02:00:00:00:00:06",
        ];
        let view = view(&group, OWN, "Hall", &seen, |mac| {
            refused.iter().any(|r| same_mac(r, mac))
        });
        assert!(view.in_group);
        let names: Vec<(&str, bool)> = view
            .members
            .iter()
            .map(|m| (m.name.as_str(), m.this_satellite))
            .collect();
        assert_eq!(
            names,
            [
                ("Attic", false),
                ("Garage", false),
                ("Hall", true),
                ("Kitchen", false),
                ("Loft", false),
                ("Office", false),
                ("Study", false),
            ]
        );
        assert_eq!(
            states(&view),
            [
                ("Office", CandidateState::LeftTheGroup, true),
                ("Attic", CandidateState::LeftTheGroup, false),
                ("Cellar", CandidateState::Addable, true),
                ("Garage", CandidateState::InAnotherGroup, false),
            ]
        );
    }

    #[test]
    fn a_name_that_add_would_refuse_is_named() {
        let group = in_group(vec![
            member(OWN, "Hall", false),
            member("02:00:00:00:00:02", "Kitchen", false),
        ]);
        let seen = seen(vec![
            found("02:00:00:00:00:03", "kitchen", Some("")),
            found("02:00:00:00:00:04", "All", Some("")),
            found("02:00:00:00:00:05", "Hall", Some("")),
            found("02:00:00:00:00:06", "Office", Some("")),
            // Add isn't offered, so its name isn't checked.
            found("02:00:00:00:00:07", "Kitchen", None),
        ]);
        let view = view(&group, OWN, "Hall", &seen, |_| false);
        let refusals: Vec<(&str, Option<&str>)> = view
            .candidates
            .iter()
            .map(|c| (c.found.friendly_name.as_str(), c.name_refusal.as_deref()))
            .collect();
        assert_eq!(
            refusals,
            [
                (
                    "kitchen",
                    Some("A member is called Kitchen already. Rename it first on its dashboard.")
                ),
                (
                    "All",
                    Some(
                        "Its name is \"all\", which stands for every member. Rename it first \
                         on its dashboard."
                    )
                ),
                (
                    "Hall",
                    Some(
                        "It has the same name as this satellite, Hall. Rename it first on its \
                         dashboard."
                    )
                ),
                ("Office", None),
                ("Kitchen", None),
            ]
        );
    }

    #[test]
    fn a_default_name_is_noted() {
        let group = Persisted {
            on: true,
            ..Persisted::default()
        };
        let seen = seen(vec![
            found("f0:27:2d:2a:91:4e", "Echo Dot 2a914e", Some("")),
            found("f0:27:2d:2a:91:4f", "Kitchen", Some("")),
        ]);
        let view = view(&group, OWN, "Hall", &seen, |_| false);
        let notes: Vec<(&str, bool)> = view
            .candidates
            .iter()
            .map(|c| (c.found.friendly_name.as_str(), c.default_name))
            .collect();
        assert_eq!(notes, [("Echo Dot 2a914e", true), ("Kitchen", false)]);
        // It can be added all the same.
        assert!(view.candidates[0].addable);
        assert!(view.candidates[0].name_refusal.is_none());
    }

    #[test]
    fn the_view_never_contains_the_key() {
        let group = in_group(vec![member(OWN, "Hall", false)]);
        let view = view(&group, OWN, "Hall", &seen(Vec::new()), |_| false);
        let key = Key::new([7; 32]);
        assert!(!format!("{view:?}").contains(&key.to_base64()));
    }
}
