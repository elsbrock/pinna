//! The update manifest's signature and the limits both its writer
//! (`pinna-image manifest`) and its readers (the updater) keep (ADR 0022).
//!
//! `manifest.json.sig`, next to `manifest.json`, is an ed25519 signature
//! over the manifest's exact bytes, made with the release key: the raw 64
//! bytes, nothing else. A signature over the bytes needs no canonical JSON.

use anyhow::{Result, bail, ensure};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use pinna_board::{BootFormat, Model, Slots};
use std::path::PathBuf;

/// The lowest boot level a root built from this tree needs (ADR 0022); the
/// manifest copies it into `requires.boot_level`. Raise it together with
/// `pinna_bootimg::BOOT_LEVEL`, and only when there is no other way.
pub const ROOT_BOOT_LEVEL: u32 = 1;

/// Home Assistant's update card shows at most this much of the summary.
pub const SUMMARY_MAX: usize = 255;

/// A manifest is small; anything bigger is refused before it is read.
pub const MANIFEST_MAX: u64 = 1 << 20;

/// The models a release can name in `requires.models`: every model's id
/// ([`pinna_board::Model::id`]).
pub fn models() -> Vec<&'static str> {
    pinna_board::Model::ALL.map(pinna_board::Model::id).to_vec()
}

/// Whether a manifest is root-only: no `--models` and no boot image. It
/// names no models, so every satellite and the virtual machines (which are
/// no model) accept it, as every manifest did before boot images were per
/// model; a FIT board's updater refuses it for want of its boot image.
/// Releases always name their models.
pub fn root_only(ids: &[String], has_boot_image: bool) -> bool {
    ids.is_empty() && !has_boot_image
}

/// The models a release is for, from their ids; every model when none is
/// named.
pub fn parse_models(ids: &[String]) -> Result<Vec<Model>> {
    if ids.is_empty() {
        return Ok(Model::ALL.to_vec());
    }
    let mut models = Vec::new();
    for id in ids {
        let Some(m) = Model::from_id(id) else {
            bail!(
                "unknown model {id:?}; the models are {}",
                self::models().join(", ")
            );
        };
        if !models.contains(&m) {
            models.push(m);
        }
    }
    Ok(models)
}

/// `1234567` as `1,234,567`.
fn grouped(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Megabytes, rounded up so that an image over its slot never reads as the
/// slot's own size.
fn megabytes(n: u64) -> u64 {
    n.div_ceil(1_000_000)
}

/// The model with the smallest slot of the kind `pick` returns, and its size.
fn tightest(models: &[Model], pick: fn(&Slots) -> u64) -> Option<(Model, u64)> {
    models
        .iter()
        .map(|&m| (m, pick(&m.board().slots)))
        .min_by_key(|&(_, size)| size)
}

/// Refuse a root image larger than the smallest root slot of `models`: one
/// root serves every model, so the tightest slot is the limit (ADR 0035).
pub fn check_root_size(size: u64, models: &[Model]) -> Result<()> {
    let Some((model, slot)) = tightest(models, |s| s.root) else {
        return Ok(());
    };
    ensure!(
        size <= slot,
        "the root image is {} MB, larger than the {}'s {}-byte system slot",
        megabytes(size),
        model.name(),
        grouped(slot)
    );
    Ok(())
}

fn format_name(f: BootFormat) -> &'static str {
    match f {
        BootFormat::Android => "an Android boot image",
        BootFormat::Fit => "a U-Boot FIT",
    }
}

/// Check a boot image (`label` names it in errors) for `models`: it has the
/// format every model's bootloader reads, fits the smallest boot slot and
/// provides the boot level the root needs. Returns its level.
pub fn check_boot(label: &str, image: &[u8], models: &[Model]) -> Result<u32> {
    let format = match pinna_bootimg::format(image) {
        Some(pinna_bootimg::Format::Android) => BootFormat::Android,
        Some(pinna_bootimg::Format::Fit) => BootFormat::Fit,
        None => bail!("{label} is not a boot image (neither ANDROID! nor a FIT)"),
    };
    for m in models {
        let want = m.board().bootloader.boot_format();
        ensure!(
            want == format,
            "{label} is {}, but the {} boots {}",
            format_name(format),
            m.name(),
            format_name(want)
        );
    }
    if let Some((model, slot)) = tightest(models, |s| s.boot) {
        ensure!(
            image.len() as u64 <= slot,
            "{label} is {} bytes, larger than the {}'s {}-byte boot slot",
            grouped(image.len() as u64),
            model.name(),
            grouped(slot)
        );
    }
    let level = pinna_bootimg::level(image).unwrap_or(1);
    ensure!(
        level >= ROOT_BOOT_LEVEL,
        "{label} provides boot level {level}; the root needs {ROOT_BOOT_LEVEL}"
    );
    Ok(level)
}

/// A `--boot-for` value: `model[,model...]=path`.
pub fn parse_boot_for(arg: &str) -> Result<(Vec<Model>, PathBuf)> {
    let Some((ids, path)) = arg.split_once('=') else {
        bail!(
            "--boot-for {arg}: expected MODELS=PATH, such as echo-dot-5=build/pinna-boot-mt8519.itb"
        );
    };
    ensure!(!path.is_empty(), "--boot-for {arg}: no path");
    let ids: Vec<String> = ids.split(',').map(str::to_owned).collect();
    Ok((parse_models(&ids)?, path.into()))
}

pub const SIGNATURE_LEN: usize = 64;

pub fn sign(manifest: &[u8], key: &SigningKey) -> [u8; SIGNATURE_LEN] {
    key.sign(manifest).to_bytes()
}

