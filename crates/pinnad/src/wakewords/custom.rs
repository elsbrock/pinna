//! The custom wake words on disk (ADR 0013): `index.toml` and the model
//! files beside it, and the mark that says a model is loading.

use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::store::write_durable;

/// How many custom words a satellite keeps, failed ones included.
pub const MAX_CUSTOM: usize = 8;
/// The largest model accepted.
pub const MAX_MODEL_BYTES: usize = 2 * 1024 * 1024;

const INDEX: &str = "index.toml";
const LOADING: &str = "loading";
const FILE_MODE: u32 = 0o600;

pub const MISSING: &str = "Its model file is missing. Remove it and add it again.";
pub const CRASHED: &str = "It stopped the satellite's software while loading. Remove it.";
pub const CHANGED: &str = "Its model file changed after it was added. Remove it and add it again.";

/// One custom word in `index.toml`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    pub phrase: String,
    /// The model's file name in the directory.
    pub file: String,
    pub sha256: String,
    /// Why the word can't listen, kept across restarts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed: Option<String>,
}

#[derive(Default, Serialize, Deserialize)]
struct Index {
    #[serde(default)]
    words: Vec<Entry>,
}

/// The words in `dir`. A missing index is no words; a corrupt one is no words
/// too, so that a damaged file never stops pinnad from starting.
pub fn read_index(dir: &Path) -> Vec<Entry> {
    let path = dir.join(INDEX);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Vec::new(),
        Err(e) => {
            tracing::error!("custom wake words: reading {}: {e}", path.display());
            return Vec::new();
        }
    };
    match toml::from_str::<Index>(&text) {
        Ok(index) => index.words,
        Err(e) => {
            tracing::error!(
                "custom wake words: {} is corrupt, so there are none: {e}",
                path.display()
            );
            Vec::new()
        }
    }
}

pub fn write_index(dir: &Path, entries: &[Entry]) -> io::Result<()> {
    let text = toml::to_string(&Index {
        words: entries.to_vec(),
    })
    .map_err(io::Error::other)?;
    write_durable(&dir.join(INDEX), text.as_bytes(), FILE_MODE)
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Say that the model of `id` is about to load. Durable, because a crash
/// follows it: the next start reads the mark (see [`take_loading_mark`]).
pub fn mark_loading(dir: &Path, id: &str) {
    if let Err(e) = write_durable(&dir.join(LOADING), id.as_bytes(), FILE_MODE) {
        tracing::warn!("custom wake words: marking {id} as loading: {e}");
    }
}

pub fn clear_loading(dir: &Path) {
    match std::fs::remove_file(dir.join(LOADING)) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => {
            tracing::warn!("custom wake words: clearing the loading mark: {e}");
        }
        _ => (),
    }
}

/// Delete uploads that were being tested when pinnad stopped: nothing refers
/// to them, and each can take 2 MiB.
pub fn remove_staged(dir: &Path) {
    let Ok(files) = std::fs::read_dir(dir) else {
        return;
    };
    for file in files.flatten() {
        if file.file_name().to_string_lossy().starts_with(".upload-")
            && let Err(e) = std::fs::remove_file(file.path())
        {
            tracing::warn!("removing {}: {e}", file.path().display());
        }
    }
}

/// The id a loading mark left behind names, and remove the mark.
pub fn take_loading_mark(dir: &Path) -> Option<String> {
    let id = std::fs::read_to_string(dir.join(LOADING)).ok()?;
    clear_loading(dir);
    Some(id.trim().to_owned()).filter(|id| !id.is_empty())
}
