//! The `send_message` tool (ADR 0026): the brain hands a message to another
//! member of the satellite group, or to all of them, and says what each one
//! did with it.
//!
//! It is offered only to a brain whose tools pinnad runs, and only while
//! another member was last heard in standalone mode. Every member is called
//! at once, at the addresses the search heard, inside the brain's tool
//! limit. A message's text never reaches a log line.

use futures::future::join_all;
use pinna_api::PeerDelivery;
use pinna_api::peer_delivery::Outcome;

use crate::group::Group;
use crate::group::client::{Failed, To};
use crate::group::model::{ALL, Member, name_key};
use crate::settings::Mode;

pub const NAME: &str = "send_message";

/// Why the tool isn't offered, when a brain calls it anyway.
pub const NO_ONE: &str = "No other satellite of this one's group is in standalone mode, so there is no one to send a \
     message to.";

/// Whether `send_message` is offered: this satellite is in a group with
/// another member whose last signed call or answer named standalone mode.
pub fn offered(group: &Group) -> bool {
    group
        .other_members()
        .iter()
        .any(|(_, mode)| *mode == Some(Mode::Standalone))
}

/// Hand `text` to the member named `to`, or to every other member for
/// `all`, and say what each did with it, one line each.
pub async fn send(group: &Group, to: &str, text: &str) -> Result<String, String> {
    let text = crate::checks::speak_text(text)?;
    let Some(joined) = group.service().group().joined else {
        return Err(NO_ONE.into());
    };
    let members = group.other_members();
    let targets = targets(group, &members, to)?;
    let seen = group.seen();
    let characters = text.chars().count();
    let lines = join_all(targets.into_iter().map(|(member, mode)| {
        let (seen, joined, text) = (&seen, &joined, &text);
        async move {
            let name = member.name.as_str();
            let line = if mode == Some(Mode::HomeAssistant) {
                format!("{name} is in Home Assistant mode, out of reach.")
            } else {
                // A member the search no longer lists (ADR 0021), or one it
                // heard no address for, isn't called.
                match seen
                    .by_mac(&member.mac)
                    .filter(|f| !f.ipv4.is_empty() || !f.ipv6.is_empty())
                {
                    None => format!("{name}: unreachable."),
                    Some(found) => {
                        let deadline = group.limits().call;
                        let answer = group
                            .deliver(&To::heard(found), joined, &member.mac, text, deadline)
                            .await;
                        answered(group, &member, answer)
                    }
                }
            };
            tracing::info!("satellite group: a message, {characters} characters. {line}");
            line
        }
    }))
    .await;
    Ok(lines.join("\n"))
}

/// The members `to` names: one by its name, or every other one for `all`.
fn targets(
    group: &Group,
    members: &[(Member, Option<Mode>)],
    to: &str,
) -> Result<Vec<(Member, Option<Mode>)>, String> {
    let key = name_key(to);
    if key.is_empty() {
        return Err(format!("{NAME} needs to: a satellite's name, or all."));
    }
    if key == name_key(ALL) {
        return Ok(members.to_vec());
    }
    // This satellite's name: a member that shares it after a rename is
    // refused as the control tools refuse it (ADR 0026), not taken for the
    // one meant.
    let own = group.own_name();
    if key == name_key(&own) {
        let others: Vec<&Member> = members
            .iter()
            .map(|(m, _)| m)
            .filter(|m| name_key(&m.name) == key)
            .collect();
        if others.is_empty() {
            return Err(format!(
                "{} is this satellite, and it can't send a message to itself. The other members \
                 of its group are {}.",
                own.trim(),
                names(members)
            ));
        }
        let own_host = group.status().name.clone();
        return Err(shared(group, &own, others.into_iter(), Some(&own_host)));
    }
    named(group, members, to).map(|one| vec![one])
}

/// The one other member of `members` named `name`. Another tool that names
/// a member finds it the same way (ADR 0026).
pub(super) fn named(
    group: &Group,
    members: &[(Member, Option<Mode>)],
    name: &str,
) -> Result<(Member, Option<Mode>), String> {
    let key = name_key(name);
    let named: Vec<_> = members
        .iter()
        .filter(|(m, _)| name_key(&m.name) == key)
        .collect();
    match named.as_slice() {
        [] => Err(format!(
            "No satellite of this one's group is named {}. Its other members are {}.",
            name.trim(),
            names(members)
        )),
        [one] => Ok((*one).clone()),
        _ => Err(shared(
            group,
            &named[0].0.name,
            named.iter().map(|(m, _)| m),
            None,
        )),
    }
}

/// The names of `members`, for a sentence.
pub(super) fn names(members: &[(Member, Option<Mode>)]) -> String {
    let names: Vec<&str> = members.iter().map(|(m, _)| m.name.as_str()).collect();
    names.join(", ")
}

/// Why `name` is refused when a rename made several satellites of the
/// group share it (ADR 0026): it names them by their host names, which are
/// unique on the network, this satellite's (`own_host`) first when it is
/// one of them.
pub(super) fn shared<'a>(
    group: &Group,
    name: &str,
    named: impl Iterator<Item = &'a Member>,
    own_host: Option<&str>,
) -> String {
    let seen = group.seen();
    let hosts: Vec<String> = own_host
        .map(str::to_owned)
        .into_iter()
        .chain(named.map(|m| {
            seen.by_mac(&m.mac)
                .map_or_else(|| m.mac.clone(), |f| f.host.clone())
        }))
        .collect();
    format!(
        "{} satellites of this one's group are named {name}: {}. Rename one on its dashboard \
         first.",
        hosts.len(),
        hosts.join(", ")
    )
}

/// One member's answer as a line for the brain.
fn answered(group: &Group, member: &Member, answer: Result<PeerDelivery, Failed>) -> String {
    let name = &member.name;
    match answer {
        Ok(delivery) => match delivery.outcome.as_known() {
            Some(Outcome::OUTCOME_PLAYED) => format!("{name}: played now."),
            Some(Outcome::OUTCOME_HELD) => format!("{name}: held until it is free."),
            _ if delivery.reason.trim().is_empty() => format!("{name}: refused."),
            _ => format!("{name}: refused: {}", delivery.reason.trim()),
        },
        Err(failed) => {
            group.failed(&member.mac, name, "sending a message to", &failed);
            match failed {
                Failed::Refused(e) => format!(
                    "{name}: refused: {}",
                    e.message.as_deref().unwrap_or("it gave no reason.")
                ),
                Failed::Unreachable(_) | Failed::BadAnswer => format!("{name}: unreachable."),
            }
        }
    }
}
