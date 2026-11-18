//! Durable TOML storage with per-key recovery (ADR 0007, ADR 0017).

use std::fs::{self, File, OpenOptions, Permissions};
use std::io::{self, Write};
use std::marker::PhantomData;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use serde::{Serialize, de::DeserializeOwned};

pub fn write_durable(path: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(dir)?;
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    // Each file has one writer. Discard a partial write left by a power cut.
    match fs::remove_file(&tmp) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
        _ => (),
    }
    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(&tmp)?;
    let result = (|| {
        f.set_permissions(Permissions::from_mode(mode))?;
        f.write_all(bytes)?;
        f.sync_all()?;
        fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result?;
    File::open(dir)?.sync_all()
}

/// One TOML file of type `T`. Entries this build can't read are kept aside
/// at load and written back on save, so a newer build finds them again after
/// a rollback and a roll-forward.
pub struct Store<T> {
    path: PathBuf,
    mode: u32,
    /// Array elements and table entries that didn't deserialise, by key.
    rejected: Mutex<toml::Table>,
    value: PhantomData<fn() -> T>,
}

impl<T: Serialize + DeserializeOwned> Store<T> {
    pub fn new(path: &Path, mode: u32) -> Self {
        Self {
            path: path.to_owned(),
            mode,
            rejected: Mutex::new(toml::Table::new()),
            value: PhantomData,
        }
    }

    /// Read the file, salvaging what it can: a missing file gives `defaults`,
    /// and the names of whatever was dropped are returned and logged.
    pub fn load(path: &Path, mode: u32, defaults: T) -> (Self, T, Vec<String>) {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) => {
                if e.kind() != io::ErrorKind::NotFound {
                    tracing::warn!("{}: {e}; using defaults", path.display());
                }
                return (Self::new(path, mode), defaults, vec![]);
            }
        };
        let salvaged = match text.parse::<toml::Table>() {
            Ok(table) => salvage_table(table, &defaults),
            Err(e) => {
                tracing::warn!("{}: not valid TOML ({e}); using defaults", path.display());
                Salvaged {
                    value: defaults,
                    dropped: vec![],
                    rejected: toml::Table::new(),
                }
            }
        };
        for key in &salvaged.dropped {
            tracing::warn!("{}: ignoring invalid key {key}", path.display());
        }
        let store = Self {
            rejected: Mutex::new(salvaged.rejected),
            ..Self::new(path, mode)
        };
        (store, salvaged.value, salvaged.dropped)
    }

    pub fn try_save(&self, value: &T) -> io::Result<()> {
        self.try_save_dropping(value, &[])
    }

    /// Save `value` without the entries set aside under each key of `stale`,
    /// and forget them once it is saved: they belonged to something `value`
    /// no longer holds. A failed save keeps them.
    pub fn try_save_dropping(&self, value: &T, stale: &[&str]) -> io::Result<()> {
        let mut rejected = self.rejected.lock().unwrap_or_else(PoisonError::into_inner);
        let mut kept = rejected.clone();
        kept.retain(|key, _| !stale.contains(&key));
        let text = if kept.is_empty() {
            toml::to_string(value)
        } else {
            let mut table = toml::Table::try_from(value).map_err(io::Error::other)?;
            put_back(&mut table, &kept);
            toml::to_string(&table)
        }
        .map_err(io::Error::other)?;
        write_durable(&self.path, text.as_bytes(), self.mode)?;
        *rejected = kept;
        Ok(())
    }

    pub fn save(&self, value: &T) {
        if let Err(e) = self.try_save(value) {
            tracing::warn!("saving {}: {e}", self.path.display());
        }
    }
}

/// What [`salvage_table`] could read, and what it couldn't.
struct Salvaged<T> {
    value: T,
    dropped: Vec<String>,
    rejected: toml::Table,
}

