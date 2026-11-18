//! The satellite group's part of the standalone configuration (ADR 0026).
//!
//! Every change is one commit, so the switches, the id, the member list and
//! the key change together, and the key is written before the id. A change
//! that depends on what another satellite said is checked against the group
//! inside the commit: the caller talks to the other satellite first, and
//! never while a commit runs.

use pinna_assist::config::AssistConfig;

use super::{Draft, Refusal, Service};
use crate::group::model::{self, Member};
use crate::group::{Joined, Persisted, seal};

const OFF_HERE: &str = "Satellite group is off on this satellite. Switch it on first.";
const IN_A_GROUP_HERE: &str =
    "This satellite is in a group already. Reload the page, then add the satellite to that group.";
const OFF_THERE: &str =
    "Satellite group is off on the satellite being added. Switch it on there first.";
const IN_A_GROUP_THERE: &str = "The satellite being added is in a group already. Remove it from \
     that group first, or switch Satellite group off and on there.";
const NOT_AN_ID: &str = "The group's id isn't 32 lowercase hex digits.";
const NOT_LISTED: &str = "The group's member list doesn't name the satellite being added.";
const NO_OTHER: &str = "The group's member list has no other member.";
const CHANGED: &str = "The satellite group changed meanwhile. Reload and try again.";

/// What merging another member's list did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Merging {
    /// It brought nothing new, so nothing was written.
    Unchanged,
    /// The merged list was saved.
    Saved,
    /// It marks this satellite removed, or leaves no other member: the
    /// group was forgotten, with its key. The switch stays on.
    Forgot,
}

impl Draft {
    fn group(&self) -> Persisted {
        Persisted::read(&self.config.group, &self.config.group_member, &self.secrets)
    }

    fn put_group(&mut self, group: &Persisted) {
        group.write(
            &mut self.config.group,
            &mut self.config.group_member,
            &mut self.secrets,
        );
    }
}

