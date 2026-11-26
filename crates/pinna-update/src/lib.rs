//! Install a release into the inactive A/B slot: its kernel into
//! `boot_<slot>` and its root into `system_<slot>` (ADR 0019), from a signed
//! manifest (ADR 0022).
//!
//! The manifest is JSON: `{ "version", "channel", "summary", "notes_url",
//! "requires": { "boot_level", "models" }, "root": { "url", "size",
//! "sha256" }, "boot": { … }, "boots": [{ "models", "url", "size", "sha256" }] }`,
//! `boot` and `boots` optional. `manifest.json.sig`, next
//! to it after redirects, is an ed25519 signature over its exact bytes; it is
//! checked before anything in the manifest is read. URLs may be relative to
//! the manifest's address after redirects. Manifest and images can come from
//! `http(s)://`, `file://` or a plain path.
//!
//! 1. No `just push` overlay: it would hide the new release's programs.
//! 2. The booted slot must be marked good: the other slot is its only fallback.
//! 3. The manifest's signature, then the rules: the version a check showed,
//!    a channel's (never back in version; stable takes no beta), the model,
//!    and the boot level of the kernel that will start the target slot.
//! 4. The kernel: the manifest's boot image, or without one the running
//!    slot's, marked for the target slot. boot_b is written unless a boot
//!    from b showed that the bootloader ignores the slot for the kernel;
//!    boot_a only once a boot from b showed that it loads boot_b. On a
//!    bootloader with FIT boot images (Amazon's U-Boot) the kernel's slot is
//!    the root's: the release's boot image for the model always goes into the
//!    target slot's boot volume, and without one the release is blocked.
//! 5. The target slot is marked unbootable before it is overwritten.
//! 6. The kernel is written and read back; the root is streamed into the
//!    target partition (or UBI volume, replaced whole) while its size and
//!    sha256 are checked.
//! 7. Read back from the device: footer signature (release key), version, and
//!    the whole dm-verity hash tree recomputed from the data.
//! 8. Only then the target becomes active for a trial with N boot attempts.

use std::fmt;
use std::fs::File;
use std::io::Read;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;

use anyhow::{Context, Result, bail, ensure};
use pinna_board::{Board, BootFormat};
use pinna_bootctl::bcb;
use pinna_bootctl::slot::{self, KernelSlots, Slot};
use pinna_bootctl::storage::Parts;
use pinna_http::reqwest;
use pinna_image::keys::{self, VerifyingKey};
use pinna_image::manifest::{self as signed, MANIFEST_MAX, SIGNATURE_LEN};
use pinna_image::version::{self, Version};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use url::Url;
use writer::{SlotWriter, UbiIoctl, VolumeUpdate};

pub mod writer;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Manifest {
    pub version: String,
    /// `stable` or `beta`, as the version says.
    pub channel: String,
    /// One line for Home Assistant's update card.
    #[serde(default)]
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes_url: Option<String>,
    #[serde(default)]
    pub requires: Requires,
    pub root: Artifact,
    /// The boot image (kernel and pinna-init) that goes with the root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub boot: Option<Artifact>,
    /// Boot images by the models they are for: a satellite takes the entry
    /// naming its model, else `boot` if its bootloader reads Android boot
    /// images.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub boots: Vec<BootEntry>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BootEntry {
    pub models: Vec<String>,
    #[serde(flatten)]
    pub artifact: Artifact,
}