/// `text` as `T`, salvaging key by key; invalid TOML gives `defaults`.
#[cfg(test)]
pub fn salvage<T: Serialize + DeserializeOwned>(text: &str, defaults: &T) -> (T, Vec<String>) {
    match text.parse::<toml::Table>() {
        Ok(table) => {
            let salvaged = salvage_table(table, defaults);
            (salvaged.value, salvaged.dropped)
        }
        Err(_) => (
            toml::Table::try_from(defaults)
                .expect("serializable store defaults")
                .try_into()
                .expect("deserializable store defaults"),
            vec![],
        ),
    }
}

/// Read `table` as `T`. When the whole doesn't deserialise, keep each
/// top-level key that does; for an array or a table that doesn't, keep each
/// element or entry that does, and set the rest aside (named `key[i]` or
/// `key.entry`). A scalar that doesn't is dropped and falls back to
/// `defaults`.
fn salvage_table<T: Serialize + DeserializeOwned>(table: toml::Table, defaults: &T) -> Salvaged<T> {
    if let Ok(value) = table.clone().try_into() {
        return Salvaged {
            value,
            dropped: vec![],
            rejected: toml::Table::new(),
        };
    }
    let mut kept = toml::Table::try_from(defaults).expect("serializable store defaults");
    let mut dropped = vec![];
    let mut rejected = toml::Table::new();
    let reads = |kept: &toml::Table, key: &str, value: toml::Value| {
        let mut candidate = kept.clone();
        candidate.insert(key.to_owned(), value);
        candidate
            .clone()
            .try_into::<T>()
            .is_ok()
            .then_some(candidate)
    };
    for (key, value) in table {
        if let Some(candidate) = reads(&kept, &key, value.clone()) {
            kept = candidate;
            continue;
        }
        match value {
            toml::Value::Array(items) => {
                let (mut good, mut bad) = (Vec::new(), Vec::new());
                for (i, item) in items.into_iter().enumerate() {
                    let mut trial = good.clone();
                    trial.push(item.clone());
                    if let Some(candidate) = reads(&kept, &key, toml::Value::Array(trial.clone())) {
                        kept = candidate;
                        good = trial;
                    } else {
                        dropped.push(format!("{key}[{i}]"));
                        bad.push(item);
                    }
                }
                if !bad.is_empty() {
                    rejected.insert(key, toml::Value::Array(bad));
                }
            }
            toml::Value::Table(entries) => {
                let (mut good, mut bad) = (toml::Table::new(), toml::Table::new());
                for (name, entry) in entries {
                    let mut trial = good.clone();
                    trial.insert(name.clone(), entry.clone());
                    if let Some(candidate) = reads(&kept, &key, toml::Value::Table(trial.clone())) {
                        kept = candidate;
                        good = trial;
                    } else {
                        dropped.push(format!("{key}.{name}"));
                        bad.insert(name, entry);
                    }
                }
                if !bad.is_empty() {
                    rejected.insert(key, toml::Value::Table(bad));
                }
            }
            _ => dropped.push(key),
        }
    }
    Salvaged {
        value: kept.try_into().expect("deserializable store defaults"),
        dropped,
        rejected,
    }
}

