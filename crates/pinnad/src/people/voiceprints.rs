//! The files that hold voiceprints (ADR 0038).
//!
//! `<dir>/<id>.vp`, mode 0600, written durably as custom wake words are:
//! the magic `PVP1`, a u32 capture version, the 32 bytes of the sha256 of the
//! model that made it, a u32 dimension, then that many f32 values. All
//! integers and floats are little endian.

use std::io;
use std::path::{Path, PathBuf};

use pinna_speaker::Voiceprint;

use crate::store::write_durable;

/// Bumped when an update changes the microphone processing (the front end,
/// the gain, the echo canceller): voiceprints drift with it, and one from an
/// older version is still used but the dashboard suggests enrolling again
/// (ADR 0038).
pub const CAPTURE_VERSION: u32 = 1;

const MAGIC: &[u8; 4] = b"PVP1";
const SHA_BYTES: usize = 32;
const HEADER: usize = 4 + 4 + SHA_BYTES + 4;
const MODE: u32 = 0o600;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("The voiceprint file {0}. Enroll the person again.")]
    Invalid(&'static str),
    #[error("The model's sha256 must be 64 hexadecimal digits, got {0:?}.")]
    Sha(String),
    #[error(transparent)]
    Voiceprint(#[from] pinna_speaker::Error),
    #[error("Can't use the voiceprint file {path}: {cause}. Check the storage.")]
    Io { path: String, cause: io::Error },
}

/// What a voiceprint file says.
#[derive(Debug, Clone)]
pub struct Stored {
    pub capture_version: u32,
    /// Lowercase hex, as `pinna_speaker::Model::sha256` returns it.
    pub model_sha: String,
    pub voiceprint: Voiceprint,
}

/// What a person's voiceprint is worth, against the installed model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Voice {
    /// There is no usable file.
    None,
    Enrolled,
    /// Made by another model: ignored for matching.
    OtherModel,
    /// Made with older microphone processing: still used.
    OlderCapture,
}

impl Voice {
    /// Whether matching uses it.
    pub fn used(self) -> bool {
        matches!(self, Voice::Enrolled | Voice::OlderCapture)
    }
}

/// `installed` is the model's sha256, or `None` until it is known: the file
/// is then taken to match.
pub fn classify(stored: &Stored, installed: Option<&str>) -> Voice {
    if installed.is_some_and(|sha| sha != stored.model_sha) {
        Voice::OtherModel
    } else if stored.capture_version < CAPTURE_VERSION {
        Voice::OlderCapture
    } else {
        Voice::Enrolled
    }
}

fn sha_bytes(hex: &str) -> Result<[u8; SHA_BYTES], Error> {
    let bad = || Error::Sha(hex.to_owned());
    if hex.len() != SHA_BYTES * 2 || !hex.is_ascii() {
        return Err(bad());
    }
    let mut out = [0u8; SHA_BYTES];
    for (byte, pair) in out.iter_mut().zip(hex.as_bytes().chunks_exact(2)) {
        let pair = std::str::from_utf8(pair).map_err(|_| bad())?;
        *byte = u8::from_str_radix(pair, 16).map_err(|_| bad())?;
    }
    Ok(out)
}

pub fn encode(voiceprint: &Voiceprint, model_sha: &str) -> Result<Vec<u8>, Error> {
    let mut out = Vec::with_capacity(HEADER + voiceprint.dim() * 4);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&CAPTURE_VERSION.to_le_bytes());
    out.extend_from_slice(&sha_bytes(model_sha)?);
    out.extend_from_slice(&(voiceprint.dim() as u32).to_le_bytes());
    out.extend_from_slice(&voiceprint.to_le_bytes());
    Ok(out)
}

pub fn decode(bytes: &[u8]) -> Result<Stored, Error> {
    if bytes.len() < HEADER {
        return Err(Error::Invalid("is cut short"));
    }
    if &bytes[..4] != MAGIC {
        return Err(Error::Invalid("isn't a voiceprint"));
    }
    let word =
        |at: usize| u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
    let sha = &bytes[8..8 + SHA_BYTES];
    let dim = word(8 + SHA_BYTES) as usize;
    let values = &bytes[HEADER..];
    if dim.checked_mul(4) != Some(values.len()) {
        return Err(Error::Invalid("is cut short or has extra bytes"));
    }
    Ok(Stored {
        capture_version: word(4),
        model_sha: sha.iter().map(|b| format!("{b:02x}")).collect(),
        voiceprint: Voiceprint::from_le_bytes(values)?,
    })
}

pub fn path(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{id}.vp"))
}

