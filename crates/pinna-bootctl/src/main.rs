use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::Context;
use clap::{Parser, Subcommand};
use pinna_bootctl::bcb::{self, Record};
use pinna_bootctl::slot::{self, Slot};
use pinna_bootctl::*;

#[derive(Parser)]
#[command(about = "Bootloader message and A/B slot control for the MT8163 Echo devices")]
struct Cli {
    #[arg(long, default_value = "/dev/disk/by-partlabel/misc")]
    misc: PathBuf,
    #[arg(long, default_value = "/dev/disk/by-partlabel/expdb")]
    expdb: PathBuf,
    /// Where the slot partitions (system_a, system_b) are found by name.
    #[arg(long, default_value = "/dev/disk/by-partlabel")]
    partdir: PathBuf,
    /// Where pinna-init records the booted slot and version.
    #[arg(long, default_value = slot::RUN_DIR)]
    run_dir: PathBuf,
    /// Where mark-good keeps what the bootloader does with the slot for the kernel.
    #[arg(long, default_value = slot::KERNEL_SLOTS)]
    kernel_slots: PathBuf,
    /// Arm even without a Kaeru payload in expdb (tests only).
    #[arg(long)]
    allow_no_kaeru: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Show the current bootloader command.
    Status,
    /// Arm a one-shot "enter fastboot on next boot" (dev safety net). Skipped
    /// during a trial boot of a new slot that has a good fallback.
    GuardArm,
    /// Clear our own one-shot request once the system is healthy.
    GuardClear,
    /// Arm and reboot into fastboot now.
    RebootBootloader,
    /// Show the slot record, the images in the slots and the booted slot.
    SlotStatus,
    /// Mark the booted slot good (after the health check); a no-op without pinna-init.
    MarkGood,
    /// Boot a slot next, as a trial with a number of boot attempts.
    SetActive {
        slot: String,
        #[arg(long = "tries", default_value_t = bcb::DEFAULT_ATTEMPTS,
              value_parser = clap::value_parser!(u8).range(1..=bcb::MAX_ATTEMPTS as i64))]
        attempts: u8,
        /// Do not check the slot's image signature first.
        #[arg(long)]
        force: bool,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(&cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("pinna-bootctl: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn kaeru_ok(cli: &Cli) -> anyhow::Result<bool> {
    if cli.allow_no_kaeru || kaeru_present(&cli.expdb)? {
        return Ok(true);
    }
    eprintln!(
        "pinna-bootctl: no Kaeru payload in {} — nothing would honour the request; misc untouched",
        cli.expdb.display()
    );
    Ok(false)
}

/// The signed image in a slot partition, or why it is not bootable.
fn slot_image(partdir: &Path, s: Slot) -> anyhow::Result<pinna_image::Footer> {
    slot::image(partdir, s, &pinna_image::keys::release_key())
}

fn run(cli: &Cli) -> anyhow::Result<()> {
    match &cli.cmd {
        Cmd::Status => {
            let c = read_command(&cli.misc)?;
            println!("command: {c:?}");
            println!("kaeru: {}", kaeru_present(&cli.expdb)?);
        }
        Cmd::GuardArm => {
            let booted = slot::booted_slot(&cli.run_dir)?;
            // An unreadable slot record must not disable the safety net.
            let record = bcb::read(&cli.misc).unwrap_or(None);
            if !bcb::guard_should_arm(record, booted) {
                let b = booted.expect("a trial boot has a booted slot");
                println!(
                    "not arming: trial boot of slot {b}, a failed boot rolls back to slot {}",
                    b.other()
                );
            } else if kaeru_ok(cli)? {
                arm(&cli.misc)?;
                println!("armed: next boot enters fastboot unless cleared");
            }
        }
        Cmd::GuardClear => {
            if clear(&cli.misc)? {
                println!("cleared: boot marked healthy");
            } else {
                println!("nothing to clear");
            }
        }
        Cmd::RebootBootloader => {
            anyhow::ensure!(kaeru_ok(cli)?, "refusing: no Kaeru payload");
            arm(&cli.misc)?;
            let _ = reboot::record_at(&cli.run_dir, "bootloader");
            let status = std::process::Command::new("systemctl")
                .arg("reboot")
                .status()?;
            anyhow::ensure!(status.success(), "systemctl reboot failed");
        }
        Cmd::SlotStatus => {
            let record = bcb::read(&cli.misc)?;
            let r = record.unwrap_or(Record::KAERU_DEFAULT);
            match record {
                Some(_) => println!("state: valid"),
                None => println!("state: missing (the bootloader writes slot a, good)"),
            }
            // The slot the next boot starts, after the bootloader's count.
            match r.next_boot() {
                Some(s) => println!("active: {s}"),
                None => println!("active: none (no slot can boot)"),
            }
            for x in [Slot::A, Slot::B] {
                let image = match slot_image(&cli.partdir, x) {
                    Ok(f) => format!("version {}", f.version),
                    Err(e) => format!("no valid image ({e:#})"),
                };
                println!("{x}: {}, {image}", r.describe(x));
            }
            match slot::booted_slot(&cli.run_dir)? {
                Some(b) => println!(
                    "booted: {b} (version {})",
                    slot::booted_version(&cli.run_dir)?.unwrap_or_else(|| "?".into())
                ),
                None => println!("booted: unknown (not started through pinna-init)"),
            }
            // Which kernel runs, and whether updates can replace it (ADR 0019).
            match slot::booted_kernel(&cli.run_dir).ok().flatten() {
                Some(k) => println!("kernel: from boot_{k}"),
                None => println!("kernel: unknown (the boot image doesn't say which partition)"),
            }
            println!(
                "bootloader: {}",
                match slot::KernelSlots::read(&cli.kernel_slots) {
                    slot::KernelSlots::Honoured => "loads the kernel of the slot it starts",
                    slot::KernelSlots::Ignored =>
                        "loads boot_a whichever slot it starts: new kernels need fastboot",
                    slot::KernelSlots::Unknown =>
                        "not known yet whether it loads boot_b (a healthy boot from slot b shows it)",
                }
            );
        }
        Cmd::MarkGood => {
            // Booted by the kernel's root= (a boot image without pinna-init):
            // no slot to mark, and no reason to fail the health check.
            let Some(booted) = slot::booted_slot(&cli.run_dir)? else {
                println!(
                    "not started through pinna-init (no {}/boot-slot): no slot to mark",
                    cli.run_dir.display()
                );
                return Ok(());
            };
            let old = bcb::read(&cli.misc)?;
            let new = bcb::mark_good(old.unwrap_or(Record::KAERU_DEFAULT), booted);
            if old != Some(new) {
                bcb::write(&cli.misc, &new)?;
            }
            // The verdict, for pinnad's stage 7 on the ring (ADR 0012) and for
            // the next boot's "how the last boot ended".
            let _ = std::fs::write(cli.run_dir.join("boot-ok"), "1\n");
            // A healthy boot of slot b shows whether the bootloader loads
            // boot_b: the updater writes boot_a only once it has (ADR 0019).
            if let Some(seen) =
                slot::KernelSlots::seen(booted, slot::booted_kernel(&cli.run_dir).ok().flatten())
                && let Err(e) = seen.write(&cli.kernel_slots)
            {
                eprintln!("pinna-bootctl: {e:#}");
            }
            println!(
                "slot {booted} marked good (version {}); next boot: slot {}",
                slot::booted_version(&cli.run_dir)?.unwrap_or_else(|| "?".into()),
                new.next_boot()
                    .map_or_else(|| "none".to_owned(), |s| s.to_string())
            );
        }
        Cmd::SetActive {
            slot: s,
            attempts,
            force,
        } => {
            let s = Slot::parse(s)?;
            let version = if *force {
                "unchecked".to_owned()
            } else {
                slot_image(&cli.partdir, s)
                    .with_context(|| {
                        format!("slot {s} has no bootable image (--force skips this check)")
                    })?
                    .version
            };
            let old = bcb::read_or_default(&cli.misc)?;
            if slot::booted_slot(&cli.run_dir)? == Some(s) {
                // The running slot: undo a pending switch.
                bcb::write(&cli.misc, &bcb::keep_booted(old, s)?)?;
                println!("slot {s} (version {version}) keeps booting");
            } else {
                bcb::write(&cli.misc, &bcb::set_active(old, s, *attempts)?)?;
                println!(
                    "slot {s} (version {version}) boots next as a trial with {attempts} attempts; slot {} is the fallback",
                    s.other()
                );
            }
        }
    }
    Ok(())
}
