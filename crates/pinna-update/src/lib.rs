//! Install a release into the inactive A/B slot.
//!
//! The manifest is JSON: `{ "version", "root": { "url", "size", "sha256" } }`;
//! `root.url` may be relative to the manifest. Manifest and image can come
//! from `http(s)://`, `file://` or a plain path.
//!
//! 1. The booted slot must be marked good: the other slot is its only fallback.
//! 2. The target slot is marked unbootable before it is overwritten.
//! 3. The image is streamed into the target partition while its size and
//!    sha256 are checked against the manifest.
//! 4. Read back from the device: footer signature (release key), version, and
//!    the whole dm-verity hash tree recomputed from the data.
//! 5. Only then the target becomes active for a trial boot with N tries.

use std::fs::{File, OpenOptions};
use std::io::Read;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileExt, FileTypeExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use pinna_bootctl::slot::{self, Slot};
use pinna_image::ReadAt;
use pinna_image::keys::{self, VerifyingKey};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use url::Url;

#[derive(Debug, Clone, Deserialize)]
pub struct Manifest {
    pub version: String,
    pub root: Artifact,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Artifact {
    pub url: String,
    pub size: u64,
    pub sha256: String,
}

pub struct Options {
    /// Tries the new slot gets before rolling back.
    pub tries: u8,
    pub misc: PathBuf,
    /// Directory with the slot partitions by name (system_a, system_b).
    pub partdir: PathBuf,
    /// Where pinna-init records the booted slot.
    pub run_dir: PathBuf,
    pub key: VerifyingKey,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            tries: slot::DEFAULT_TRIES,
            misc: "/dev/disk/by-partlabel/misc".into(),
            partdir: "/dev/disk/by-partlabel".into(),
            run_dir: slot::RUN_DIR.into(),
            key: keys::release_key(),
        }
    }
}

#[derive(Debug)]
pub struct Installed {
    pub slot: Slot,
    pub version: String,
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
            // The timeout applies to each read: a stalled download fails.
            let client = reqwest::blocking::Client::builder()
                .connect_timeout(Duration::from_secs(30))
                .timeout(Duration::from_secs(60))
                .user_agent(concat!("pinna-update/", env!("CARGO_PKG_VERSION")))
                .build()?;
            let resp = client
                .get(url.clone())
                .send()
                .with_context(|| format!("GET {url}"))?
                .error_for_status()?;
            Ok(Box::new(resp))
        }
        other => bail!("unsupported URL scheme {other:?} ({url})"),
    }
}

pub fn fetch_manifest(url: &Url) -> Result<Manifest> {
    let mut text = String::new();
    open(url)?
        .take(1 << 20)
        .read_to_string(&mut text)
        .with_context(|| format!("reading {url}"))?;
    let m: Manifest =
        serde_json::from_str(&text).with_context(|| format!("parsing the manifest {url}"))?;
    pinna_image::footer::check_version(&m.version)?;
    ensure!(m.root.size > 0, "manifest: root.size is 0");
    ensure!(
        m.root.sha256.len() == 64 && m.root.sha256.bytes().all(|c| c.is_ascii_hexdigit()),
        "manifest: root.sha256 is not a hex sha256"
    );
    Ok(m)
}

/// The slot partition, refusing anything in use (mounted, held by dm-verity).
fn open_target(path: &Path) -> Result<File> {
    let real = path
        .canonicalize()
        .with_context(|| format!("resolving {}", path.display()))?;
    let is_dev = std::fs::metadata(&real)?.file_type().is_block_device();
    if is_dev {
        let name = real.file_name().unwrap().to_string_lossy().into_owned();
        let holders = Path::new("/sys/class/block").join(&name).join("holders");
        if let Ok(mut d) = std::fs::read_dir(&holders) {
            ensure!(
                d.next().is_none(),
                "{} is in use (holders in {})",
                real.display(),
                holders.display()
            );
        }
    }
    // O_EXCL on a block device: exclusive claim, EBUSY if mounted or held.
    OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(if is_dev { libc::O_EXCL } else { 0 })
        .open(&real)
        .with_context(|| format!("opening {} for writing", real.display()))
}

fn drop_cache(f: &File) {
    // SAFETY: plain fadvise on an open fd.
    unsafe {
        libc::posix_fadvise(f.as_raw_fd(), 0, 0, libc::POSIX_FADV_DONTNEED);
    }
}

fn mib(n: u64) -> String {
    format!("{:.1} MiB", n as f64 / (1 << 20) as f64)
}

/// Install the release described by the manifest at `manifest` into the slot
/// that is not booted, and make it active for a trial boot.
pub fn install(opts: &Options, manifest: &str, say: &mut dyn FnMut(&str)) -> Result<Installed> {
    let booted = slot::booted_slot(&opts.run_dir)?.with_context(|| {
        format!(
            "no {}/boot-slot: not started through pinna-init, refusing to pick a slot",
            opts.run_dir.display()
        )
    })?;
    let target = booted.other();
    let state = slot::read_state(&opts.misc)?;
    let current = state.unwrap_or_else(slot::SlotState::implicit);
    if !current.get(booted).successful {
        bail!(
            "the booted slot {booted} is not marked good yet (trial boot); slot {target} is its only \
             fallback and is not overwritten. Wait for pinna-boot-ok.service or run `pinna-bootctl mark-good`."
        );
    }
    let booted_version = slot::booted_version(&opts.run_dir)?.unwrap_or_else(|| "?".into());

    let manifest_url = resolve(None, manifest)?;
    let m = fetch_manifest(&manifest_url)?;
    let image_url = resolve(Some(&manifest_url), &m.root.url)?;
    // pinnad's updates.rs (`stage_of`) maps these lines to install stages, so keep their prefixes.
    say(&format!(
        "booted slot {booted} (version {booted_version}); installing version {} into slot {target}",
        m.version
    ));

    let part = opts.partdir.join(target.partname());
    let dev = open_target(&part)?;
    let part_size = dev.size()?;
    ensure!(
        m.root.size <= part_size,
        "image ({}) does not fit into {} ({})",
        mib(m.root.size),
        part.display(),
        mib(part_size)
    );

    // From here on the target is being overwritten: it must not count as a fallback.
    let invalid = slot::invalidate(state, booted, target);
    if state != Some(invalid) {
        slot::write_state(&opts.misc, &invalid)?;
    }

    say(&format!(
        "downloading {image_url} ({}) into {}",
        mib(m.root.size),
        part.display()
    ));
    let mut src = open(&image_url)?;
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
        dev.write_all_at(&buf[..n], written)
            .with_context(|| format!("writing {}", part.display()))?;
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
    dev.sync_all()?;
    say(&format!("written and flushed {}; sha256 OK", mib(written)));

    // Verify what is on the device now, not what went through memory.
    drop_cache(&dev);
    let footer = pinna_image::verify(&dev, &opts.key)
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

    let now = slot::read_state(&opts.misc)?;
    let active = slot::set_active(now, target, opts.tries);
    slot::write_state(&opts.misc, &active)?;
    say(&format!(
        "installed version {} into slot {target}: the next boot tries it ({} tries), \
         falling back to slot {booted} (version {booted_version})",
        m.version, opts.tries
    ));
    Ok(Installed {
        slot: target,
        version: m.version,
    })
}

#[cfg(test)]
mod tests;