impl Manifest {
    /// The boot image for a satellite of `model` whose bootloader reads
    /// `format` images.
    pub fn boot_for(&self, model: Option<&str>, format: BootFormat) -> Option<&Artifact> {
        let named = model.and_then(|m| {
            self.boots
                .iter()
                .find(|e| e.models.iter().any(|x| x == m))
                .map(|e| &e.artifact)
        });
        named.or(match format {
            BootFormat::Android => self.boot.as_ref(),
            BootFormat::Fit => None,
        })
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Requires {
    /// The lowest boot level the root needs (`pinna.level=` in a boot image).
    #[serde(default = "level_one")]
    pub boot_level: u32,
    /// The models the release is for; `None` is every model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub models: Option<Vec<String>>,
}

fn level_one() -> u32 {
    1
}

impl Default for Requires {
    fn default() -> Self {
        Self {
            boot_level: 1,
            models: None,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Artifact {
    pub url: String,
    pub size: u64,
    pub sha256: String,
}

/// An update channel (ADR 0022).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    Stable,
    Beta,
}

impl Channel {
    pub fn as_str(self) -> &'static str {
        match self {
            Channel::Stable => "stable",
            Channel::Beta => "beta",
        }
    }
}

impl FromStr for Channel {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "stable" => Ok(Channel::Stable),
            "beta" => Ok(Channel::Beta),
            _ => Err(format!("unknown channel {s:?}: stable or beta")),
        }
    }
}

/// What an install must hold to beyond the signatures.
#[derive(Debug, Clone, Default)]
pub struct Rules {
    /// An install from this channel: only a newer version, and on stable no
    /// beta.
    pub channel: Option<Channel>,
    /// Only this version: the one a check showed.
    pub expect_version: Option<String>,
}

pub struct Options {
    /// Boot attempts the new slot gets before rolling back.
    pub attempts: u8,
    pub misc: PathBuf,
    /// The slot partitions by name (system_a, boot_a, …): the board's, or a
    /// directory of them.
    pub parts: Parts,
    /// Where pinna-init records the booted slot.
    pub run_dir: PathBuf,
    /// What `pinna-bootctl mark-good` learned about the bootloader.
    pub kernel_slots: PathBuf,
    pub key: VerifyingKey,
    /// This satellite's model (`echo-dot-2`, `echo-2`); `None` when unknown.
    pub model: Option<String>,
    /// This satellite's board: its bootloader decides how the kernel goes
    /// along, and its slots how large an image may be. `None` on hardware
    /// Pinna doesn't know (the VMs), which takes Android boot images.
    pub board: Option<&'static Board>,
    /// The `just push` overlay (ADR 0025).
    pub dev_overlay: PathBuf,
    /// Replaces a UBI volume; the kernel's ioctl unless a test says otherwise.
    pub volume_update: Arc<dyn VolumeUpdate>,
}

impl Default for Options {
    fn default() -> Self {
        let parts = Parts::board();
        Self {
            attempts: bcb::DEFAULT_ATTEMPTS,
            misc: parts.path("misc"),
            parts,
            run_dir: slot::RUN_DIR.into(),
            kernel_slots: slot::KERNEL_SLOTS.into(),
            key: keys::release_key(),
            model: None,
            board: pinna_board::detect(),
            dev_overlay: "/var/lib/extensions/pinna-dev".into(),
            volume_update: Arc::new(UbiIoctl),
        }
    }
}

#[derive(Debug)]
pub struct Installed {
    pub slot: Slot,
    pub version: String,
}

/// Why fetching a manifest failed, in words an owner can act on.
#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    #[error("{host} did not answer. Check that the satellite can reach the internet.")]
    NoAnswer { host: String, detail: String },
    #[error("{host} answered {status}{}", body_suffix(.body))]
    Status {
        host: String,
        status: u16,
        body: String,
    },
    #[error("Can't read {path}: {detail}")]
    Unreadable { path: String, detail: String },
    #[error(
        "The release has no signature (manifest.json.sig). The satellite installs only signed releases."
    )]
    NoSignature,
    #[error(
        "The release's signature doesn't match the key this satellite trusts, so it isn't installed."
    )]
    BadSignature,
    #[error("The release's manifest is not valid: {0}.")]
    BadManifest(String),
}

fn body_suffix(body: &str) -> String {
    if body.is_empty() {
        ".".into()
    } else {
        format!(": {body}")
    }
}

impl FetchError {
    /// A short machine name for `pinnactl update --check --json`.
    pub fn kind(&self) -> &'static str {
        match self {
            FetchError::NoAnswer { .. } => "no_answer",
            FetchError::Status { .. } => "status",
            FetchError::Unreadable { .. } => "unreadable",
            FetchError::NoSignature => "no_signature",
            FetchError::BadSignature => "bad_signature",
            FetchError::BadManifest(_) => "bad_manifest",
        }
    }
}

/// Why an install would be refused although the release is signed and newer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Blocked {
    /// The kernel that would start the target slot provides less.
    BootLevel { needs: u32, has: u32 },
    /// Not for this model.
    Model {
        model: Option<String>,
        models: Vec<String>,
    },
    /// A `just push` overlay is active.
    Overlay(PathBuf),
    /// The release has no boot image for this satellite's model, and its
    /// bootloader can't start the kernel of another format.
    NoBootImage { model: Option<String> },
}

