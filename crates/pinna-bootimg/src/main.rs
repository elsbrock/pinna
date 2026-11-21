use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use pinna_bootimg::*;

#[derive(Parser)]
#[command(about = "Android v0 boot images for the MT8163 and MT8516 Echo devices")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Print header fields and section sizes.
    Info { image: PathBuf },
    /// Extract the kernel (zImage, or Image.gz in the 64-bit layout), the
    /// DTBs, ramdisk and second into a directory.
    Unpack { image: PathBuf, dir: PathBuf },
    /// Build a boot image from a kernel, DTB and optional ramdisk: a zImage
    /// and one DTB for the MT8163, or an Image.gz and one DTB per board
    /// revision in the 64-bit layout for the MT8516 (ADR 0031).
    Pack {
        #[arg(
            long,
            conflicts_with = "image_gz",
            required_unless_present = "image_gz"
        )]
        zimage: Option<PathBuf>,
        #[arg(long)]
        image_gz: Option<PathBuf>,
        #[arg(long, required = true)]
        dtb: Vec<PathBuf>,
        /// Ramdisk (cpio, optionally compressed). Defaults to a placeholder
        /// cpio without /init, so the kernel mounts root= itself.
        #[arg(long)]
        ramdisk: Option<PathBuf>,
        #[arg(long)]
        out: PathBuf,
        /// Do not pad to the 16 MiB partition size.
        #[arg(long)]
        no_pad: bool,
    },
    /// Re-serialize an image and check it is byte-identical.
    Roundtrip { image: PathBuf },
    /// Build the initramfs (newc cpio) around a static pinna-init.
    Initramfs {
        #[arg(long)]
        init: PathBuf,
        /// A static busybox for rescue mode (scripts/fetch-busybox.sh).
        #[arg(long)]
        busybox: Option<PathBuf>,
        #[arg(long)]
        out: PathBuf,
    },
}

fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::Info { image } => {
            let data = fs::read(&image)?;
            let img = BootImage::parse(&data)?;
            let h = &img.header;
            println!("size            {}", data.len());
            println!("kernel_addr     {:#010x}", h.kernel_addr);
            println!("ramdisk_addr    {:#010x}", h.ramdisk_addr);
            println!("second_addr     {:#010x}", h.second_addr);
            println!("tags_addr       {:#010x}", h.tags_addr);
            println!("page_size       {}", h.page_size);
            println!("cmdline         {}", String::from_utf8_lossy(&h.cmdline));
            println!("kernel_size     {}", img.kernel.len());
            println!("ramdisk_size    {}", img.ramdisk.len());
            println!("second_size     {}", img.second.len());
            let ok = img.id == img.compute_id();
            println!(
                "id              {} ({})",
                hex(&img.id[..20]),
                if ok { "valid" } else { "MISMATCH" }
            );
            if let Some((gz, dtbs)) = arm64::split_kernel_blob(&img.kernel) {
                println!("Image.gz        {} bytes", gz.len());
                for d in &dtbs {
                    let compat = arm64::root_compatibles(d).unwrap_or_default();
                    println!("dtb             {} bytes {}", d.len(), compat.join(" "));
                }
            } else if let Ok((z, d)) = split_kernel_blob(&img.kernel) {
                println!("zImage          {} bytes", z.len());
                println!("dtb             {} bytes", d.len());
            }
        }
        Cmd::Unpack { image, dir } => {
            let img = BootImage::parse(&fs::read(&image)?)?;
            fs::create_dir_all(&dir)?;
            if let Some((gz, dtbs)) = arm64::split_kernel_blob(&img.kernel) {
                fs::write(dir.join("Image.gz"), gz)?;
                for (i, d) in dtbs.iter().enumerate() {
                    fs::write(dir.join(format!("dtb-{i}")), d)?;
                }
            } else {
                let (z, d) = split_kernel_blob(&img.kernel)?;
                fs::write(dir.join("zImage"), z)?;
                fs::write(dir.join("dtb"), d)?;
            }
            fs::write(dir.join("ramdisk"), &img.ramdisk)?;
            if !img.second.is_empty() {
                fs::write(dir.join("second"), &img.second)?;
            }
            fs::write(dir.join("cmdline"), &img.header.cmdline)?;
        }
        Cmd::Pack {
            zimage,
            image_gz,
            dtb,
            ramdisk,
            out,
            no_pad,
        } => {
            let read =
                |p: &PathBuf| fs::read(p).with_context(|| format!("reading {}", p.display()));
            let dtbs = dtb.iter().map(read).collect::<Result<Vec<_>>>()?;
            let ramdisk = match ramdisk {
                Some(p) => fs::read(&p).with_context(|| format!("reading {}", p.display()))?,
                None => newc_cpio(&[(
                    "pinna-placeholder",
                    b"Pinna boots without an initramfs; the kernel mounts root= itself.\n",
                )]),
            };
            let (header, kernel, pad) = match (zimage, image_gz) {
                (Some(z), _) => {
                    ensure!(dtbs.len() == 1, "the MT8163 layout takes one DTB");
                    let header = Header {
                        cmdline: default_cmdline().into_bytes(),
                        ..Header::default()
                    };
                    (header, kernel_blob(&read(&z)?, &dtbs[0])?, !no_pad)
                }
                // The MT8516 boot partitions' size isn't measured yet, so
                // these images are not padded.
                (None, Some(gz)) => {
                    let dtbs: Vec<&[u8]> = dtbs.iter().map(Vec::as_slice).collect();
                    (
                        arm64::header(),
                        arm64::kernel_blob(&read(&gz)?, &dtbs)?,
                        false,
                    )
                }
                (None, None) => unreachable!("clap requires --zimage or --image-gz"),
            };
            let img = BootImage {
                header,
                kernel,
                ramdisk,
                second: Vec::new(),
                id: [0; 32],
            };
            let bytes = img.to_bytes(pad.then_some(PARTITION_SIZE))?;
            fs::write(&out, &bytes)?;
            println!(
                "{}: {} bytes (kernel {}, ramdisk {})",
                out.display(),
                bytes.len(),
                img.kernel.len(),
                img.ramdisk.len()
            );
        }
        Cmd::Initramfs { init, busybox, out } => {
            let elf = |p: &PathBuf| -> Result<Vec<u8>> {
                let bin = fs::read(p).with_context(|| format!("reading {}", p.display()))?;
                ensure!(
                    bin.starts_with(b"\x7fELF"),
                    "{} is not an ELF binary",
                    p.display()
                );
                Ok(bin)
            };
            let bin = elf(&init)?;
            let bb = busybox.as_ref().map(elf).transpose()?;
            let cpio = initramfs(&bin, bb.as_deref());
            fs::write(&out, &cpio)?;
            println!(
                "{}: {} bytes (init {} bytes, busybox {} bytes)",
                out.display(),
                cpio.len(),
                bin.len(),
                bb.as_ref().map_or(0, Vec::len)
            );
        }
        Cmd::Roundtrip { image } => {
            let data = fs::read(&image)?;
            let img = BootImage::parse(&data)?;
            let again = img.to_bytes(Some(data.len()))?;
            ensure!(again == data, "round-trip differs");
            println!("{}: byte-identical round-trip", image.display());
        }
    }
    Ok(())
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}