impl Service {
    /// The satellite group as last committed.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "the dashboard reads it (ADR 0026), in a later change"
        )
    )]
    pub(crate) fn group(&self) -> Persisted {
        self.committed.live.borrow().group.clone()
    }

    /// Turn *Satellite group* on or off. Off leaves the group: the group it
    /// left is returned, so its members can be told.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "the dashboard sets it (ADR 0026), in a later change"
        )
    )]
    pub(crate) async fn set_group_switch(&self, on: bool) -> Result<Option<Joined>, Refusal> {
        let mut changed = false;
        let mut left = None;
        self.commit(|d| {
            let mut group = d.group();
            changed = group.on != on;
            if !on {
                left = group.joined.take();
            }
            group.on = on;
            d.put_group(&group);
            Ok(())
        })
        .await?;
        if changed {
            tracing::info!("satellite group: {}", if on { "on" } else { "off" });
        }
        if let Some(left) = &left {
            tracing::info!("left the satellite group {}", left.id);
        }
        Ok(left)
    }

    /// Turn *Control from the group* on or off.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "the dashboard sets it (ADR 0026), in a later change"
        )
    )]
    pub(crate) async fn set_group_control(&self, control: bool) -> Result<(), Refusal> {
        self.commit(|d| {
            let mut group = d.group();
            group.control = control;
            d.put_group(&group);
            Ok(())
        })
        .await?;
        Ok(())
    }

    /// Save a group this satellite started by adding another one, after that
    /// one took it ([`Joined::start`] makes it). Refused unless the switch
    /// is on and this satellite is in no group.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "Add starts a group (ADR 0026), in a later change")
    )]
    pub(crate) async fn start_group(&self, started: Joined, own_mac: &str) -> Result<(), Refusal> {
        let started = valid(started, own_mac)?;
        let (id, members) = (started.id.clone(), started.members.len());
        self.enter(started, OFF_HERE, IN_A_GROUP_HERE).await?;
        tracing::info!("started the satellite group {id} with {members} members");
        Ok(())
    }

    /// Join the group another member adds this satellite to. Refused unless
    /// the switch is on and this satellite is in no group: its switch is its
    /// owner's consent. Returns this satellite's own entry, under its name.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "Join (ADR 0026), in a later change")
    )]
    pub(crate) async fn join_group(
        &self,
        group: Joined,
        own_mac: &str,
        own_name: &str,
    ) -> Result<Member, Refusal> {
        let mut group = valid(group, own_mac)?;
        model::heard_name(&mut group.members, own_mac, own_name);
        let own = group
            .members
            .iter()
            .find(|m| model::same_mac(&m.mac, own_mac))
            .cloned()
            .ok_or_else(|| Refusal::Invalid(NOT_LISTED.into()))?;
        let id = group.id.clone();
        self.enter(group, OFF_THERE, IN_A_GROUP_THERE).await?;
        tracing::info!("joined the satellite group {id}");
        Ok(own)
    }

    /// Commit `group` as the group this satellite is in, if it is in none.
    async fn enter(&self, group: Joined, off: &str, in_a_group: &str) -> Result<(), Refusal> {
        self.commit(|d| {
            let mut current = d.group();
            if !current.on {
                return Err(Refusal::Precondition(off.into()));
            }
            if current.joined.is_some() {
                return Err(Refusal::Precondition(in_a_group.into()));
            }
            current.joined = Some(group);
            d.put_group(&current);
            Ok(())
        })
        .await?;
        Ok(())
    }

    /// Merge another member's list of group `id` into this satellite's,
    /// entry by entry, and save it if anything changed. A list that marks
    /// this satellite removed, or leaves it no other member, ends the group
    /// here in the same commit. Refused as a conflict when this satellite is
    /// no longer in group `id`.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "the minute's exchange merges (ADR 0026), in a later change"
        )
    )]
    pub(crate) async fn merge_members(
        &self,
        id: &str,
        remote: &[Member],
        own_mac: &str,
    ) -> Result<Merging, Refusal> {
        let mut outcome = Merging::Unchanged;
        self.commit(|d| {
            let mut group = d.group();
            let joined = group
                .joined
                .as_mut()
                .filter(|joined| joined.id == id)
                .ok_or_else(|| Refusal::Conflict(CHANGED.into()))?;
            let merged = model::merge(&joined.members, remote);
            if !merged.changed {
                return Ok(());
            }
            if model::own_removed(&merged.members, own_mac)
                || model::alone(&merged.members, own_mac)
            {
                group.joined = None;
                outcome = Merging::Forgot;
            } else {
                joined.members = merged.members;
                outcome = Merging::Saved;
            }
            d.put_group(&group);
            Ok(())
        })
        .await?;
        if outcome == Merging::Forgot {
            tracing::info!(
                "the satellite group {id} ended here: this satellite was removed, or is its last member"
            );
        }
        Ok(outcome)
    }

    /// Forget group `id` and its key. Refused as a conflict when this
    /// satellite is no longer in it, so a decision made about one group
    /// never ends the next.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "a member leaves by itself (ADR 0026), in a later change"
        )
    )]
    pub(crate) async fn forget_group(&self, id: &str) -> Result<Joined, Refusal> {
        let mut forgot = None;
        self.commit(|d| {
            let mut group = d.group();
            if group.joined.as_ref().is_none_or(|joined| joined.id != id) {
                return Err(Refusal::Conflict(CHANGED.into()));
            }
            forgot = group.joined.take();
            d.put_group(&group);
            Ok(())
        })
        .await?;
        tracing::info!("left the satellite group {id}");
        forgot.ok_or_else(|| Refusal::Conflict(CHANGED.into()))
    }
}

/// The keys of standalone.toml whose set-aside entries go stale when the
/// configuration changes from `before` to `after`. A `[[group_member]]`
/// entry this image couldn't read belongs to the group it was saved in: once
/// that group ends, or another takes its place, writing it back would let a
/// newer image read it as a member of the next group.
pub(super) fn stale_set_aside(
    before: &AssistConfig,
    after: &AssistConfig,
) -> &'static [&'static str] {
    if before.group.id == after.group.id {
        &[]
    } else {
        &["group_member"]
    }
}