impl Blocked {
    pub fn kind(&self) -> &'static str {
        match self {
            Blocked::BootLevel { .. } => "boot_level",
            Blocked::Model { .. } => "model",
            Blocked::Overlay(_) => "overlay",
            Blocked::NoBootImage { .. } => "boot_image",
        }
    }
}

impl fmt::Display for Blocked {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Blocked::BootLevel { needs, has } => write!(
                f,
                "Needs a new boot image: install it over USB. (The release needs boot level {needs}; \
                 the kernel that would start it has {has}.)"
            ),
            Blocked::Model { model, models } => write!(
                f,
                "This release is for {}, not for {}.",
                models
                    .iter()
                    .map(|m| model_name(m))
                    .collect::<Vec<_>>()
                    .join(" and "),
                model.as_deref().map_or_else(
                    || "this satellite's model, which is unknown".to_owned(),
                    model_name
                )
            ),
            Blocked::NoBootImage { model } => write!(
                f,
                "This release has no boot image for {}: install a release built for it.",
                model.as_deref().map_or_else(
                    || "this satellite's model, which is unknown".to_owned(),
                    model_name
                )
            ),
            Blocked::Overlay(p) => write!(
                f,
                "A development overlay is active ({}): after the restart it would hide the new \
                 release's programs. Remove it with scripts/push-dev.sh --remove, on your computer.",
                p.display()
            ),
        }
    }
}

/// A model id as owners know the device.
fn model_name(id: &str) -> String {
    match pinna_board::Model::from_id(id) {
        Some(m) => format!("the {}", m.name()),
        None => id.to_owned(),
    }
}

/// An absolute URL, a URL relative to `base`, or a local path.
pub fn resolve(base: Option<&Url>, s: &str) -> Result<Url> {
    if let Ok(u) = Url::parse(s) {
        return Ok(u);
    }
    if let Some(b) = base {
        return b
            .join(s)
            .with_context(|| format!("resolving {s:?} against {b}"));
    }
    let p = std::path::absolute(s)?;
    Url::from_file_path(&p).map_err(|_| anyhow::anyhow!("bad path {s:?}"))
}

fn client() -> reqwest::Result<reqwest::blocking::Client> {
    // The timeout applies to each read: a stalled download fails.
    Ok(pinna_http::blocking_client(pinna_http::ClientKind::Update))
}

/// Open a URL for reading.
pub fn open(url: &Url) -> Result<Box<dyn Read>> {
    match url.scheme() {
        "file" => {
            let p = url
                .to_file_path()
                .map_err(|_| anyhow::anyhow!("bad file URL {url}"))?;
            Ok(Box::new(
                File::open(&p).with_context(|| format!("opening {}", p.display()))?,
            ))
        }
        "http" | "https" => {
            let resp = client()?
                .get(url.clone())
                .send()
                .with_context(|| format!("GET {url}"))?;
            let status = resp.status();
            if !status.is_success() {
                let file = url
                    .path_segments()
                    .and_then(|mut p| p.next_back())
                    .unwrap_or("");
                bail!(
                    "{} answered {} for {file}. Check that the release folder holds all of the \
                     release's files",
                    url.host_str().unwrap_or("the server"),
                    status.as_u16()
                );
            }
            Ok(Box::new(resp))
        }
        other => bail!("unsupported URL scheme {other:?} ({url})"),
    }
}

