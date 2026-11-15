use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::Context;
use clap::{Parser, Subcommand};
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
    /// Show the A/B slot state, the images in the slots and the booted slot.
    SlotStatus,
    /// Mark the booted slot good (after the health check); a no-op without pinna-init.
    MarkGood,
    /// Boot a slot next, as a trial with a number of tries.
    SetActive {
        slot: String,
        #[arg(long, default_value_t = slot::DEFAULT_TRIES,
              value_parser = clap::value_parser!(u8).range(1..))]
        tries: u8,
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
    let path = partdir.join(s.partname());
    let dev = std::fs::File::open(&path).with_context(|| format!("opening {}", path.display()))?;
    let f = pinna_image::read_footer(&dev, &pinna_image::keys::release_key())?;
    pinna_image::check_top_hash(&dev, &f)?;
    Ok(f)
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
            // An unreadable slot state must not disable the safety net.
            let state = slot::read_state(&cli.misc).unwrap_or(None);
            if !slot::guard_should_arm(state, booted) {
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
            let state = slot::read_state(&cli.misc)?;
            let s = state.unwrap_or_else(slot::SlotState::implicit);
            match state {
                Some(_) => println!("state: valid"),
                None => println!("state: missing (first flash: slot a counts as good)"),
            }
            println!("active: {}", s.active);
            for x in [Slot::A, Slot::B] {
                let image = match slot_image(&cli.partdir, x) {
                    Ok(f) => format!("version {}", f.version),
                    Err(e) => format!("no valid image ({e:#})"),
                };
                println!("{x}: {}, {image}", s.describe(x));
            }
            match slot::booted_slot(&cli.run_dir)? {
                Some(b) => println!(
                    "booted: {b} (version {})",
                    slot::booted_version(&cli.run_dir)?.unwrap_or_else(|| "?".into())
                ),
                None => println!("booted: unknown (not started through pinna-init)"),
            }
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
            let old = slot::read_state(&cli.misc)?;
            let new = slot::mark_good(old, booted);
            if old != Some(new) {
                slot::write_state(&cli.misc, &new)?;
            }
            // The verdict, for pinnad's stage 7 on the ring (ADR 0012) and for
            // the next boot's "how the last boot ended".
            let _ = std::fs::write(cli.run_dir.join("boot-ok"), "1\n");
            println!(
                "slot {booted} marked good (version {}); active slot {}",
                slot::booted_version(&cli.run_dir)?.unwrap_or_else(|| "?".into()),
                new.active
            );
        }
        Cmd::SetActive {
            slot: s,
            tries,
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
            let new = slot::set_active(slot::read_state(&cli.misc)?, s, *tries);
            slot::write_state(&cli.misc, &new)?;
            println!(
                "slot {s} (version {version}) boots next as a trial with {tries} tries; slot {} is the fallback",
                s.other()
            );
        }
    }
    Ok(())
}
