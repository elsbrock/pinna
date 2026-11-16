//! The satellite's time zone (ADR 0007). The root is read-only: `/etc/localtime`
//! links to `/var/lib/pinna/localtime`, and pinnad points that link at
//! `/usr/share/zoneinfo/<zone>`.

use std::path::Path;

use anyhow::{Context, Result};

pub const ZONEINFO: &str = "/usr/share/zoneinfo";
pub const LINK: &str = "/var/lib/pinna/localtime";
pub const ETC_LOCALTIME: &str = "/etc/localtime";

/// The zone ids tzdata lists in `zone1970.tab`, and UTC; sorted. Just UTC
/// where tzdata isn't installed.
pub fn zones(zoneinfo: &Path) -> Vec<String> {
    let mut zones = vec!["UTC".to_owned()];
    if let Ok(tab) = std::fs::read_to_string(zoneinfo.join("zone1970.tab")) {
        zones.extend(
            tab.lines()
                .filter(|l| !l.starts_with('#'))
                .filter_map(|l| l.split('\t').nth(2))
                .map(|z| z.trim().to_owned())
                .filter(|z| !z.is_empty()),
        );
    }
    zones.sort();
    zones.dedup();
    zones
}

/// `/etc/localtime` is the image's link to `link`. Anywhere else (a
/// developer's computer) pinnad leaves the time zone alone.
pub fn delegated(etc_localtime: &Path, link: &Path) -> bool {
    let Ok(target) = std::fs::read_link(etc_localtime) else {
        return false;
    };
    let Some(dir) = etc_localtime.parent() else {
        return false;
    };
    normalise(&dir.join(target)) == normalise(link)
}

/// `a/b/../c` as `a/c`, without touching the file system.
fn normalise(path: &Path) -> std::path::PathBuf {
    use std::path::Component;
    let mut out = std::path::PathBuf::new();
    for c in path.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            c => out.push(c),
        }
    }
    out
}

/// Point the zone link at `zone`. `Ok(true)` when it changed; nothing happens
/// unless the image delegates `/etc/localtime` to the link.
pub fn apply(zone: &str, etc_localtime: &Path, link: &Path, zoneinfo: &Path) -> Result<bool> {
    if !delegated(etc_localtime, link) {
        return Ok(false);
    }
    let target = zoneinfo.join(zone);
    if std::fs::read_link(link).is_ok_and(|t| t == target) {
        return Ok(false);
    }
    let dir = link.parent().context("the zone link has no directory")?;
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let tmp = dir.join(".localtime.new");
    let _ = std::fs::remove_file(&tmp);
    std::os::unix::fs::symlink(&target, &tmp)
        .with_context(|| format!("linking {}", tmp.display()))?;
    std::fs::rename(&tmp, link).with_context(|| format!("replacing {}", link.display()))?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn zones_come_from_zone1970_tab() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(zones(dir.path()), ["UTC"]);
        std::fs::write(
            dir.path().join("zone1970.tab"),
            "# comment\nDE,DK\t+5230+01322\tEurope/Berlin\tGermany\nUS\t+404251-0740023\tAmerica/New_York\tEastern\nXX\t+0000+00000\tEurope/Berlin\tAgain\n",
        )
        .unwrap();
        assert_eq!(
            zones(dir.path()),
            ["America/New_York", "Europe/Berlin", "UTC"]
        );
    }

    #[test]
    fn only_the_images_link_is_delegated() {
        let dir = tempfile::tempdir().unwrap();
        let etc = dir.path().join("etc");
        std::fs::create_dir_all(&etc).unwrap();
        let link = dir.path().join("var/lib/pinna/localtime");
        let localtime = etc.join("localtime");
        assert!(!delegated(&localtime, &link));
        symlink("../var/lib/pinna/localtime", &localtime).unwrap();
        assert!(delegated(&localtime, &link));
        std::fs::remove_file(&localtime).unwrap();
        symlink("/usr/share/zoneinfo/UTC", &localtime).unwrap();
        assert!(!delegated(&localtime, &link));
        std::fs::remove_file(&localtime).unwrap();
        std::fs::write(&localtime, "TZif").unwrap();
        assert!(!delegated(&localtime, &link));
    }

    #[test]
    fn apply_points_the_link_at_the_zone() {
        let dir = tempfile::tempdir().unwrap();
        let etc = dir.path().join("etc");
        std::fs::create_dir_all(&etc).unwrap();
        let link = dir.path().join("var/lib/pinna/localtime");
        let localtime = etc.join("localtime");
        let zoneinfo = dir.path().join("zoneinfo");
        // Not the image's layout: nothing happens.
        assert!(!apply("UTC", &localtime, &link, &zoneinfo).unwrap());
        assert!(link.symlink_metadata().is_err());
        symlink("../var/lib/pinna/localtime", &localtime).unwrap();
        assert!(apply("Europe/Berlin", &localtime, &link, &zoneinfo).unwrap());
        assert_eq!(
            std::fs::read_link(&link).unwrap(),
            zoneinfo.join("Europe/Berlin")
        );
        assert!(!apply("Europe/Berlin", &localtime, &link, &zoneinfo).unwrap());
        assert!(apply("UTC", &localtime, &link, &zoneinfo).unwrap());
        assert_eq!(std::fs::read_link(&link).unwrap(), zoneinfo.join("UTC"));
    }
}
