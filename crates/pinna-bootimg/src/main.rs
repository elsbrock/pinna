use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use pinna_bootimg::*;

#[derive(Parser)]
#[command(about = "Android v0 boot images for the MT8163 Echo devices")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Print header fields and section sizes.
    Info { image: PathBuf },
    /// Extract zImage, DTB, ramdisk and second into a directory.
    Unpack { image: PathBuf, dir: PathBuf },
    /// Build a boot image from a zImage, DTB and optional ramdisk.
    Pack {
        #[arg(long)]
        zimage: PathBuf,
        #[arg(long)]
        dtb: PathBuf,
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
            if let Ok((z, d)) = split_kernel_blob(&img.kernel) {
                println!("zImage          {} bytes", z.len());
                println!("dtb             {} bytes", d.len());
            }
        }
        Cmd::Unpack { image, dir } => {
            let img = BootImage::parse(&fs::read(&image)?)?;
            fs::create_dir_all(&dir)?;
            let (z, d) = split_kernel_blob(&img.kernel)?;
            fs::write(dir.join("zImage"), z)?;
            fs::write(dir.join("dtb"), d)?;
            fs::write(dir.join("ramdisk"), &img.ramdisk)?;
            if !img.second.is_empty() {
                fs::write(dir.join("second"), &img.second)?;
            }
            fs::write(dir.join("cmdline"), &img.header.cmdline)?;
        }
        Cmd::Pack {
            zimage,
            dtb,
            ramdisk,
            out,
            no_pad,
        } => {
            let z = fs::read(&zimage).with_context(|| format!("reading {}", zimage.display()))?;
            let d = fs::read(&dtb).with_context(|| format!("reading {}", dtb.display()))?;
            let ramdisk = match ramdisk {
                Some(p) => fs::read(&p).with_context(|| format!("reading {}", p.display()))?,
                None => newc_cpio(&[(
                    "pinna-placeholder",
                    b"Pinna boots without an initramfs; the kernel mounts root= itself.\n",
                )]),
            };
            let img = BootImage {
                header: Header::default(),
                kernel: kernel_blob(&z, &d)?,
                ramdisk,
                second: Vec::new(),
                id: [0; 32],
            };
            let bytes = img.to_bytes((!no_pad).then_some(PARTITION_SIZE))?;
            fs::write(&out, &bytes)?;
            println!(
                "{}: {} bytes (kernel {}, ramdisk {})",
                out.display(),
                bytes.len(),
                img.kernel.len(),
                img.ramdisk.len()
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
