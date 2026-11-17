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
        /// The boot image that goes with the root (ADR 0019), listed by
        /// its file name next to the manifest.
        #[arg(long)]
        boot: Option<PathBuf>,
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
        /// A model the release is for (echo-dot-2, echo-2); repeat for more.
        /// Without one, the release is for every model.
        #[arg(long = "model")]
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
            input,
            output,
        } => {
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
            for m in &models {
                ensure!(
                    manifest::MODELS.contains(&m.as_str()),
                    "unknown model {m:?}; the models are {}",
                    manifest::MODELS.join(", ")
                );
            }
            let url = match url {
                Some(u) => u,
                None => file_name(&image)?,
            };
            let mut requires = serde_json::json!({ "boot_level": manifest::ROOT_BOOT_LEVEL });
            if !models.is_empty() {
                requires["models"] = serde_json::json!(models);
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
            if let Some(b) = boot {
                let head = fs::read(&b).with_context(|| format!("reading {}", b.display()))?;
                let level = pinna_bootimg::level(&head)
                    .with_context(|| format!("{} is not a boot image", b.display()))?;
                ensure!(
                    level >= manifest::ROOT_BOOT_LEVEL,
                    "{} provides boot level {level}; the root needs {}",
                    b.display(),
                    manifest::ROOT_BOOT_LEVEL
                );
                m["boot"] = serde_json::json!({
                    "url": file_name(&b)?,
                    "size": head.len(),
                    "sha256": sha256_file(&b)?,
                });
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