/// Add the set-aside entries back to a table about to be written: array
/// elements after the build's own, table entries whose name the build left
/// free. A key the build now writes as something else keeps the build's value.
fn put_back(table: &mut toml::Table, rejected: &toml::Table) {
    for (key, value) in rejected {
        match (table.get_mut(key), value) {
            (Some(toml::Value::Array(own)), toml::Value::Array(more)) => {
                own.extend(more.iter().cloned());
            }
            (Some(toml::Value::Table(own)), toml::Value::Table(more)) => {
                for (name, entry) in more {
                    own.entry(name.clone()).or_insert_with(|| entry.clone());
                }
            }
            (None, value) => {
                table.insert(key.clone(), value.clone());
            }
            (Some(_), _) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::Settings;
    use serde::Deserialize;

    #[test]
    fn invalid_volume_preserves_mute() {
        let defaults = Settings::default();
        let (settings, dropped) = salvage("muted = true\nvolume = 'loud'", &defaults);
        assert_eq!(dropped, ["volume"]);
        assert!(settings.muted);
        assert_eq!(settings.volume, defaults.volume);
    }

    #[derive(Default, Serialize, Deserialize, Debug, PartialEq)]
    struct Config {
        entries: Vec<Entry>,
        #[serde(flatten)]
        extra: toml::Table,
    }
    #[derive(Serialize, Deserialize, Debug, PartialEq)]
    struct Entry {
        name: String,
    }

    #[test]
    fn private_file_round_trips_known_and_unknown_tables() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let text = "flat = 1\n[nested]\nx = 2\n[[entries]]\nname = 'one'\n[[future]]\ny = 3\n";
        fs::write(&path, text).unwrap();
        let (store, cfg, dropped) = Store::load(&path, 0o600, Config::default());
        assert!(dropped.is_empty());
        store.try_save(&cfg).unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
        assert_eq!(
            fs::read_to_string(path)
                .unwrap()
                .parse::<toml::Table>()
                .unwrap(),
            text.parse::<toml::Table>().unwrap()
        );
    }

    #[derive(Default, Serialize, Deserialize, Debug, PartialEq)]
    struct Catalogue {
        entries: Vec<Item>,
        #[serde(default)]
        named: std::collections::BTreeMap<String, Item>,
    }
    #[derive(Serialize, Deserialize, Debug, PartialEq)]
    struct Item {
        name: String,
        kind: Kind,
    }
    #[derive(Serialize, Deserialize, Debug, PartialEq)]
    #[serde(rename_all = "lowercase")]
    enum Kind {
        Known,
    }

    #[test]
    fn an_entry_a_newer_build_wrote_is_kept_aside_and_written_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("catalogue.toml");
        let text = "[[entries]]\nname = 'one'\nkind = 'known'\n\
                    [[entries]]\nname = 'two'\nkind = 'future'\n\
                    [named.a]\nname = 'a'\nkind = 'known'\n\
                    [named.b]\nname = 'b'\nkind = 'future'\n";
        fs::write(&path, text).unwrap();

        let (store, cat, dropped) = Store::load(&path, 0o600, Catalogue::default());
        // One unreadable element costs only itself.
        assert_eq!(dropped, ["entries[1]", "named.b"]);
        assert_eq!(cat.entries.len(), 1);
        assert_eq!(cat.entries[0].name, "one");
        assert_eq!(cat.named.keys().collect::<Vec<_>>(), ["a"]);

        // Saving keeps the set-aside entries for the newer build.
        store.try_save(&cat).unwrap();
        let saved = fs::read_to_string(&path).unwrap();
        assert!(saved.contains("name = \"two\""), "{saved}");
        assert!(saved.contains("[named.b]"), "{saved}");
        let (_, again, dropped) = Store::load(&path, 0o600, Catalogue::default());
        assert_eq!(again, cat);
        assert_eq!(dropped, ["entries[1]", "named.b"]);
    }

    #[test]
    fn set_aside_entries_that_went_stale_are_dropped_once_saved() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("catalogue.toml");
        let text = "[[entries]]\nname = 'one'\nkind = 'known'\n\
                    [[entries]]\nname = 'two'\nkind = 'future'\n\
                    [named.b]\nname = 'b'\nkind = 'future'\n";
        fs::write(&path, text).unwrap();
        let (store, cat, _) = Store::load(&path, 0o600, Catalogue::default());

        // A save that fails keeps them.
        fs::remove_file(&path).unwrap();
        fs::create_dir_all(path.join("blocked")).unwrap();
        assert!(store.try_save_dropping(&cat, &["entries"]).is_err());
        fs::remove_dir_all(&path).unwrap();
        store.try_save(&cat).unwrap();
        assert!(fs::read_to_string(&path).unwrap().contains("two"));

        // A save that succeeds drops them, now and in every later save.
        store.try_save_dropping(&cat, &["entries"]).unwrap();
        store.try_save(&cat).unwrap();
        let saved = fs::read_to_string(&path).unwrap();
        assert!(!saved.contains("two"), "{saved}");
        // Only the keys named.
        assert!(saved.contains("[named.b]"), "{saved}");
        let (_, _, dropped) = Store::load(&path, 0o600, Catalogue::default());
        assert_eq!(dropped, ["named.b"]);
    }
}
