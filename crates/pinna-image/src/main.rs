use std::fs::{self, File};
use std::io::{BufWriter, Read};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use pinna_image::version::Version;
use pinna_image::{footer, keys, manifest, verity};
use sha2::{Digest, Sha256};

#[derive(Parser)]
#[command(about = "Signed, dm-verity protected Pinna slot images")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Create a signing key of your own if missing and write its public key
    /// (compiled in instead of keys/release.pub).
    Keygen {
        #[arg(long, default_value = "secrets/release.key")]
        secret: PathBuf,
        #[arg(long, default_value = "secrets/release.pub")]
        public: PathBuf,
    },
    /// Print the compiled-in release public key (hex) and where it came from.
    Key,
    /// Seal an erofs root (mkosi output) into a signed slot image.
    Seal {
        #[arg(long, default_value = "secrets/release.key")]
        key: PathBuf,
        /// Image version; must equal IMAGE_VERSION in the image's os-release.
        #[arg(long)]
        version: String,
        /// Hex salt (32 bytes); random by default.
        #[arg(long)]
        salt: Option<String>,
        /// The models the root is for, comma-separated; it must fit the
        /// smallest root slot of them (ADR 0035). Default: every model.
        #[arg(long, value_delimiter = ',')]
        models: Vec<String>,
        /// Skip the slot size check, for an image no satellite installs (the
        /// x86_64 virtual machine).
        #[arg(long, conflicts_with = "models")]
        no_slot_check: bool,
        input: PathBuf,
        output: PathBuf,
    },
    /// Check signature, footer and the whole hash tree of a slot image.
    Verify {
        /// Public key file (hex); defaults to the compiled-in release key.
        #[arg(long)]
        pubkey: Option<PathBuf>,
        image: PathBuf,
    },
    /// Print the footer (signature not checked).
    Info { image: PathBuf },
    /// Write a signed update manifest for a slot image (ADR 0022):
    /// manifest.json and manifest.json.sig in --out.
    Manifest {
        /// URL of the image, absolute or relative to the manifest.
        #[arg(long)]
        url: Option<String>,
        /// The Android boot image that goes with the root (ADR 0019), for the
        /// models that boot one (echo-dot-2, echo-2), listed by its file name
        /// next to the manifest. Old satellites read it as `boot`.
        #[arg(long)]
        boot: Option<PathBuf>,
        /// A boot image for the models that boot its format, as
        /// MODELS=PATH with the models comma-separated (ADR 0035), such as
        /// echo-dot-5=pinna-boot-mt8519.itb; repeat for more. Listed in
        /// `boots`.
        #[arg(long = "boot-for", value_name = "MODELS=PATH")]
        boot_for: Vec<String>,
        /// pinna-bootctl for the models the browser installer runs it on
        /// (ADR 0016), as MODELS=PATH, such as
        /// echo-dot-5=pinna-bootctl-armv7; repeat for more. Listed in
        /// `bootctl`, so the page checks it against the signature.
        /// Satellites don't read it.
        #[arg(long = "bootctl-for", value_name = "MODELS=PATH")]
        bootctl_for: Vec<String>,
        /// The key that signs the manifest; its public half must be compiled in.
        #[arg(long, default_value = "secrets/release.key")]
        key: PathBuf,
        /// One line for Home Assistant's update card, at most 255
        /// characters. Default: "Pinna <version>."
        #[arg(long)]
        summary: Option<String>,
        /// The release notes' page.
        #[arg(long)]
        notes_url: Option<String>,
        /// The models the release is built for (echo-dot-2, echo-2,
        /// echo-dot-5), comma-separated or repeated. Default: every model.
        /// Each needs a boot image of its format, and the images must fit
        /// the smallest slot of these models.
        #[arg(long, alias = "model", value_delimiter = ',')]
        models: Vec<String>,
        /// Directory for manifest.json and manifest.json.sig.
        #[arg(long)]
        out: PathBuf,
        image: PathBuf,
    },
}

fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::Keygen { secret, public } => {
            let created = keys::keygen(&secret, &public)?;
            println!(
                "{} {} (keep it secret, never commit it); public key in {}",
                if created { "created" } else { "kept existing" },
                secret.display(),
                public.display()
            );
        }
        Cmd::Key => {
            println!("{}", keys::hex(&keys::RELEASE_PUBLIC_KEY));
            println!("{}", env!("PINNA_RELEASE_PUB_SOURCE"));
        }
        Cmd::Seal {
            key,
            version,
            salt,
            models,
            no_slot_check,
            input,
            output,
        } => {
            let models = manifest::parse_models(&models)?;
            let path = key;
            let key = keys::read_secret(&path)?;
            ensure!(
                key.verifying_key() == keys::release_key(),
                "{path}: its public key is not the one compiled in ({source}); a root sealed with it would not boot. Use the matching key, or run just keygen",
                path = path.display(),
                source = env!("PINNA_RELEASE_PUB_SOURCE"),
            );
            let salt: [u8; footer::SALT_LEN] = match salt {
                Some(hex) => keys::unhex(&hex)?
                    .try_into()
                    .map_err(|_| anyhow::anyhow!("salt must be 32 bytes"))?,
                None => {
                    let mut s = [0u8; footer::SALT_LEN];
                    File::open("/dev/urandom")?.read_exact(&mut s)?;
                    s
                }
            };
            let src = File::open(&input).with_context(|| format!("opening {}", input.display()))?;
            let tmp = output.with_extension("tmp");
            let mut out = BufWriter::new(File::create(&tmp)?);
            let f = pinna_image::seal(&src, &version, salt, &key, &mut out)?;
            out.into_inner()?.sync_all()?;
            // The slot holds the sealed image: the erofs, the footer and the
            // hash tree.
            if !no_slot_check && let Err(e) = manifest::check_root_size(f.image_size(), &models) {
                let _ = fs::remove_file(&tmp);
                return Err(e);
            }
            fs::rename(&tmp, &output)?;
            println!(
                "{}: version {}, erofs {} + footer 4096 + hash tree {} = {} bytes, root hash {}",
                output.display(),
                f.version,
                f.data_size,
                f.hash_size,
                f.image_size(),
                keys::hex(&f.root_hash)
            );
        }
        Cmd::Verify { pubkey, image } => {
            let key = match pubkey {
                Some(p) => keys::read_public(&p)?,
                None => keys::release_key(),
            };
            let dev = File::open(&image).with_context(|| format!("opening {}", image.display()))?;
            let f = pinna_image::verify(&dev, &key)
                .with_context(|| format!("{} does not verify", image.display()))?;
            println!(
                "{}: OK, version {}, {} bytes, root hash {}",
                image.display(),
                f.version,
                f.image_size(),
                keys::hex(&f.root_hash)
            );
        }
        Cmd::Info { image } => {
            let dev = File::open(&image).with_context(|| format!("opening {}", image.display()))?;
            let size = pinna_image::data_size(&dev)?;
            let mut buf = vec![0u8; footer::SIZE];
            pinna_image::ReadAt::read_exact_at(&dev, &mut buf, size)?;
            let (f, _) = footer::Footer::parse_unverified(&buf)?;
            println!("version: {}", f.version);
            println!("data_size: {}", f.data_size);
            println!("hash_offset: {}", f.hash_offset);
            println!("hash_size: {}", f.hash_size);
            println!("image_size: {}", f.image_size());
            println!("data_blocks: {}", f.data_blocks());
            println!("hash_start_block: {}", f.hash_start_block());
            println!("algorithm: {}", f.algorithm);
            println!("salt: {}", keys::hex(&f.salt));
            println!("root_hash: {}", keys::hex(&f.root_hash));
            println!("levels: {:?}", verity::level_blocks(f.data_blocks()));
        }
        Cmd::Manifest {
            url,
            boot,
            boot_for,
            bootctl_for,
            key,
            summary,
            notes_url,
            models,
            out,
            image,
        } => {
            let secret = keys::read_secret(&key)?;
            ensure!(
                secret.verifying_key() == keys::release_key(),
                "{}: its public key is not the one compiled in ({}); satellites would refuse the manifest",
                key.display(),
                env!("PINNA_RELEASE_PUB_SOURCE"),
            );
            let dev = File::open(&image).with_context(|| format!("opening {}", image.display()))?;
            let f = pinna_image::read_footer(&dev, &keys::release_key())?;
            let size = fs::metadata(&image)?.len();
            ensure!(
                size == f.image_size(),
                "{} is {size} bytes, its footer says {}",
                image.display(),
                f.image_size()
            );
            let version = Version::parse(&f.version).with_context(|| {
                format!(
                    "the image's version {:?} is not a semantic version (ADR 0022): build it without \
                     PINNA_VERSION, or with one such as 0.4.0",
                    f.version
                )
            })?;
            let summary = summary.unwrap_or_else(|| format!("Pinna {version}."));
            manifest::check_summary(&summary)?;
            let root_only = manifest::root_only(&models, boot.is_some() || !boot_for.is_empty());
            let models = manifest::parse_models(&models)?;
            manifest::check_root_size(size, &models)?;
            let url = match url {
                Some(u) => u,
                None => file_name(&image)?,
            };
            let ids: Vec<&str> = models.iter().map(|m| m.id()).collect();
            let mut requires = serde_json::json!({ "boot_level": manifest::ROOT_BOOT_LEVEL });
            if !root_only {
                requires["models"] = serde_json::json!(ids);
            }
            let mut boot_files: Vec<(Vec<pinna_board::Model>, PathBuf)> = Vec::new();
            for arg in &boot_for {
                boot_files.push(manifest::parse_boot_for(arg)?);
            }
            // `--boot` is the Android image: it serves the models that boot one.
            let android: Vec<_> = models
                .iter()
                .copied()
                .filter(|m| m.board().bootloader.boot_format() == pinna_board::BootFormat::Android)
                .collect();
            ensure!(
                boot.is_none() || !android.is_empty(),
                "--boot is an Android boot image, but none of the models ({}) boots one",
                ids.join(", ")
            );
            // Every model has exactly one boot image.
            let mut covered: Vec<pinna_board::Model> = Vec::new();
            for (for_models, path) in &boot_files {
                for m in for_models {
                    ensure!(
                        models.contains(m),
                        "{} is for the {}, which is not in --models ({})",
                        path.display(),
                        m.name(),
                        ids.join(", ")
                    );
                    ensure!(
                        !(covered.contains(m) || boot.is_some() && android.contains(m)),
                        "the {} has more than one boot image",
                        m.name()
                    );
                    covered.push(*m);
                }
            }
            if boot.is_some() {
                covered.extend(&android);
            }
            for m in models.iter().filter(|_| !root_only) {
                ensure!(
                    covered.contains(m),
                    "no boot image for the {}: it needs {} (--{})",
                    m.name(),
                    match m.board().bootloader.boot_format() {
                        pinna_board::BootFormat::Android => "an Android boot image",
                        pinna_board::BootFormat::Fit => "a U-Boot FIT",
                    },
                    match m.board().bootloader.boot_format() {
                        pinna_board::BootFormat::Android => "boot".to_string(),
                        pinna_board::BootFormat::Fit => format!("boot-for {}=PATH", m.id()),
                    }
                );
            }
            let mut m = serde_json::json!({
                "version": f.version,
                "channel": version.channel(),
                "summary": summary,
            });
            if let Some(n) = notes_url {
                m["notes_url"] = serde_json::json!(n);
            }
            m["requires"] = requires;
            m["root"] =
                serde_json::json!({ "url": url, "size": size, "sha256": sha256_file(&image)? });
            let entry = |path: &Path| -> Result<(serde_json::Value, Vec<u8>)> {
                let bytes =
                    fs::read(path).with_context(|| format!("reading {}", path.display()))?;
                Ok((
                    serde_json::json!({
                        "url": file_name(path)?,
                        "size": bytes.len(),
                        "sha256": sha256_file(path)?,
                    }),
                    bytes,
                ))
            };
            if let Some(b) = &boot {
                let (value, bytes) = entry(b)?;
                manifest::check_boot(&b.display().to_string(), &bytes, &android)?;
                m["boot"] = value;
            }
            let mut boots = Vec::new();
            for (for_models, path) in &boot_files {
                let (mut value, bytes) = entry(path)?;
                manifest::check_boot(&path.display().to_string(), &bytes, for_models)?;
                let ids: Vec<&str> = for_models.iter().map(|m| m.id()).collect();
                value["models"] = serde_json::json!(ids);
                boots.push(value);
            }
            if !boots.is_empty() {
                m["boots"] = serde_json::json!(boots);
            }
            let mut bootctl = Vec::new();
            let mut tooled: Vec<pinna_board::Model> = Vec::new();
            for arg in &bootctl_for {
                let (for_models, path) = manifest::parse_bootctl_for(arg)?;
                for m in &for_models {
                    ensure!(
                        models.contains(m),
                        "{} is for the {}, which is not in --models ({})",
                        path.display(),
                        m.name(),
                        ids.join(", ")
                    );
                    ensure!(
                        !tooled.contains(m),
                        "the {} has more than one pinna-bootctl",
                        m.name()
                    );
                    tooled.push(*m);
                }
                let (mut value, _) = entry(&path)?;
                let for_ids: Vec<&str> = for_models.iter().map(|m| m.id()).collect();
                value["models"] = serde_json::json!(for_ids);
                bootctl.push(value);
            }
            if !bootctl.is_empty() {
                m["bootctl"] = serde_json::json!(bootctl);
            }
            let text = format!("{}\n", serde_json::to_string_pretty(&m)?);
            fs::create_dir_all(&out)?;
            let (json, sig) = (out.join("manifest.json"), out.join("manifest.json.sig"));
            fs::write(&json, &text).with_context(|| format!("writing {}", json.display()))?;
            fs::write(&sig, manifest::sign(text.as_bytes(), &secret))
                .with_context(|| format!("writing {}", sig.display()))?;
            println!("{}\n{}", json.display(), sig.display());
        }
    }
    Ok(())
}

fn file_name(p: &Path) -> Result<String> {
    Ok(p.file_name()
        .context("image path has no file name")?
        .to_string_lossy()
        .into_owned())
}

fn sha256_file(p: &Path) -> Result<String> {
    let mut f = File::open(p)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(keys::hex(&h.finalize()))
}