/// A small file, at most `max` bytes, and its address after redirects.
fn fetch_small(url: &Url, max: u64) -> Result<(Vec<u8>, Url), FetchError> {
    let host = || url.host_str().unwrap_or("the server").to_owned();
    let mut data = Vec::new();
    let final_url = match url.scheme() {
        "file" => {
            let unreadable = |detail: String| FetchError::Unreadable {
                path: url.path().to_owned(),
                detail,
            };
            let p = url
                .to_file_path()
                .map_err(|_| unreadable("not a local path".into()))?;
            File::open(&p)
                .and_then(|f| f.take(max + 1).read_to_end(&mut data))
                .map_err(|e| unreadable(e.to_string()))?;
            url.clone()
        }
        "http" | "https" => {
            let no_answer = |e: reqwest::Error| FetchError::NoAnswer {
                host: host(),
                detail: format!("{e:#}"),
            };
            let resp = client()
                .and_then(|c| c.get(url.clone()).send())
                .map_err(no_answer)?;
            let status = resp.status();
            let final_url = resp.url().clone();
            let mut resp = resp.take(max + 1);
            if !status.is_success() {
                let mut body = String::new();
                let _ = resp.take(512).read_to_string(&mut body);
                let body = body.lines().next().unwrap_or("").trim();
                return Err(FetchError::Status {
                    host: final_url.host_str().unwrap_or("the server").to_owned(),
                    status: status.as_u16(),
                    // Only a short sentence, such as the release service's.
                    body: if body.len() <= 200 && !body.starts_with('<') {
                        body.to_owned()
                    } else {
                        String::new()
                    },
                });
            }
            resp.read_to_end(&mut data)
                .map_err(|e| FetchError::NoAnswer {
                    host: host(),
                    detail: e.to_string(),
                })?;
            final_url
        }
        other => {
            return Err(FetchError::Unreadable {
                path: url.to_string(),
                detail: format!("unsupported URL scheme {other:?}"),
            });
        }
    };
    if data.len() as u64 > max {
        return Err(FetchError::BadManifest(format!(
            "{final_url} is larger than {max} bytes"
        )));
    }
    Ok((data, final_url))
}

/// The manifest's signature file: its address after redirects, plus `.sig`.
fn signature_url(manifest: &Url) -> Url {
    let mut u = manifest.clone();
    u.set_query(None);
    u.set_fragment(None);
    u.set_path(&format!("{}.sig", manifest.path()));
    u
}

/// A manifest whose signature was checked, and where it came from after
/// redirects (relative URLs in it resolve against that).
#[derive(Debug, Clone)]
pub struct Fetched {
    pub manifest: Manifest,
    pub url: Url,
}

/// Fetch a manifest and its signature, check the signature against `key`,
/// then parse and check the manifest.
pub fn fetch_manifest(url: &Url, key: &VerifyingKey) -> Result<Fetched, FetchError> {
    let (bytes, final_url) = fetch_small(url, MANIFEST_MAX)?;
    let sig = match fetch_small(&signature_url(&final_url), SIGNATURE_LEN as u64 * 4) {
        Ok((sig, _)) => sig,
        Err(FetchError::Status { status: 404, .. } | FetchError::Unreadable { .. }) => {
            return Err(FetchError::NoSignature);
        }
        Err(e) => return Err(e),
    };
    signed::verify(&bytes, &sig, key).map_err(|_| FetchError::BadSignature)?;
    let manifest: Manifest =
        serde_json::from_slice(&bytes).map_err(|e| FetchError::BadManifest(e.to_string()))?;
    check_manifest(&manifest).map_err(|e| FetchError::BadManifest(format!("{e:#}")))?;
    Ok(Fetched {
        manifest,
        url: final_url,
    })
}

fn check_manifest(m: &Manifest) -> Result<()> {
    pinna_image::footer::check_version(&m.version)?;
    let v = Version::parse(&m.version)
        .with_context(|| format!("version {:?} is not a semantic version", m.version))?;
    ensure!(
        m.channel == v.channel(),
        "its channel is {:?}, but version {} belongs on {}",
        m.channel,
        m.version,
        v.channel()
    );
    if !m.summary.is_empty() {
        signed::check_summary(&m.summary)?;
    }
    if let Some(models) = &m.requires.models {
        ensure!(!models.is_empty(), "requires.models is empty");
    }
    check_artifact("root", &m.root)?;
    if let Some(b) = &m.boot {
        check_artifact("boot", b)?;
    }
    for (i, e) in m.boots.iter().enumerate() {
        ensure!(!e.models.is_empty(), "boots[{i}].models is empty");
        check_artifact(&format!("boots[{i}]"), &e.artifact)?;
    }
    Ok(())
}

fn check_artifact(name: &str, a: &Artifact) -> Result<()> {
    ensure!(a.size > 0, "{name}.size is 0");
    ensure!(
        a.sha256.len() == 64 && a.sha256.bytes().all(|c| c.is_ascii_hexdigit()),
        "{name}.sha256 is not a hex sha256"
    );
    Ok(())
}

