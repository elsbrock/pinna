//! Durable TOML storage with per-key recovery (ADR 0007, ADR 0017).

use std::fs::{self, File, OpenOptions, Permissions};
use std::io::{self, Write};
use std::marker::PhantomData;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

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

pub struct Store<T> {
    path: PathBuf,
    mode: u32,
    value: PhantomData<fn() -> T>,
}

impl<T: Serialize + DeserializeOwned> Store<T> {
    pub fn new(path: &Path, mode: u32) -> Self {
        Self {
            path: path.to_owned(),
            mode,
            value: PhantomData,
        }
    }

    pub fn load(path: &Path, mode: u32, defaults: T) -> (Self, T, Vec<String>) {
        let (value, dropped) = match fs::read_to_string(path) {
            Ok(text) => salvage(&text, &defaults),
            Err(e) => {
                if e.kind() != io::ErrorKind::NotFound {
                    tracing::warn!("{}: {e}; using defaults", path.display());
                }
                (defaults, vec![])
            }
        };
        for key in &dropped {
            tracing::warn!("{}: ignoring invalid key {key}", path.display());
        }
        (Self::new(path, mode), value, dropped)
    }

    pub fn try_save(&self, value: &T) -> io::Result<()> {
        let text = toml::to_string(value).map_err(io::Error::other)?;
        write_durable(&self.path, text.as_bytes(), self.mode)
    }

    pub fn save(&self, value: &T) {
        if let Err(e) = self.try_save(value) {
            tracing::warn!("saving {}: {e}", self.path.display());
        }
    }
}

pub fn salvage<T: Serialize + DeserializeOwned>(text: &str, defaults: &T) -> (T, Vec<String>) {
    let file = text.parse::<toml::Table>();
    if let Ok(table) = &file
        && let Ok(value) = table.clone().try_into()
    {
        return (value, vec![]);
    }
    let mut kept = toml::Table::try_from(defaults).expect("serializable store defaults");
    let mut dropped = vec![];
    if let Ok(table) = file {
        for (key, value) in table {
            let mut candidate = kept.clone();
            candidate.insert(key.clone(), value);
            if candidate.clone().try_into::<T>().is_ok() {
                kept = candidate;
            } else {
                dropped.push(key);
            }
        }
    } else {
        tracing::warn!("Invalid TOML; using defaults");
    }
    (
        kept.try_into().expect("deserializable store defaults"),
        dropped,
    )
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
}
