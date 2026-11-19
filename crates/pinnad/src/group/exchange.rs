//! The minute's exchange (ADR 0026): after each search, a member asks every
//! other member the search lists for its list, merges them all in one
//! commit, and takes the friendly names the search heard. A list signed with
//! the key that marks this satellite removed, or leaves it alone, ends the
//! group here.
//!
//! A member also leaves by itself once every other member has answered two
//! searches in a row outside the group and refused its `GetGroup` as not in
//! this group: nobody is left to send it a signed removal. The record alone
//! never counts, since a reload can leave an old one published (ADR 0021)
//! and an older image publishes no `group` key; nor does a refusal alone.

use std::sync::Arc;

use futures::future::join_all;

use super::client::To;
use super::model::Member;
use super::runtime::{Group, listed, others};
use crate::satellites::Seen;
use crate::standalone::service::group::Merging;

/// Searches in a row a member must answer outside the group before it counts
/// as gone: one stale record or one lost answer shouldn't end a group.
pub const OUTSIDE: u32 = 2;

/// Run an exchange after every search, for as long as pinnad does.
pub async fn run(group: Arc<Group>) {
    let mut satellites = group.satellites();
    let mut last = None;
    while satellites.changed().await.is_ok() {
        let seen = satellites.borrow_and_update().clone();
        if seen.searched && seen.on_network {
            once(&group, &seen, last).await;
            last = Some(seen.search);
        }
    }
}

/// One exchange after the search `seen`. `last` is the search the previous
/// exchange followed: searches can pass unseen while an exchange runs, and
/// what happened in them isn't known.
pub async fn once(group: &Group, seen: &Seen, last: Option<u64>) {
    if last.is_some_and(|last| seen.search > last + 1) {
        group.reset_outside();
    }
    let Some(joined) = group.service().group().joined else {
        group.reset_outside();
        return;
    };
    let own_mac = group.own_mac().to_owned();
    let limit = group.limits().exchange;

    // Only members the search lists are called (ADR 0026); the others are
    // unreachable, and their count of searches outside the group ends.
    let called = listed(seen, &joined, &own_mac);
    for mac in others(&joined, &own_mac) {
        if !called.iter().any(|(m, _)| *m == mac) {
            group.outside(&mac, false);
        }
    }
    let answers = join_all(called.iter().map(|(mac, found)| async {
        let answer = group
            .get_group(&To::heard(found), &joined, mac, limit)
            .await;
        (mac.as_str(), *found, answer)
    }))
    .await;

    let mut lists: Vec<Member> = Vec::new();
    let mut heard: Vec<(String, String)> = Vec::new();
    for (mac, found, answer) in answers {
        if found.missed == 0 {
            group.seen_by_search(mac);
            heard.push((mac.to_owned(), found.friendly_name.clone()));
        }
        let outside = found.missed == 0
            && found.group.as_deref() != Some(joined.id.as_str())
            && matches!(&answer, Err(failed) if failed.is_not_in_this_group());
        group.outside(mac, outside);
        match answer {
            Ok(list) => lists.extend(list),
            Err(failed) => group.failed(mac, &found.friendly_name, "asking", &failed),
        }
    }
    // This satellite's own name, which the search never lists.
    heard.push((own_mac.clone(), group.own_name()));

    match group.merge(&joined.id, &lists, &heard).await {
        Ok(Merging::Forgot) => {
            group.reset_outside();
            return;
        }
        Ok(_) => {}
        // The group changed meanwhile; the next search finds out more.
        Err(e) => {
            tracing::info!("satellite group: the minute's exchange: {e}");
            return;
        }
    }

    let Some(now) = group
        .service()
        .group()
        .joined
        .filter(|now| now.id == joined.id)
    else {
        return;
    };
    let remaining: Vec<String> = others(&now, &own_mac).collect();
    let gone = !remaining.is_empty()
        && remaining
            .iter()
            .all(|mac| group.peer(mac).is_some_and(|peer| peer.outside >= OUTSIDE));
    if gone {
        tracing::info!(
            "satellite group: every other member of {} answers outside it, so this satellite leaves it",
            now.id
        );
        if let Err(e) = group.service().forget_group(&now.id).await {
            tracing::info!("satellite group: leaving: {e}");
        }
        group.reset_outside();
    }
}