/// `None` when the person has no file.
pub fn read(dir: &Path, id: &str) -> Result<Option<Stored>, Error> {
    let file = path(dir, id);
    match std::fs::read(&file) {
        Ok(bytes) => decode(&bytes).map(Some),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(cause) => Err(Error::Io {
            path: file.display().to_string(),
            cause,
        }),
    }
}

pub fn write(dir: &Path, id: &str, voiceprint: &Voiceprint, model_sha: &str) -> Result<(), Error> {
    let file = path(dir, id);
    write_durable(&file, &encode(voiceprint, model_sha)?, MODE).map_err(|cause| Error::Io {
        path: file.display().to_string(),
        cause,
    })
}

/// A missing file is fine.
pub fn remove(dir: &Path, id: &str) -> Result<(), Error> {
    let file = path(dir, id);
    match std::fs::remove_file(&file) {
        Err(cause) if cause.kind() != io::ErrorKind::NotFound => Err(Error::Io {
            path: file.display().to_string(),
            cause,
        }),
        _ => Ok(()),
    }
}

#[cfg(test)]
pub fn synthetic(values: &[f32]) -> Voiceprint {
    let bytes: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
    Voiceprint::from_le_bytes(&bytes).unwrap()
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    const SHA: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    #[test]
    fn a_voiceprint_survives_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let vp = synthetic(&[1.0, 2.0, 3.0, 4.0]);
        write(dir.path(), "p1", &vp, SHA).unwrap();
        let mode = std::fs::metadata(path(dir.path(), "p1"))
            .unwrap()
            .permissions();
        assert_eq!(mode.mode() & 0o777, 0o600);
        let stored = read(dir.path(), "p1").unwrap().unwrap();
        // Reading normalises again, which may move the last bit.
        assert!(stored.voiceprint.cosine(&vp) > 0.99999);
        assert_eq!(stored.model_sha, SHA);
        assert_eq!(stored.capture_version, CAPTURE_VERSION);
        assert_eq!(classify(&stored, Some(SHA)), Voice::Enrolled);
    }

    #[test]
    fn the_header_is_laid_out_as_documented() {
        let bytes = encode(&synthetic(&[3.0, 4.0]), SHA).unwrap();
        assert_eq!(&bytes[..4], b"PVP1");
        assert_eq!(bytes[4..8], CAPTURE_VERSION.to_le_bytes());
        assert_eq!(bytes[8], 0x01);
        assert_eq!(bytes[39], 0xef);
        assert_eq!(bytes[40..44], 2u32.to_le_bytes());
        assert_eq!(bytes.len(), 44 + 8);
    }

    #[test]
    fn no_file_is_none_and_removing_it_is_fine() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read(dir.path(), "p9").unwrap().is_none());
        remove(dir.path(), "p9").unwrap();
    }

    #[test]
    fn a_bad_magic_is_refused() {
        let mut bytes = encode(&synthetic(&[1.0, 1.0]), SHA).unwrap();
        bytes[0] = b'X';
        let err = decode(&bytes).unwrap_err().to_string();
        assert!(err.contains("isn't a voiceprint"), "{err}");
        assert!(err.contains("Enroll the person again"), "{err}");
    }

    #[test]
    fn a_truncated_file_is_refused() {
        let bytes = encode(&synthetic(&[1.0, 1.0, 1.0]), SHA).unwrap();
        assert!(decode(&bytes[..bytes.len() - 1]).is_err());
        assert!(decode(&bytes[..20]).is_err());
        let mut longer = bytes;
        longer.push(0);
        assert!(decode(&longer).is_err());
    }

    #[test]
    fn another_models_voiceprint_is_ignored_and_an_older_capture_still_used() {
        let mut stored = decode(&encode(&synthetic(&[1.0, 0.0]), SHA).unwrap()).unwrap();
        let other = "f".repeat(64);
        assert_eq!(classify(&stored, Some(&other)), Voice::OtherModel);
        assert!(!Voice::OtherModel.used());
        assert_eq!(
            classify(&stored, None),
            Voice::Enrolled,
            "sha not known yet"
        );
        stored.capture_version = CAPTURE_VERSION - 1;
        assert_eq!(classify(&stored, Some(SHA)), Voice::OlderCapture);
        assert!(Voice::OlderCapture.used());
    }

    #[test]
    fn a_bad_model_sha_is_refused_when_writing() {
        assert!(encode(&synthetic(&[1.0]), "abc").is_err());
        assert!(encode(&synthetic(&[1.0]), &"zz".repeat(32)).is_err());
    }
}