/// A whole file into memory, checked against the manifest.
fn fetch_artifact(url: &Url, a: &Artifact) -> Result<Vec<u8>> {
    let mut data = Vec::new();
    open(url)?
        .take(a.size + 1)
        .read_to_end(&mut data)
        .with_context(|| format!("reading {url}"))?;
    ensure!(
        data.len() as u64 == a.size,
        "{url} has {} bytes, the manifest says {}",
        data.len(),
        a.size
    );
    let sha = keys::hex(&Sha256::digest(&data));
    ensure!(
        sha.eq_ignore_ascii_case(&a.sha256),
        "sha256 mismatch for {url}: got {sha}, manifest says {}",
        a.sha256
    );
    Ok(data)
}

/// What goes into the target slot's boot partition.
enum Kernel {
    /// It stays as it is, for this reason.
    Stays(String),
    /// This image, marked for the target slot, from `from`.
    Write {
        part: PathBuf,
        image: Vec<u8>,
        from: String,
    },
}

/// Which kernel will start the target slot, decided before anything is
/// fetched.
enum KernelPlan {
    /// The target's boot partition stays as it is, for `why`; the bootloader
    /// then starts the target with the kernel in `loads` (none: no boot
    /// partitions, level 1).
    Stays { why: String, loads: Option<PathBuf> },
    /// The manifest's boot image goes into `part`.
    FromManifest { part: PathBuf },
    /// The running slot's boot image, from `src`, goes into `part`.
    FromRunning { part: PathBuf, src: PathBuf },
    /// A FIT bootloader needs the release's boot image for the target slot,
    /// and the release has none for this model.
    NoBootImage,
}

fn boot_part(parts: &Parts, s: Slot) -> PathBuf {
    parts.path(&format!("boot_{s}"))
}

/// The format of the boot images the bootloader reads.
fn boot_format(opts: &Options) -> BootFormat {
    opts.board
        .map_or(BootFormat::Android, |b| b.bootloader.boot_format())
}

fn kernel_plan(opts: &Options, has_boot: bool, booted: Slot, target: Slot) -> KernelPlan {
    let part = boot_part(&opts.parts, target);
    // A FIT bootloader starts boot_<slot> with root <slot>: the kernel is
    // always the release's, and none of the Android slot rules applies.
    if boot_format(opts) == BootFormat::Fit {
        return if has_boot {
            KernelPlan::FromManifest { part }
        } else {
            KernelPlan::NoBootImage
        };
    }
    if !part.exists() {
        return KernelPlan::Stays {
            why: format!("no {} partition: the kernel stays as it is", part.display()),
            loads: None,
        };
    }
    let boot_a = boot_part(&opts.parts, Slot::A);
    let kernel_slots = KernelSlots::read(&opts.kernel_slots);
    if kernel_slots == KernelSlots::Ignored {
        return KernelPlan::Stays {
            why: "the kernel stays: this bootloader loads boot_a whichever slot it starts, so a \
                  new kernel needs fastboot (flash-gen2.sh boot)"
                .into(),
            loads: Some(boot_a),
        };
    }
    if target == Slot::A && kernel_slots != KernelSlots::Honoured {
        return KernelPlan::Stays {
            why: "the kernel in boot_a stays: this bootloader hasn't been seen loading boot_b \
                  yet, so a new kernel in boot_a would have no fallback"
                .into(),
            loads: Some(boot_a),
        };
    }
    if has_boot {
        KernelPlan::FromManifest { part }
    } else {
        KernelPlan::FromRunning {
            part,
            src: boot_part(&opts.parts, booted),
        }
    }
}

/// The boot level of the boot image in a partition (ADR 0022).
fn partition_level(part: &Path) -> Result<u32> {
    let mut head = vec![0u8; 4096];
    let n = File::open(part)
        .and_then(|mut f| f.read(&mut head))
        .with_context(|| format!("reading {}", part.display()))?;
    head.truncate(n);
    // Not a boot image: nothing would start anyway; count it as level 1.
    Ok(pinna_bootimg::level(&head).unwrap_or(1))
}

