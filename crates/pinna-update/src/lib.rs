//! Install a release into the inactive A/B slot: its kernel into
//! `boot_<slot>` and its root into `system_<slot>` (ADR 0019).
//!
//! The manifest is JSON: `{ "version", "root": { "url", "size", "sha256" },
//! "boot": { … } }`, `boot` optional; URLs may be relative to the manifest.
//! Manifest and images can come from `http(s)://`, `file://` or a plain path.
//!
//! 1. The booted slot must be marked good: the other slot is its only fallback.
//! 2. The kernel: the manifest's boot image, or without one the running
//!    slot's, marked for the target slot. boot_b is always written (a
//!    bootloader that ignores the slot never loads it); boot_a only once a
//!    boot from b showed that the bootloader loads boot_b too.
//! 3. The target slot is marked unbootable before it is overwritten.
//! 4. The kernel is written and read back; the root is streamed into the
//!    target partition while its size and sha256 are checked.
//! 5. Read back from the device: footer signature (release key), version, and
//!    the whole dm-verity hash tree recomputed from the data.
//! 6. Only then the target becomes active for a trial with N boot attempts.

use std::fs::{File, OpenOptions};
use std::io::Read;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileExt, FileTypeExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use pinna_bootctl::bcb;
use pinna_bootctl::slot::{self, KernelSlots, Slot};
use pinna_image::ReadAt;
use pinna_image::keys::{self, VerifyingKey};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use url::Url;

#[derive(Debug, Clone, Deserialize)]
pub struct Manifest {
    pub version: String,
    pub root: Artifact,
    /// The boot image (kernel and pinna-init) that goes with the root.
    #[serde(default)]
    pub boot: Option<Artifact>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Artifact {
    pub url: String,
    pub size: u64,
    pub sha256: String,
}

pub struct Options {
    /// Boot attempts the new slot gets before rolling back.
    pub attempts: u8,
    pub misc: PathBuf,
    /// Directory with the slot partitions by name (system_a, boot_a, …).
    pub partdir: PathBuf,
    /// Where pinna-init records the booted slot.
    pub run_dir: PathBuf,
    /// What `pinna-bootctl mark-good` learned about the bootloader.
    pub kernel_slots: PathBuf,
    pub key: VerifyingKey,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            attempts: bcb::DEFAULT_ATTEMPTS,
            misc: "/dev/disk/by-partlabel/misc".into(),
            partdir: "/dev/disk/by-partlabel".into(),
            run_dir: slot::RUN_DIR.into(),
            kernel_slots: slot::KERNEL_SLOTS.into(),
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
    check_artifact("root", &m.root)?;
    if let Some(b) = &m.boot {
        check_artifact("boot", b)?;
    }
    Ok(m)
}

fn check_artifact(name: &str, a: &Artifact) -> Result<()> {
    ensure!(a.size > 0, "manifest: {name}.size is 0");
    ensure!(
        a.sha256.len() == 64 && a.sha256.bytes().all(|c| c.is_ascii_hexdigit()),
        "manifest: {name}.sha256 is not a hex sha256"
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

fn boot_part(partdir: &Path, s: Slot) -> PathBuf {
    partdir.join(format!("boot_{s}"))
}

fn plan_kernel(
    opts: &Options,
    m: &Manifest,
    manifest_url: &Url,
    booted: Slot,
    target: Slot,
) -> Result<Kernel> {
    let part = boot_part(&opts.partdir, target);
    if !part.exists() {
        return Ok(Kernel::Stays(format!(
            "no {} partition: the kernel stays as it is",
            part.display()
        )));
    }
    if target == Slot::A && KernelSlots::read(&opts.kernel_slots) != KernelSlots::Honoured {
        return Ok(Kernel::Stays(
            "the kernel in boot_a stays: this bootloader hasn't been seen loading boot_b yet, so a \
             new kernel in boot_a would have no fallback"
                .into(),
        ));
    }
    let (mut image, from) = match &m.boot {
        Some(b) => {
            let url = resolve(Some(manifest_url), &b.url)?;
            (fetch_artifact(&url, b)?, url.to_string())
        }
        None => {
            let src = boot_part(&opts.partdir, booted);
            let image = std::fs::read(&src)
                .with_context(|| format!("reading the running kernel from {}", src.display()))?;
            (image, format!("the running {}", src.display()))
        }
    };
    let size = std::fs::File::open(&part)?.size()?;
    ensure!(
        image.len() as u64 <= size,
        "the boot image ({}) does not fit into {} ({})",
        mib(image.len() as u64),
        part.display(),
        mib(size)
    );
    pinna_bootimg::tag_slot(&mut image, target.letter())
        .with_context(|| format!("marking the boot image from {from} for slot {target}"))?;
    Ok(Kernel::Write { part, image, from })
}

/// Write the boot image and read it back from the device.
fn write_kernel(part: &Path, image: &[u8]) -> Result<()> {
    let dev = open_target(part)?;
    dev.write_all_at(image, 0)
        .with_context(|| format!("writing {}", part.display()))?;
    dev.sync_all()?;
    drop_cache(&dev);
    let mut back = vec![0u8; image.len()];
    FileExt::read_exact_at(&dev, &mut back, 0)?;
    ensure!(
        back == image,
        "{} doesn't hold the boot image after writing it",
        part.display()
    );
    Ok(())
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
    let record = bcb::read(&opts.misc)?;
    let current = record.unwrap_or(bcb::Record::KAERU_DEFAULT);
    if !current.get(booted).good {
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
            write_kernel(&part, &image)?;
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
