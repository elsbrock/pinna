//! The update tools: what the dashboard's Updates page shows and does,
//! through the same `Updater` calls (ADR 0017, amending ADR 0022).

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use connectrpc::ConnectError;
use pinna_api::{GetUpdatesResponse, InstallStage, ReleaseAction};
use serde_json::{Map, Value, json};

use super::{Args, DeviceTools};
use crate::updates::{Origin, Source, Updater};

/// How long `check_for_updates` waits. A check is given up after three
/// minutes, a tool call after 10 s (`pinna_assist::limits`): at 8 s the tool
/// still answers, with the result or with word that the check goes on.
const CHECK_WAIT: Duration = Duration::from_secs(8);

pub(super) const NO_UPDATER: &str = "The satellite can't check or install updates here.";

fn updater(tools: &DeviceTools) -> Result<&std::sync::Arc<Updater>, String> {
    tools.kit.updater.as_ref().ok_or_else(|| NO_UPDATER.into())
}

/// The Updater's own sentence for a refusal.
fn sentence(e: ConnectError) -> String {
    e.message
        .unwrap_or_else(|| "The update could not be done.".into())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// `updates`: where the satellite stands.
pub(super) async fn updates(tools: &DeviceTools) -> Result<String, String> {
    Ok(state(updater(tools)?, now_ms()).await.to_string())
}

/// `check_for_updates`: checks now and answers with what it found.
pub(super) async fn check(tools: &DeviceTools) -> Result<String, String> {
    check_within(updater(tools)?, CHECK_WAIT).await
}

async fn check_within(updater: &std::sync::Arc<Updater>, wait: Duration) -> Result<String, String> {
    // Its own task: the check goes on after the tool has answered.
    let task = {
        let updater = updater.clone();
        tokio::spawn(async move { updater.check().await })
    };
    match tokio::time::timeout(wait, task).await {
        Ok(Ok(Ok(()))) => Ok(state(updater, now_ms()).await.to_string()),
        Ok(Ok(Err(e))) => Err(sentence(e)),
        Ok(Err(_)) => Err("The check stopped before it finished.".into()),
        Err(_) => Ok(
            "The check is still running and goes on. The updates tool has the result once it is done."
                .into(),
        ),
    }
}

/// `install_update`: installs the release the last check found.
pub(super) async fn install(tools: &DeviceTools, args: &Args) -> Result<String, String> {
    let updater = updater(tools)?;
    let version = args.need_text("version")?.trim();
    // Only to word the answer; Install decides.
    let action = updater
        .get_updates()
        .await
        .release
        .as_option()
        .filter(|r| r.version == version)
        .and_then(|r| r.action.as_known());
    updater
        .install_release(version, false, Origin::Brain)
        .await
        .map_err(sentence)?;
    Ok(match action {
        Some(ReleaseAction::RELEASE_ACTION_START) => format!(
            "{version} is already in the other slot. The satellite restarts into it once it is idle."
        ),
        Some(ReleaseAction::RELEASE_ACTION_RESTART) => {
            format!(
                "{version} is already installed. The satellite restarts into it once it is idle."
            )
        }
        _ => format!(
            "Installing {version}. It downloads in the background, and the satellite restarts once it is idle."
        ),
    })
}

async fn state(updater: &Updater, now_ms: u64) -> Value {
    view(
        &updater.get_updates().await,
        &updater.source(),
        updater.install_progress(),
        now_ms,
    )
}

/// Free text without web addresses: a model is not handed a link that could
/// carry a token or be followed.
fn plain(text: &str) -> String {
    text.split_whitespace()
        .map(|word| {
            if word.contains("://") {
                "(address)"
            } else {
                word
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn minutes_since(then_ms: u64, now_ms: u64) -> u64 {
    now_ms.saturating_sub(then_ms) / 60_000
}

fn action_word(action: Option<ReleaseAction>) -> &'static str {
    use ReleaseAction::*;
    match action {
        Some(RELEASE_ACTION_INSTALL) => "install",
        Some(RELEASE_ACTION_UP_TO_DATE) => "up_to_date",
        Some(RELEASE_ACTION_OLDER) => "older",
        Some(RELEASE_ACTION_START) => "start",
        Some(RELEASE_ACTION_RESTART) => "restart",
        Some(RELEASE_ACTION_FAILED) => "failed",
        Some(RELEASE_ACTION_BLOCKED) => "blocked",
        Some(RELEASE_ACTION_OTHER_MODEL) => "other_model",
        _ => "unknown",
    }
}

fn stage_word(stage: InstallStage) -> &'static str {
    match stage {
        InstallStage::INSTALL_STAGE_CHECK => "checking",
        InstallStage::INSTALL_STAGE_DOWNLOAD => "downloading",
        InstallStage::INSTALL_STAGE_WRITE => "writing",
        InstallStage::INSTALL_STAGE_VERIFY => "verifying",
        InstallStage::INSTALL_STAGE_INSTALLED => "installed",
        _ => "starting",
    }
}

/// What the Updates page shows, for a model. No web address is part of it:
/// not the manifest's, the notes' or one inside a sentence.
fn view(
    resp: &GetUpdatesResponse,
    source: &Source,
    progress: Option<(InstallStage, Option<f32>)>,
    now_ms: u64,
) -> Value {
    let mut out = Map::new();
    out.insert("version".into(), resp.booted_version.as_str().into());
    if resp.development_build {
        out.insert("development_build".into(), true.into());
    }
    if !resp.available {
        out.insert("unavailable".into(), plain(&resp.unavailable_reason).into());
    }
    if let Some(booted) = resp.slots.iter().find(|s| s.booted) {
        out.insert("slot".into(), booted.name.as_str().into());
    }
    if let Some(next) = resp.slots.iter().find(|s| s.active && !s.booted) {
        out.insert("next_boot".into(), next.name.as_str().into());
    }
    if let Some(other) = resp.slots.iter().find(|s| !s.booted) {
        let mut slot = json!({ "name": other.name, "state": other.state });
        if !other.version.is_empty() {
            slot["version"] = other.version.as_str().into();
        }
        out.insert("other_slot".into(), slot);
    }
    let channel = match source {
        Source::Channel(c) => c.as_str(),
        Source::Manifest(_) => "manifest",
    };
    out.insert("channel".into(), channel.into());

    if let Some(check) = resp.check.as_option() {
        let mut c = json!({ "checking": check.checking });
        if check.finished_unix_ms > 0 {
            c["minutes_since_last_check"] = minutes_since(check.finished_unix_ms, now_ms).into();
        }
        if !check.error.is_empty() {
            c["error"] = plain(&check.error).into();
        }
        if check.no_release {
            c["no_release"] = true.into();
        }
        if check.next_unix_ms > 0 {
            c["minutes_until_next_check"] = check
                .next_unix_ms
                .saturating_sub(now_ms)
                .div_ceil(60_000)
                .into();
        }
        out.insert("check".into(), c);
    }
    if let Some(release) = resp.release.as_option() {
        let mut r = json!({
            "version": release.version,
            "channel": release.channel,
            "summary": plain(&release.summary),
            "action": action_word(release.action.as_known()),
        });
        if !release.note.is_empty() {
            r["note"] = plain(&release.note).into();
        }
        out.insert("release".into(), r);
    }
    if let Some((stage, fraction)) = progress {
        out.insert(
            "install".into(),
            json!({
                "running": true,
                "stage": stage_word(stage),
                "percent": crate::updates::stage_percent(stage, fraction).round(),
            }),
        );
    }
    if let Some(last) = resp.last_install.as_option() {
        let mut l = json!({
            "ok": last.ok,
            "minutes_ago": minutes_since(last.finished_unix_ms, now_ms),
        });
        if !last.ok {
            l["error"] = plain(&last.last_line).into();
        }
        out.insert("last_install".into(), l);
    }
    out.insert("restart_pending".into(), resp.restart_pending.into());
    Value::Object(out)
}

/// An updater over `dir` booted from slot a at 1.0.0, whose `pinnactl` finds
/// release `found` on a check and echoes its arguments on an install.
#[cfg(test)]
pub(super) fn rigged_updater(
    dir: &std::path::Path,
    found: &str,
    channel: crate::updates::UpdateChannel,
) -> std::sync::Arc<Updater> {
    use std::os::unix::fs::PermissionsExt;

    use crate::updates::{Clock, UpdaterPaths};

    std::fs::create_dir_all(dir.join("run")).unwrap();
    std::fs::create_dir_all(dir.join("parts")).unwrap();
    // An empty bootloader record: without one the slots can't be read, and
    // every release would look newer than an unknown running version.
    std::fs::write(dir.join("misc"), vec![0u8; 0x80000]).unwrap();
    std::fs::write(dir.join("run/boot-slot"), "a\n").unwrap();
    std::fs::write(dir.join("run/boot-version"), "1.0.0\n").unwrap();
    let script = dir.join("pinnactl");
    std::fs::write(
        &script,
        format!(
            r#"#!/bin/sh
if [ "$2" = --check ]; then echo '{{"ok":true,"url":"https://u/v1/releases/v{found}/manifest.json","manifest":{{"version":"{found}","channel":"stable","summary":"Faster wake word. https://notes.test/x?token=secret","notes_url":"https://n"}},"blocked":null}}'; else echo "args: $*"; fi
"#
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let updater = Updater::new(UpdaterPaths {
        misc: dir.join("misc"),
        partdir: dir.join("parts"),
        run_dir: dir.join("run"),
        pinnactl: script,
        reboot: vec!["true".into()],
        reset_mark: dir.join("factory-reset"),
        overlay: dir.join("pinna-dev"),
        check_minute: dir.join("update-check"),
        restart_mark: dir.join("run/update-restart"),
        clock: Clock::Fixed(true),
        ..UpdaterPaths::default()
    });
    updater.init_source(Source::Channel(channel));
    std::sync::Arc::new(updater)
}

#[cfg(test)]
mod tests {
    use pinna_api::{InstallResult, Release, Slot, UpdateCheck};

    use super::*;
    use crate::updates::UpdateChannel;

    fn slot(name: &str, version: &str, state: &str, active: bool, booted: bool) -> Slot {
        Slot {
            name: name.into(),
            version: version.into(),
            state: state.into(),
            active,
            booted,
            ..Default::default()
        }
    }

    fn resp() -> GetUpdatesResponse {
        GetUpdatesResponse {
            available: true,
            booted_version: "1.0.0".into(),
            slots: vec![
                slot("a", "1.0.0", "good", false, true),
                slot("b", "0.9.0", "good", true, false),
            ],
            suggested_manifest: "http://192.168.1.5:8000/manifest.json".into(),
            manifest_override: "https://manifests.test/m.json?key=secret".into(),
            check: UpdateCheck {
                finished_unix_ms: 1_000_000,
                error: "https://updates.test/x did not answer.".into(),
                next_unix_ms: 1_000_000 + 90 * 60_000,
                ..Default::default()
            }
            .into(),
            release: Release {
                version: "2.0.0".into(),
                channel: "stable".into(),
                summary: "Faster. Notes at https://n.test/2".into(),
                notes_url: "https://n.test/2".into(),
                action: ReleaseAction::RELEASE_ACTION_INSTALL.into(),
                ..Default::default()
            }
            .into(),
            last_install: InstallResult {
                ok: false,
                manifest: "https://u.test/manifest.json".into(),
                finished_unix_ms: 1_000_000,
                last_line: "fetching https://u.test/r.img failed".into(),
                ..Default::default()
            }
            .into(),
            ..Default::default()
        }
    }

    #[test]
    fn updates_tell_the_state_and_never_show_an_address() {
        let shown = view(
            &resp(),
            &Source::Manifest("https://manifests.test/m.json?key=secret".into()),
            Some((InstallStage::INSTALL_STAGE_VERIFY, None)),
            1_000_000 + 5 * 60_000,
        );

        assert_eq!(shown["version"], "1.0.0");
        assert_eq!(shown["slot"], "a");
        assert_eq!(shown["next_boot"], "b");
        assert_eq!(
            shown["other_slot"],
            json!({"name": "b", "state": "good", "version": "0.9.0"})
        );
        assert_eq!(shown["channel"], "manifest");
        assert_eq!(shown["release"]["action"], "install");
        assert_eq!(shown["release"]["version"], "2.0.0");
        assert_eq!(shown["check"]["minutes_since_last_check"], 5);
        // The words stay; only the addresses go.
        assert_eq!(shown["check"]["error"], "(address) did not answer.");
        assert_eq!(shown["release"]["summary"], "Faster. Notes at (address)");
        assert_eq!(shown["last_install"]["error"], "fetching (address) failed");
        assert_eq!(shown["check"]["minutes_until_next_check"], 85);
        assert_eq!(
            shown["install"],
            json!({"running": true, "stage": "verifying", "percent": 95.0})
        );
        assert_eq!(shown["last_install"]["ok"], false);
        assert_eq!(shown["last_install"]["minutes_ago"], 5);
        let text = shown.to_string();
        assert!(!text.contains("://"), "{text}");
        assert!(!text.contains("secret"), "{text}");
    }

    #[test]
    fn why_updates_cant_run_keeps_its_words_without_the_address() {
        let shown = view(
            &GetUpdatesResponse {
                available: false,
                unavailable_reason:
                    "Not started from a slot image. See https://getpinna.dev/docs/update/".into(),
                booted_version: "1.0.0".into(),
                slots: vec![
                    slot("a", "1.0.0", "good", true, true),
                    slot("b", "", "unbootable", false, false),
                ],
                release: Release {
                    version: "2.0.0".into(),
                    action: ReleaseAction::RELEASE_ACTION_OTHER_MODEL.into(),
                    note: "For echo-2 only, as https://n.test/2?token=secret says.".into(),
                    ..Default::default()
                }
                .into(),
                ..Default::default()
            },
            &Source::Channel(UpdateChannel::Stable),
            None,
            0,
        );

        assert_eq!(
            shown["unavailable"],
            "Not started from a slot image. See (address)"
        );
        assert_eq!(
            shown["release"]["note"],
            "For echo-2 only, as (address) says."
        );
        assert_eq!(shown["release"]["action"], "other_model");
        // A slot without an image has no version to tell.
        assert_eq!(
            shown["other_slot"],
            json!({"name": "b", "state": "unbootable"})
        );
        assert!(shown.get("next_boot").is_none());
    }

    #[test]
    fn what_is_not_known_is_left_out() {
        let shown = view(
            &GetUpdatesResponse {
                available: true,
                booted_version: "1.0.0".into(),
                ..Default::default()
            },
            &Source::Channel(UpdateChannel::Off),
            None,
            0,
        );

        assert_eq!(
            shown,
            json!({"version": "1.0.0", "channel": "off", "restart_pending": false})
        );
    }

    #[tokio::test]
    async fn a_failed_check_tells_the_brain_why() {
        let dir = tempfile::tempdir().unwrap();
        let updater = rigged_updater(dir.path(), "2.0.0", UpdateChannel::Stable);
        std::fs::write(
            dir.path().join("pinnactl"),
            "#!/bin/sh\necho '{\"ok\":false,\"message\":\"https://updates.test did not answer.\"}'\n",
        )
        .unwrap();

        // The check ran; its failure is part of the state, as on the Updates page.
        let said = check_within(&updater, Duration::from_secs(30))
            .await
            .unwrap();

        let shown: Value = serde_json::from_str(&said).unwrap();
        assert_eq!(shown["check"]["error"], "(address) did not answer.");
        assert!(shown.get("release").is_none(), "{said}");
    }

    #[tokio::test]
    async fn a_check_that_outlasts_the_wait_goes_on() {
        let dir = tempfile::tempdir().unwrap();
        let updater = rigged_updater(dir.path(), "2.0.0", UpdateChannel::Stable);

        let done = check_within(&updater, Duration::from_secs(30))
            .await
            .unwrap();
        assert!(done.contains("\"action\":\"install\""), "{done}");

        // A slow check: the wait ends, the check does not.
        let dir = tempfile::tempdir().unwrap();
        let updater = rigged_updater(dir.path(), "2.0.0", UpdateChannel::Stable);
        let script = dir.path().join("pinnactl");
        let body = std::fs::read_to_string(&script).unwrap();
        std::fs::write(&script, body.replace("if [", "sleep 1\nif [")).unwrap();
        let said = check_within(&updater, Duration::from_millis(50))
            .await
            .unwrap();
        assert!(said.contains("goes on"), "{said}");
        assert!(
            updater
                .get_updates()
                .await
                .check
                .as_option()
                .unwrap()
                .checking
        );
        for _ in 0..100 {
            if updater.get_updates().await.release.as_option().is_some() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("the check did not finish after the tool answered");
    }
}