/// Why installing `m` into `target` would be refused, before anything is
/// fetched but the manifest. The boot level of a boot image that comes with
/// the release is checked once it is downloaded.
fn blocked_for(
    opts: &Options,
    m: &Manifest,
    booted: Slot,
    target: Slot,
) -> Result<Option<Blocked>> {
    if opts.dev_overlay.exists() {
        return Ok(Some(Blocked::Overlay(opts.dev_overlay.clone())));
    }
    if let Some(models) = &m.requires.models
        && !opts.model.as_ref().is_some_and(|me| models.contains(me))
    {
        return Ok(Some(Blocked::Model {
            model: opts.model.clone(),
            models: models.clone(),
        }));
    }
    let has_boot = m
        .boot_for(opts.model.as_deref(), boot_format(opts))
        .is_some();
    let has = match kernel_plan(opts, has_boot, booted, target) {
        KernelPlan::NoBootImage => {
            return Ok(Some(Blocked::NoBootImage {
                model: opts.model.clone(),
            }));
        }
        KernelPlan::Stays { loads: None, .. } => 1,
        KernelPlan::Stays { loads: Some(p), .. } => partition_level(&p)?,
        KernelPlan::FromRunning { src, .. } => partition_level(&src)?,
        KernelPlan::FromManifest { .. } => return Ok(None),
    };
    let needs = m.requires.boot_level;
    Ok((has < needs).then_some(Blocked::BootLevel { needs, has }))
}

fn plan_kernel(
    opts: &Options,
    m: &Manifest,
    manifest_url: &Url,
    booted: Slot,
    target: Slot,
) -> Result<Kernel> {
    let format = boot_format(opts);
    let boot = m.boot_for(opts.model.as_deref(), format);
    let (part, mut image, from) = match kernel_plan(opts, boot.is_some(), booted, target) {
        KernelPlan::Stays { why, .. } => return Ok(Kernel::Stays(why)),
        KernelPlan::NoBootImage => bail!(
            "{}",
            Blocked::NoBootImage {
                model: opts.model.clone()
            }
        ),
        KernelPlan::FromManifest { part } => {
            let b = boot.expect("the plan saw a boot image");
            let url = resolve(Some(manifest_url), &b.url)?;
            let image = fetch_artifact(&url, b)?;
            check_boot_magic(&image, format).with_context(|| format!("{url}"))?;
            // The whole image: a FIT's level lives in the strings block at
            // its end, where a 4 KiB head wouldn't reach.
            let has = pinna_bootimg::level(&image)
                .with_context(|| format!("{url} is not a boot image"))?;
            let needs = m.requires.boot_level;
            ensure!(
                has >= needs,
                "the release's own boot image provides boot level {has}, its root needs {needs}"
            );
            if let (BootFormat::Fit, Some(board)) = (format, opts.board) {
                check_fit_board(&image, board).with_context(|| format!("{url}"))?;
            }
            (part, image, url.to_string())
        }
        KernelPlan::FromRunning { part, src } => {
            let image = std::fs::read(&src)
                .with_context(|| format!("reading the running kernel from {}", src.display()))?;
            (part, image, format!("the running {}", src.display()))
        }
    };
    let size = SlotWriter::open(&part, opts.parts.kind(), &*opts.volume_update)?.capacity()?;
    let size = size.min(opts.board.map_or(u64::MAX, |b| b.slots.boot));
    ensure!(
        image.len() as u64 <= size,
        "the boot image ({}) does not fit into {} ({})",
        mib(image.len() as u64),
        part.display(),
        mib(size)
    );
    if format == BootFormat::Android {
        pinna_bootimg::tag_slot(&mut image, target.letter())
            .with_context(|| format!("marking the boot image from {from} for slot {target}"))?;
    }
    Ok(Kernel::Write { part, image, from })
}

/// An Android boot image starts with `ANDROID!`, a FIT with the flattened
/// device tree's magic: a boot image of the other format would leave the
/// bootloader with nothing to start.
fn check_boot_magic(image: &[u8], format: BootFormat) -> Result<()> {
    let (magic, name): (&[u8], _) = match format {
        BootFormat::Android => (b"ANDROID!", "an Android boot image"),
        BootFormat::Fit => (&0xd00d_feedu32.to_be_bytes(), "a FIT"),
    };
    ensure!(
        image.starts_with(magic),
        "this satellite's bootloader needs {name}, which this boot image is not"
    );
    Ok(())
}