/// A group from Add or Join as it may be saved: a real id, the member list
/// merged as any list from the wire is, this satellite listed and not
/// removed, and another member beside it.
fn valid(group: Joined, own_mac: &str) -> Result<Joined, Refusal> {
    if !seal::is_id(&group.id) {
        return Err(Refusal::Invalid(NOT_AN_ID.into()));
    }
    let members = model::merge(&[], &group.members).members;
    let listed = members
        .iter()
        .any(|m| !m.removed && model::same_mac(&m.mac, own_mac));
    if !listed {
        return Err(Refusal::Invalid(NOT_LISTED.into()));
    }
    if model::alone(&members, own_mac) {
        return Err(Refusal::Invalid(NO_OTHER.into()));
    }
    Ok(Joined { members, ..group })
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::Arc;

    use tokio::sync::watch;

    use super::super::{NOT_SAVED, Paths};
    use super::*;
    use crate::group::seal::Key;
    use crate::voice::Snapshot;

    const OWN: &str = "02:00:00:00:00:01";
    const B: &str = "02:00:00:00:00:02";
    const C: &str = "02:00:00:00:00:03";
    const ID: &str = "00112233445566778899aabbccddeeff";

    struct Fixture {
        service: Arc<Service>,
        _status: watch::Sender<Snapshot>,
        dir: tempfile::TempDir,
    }

    impl Fixture {
        fn new() -> Self {
            Self::with_files(&[])
        }

        /// A service started on these files, beside settings.toml.
        fn with_files(files: &[(&str, &str)]) -> Self {
            let dir = tempfile::tempdir().unwrap();
            for (name, text) in files {
                fs::write(dir.path().join(name), text).unwrap();
            }
            let (status, rx) = watch::channel(Snapshot {
                friendly_name: "Hall".into(),
                ..Snapshot::default()
            });
            let service = Service::load(Paths::beside(&dir.path().join("settings.toml")), rx);
            Self {
                service,
                _status: status,
                dir,
            }
        }

        fn reload(&self) -> Arc<Service> {
            let (_, rx) = watch::channel(Snapshot::default());
            Service::load(Paths::beside(&self.dir.path().join("settings.toml")), rx)
        }

        fn file(&self, name: &str) -> String {
            fs::read_to_string(self.dir.path().join(name)).unwrap_or_default()
        }

        /// Switched on, in a group of this satellite and B.
        async fn in_group(&self) -> Joined {
            self.service.set_group_switch(true).await.unwrap();
            let group = Joined::start(m(OWN, "Hall", 1, false), m(B, "Kitchen", 1, false)).unwrap();
            self.service.start_group(group.clone(), OWN).await.unwrap();
            group
        }
    }

    fn m(mac: &str, name: &str, version: u64, removed: bool) -> Member {
        Member {
            mac: mac.into(),
            name: name.into(),
            version,
            removed,
        }
    }

    fn joined(members: Vec<Member>) -> Joined {
        Joined {
            id: ID.into(),
            key: Key::new([9; 32]),
            members,
        }
    }

    #[tokio::test]
    async fn the_switch_is_saved_and_changes_the_record() {
        let f = Fixture::new();
        assert_eq!(f.service.group(), Persisted::default());
        assert_eq!(f.service.group().record(), None);

        assert_eq!(f.service.set_group_switch(true).await.unwrap(), None);
        assert!(f.service.group().on);
        assert_eq!(f.service.group().record(), Some(String::new()));
        assert!(f.file("standalone.toml").contains("[group]\non = true"));
        assert!(f.reload().group().on);

        f.service.set_group_switch(false).await.unwrap();
        assert_eq!(f.service.group().record(), None);
        assert!(!f.reload().group().on);
    }

    #[tokio::test]
    async fn control_from_the_group_is_on_until_switched_off() {
        let f = Fixture::new();
        assert!(f.service.group().control);
        f.service.set_group_control(false).await.unwrap();
        assert!(!f.service.group().control);
        assert!(f.file("standalone.toml").contains("control = false"));
        assert!(!f.reload().group().control);
        f.service.set_group_control(true).await.unwrap();
        assert!(f.reload().group().control);
    }

    #[tokio::test]
    async fn a_started_group_is_saved_with_its_key_apart() {
        let f = Fixture::new();
        let group = f.in_group().await;
        let saved = f.service.group();
        assert_eq!(saved.joined.as_ref(), Some(&group));
        assert_eq!(saved.record(), Some(group.id.clone()));

        let config = f.file("standalone.toml");
        assert!(
            config.contains(&format!("id = \"{}\"", group.id)),
            "{config}"
        );
        assert_eq!(config.matches("[[group_member]]").count(), 2, "{config}");
        assert!(!config.contains(&group.key.to_base64()), "{config}");
        let secrets = f.file("secrets.toml");
        assert!(secrets.contains(&group.key.to_base64()), "{secrets}");
        assert!(secrets.contains(&group.id), "{secrets}");

        assert_eq!(f.reload().group(), saved);
    }

    #[tokio::test]
    async fn a_group_starts_only_with_the_switch_on_and_in_no_group() {
        let f = Fixture::new();
        let group = Joined::start(m(OWN, "Hall", 1, false), m(B, "Kitchen", 1, false)).unwrap();
        assert_eq!(
            f.service.start_group(group.clone(), OWN).await,
            Err(Refusal::Precondition(OFF_HERE.into()))
        );
        assert_eq!(f.file("secrets.toml"), "");

        let first = f.in_group().await;
        assert_eq!(
            f.service.start_group(group, OWN).await,
            Err(Refusal::Precondition(IN_A_GROUP_HERE.into()))
        );
        assert_eq!(f.service.group().joined, Some(first));
    }

    #[tokio::test]
    async fn joining_takes_the_list_and_answers_with_the_own_entry() {
        let f = Fixture::new();
        let group = joined(vec![
            m(B, "Kitchen", 3, false),
            m(OWN, "Pinna 000001", 1, false),
        ]);
        assert_eq!(
            f.service.join_group(group.clone(), OWN, "Hall").await,
            Err(Refusal::Precondition(OFF_THERE.into()))
        );

        f.service.set_group_switch(true).await.unwrap();
        let own = f
            .service
            .join_group(group.clone(), OWN, "Hall")
            .await
            .unwrap();
        assert_eq!(own, m(OWN, "Hall", 1, false));
        let saved = f.service.group().joined.unwrap();
        assert_eq!(saved.id, ID);
        assert_eq!(saved.key, group.key);
        // Sorted by MAC, with this satellite's own name.
        assert_eq!(
            saved.members,
            [m(OWN, "Hall", 1, false), m(B, "Kitchen", 3, false)]
        );
        assert_eq!(f.reload().group().joined, Some(saved));

        assert_eq!(
            f.service.join_group(group, OWN, "Hall").await,
            Err(Refusal::Precondition(IN_A_GROUP_THERE.into()))
        );
    }

    #[tokio::test]
    async fn a_group_that_cannot_be_one_is_refused() {
        let f = Fixture::new();
        f.service.set_group_switch(true).await.unwrap();
        let both = vec![m(OWN, "Hall", 1, false), m(B, "Kitchen", 1, false)];
        let cases = [
            (
                Joined {
                    id: ID.to_uppercase(),
                    ..joined(both.clone())
                },
                NOT_AN_ID,
            ),
            (joined(vec![m(B, "Kitchen", 1, false)]), NOT_LISTED),
            (
                joined(vec![m(OWN, "Hall", 2, true), m(B, "Kitchen", 1, false)]),
                NOT_LISTED,
            ),
            (
                joined(vec![m(OWN, "Hall", 1, false), m(B, "Kitchen", 2, true)]),
                NO_OTHER,
            ),
            (joined(vec![m(OWN, "Hall", 1, false)]), NO_OTHER),
        ];
        for (group, why) in cases {
            assert_eq!(
                f.service.join_group(group.clone(), OWN, "Hall").await,
                Err(Refusal::Invalid(why.into())),
                "{group:?}"
            );
            assert_eq!(
                f.service.start_group(group, OWN).await,
                Err(Refusal::Invalid(why.into()))
            );
        }
        assert_eq!(f.service.group().joined, None);
    }

    /// A version TOML can't store (its integers are signed 64-bit) never
    /// reaches standalone.toml: the file would be written, fail to parse on
    /// the next start, and take every other setting with it.
    #[tokio::test]
    async fn a_version_too_large_to_save_never_reaches_the_file() {
        let f = Fixture::with_files(&[("standalone.toml", "follow_ups = false\n")]);
        f.service.set_group_switch(true).await.unwrap();
        let huge = model::MAX_VERSION + 1;

        // The other member's entry is dropped, which leaves none.
        let group = joined(vec![
            m(OWN, "Hall", 1, false),
            m(B, "Kitchen", u64::MAX, false),
        ]);
        assert_eq!(
            f.service.join_group(group, OWN, "Hall").await,
            Err(Refusal::Invalid(NO_OTHER.into()))
        );
        // A third entry is dropped, and the group joined without it.
        let group = joined(vec![
            m(OWN, "Hall", 1, false),
            m(B, "Kitchen", 1, false),
            m(C, "Office", huge, false),
        ]);
        f.service.join_group(group, OWN, "Hall").await.unwrap();
        // Nor does a merge take one, removal or not.
        for remote in [
            [m(OWN, "Hall", u64::MAX, true)],
            [m(C, "Office", huge, false)],
        ] {
            assert_eq!(
                f.service.merge_members(ID, &remote, OWN).await,
                Ok(Merging::Unchanged)
            );
        }
        // The highest version that can be saved is taken.
        let top = [m(C, "Office", model::MAX_VERSION, false)];
        assert_eq!(
            f.service.merge_members(ID, &top, OWN).await,
            Ok(Merging::Saved)
        );

        let config = f.file("standalone.toml");
        assert!(config.parse::<toml::Table>().is_ok(), "{config}");
        let reloaded = f.reload();
        assert!(!reloaded.view().follow_ups, "the other settings survive");
        assert_eq!(
            reloaded.group().joined.unwrap().members,
            [
                m(OWN, "Hall", 1, false),
                m(B, "Kitchen", 1, false),
                m(C, "Office", model::MAX_VERSION, false),
            ]
        );
    }

    #[tokio::test]
    async fn a_merge_that_brings_nothing_new_writes_nothing() {
        let f = Fixture::new();
        let group = f.in_group().await;
        let before = f.file("standalone.toml");
        let live = f.service.live();
        assert_eq!(
            f.service
                .merge_members(&group.id, &group.members, OWN)
                .await,
            Ok(Merging::Unchanged)
        );
        // An older entry, or one with only another name, changes nothing.
        let older = [m(B, "Cuisine", 1, false)];
        assert_eq!(
            f.service.merge_members(&group.id, &older, OWN).await,
            Ok(Merging::Unchanged)
        );
        assert!(!live.has_changed().unwrap(), "nothing was published");
        assert_eq!(f.file("standalone.toml"), before);
    }

    #[tokio::test]
    async fn a_merge_that_brings_a_member_saves_it() {
        let f = Fixture::new();
        let group = f.in_group().await;
        let remote = [m(B, "Kitchen", 1, false), m(C, "Office", 1, false)];
        assert_eq!(
            f.service.merge_members(&group.id, &remote, OWN).await,
            Ok(Merging::Saved)
        );
        let members = f.service.group().joined.unwrap().members;
        assert_eq!(
            members,
            [
                m(OWN, "Hall", 1, false),
                m(B, "Kitchen", 1, false),
                m(C, "Office", 1, false)
            ]
        );
        assert_eq!(f.reload().group().joined.unwrap().members, members);
        assert_eq!(
            f.file("standalone.toml")
                .matches("[[group_member]]")
                .count(),
            3
        );
    }

    #[tokio::test]
    async fn a_merge_that_removes_this_satellite_forgets_the_group() {
        let f = Fixture::new();
        let group = f.in_group().await;
        let remote = [m(OWN, "Hall", 2, true), m(B, "Kitchen", 1, false)];
        assert_eq!(
            f.service.merge_members(&group.id, &remote, OWN).await,
            Ok(Merging::Forgot)
        );
        let saved = f.service.group();
        assert!(saved.on);
        assert_eq!(saved.joined, None);
        assert_eq!(saved.record(), Some(String::new()));
        assert!(!f.file("secrets.toml").contains(&group.key.to_base64()));
        let config = f.file("standalone.toml");
        assert!(!config.contains("group_member"), "{config}");
        assert!(!config.contains(&group.id), "{config}");
        assert_eq!(f.reload().group(), saved);
    }

    #[tokio::test]
    async fn a_merge_that_leaves_no_other_member_ends_the_group() {
        let f = Fixture::new();
        let group = f.in_group().await;
        let remote = [m(B, "Kitchen", 2, true)];
        assert_eq!(
            f.service.merge_members(&group.id, &remote, OWN).await,
            Ok(Merging::Forgot)
        );
        assert_eq!(f.service.group().joined, None);
        assert!(f.service.group().on);
    }

    #[tokio::test]
    async fn a_merge_for_another_group_is_a_conflict() {
        let f = Fixture::new();
        let group = f.in_group().await;
        let remote = [m(C, "Office", 1, false)];
        assert_eq!(
            f.service.merge_members(ID, &remote, OWN).await,
            Err(Refusal::Conflict(CHANGED.into()))
        );
        assert_eq!(f.service.group().joined, Some(group));
    }

    #[tokio::test]
    async fn forgetting_needs_the_group_it_names() {
        let f = Fixture::new();
        assert_eq!(
            f.service.forget_group(ID).await,
            Err(Refusal::Conflict(CHANGED.into()))
        );
        let group = f.in_group().await;
        assert_eq!(
            f.service.forget_group(ID).await,
            Err(Refusal::Conflict(CHANGED.into()))
        );
        assert_eq!(f.service.forget_group(&group.id).await, Ok(group.clone()));
        assert_eq!(f.service.group().joined, None);
        assert!(f.service.group().on);
        assert!(!f.file("secrets.toml").contains(&group.key.to_base64()));
    }

    #[tokio::test]
    async fn switching_off_leaves_the_group() {
        let f = Fixture::new();
        let group = f.in_group().await;
        assert_eq!(
            f.service.set_group_switch(false).await,
            Ok(Some(group.clone()))
        );
        let saved = f.service.group();
        assert!(!saved.on);
        assert_eq!(saved.joined, None);
        assert_eq!(saved.record(), None);
        assert!(!f.file("secrets.toml").contains(&group.key.to_base64()));
        assert!(!f.file("standalone.toml").contains("group_member"));
        // Back on, it is in no group.
        assert_eq!(f.service.set_group_switch(true).await, Ok(None));
        assert_eq!(f.service.group().record(), Some(String::new()));
    }

    #[tokio::test]
    async fn a_failed_configuration_write_puts_the_previous_keys_back() {
        let f = Fixture::new();
        f.service.set_group_switch(true).await.unwrap();
        let config = f.dir.path().join("standalone.toml");
        fs::remove_file(&config).unwrap();
        fs::create_dir_all(config.join("blocked")).unwrap();

        let group = Joined::start(m(OWN, "Hall", 1, false), m(B, "Kitchen", 1, false)).unwrap();
        assert_eq!(
            f.service.start_group(group.clone(), OWN).await,
            Err(Refusal::Precondition(NOT_SAVED.into()))
        );
        // The key was written first, then put back.
        let keys = f.file("secrets.toml");
        assert!(!keys.contains("[group]"), "{keys}");
        assert!(!keys.contains(&group.key.to_base64()), "{keys}");
        assert_eq!(f.service.group().joined, None);
        assert_eq!(f.reload().group().joined, None);
    }

    #[test]
    fn a_crash_between_the_two_writes_reads_as_no_group() {
        let key = Key::new([9; 32]).to_base64();
        // The key was written for a new group, standalone.toml wasn't.
        let f = Fixture::with_files(&[
            ("standalone.toml", "[group]\non = true\n"),
            (
                "secrets.toml",
                &format!("[group]\nid = \"{ID}\"\nkey = \"{key}\"\n"),
            ),
        ]);
        assert_eq!(f.service.group().joined, None);
        assert_eq!(f.service.group().record(), Some(String::new()));

        // The key was removed, standalone.toml still names the group.
        let f = Fixture::with_files(&[(
            "standalone.toml",
            &format!(
                "[group]\non = true\nid = \"{ID}\"\n\n[[group_member]]\nmac = \"{OWN}\"\n\
                 name = \"Hall\"\nversion = 1\n\n[[group_member]]\nmac = \"{B}\"\n\
                 name = \"Kitchen\"\nversion = 1\n"
            ),
        )]);
        assert_eq!(f.service.group().joined, None);
        assert_eq!(f.service.group().record(), Some(String::new()));
    }

    #[tokio::test]
    async fn the_next_change_clears_a_group_left_half_written() {
        let f = Fixture::with_files(&[(
            "standalone.toml",
            &format!(
                "[group]\non = true\nid = \"{ID}\"\n\n[[group_member]]\nmac = \"{B}\"\n\
                 name = \"Kitchen\"\nversion = 1\n"
            ),
        )]);
        f.service.set_group_control(false).await.unwrap();
        let config = f.file("standalone.toml");
        assert!(!config.contains(ID), "{config}");
        assert!(!config.contains("group_member"), "{config}");
    }

    #[tokio::test]
    async fn the_group_is_read_entry_by_entry() {
        let key = Key::new([9; 32]).to_base64();
        let f = Fixture::with_files(&[
            (
                "standalone.toml",
                &format!(
                    "[group]\non = true\nid = \"{ID}\"\ncontrol = false\nanswer_once = false\n\n\
                     [[group_member]]\nmac = \"{OWN}\"\nname = \"Hall\"\nversion = 1\n\n\
                     [[group_member]]\nmac = \"{B}\"\nname = \"Kitchen\"\nversion = \"two\"\n\n\
                     [[group_member]]\nmac = \"{C}\"\nname = \"Office\"\nversion = 2\n"
                ),
            ),
            (
                "secrets.toml",
                &format!("[group]\nid = \"{ID}\"\nkey = \"{key}\"\n"),
            ),
        ]);
        let saved = f.service.group();
        assert!(saved.on && !saved.control);
        // The entry this image can't read costs that entry only.
        assert_eq!(
            saved.joined.unwrap().members,
            [m(OWN, "Hall", 1, false), m(C, "Office", 2, false)]
        );

        // Any commit writes back the unknown key and the unreadable entry.
        f.service.set_group_control(true).await.unwrap();
        let config = f.file("standalone.toml");
        assert!(config.contains("answer_once = false"), "{config}");
        assert!(config.contains("version = \"two\""), "{config}");
        assert_eq!(config.matches("[[group_member]]").count(), 3, "{config}");
    }

    #[tokio::test]
    async fn an_entry_it_cannot_read_ends_with_its_group() {
        let key = Key::new([9; 32]).to_base64();
        let f = Fixture::with_files(&[
            (
                "standalone.toml",
                &format!(
                    "[group]\non = true\nid = \"{ID}\"\n\n\
                     [[group_member]]\nmac = \"{OWN}\"\nname = \"Hall\"\nversion = 1\n\n\
                     [[group_member]]\nmac = \"{B}\"\nname = \"Kitchen\"\nversion = 1\n\n\
                     [[group_member]]\nmac = \"{C}\"\nname = \"Office\"\nversion = \"two\"\n"
                ),
            ),
            (
                "secrets.toml",
                &format!("[group]\nid = \"{ID}\"\nkey = \"{key}\"\n"),
            ),
        ]);
        f.service.forget_group(ID).await.unwrap();
        let config = f.file("standalone.toml");
        assert!(!config.contains("group_member"), "{config}");

        // Nor does it come back with the next group, or after a restart.
        f.in_group().await;
        let config = f.file("standalone.toml");
        assert!(!config.contains("\"two\""), "{config}");
        assert_eq!(config.matches("[[group_member]]").count(), 2, "{config}");
        f.reload().set_group_control(false).await.unwrap();
        assert!(!f.file("standalone.toml").contains("\"two\""));
    }

    #[test]
    fn a_switch_it_cannot_read_falls_back_to_its_default_alone() {
        let f = Fixture::with_files(&[(
            "standalone.toml",
            "follow_ups = false\n[group]\non = \"yes\"\ncontrol = false\n",
        )]);
        let saved = f.service.group();
        assert!(!saved.on);
        assert!(!saved.control);
        assert!(!f.service.view().follow_ups);
    }
}
