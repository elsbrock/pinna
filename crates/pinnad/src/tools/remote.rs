//! The brain acts on another member of the satellite group (ADR 0026): a
//! device tool that acts on the satellite takes the member's name as
//! `satellite`, and the member runs the tool as its own `/mcp` would.
//!
//! The member is called at the addresses the search heard, and gives up
//! inside the brain's tool limit. A member last heard in Home Assistant
//! mode, or one the search no longer lists, isn't called.

use serde_json::Value;

use super::message;
use crate::group::Group;
use crate::group::client::{Failed, To};
use crate::group::model::{ALL, Member, name_key};
use crate::settings::Mode;

/// Why the brain's call that names another member isn't made: this
/// satellite left the group, or the group ended, after the run's tool list
/// was made.
pub(super) const NO_GROUP: &str = "This satellite is in no satellite group now.";

/// Where a tool runs.
pub(super) enum Target {
    Here,
    /// On another member, last heard in this mode.
    Member(Member, Option<Mode>),
}

/// The satellite `satellite` names for `tool`: this one when it is empty or
/// this one's name (`own`, its host name `own_host`), or another member.
pub(super) fn target(
    group: &Group,
    own: &str,
    own_host: &str,
    tool: &str,
    satellite: Option<&str>,
) -> Result<Target, String> {
    let Some(wanted) = satellite.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(Target::Here);
    };
    let members = group.other_members();
    let key = name_key(wanted);
    // The group ended after the run's tool list was made.
    if members.is_empty() && key != name_key(own) {
        return Err(NO_GROUP.into());
    }
    if key == name_key(ALL) {
        return Err(format!(
            "{tool} acts on one satellite at a time. Call it once for each of {}.",
            message::names(&members)
        ));
    }
    if !key.is_empty() && key == name_key(own) {
        let others: Vec<&Member> = members
            .iter()
            .map(|(m, _)| m)
            .filter(|m| name_key(&m.name) == key)
            .collect();
        if others.is_empty() {
            return Ok(Target::Here);
        }
        return Err(message::shared(
            group,
            own,
            others.into_iter(),
            Some(own_host),
        ));
    }
    message::named(group, &members, wanted).map(|(member, mode)| Target::Member(member, mode))
}

/// Why `tool`, which acts on another member only, isn't run here.
pub(super) fn not_here(group: &Group, tool: &str) -> String {
    let members = group.other_members();
    if members.is_empty() {
        return NO_GROUP.into();
    }
    format!(
        "{tool} acts on another satellite of the group, not this one. Its other members are {}.",
        message::names(&members)
    )
}

/// Have `member` run `tool` with `args`, its arguments without
/// `satellite`, and say what it answered, after its name.
pub(super) async fn call(
    group: &Group,
    member: &Member,
    mode: Option<Mode>,
    tool: &str,
    args: Value,
) -> Result<String, String> {
    let name = &member.name;
    if mode == Some(Mode::HomeAssistant) {
        return Err(format!("{name} is in Home Assistant mode, out of reach."));
    }
    let Some(joined) = group.service().group().joined else {
        return Err(NO_GROUP.into());
    };
    // A member the search no longer lists (ADR 0021), or one it heard no
    // address for, isn't called.
    let seen = group.seen();
    let Some(found) = seen
        .by_mac(&member.mac)
        .filter(|f| !f.ipv4.is_empty() || !f.ipv6.is_empty())
    else {
        return Err(format!("{name} is unreachable."));
    };
    let deadline = group.limits().call;
    let answer = group
        .call(
            &To::heard(found),
            &joined,
            &member.mac,
            tool,
            &args.to_string(),
            deadline,
        )
        .await;
    match answer {
        Ok(result) if result.is_error => Err(format!("{name}: {}", result.text.trim())),
        Ok(result) => Ok(format!("{name}: {}", result.text.trim())),
        Err(failed) => {
            group.failed(&member.mac, name, "running a tool on", &failed);
            Err(match failed {
                Failed::Refused(e) => format!(
                    "{name}: refused: {}",
                    e.message.as_deref().unwrap_or("it gave no reason.")
                ),
                Failed::Unreachable(e) if e.code == connectrpc::ErrorCode::DeadlineExceeded => {
                    format!(
                        "{name} didn't answer within {} seconds. It may still do it.",
                        deadline.as_secs_f64()
                    )
                }
                Failed::Unreachable(_) | Failed::BadAnswer => format!("{name} is unreachable."),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tokio::sync::watch;

    use super::*;
    use crate::group::Limits;
    use crate::satellites::Seen;
    use crate::standalone::service::{Paths, Service};
    use crate::voice::Snapshot;

    #[tokio::test]
    async fn a_member_of_a_group_this_satellite_left_isnt_called() {
        let dir = tempfile::tempdir().unwrap();
        let (_status_tx, status) = watch::channel(Snapshot::default());
        let (_seen_tx, seen) = watch::channel(Seen::default());
        let service = Service::load(
            Paths::beside(&dir.path().join("settings.toml")),
            status.clone(),
        );
        let group = Group::new(
            service,
            "02:00:00:00:00:01",
            80,
            status,
            seen,
            Limits::default(),
        );
        // Named while the group was there.
        let kitchen = Member {
            mac: "02:00:00:00:00:02".into(),
            name: "Kitchen".into(),
            version: 1,
            removed: false,
        };

        let out = call(
            &group,
            &kitchen,
            Some(Mode::Standalone),
            "set_volume",
            json!({"percent": 40}),
        )
        .await;

        assert_eq!(out, Err(NO_GROUP.into()));
    }
}