/// A FIT carries a device tree per board revision; at least one must be
/// for this satellite's SoC, or U-Boot would start a kernel for another
/// device.
fn check_fit_board(image: &[u8], board: &Board) -> Result<()> {
    let soc = board.soc.compatible();
    let trees = pinna_bootimg::fit::compatibles(image)?;
    ensure!(
        trees.iter().any(|c| c.iter().any(|s| s == soc)),
        "this boot image is for another device: none of its device trees is for {soc}"
    );
    Ok(())
}

/// Write the boot image and read it back from the device.
fn write_kernel(opts: &Options, part: &Path, image: &[u8]) -> Result<()> {
    let mut dev = SlotWriter::open(part, opts.parts.kind(), &*opts.volume_update)?;
    dev.begin(image.len() as u64)?;
    dev.write(image)?;
    dev.finish()?;
    let mut back = vec![0u8; image.len()];
    FileExt::read_exact_at(dev.file(), &mut back, 0)?;
    ensure!(
        back == image,
        "{} doesn't hold the boot image after writing it",
        part.display()
    );
    Ok(())
}

fn mib(n: u64) -> String {
    format!("{:.1} MiB", n as f64 / (1 << 20) as f64)
}

/// A checked manifest: what the dashboard and Home Assistant show, and why
/// installing it would be refused, if it would.
#[derive(Debug, Clone)]
pub struct Checked {
    pub fetched: Fetched,
    pub blocked: Option<Blocked>,
}

/// Fetch and check a manifest without writing anything (`pinnactl update
/// --check`). Refusals that need the slots are left out when the system
/// wasn't started through pinna-init.
pub fn check(opts: &Options, manifest: &str) -> Result<Checked, FetchError> {
    let url = resolve(None, manifest).map_err(|e| FetchError::Unreadable {
        path: manifest.to_owned(),
        detail: format!("{e:#}"),
    })?;
    let fetched = fetch_manifest(&url, &opts.key)?;
    let blocked = match slot::booted_slot(&opts.run_dir) {
        Ok(Some(booted)) => {
            // A boot partition that can't be read: the install reads it again
            // and says so.
            blocked_for(opts, &fetched.manifest, booted, booted.other())
                .ok()
                .flatten()
        }
        _ => opts
            .dev_overlay
            .exists()
            .then(|| Blocked::Overlay(opts.dev_overlay.clone())),
    };
    Ok(Checked { fetched, blocked })
}

