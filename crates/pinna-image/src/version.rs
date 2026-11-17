//! Image versions are semantic versions (SemVer 2.0.0, ADR 0022): releases
//! `0.4.0`, betas `0.4.0-beta.1`, development builds
//! `0.4.1-dev.202611171405.gabc1234`. Only lowercase letters, digits, `.` and
//! `-`: `IMAGE_VERSION` in os-release allows no `+`, so there is no build
//! metadata. Every comparison uses SemVer precedence.

use std::cmp::Ordering;
use std::fmt;

/// One dot-separated identifier of a prerelease part.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ident {
    Num(u64),
    Alpha(String),
}

impl Ord for Ident {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Ident::Num(a), Ident::Num(b)) => a.cmp(b),
            (Ident::Num(_), Ident::Alpha(_)) => Ordering::Less,
            (Ident::Alpha(_), Ident::Num(_)) => Ordering::Greater,
            (Ident::Alpha(a), Ident::Alpha(b)) => a.cmp(b),
        }
    }
}

impl PartialOrd for Ident {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    /// Empty for a release.
    pub pre: Vec<Ident>,
}

/// A number without leading zeros.
fn number(s: &str) -> Option<u64> {
    let ok =
        !s.is_empty() && s.bytes().all(|c| c.is_ascii_digit()) && (s == "0" || !s.starts_with('0'));
    ok.then(|| s.parse().ok()).flatten()
}

impl Version {
    pub fn parse(s: &str) -> Option<Self> {
        if !s
            .bytes()
            .all(|c| c.is_ascii_digit() || c.is_ascii_lowercase() || c == b'.' || c == b'-')
        {
            return None;
        }
        let (core, pre) = match s.split_once('-') {
            Some((core, pre)) => (core, Some(pre)),
            None => (s, None),
        };
        let mut parts = core.split('.');
        let major = number(parts.next()?)?;
        let minor = number(parts.next()?)?;
        let patch = number(parts.next()?)?;
        if parts.next().is_some() {
            return None;
        }
        let pre = match pre {
            None => Vec::new(),
            Some(pre) => pre
                .split('.')
                .map(|id| {
                    if id.is_empty() {
                        None
                    } else if id.bytes().all(|c| c.is_ascii_digit()) {
                        number(id).map(Ident::Num)
                    } else {
                        Some(Ident::Alpha(id.to_owned()))
                    }
                })
                .collect::<Option<_>>()?,
        };
        Some(Self {
            major,
            minor,
            patch,
            pre,
        })
    }

    pub fn is_prerelease(&self) -> bool {
        !self.pre.is_empty()
    }

    /// The channel a release of this version is published on (ADR 0022):
    /// any prerelease part means `beta`.
    pub fn channel(&self) -> &'static str {
        if self.is_prerelease() {
            "beta"
        } else {
            "stable"
        }
    }

    /// A step that may need the owner to do something by hand: a new major
    /// version, or before 1.0 a new minor version (ADR 0022).
    pub fn is_major_step_from(&self, from: &Version) -> bool {
        if from.major == 0 && self.major == 0 {
            self.minor > from.minor
        } else {
            self.major > from.major
        }
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.major, self.minor, self.patch)
            .cmp(&(other.major, other.minor, other.patch))
            .then_with(|| match (self.pre.is_empty(), other.pre.is_empty()) {
                (true, true) => Ordering::Equal,
                // A prerelease comes before its release.
                (true, false) => Ordering::Greater,
                (false, true) => Ordering::Less,
                // Identifier by identifier; a shorter list first when equal so far.
                (false, false) => self.pre.cmp(&other.pre),
            })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        for (i, id) in self.pre.iter().enumerate() {
            f.write_str(if i == 0 { "-" } else { "." })?;
            match id {
                Ident::Num(n) => write!(f, "{n}")?,
                Ident::Alpha(s) => f.write_str(s)?,
            }
        }
        Ok(())
    }
}

/// Precedence of two image versions. A version that isn't a semantic
/// version, a development build from before ADR 0022
/// (`2026.11.17.1405-abc1234`), comes before every one that is.
pub fn compare(a: &str, b: &str) -> Ordering {
    match (Version::parse(a), Version::parse(b)) {
        (Some(a), Some(b)) => a.cmp(&b),
        (Some(_), None) => Ordering::Greater,
        (None, Some(_)) => Ordering::Less,
        (None, None) => a.cmp(b),
    }
}

/// `candidate` comes after `booted`.
pub fn is_newer(candidate: &str, booted: &str) -> bool {
    compare(candidate, booted) == Ordering::Greater
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap_or_else(|| panic!("{s} should parse"))
    }

    #[test]
    fn parses_the_adr_examples() {
        for s in [
            "0.4.0",
            "0.4.0-beta.1",
            "0.4.1-dev.202611171405.gabc1234",
            "0.0.0-dryrun.17.gabc1234",
            "0.0.1-dev.202611171405.g1234567",
        ] {
            assert_eq!(v(s).to_string(), s);
        }
    }

    #[test]
    fn rejects_what_semver_or_os_release_forbid() {
        for s in [
            "",
            "v0.4.0",
            "0.4",
            "0.4.0.1",
            "01.4.0",
            "0.04.0",
            "0.4.0-",
            "0.4.0-beta..1",
            "0.4.0-beta.01",
            "0.4.0+build",
            "0.4.0-Beta.1",
            "2026.11.17.1405-abc1234",
            "0.4.0 ",
        ] {
            assert!(Version::parse(s).is_none(), "{s:?} should not parse");
        }
    }

    #[test]
    fn precedence() {
        let ordered = [
            "0.0.1-dev.202611171405.gabc1234",
            "0.1.0",
            "0.4.0-beta.1",
            "0.4.0-beta.2",
            "0.4.0-beta.10",
            "0.4.0",
            "0.4.1-dev.202611171405.gabc1234",
            "0.4.1-dev.202611171406.g0000000",
            "0.4.1",
            "0.5.0-beta.2",
            "0.5.0",
            "1.0.0-alpha",
            "1.0.0-alpha.1",
            "1.0.0-beta",
            "1.0.0",
        ];
        for w in ordered.windows(2) {
            assert!(v(w[0]) < v(w[1]), "{} < {}", w[0], w[1]);
            assert!(is_newer(w[1], w[0]));
            assert!(!is_newer(w[0], w[1]));
        }
        assert!(!is_newer("0.4.0", "0.4.0"));
    }

    #[test]
    fn old_development_versions_come_first() {
        assert!(is_newer(
            "0.0.1-dev.202611171405.gabc1234",
            "2026.11.17.1405-abc1234"
        ));
        assert!(!is_newer("2026.11.17.1405-abc1234", "0.1.0"));
        assert_eq!(compare("x", "x"), Ordering::Equal);
    }

    #[test]
    fn channels_and_major_steps() {
        assert_eq!(v("0.4.0").channel(), "stable");
        assert_eq!(v("0.4.0-beta.1").channel(), "beta");
        assert!(v("0.5.0").is_major_step_from(&v("0.4.3")));
        assert!(!v("0.4.4").is_major_step_from(&v("0.4.3")));
        assert!(v("1.0.0").is_major_step_from(&v("0.9.0")));
        assert!(!v("1.2.0").is_major_step_from(&v("1.1.0")));
        assert!(v("2.0.0").is_major_step_from(&v("1.9.0")));
    }
}
