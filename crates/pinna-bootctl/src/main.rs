use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use pinna_bootctl::*;

#[derive(Parser)]
#[command(about = "Bootloader message control for the MT8163 Echo devices")]
struct Cli {
    #[arg(long, default_value = "/dev/disk/by-partlabel/misc")]
    misc: PathBuf,
    #[arg(long, default_value = "/dev/disk/by-partlabel/expdb")]
    expdb: PathBuf,
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
    /// Arm a one-shot "enter fastboot on next boot" (dev safety net).
    GuardArm,
    /// Clear our own one-shot request once the system is healthy.
    GuardClear,
    /// Arm and reboot into fastboot now.
    RebootBootloader,
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

fn run(cli: &Cli) -> anyhow::Result<()> {
    match cli.cmd {
        Cmd::Status => {
            let c = read_command(&cli.misc)?;
            println!("command: {c:?}");
            println!("kaeru: {}", kaeru_present(&cli.expdb)?);
        }
        Cmd::GuardArm => {
            if kaeru_ok(cli)? {
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
            let status = std::process::Command::new("systemctl").arg("reboot").status()?;
            anyhow::ensure!(status.success(), "systemctl reboot failed");
        }
    }
    Ok(())
}