/// Install the release described by the manifest at `manifest` into the slot
/// that is not booted, and make it active for a trial boot.
pub fn install(
    opts: &Options,
    manifest: &str,
    rules: &Rules,
    say: &mut dyn FnMut(&str),
) -> Result<Installed> {
    if opts.dev_overlay.exists() {
        bail!("{}", Blocked::Overlay(opts.dev_overlay.clone()));
    }
    let booted = slot::booted_slot(&opts.run_dir)?.with_context(|| {
        format!(
            "no {}/boot-slot: not started through pinna-init, refusing to pick a slot",
            opts.run_dir.display()
        )
    })?;
    let target = booted.other();
    let record = bcb::read(&opts.misc)?;
    let current = record.unwrap_or(bcb::Record::KAERU_DEFAULT);
    if !current.get(booted).good {
        bail!(
            "the booted slot {booted} is not marked good yet (trial boot); slot {target} is its only \
             fallback and is not overwritten. Wait for pinna-boot-ok.service or run `pinna-bootctl mark-good`."
        );
    }
    let booted_version = slot::booted_version(&opts.run_dir)?.unwrap_or_else(|| "?".into());

    let Fetched {
        manifest: m,
        url: manifest_url,
    } = fetch_manifest(&resolve(None, manifest)?, &opts.key)?;
    if let Some(want) = &rules.expect_version {
        ensure!(
            &m.version == want,
            "the manifest is for {}, not for {want}, which the last check found. Check again.",
            m.version
        );
    }
    match rules.channel {
        Some(channel) => {
            ensure!(
                !(channel == Channel::Stable && m.channel == "beta"),
                "{} is a beta, and this satellite is on stable",
                m.version
            );
            ensure!(
                version::is_newer(&m.version, &booted_version),
                "{} is not newer than the running {booted_version}; an install from a channel \
                 never goes back in version",
                m.version
            );
        }
        None => {
            if version::is_newer(&booted_version, &m.version) {
                say(&format!(
                    "version {} is older than the running {booted_version}; installing it as asked",
                    m.version
                ));
            }
        }
    }
    if let Some(b) = blocked_for(opts, &m, booted, target)? {
        bail!("{b}");
    }
    let image_url = resolve(Some(&manifest_url), &m.root.url)?;
    // pinnad's updates.rs (`stage_of`) maps these lines to install stages, so keep their prefixes.
    say(&format!(
        "booted slot {booted} (version {booted_version}); installing version {} into slot {target}",
        m.version
    ));

    let part = opts.parts.path(target.partname());
    let mut dev = SlotWriter::open(&part, opts.parts.kind(), &*opts.volume_update)?;
    let part_size = dev.capacity()?;
    ensure!(
        m.root.size <= part_size,
        "image ({}) does not fit into {} ({})",
        mib(m.root.size),
        part.display(),
        mib(part_size)
    );
    if let Some(b) = opts.board {
        ensure!(
            m.root.size <= b.slots.root,
            "image ({}) does not fit into the {}'s root slot ({})",
            mib(m.root.size),
            b.model.name(),
            mib(b.slots.root)
        );
    }

    // Before anything is overwritten: a kernel that can't be fetched leaves
    // the target slot as it was.
    let kernel = plan_kernel(opts, &m, &manifest_url, booted, target)?;

    // From here on the target is being overwritten: it must not count as a fallback.
    let invalid = bcb::invalidate(current, booted, target)?;
    if record != Some(invalid) {
        bcb::write(&opts.misc, &invalid)?;
    }

    match kernel {
        Kernel::Stays(why) => say(&why),
        Kernel::Write { part, image, from } => {
            write_kernel(opts, &part, &image)?;
            say(&format!(
                "kernel from {from} ({}) written to {} and read back",
                mib(image.len() as u64),
                part.display()
            ));
        }
    }

    say(&format!(
        "downloading {image_url} ({}) into {}",
        mib(m.root.size),
        part.display()
    ));
    let mut src = open(&image_url)?;
    dev.begin(m.root.size)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    let mut written = 0u64;
    let mut next_report = m.root.size / 4;
    loop {
        let n = src
            .read(&mut buf)
            .with_context(|| format!("reading {image_url}"))?;
        if n == 0 {
            break;
        }
        ensure!(
            written + n as u64 <= m.root.size,
            "download is larger than the manifest says ({} bytes)",
            m.root.size
        );
        h.update(&buf[..n]);
        dev.write(&buf[..n])?;
        written += n as u64;
        if written >= next_report && written < m.root.size {
            say(&format!("  {} of {}", mib(written), mib(m.root.size)));
            next_report += m.root.size / 4;
        }
    }
    ensure!(
        written == m.root.size,
        "download ended after {written} of {} bytes",
        m.root.size
    );
    let sha = keys::hex(&h.finalize());
    ensure!(
        sha.eq_ignore_ascii_case(&m.root.sha256),
        "sha256 mismatch: got {sha}, manifest says {}",
        m.root.sha256
    );
    dev.finish()?;
    say(&format!("written and flushed {}; sha256 OK", mib(written)));

    // Verify what is on the device now, not what went through memory.
    let footer = pinna_image::verify(dev.file(), &opts.key)
        .with_context(|| format!("verifying slot {target} after writing"))?;
    ensure!(
        footer.version == m.version,
        "image version {} does not match the manifest ({})",
        footer.version,
        m.version
    );
    ensure!(
        footer.image_size() == m.root.size,
        "image size {} does not match the manifest ({})",
        footer.image_size(),
        m.root.size
    );
    drop(dev);
    say(&format!(
        "slot {target} verified: signature OK, dm-verity root hash {} matches the data",
        keys::hex(&footer.root_hash)
    ));

    let now = bcb::read_or_default(&opts.misc)?;
    let active = bcb::set_active(now, target, opts.attempts)?;
    bcb::write(&opts.misc, &active)?;
    say(&format!(
        "installed version {} into slot {target}: the next boot tries it ({} attempts), \
         falling back to slot {booted} (version {booted_version})",
        m.version, opts.attempts
    ));
    Ok(Installed {
        slot: target,
        version: m.version,
    })
}

#[cfg(test)]
mod tests;