/// Check `sig` (the `.sig` file's bytes) over `manifest` with `key`.
pub fn verify(manifest: &[u8], sig: &[u8], key: &VerifyingKey) -> Result<()> {
    ensure!(
        sig.len() == SIGNATURE_LEN,
        "the signature is {} bytes, not {SIGNATURE_LEN}",
        sig.len()
    );
    let sig = Signature::from_slice(sig)?;
    key.verify(manifest, &sig)
        .map_err(|_| anyhow::anyhow!("the signature does not match the release key"))
}

/// One line of at most [`SUMMARY_MAX`] characters, without control
/// characters.
pub fn check_summary(summary: &str) -> Result<()> {
    ensure!(!summary.trim().is_empty(), "the summary is empty");
    ensure!(
        summary.chars().count() <= SUMMARY_MAX,
        "the summary has {} characters; Home Assistant shows at most {SUMMARY_MAX}",
        summary.chars().count()
    );
    ensure!(
        !summary.chars().any(char::is_control),
        "the summary has a line break or another control character"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_manifest_without_models_or_boot_images_is_root_only() {
        let none: Vec<String> = Vec::new();
        assert!(root_only(&none, false));
        // A boot image names its models: every model when --models is left out.
        assert!(!root_only(&none, true));
        assert!(!root_only(&["echo-dot-5".to_string()], false));
    }

    #[test]
    fn signature_over_the_exact_bytes() {
        let key = SigningKey::from_bytes(&[7; 32]);
        let other = SigningKey::from_bytes(&[8; 32]);
        let m = br#"{"version":"0.4.0"}"#;
        let sig = sign(m, &key);
        verify(m, &sig, &key.verifying_key()).unwrap();
        assert!(verify(m, &sig, &other.verifying_key()).is_err());
        assert!(verify(br#"{"version":"0.4.1"}"#, &sig, &key.verifying_key()).is_err());
        // A space more is another manifest.
        assert!(verify(br#"{"version": "0.4.0"}"#, &sig, &key.verifying_key()).is_err());
        assert!(verify(m, &sig[..63], &key.verifying_key()).is_err());
        let hex = crate::keys::hex(&sig);
        assert!(verify(m, hex.as_bytes(), &key.verifying_key()).is_err());
    }

    fn android(size: usize) -> Vec<u8> {
        let mut b = vec![0u8; size];
        b[..8].copy_from_slice(pinna_bootimg::MAGIC);
        b
    }

    fn fit(size: usize) -> Vec<u8> {
        let mut b = vec![0u8; size];
        b[..4].copy_from_slice(&0xD00D_FEEDu32.to_be_bytes());
        b
    }

    #[test]
    fn models_default_to_all() {
        assert_eq!(parse_models(&[]).unwrap(), Model::ALL.to_vec());
        let ids = ["echo-2".to_string(), "echo-2".to_string()];
        assert_eq!(parse_models(&ids).unwrap(), vec![Model::Echo2]);
        assert!(parse_models(&["echo-dot-3".to_string()]).is_err());
    }

    #[test]
    fn a_root_over_the_tightest_slot_is_refused() {
        let slot = Model::EchoDot5.board().slots.root;
        check_root_size(slot, &Model::ALL).unwrap();
        let err = check_root_size(132_000_000, &Model::ALL)
            .unwrap_err()
            .to_string();
        assert_eq!(
            err,
            "the root image is 132 MB, larger than the Echo Dot (5th gen)'s 131,088,384-byte system slot"
        );
        // The MT8163 Echos' slot is bigger.
        check_root_size(132_000_000, &[Model::EchoDot2, Model::Echo2]).unwrap();
    }

    #[test]
    fn a_boot_image_must_match_its_models() {
        let a = android(4096);
        let f = fit(4096);
        let mt8163 = [Model::EchoDot2, Model::Echo2];
        assert_eq!(check_boot("boot.img", &a, &mt8163).unwrap(), 1);
        assert_eq!(check_boot("boot.itb", &f, &[Model::EchoDot5]).unwrap(), 1);
        let err = check_boot("boot.img", &a, &[Model::EchoDot5])
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("the Echo Dot (5th gen) boots a U-Boot FIT"),
            "{err}"
        );
        assert!(check_boot("boot.itb", &f, &mt8163).is_err());
        assert!(check_boot("x", b"hello", &mt8163).is_err());
    }

    #[test]
    fn a_boot_image_over_its_slot_is_refused() {
        let slot = Model::EchoDot5.board().slots.boot as usize;
        check_boot("boot.itb", &fit(slot), &[Model::EchoDot5]).unwrap();
        let err = check_boot("boot.itb", &fit(slot + 1), &[Model::EchoDot5])
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("larger than the Echo Dot (5th gen)'s 12,697,600-byte boot slot"),
            "{err}"
        );
    }

    #[test]
    fn boot_for_arguments() {
        let (models, path) = parse_boot_for("echo-dot-2,echo-2=a.img").unwrap();
        assert_eq!(models, vec![Model::EchoDot2, Model::Echo2]);
        assert_eq!(path, PathBuf::from("a.img"));
        assert!(parse_boot_for("a.img").is_err());
        assert!(parse_boot_for("nope=a.img").is_err());
        assert!(parse_boot_for("echo-2=").is_err());
    }

    #[test]
    fn summaries() {
        check_summary("Pinna 0.4.0.").unwrap();
        check_summary(&"é".repeat(255)).unwrap();
        assert!(check_summary(&"a".repeat(256)).is_err());
        assert!(check_summary("").is_err());
        assert!(check_summary("two\nlines").is_err());
    }
}
