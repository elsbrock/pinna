//! pinnactl — console client for pinnad's local control API.

use std::fmt::Write as _;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use connectrpc::client::{ClientConfig, HttpClient};
use pinna_api::{
    DeviceServiceClient, GetStatusRequest, LedTestRequest, Phase, SetMuteRequest, SetVolumeRequest,
    StartConversationRequest, Status, StopConversationRequest, WatchStatusRequest,
    WatchStatusResponse,
};

type Client = DeviceServiceClient<HttpClient>;

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    /// pinnad control API.
    #[arg(long, default_value = "http://127.0.0.1:7470", global = true)]
    api: String,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Show the device status.
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Set the speaker volume.
    Volume {
        #[arg(value_parser = clap::value_parser!(u8).range(0..=100))]
        percent: u8,
    },
    /// Switch the software microphone mute.
    Mute { state: Switch },
    /// Start a conversation, like pressing the action button.
    Listen,
    /// Stop the current conversation, announcement or timer alarm.
    Stop,
    /// Run a short LED animation cycle.
    LedTest,
    /// Print the status on every change.
    Watch {
        #[arg(long)]
        json: bool,
    },
    /// Install a release into the inactive A/B slot (as root, on the device).
    Update(UpdateArgs),
}

#[derive(clap::Args)]
struct UpdateArgs {
    /// Manifest: http(s) URL, file:// URL or path.
    manifest: String,
    /// Reboot into the new slot when installed.
    #[arg(long)]
    reboot: bool,
    /// Boot attempts the new slot gets before rolling back.
    #[arg(long, default_value_t = pinna_bootctl::slot::DEFAULT_TRIES,
          value_parser = clap::value_parser!(u8).range(1..))]
    tries: u8,
    #[arg(long, default_value = "/dev/disk/by-partlabel/misc", hide = true)]
    misc: std::path::PathBuf,
    #[arg(long, default_value = "/dev/disk/by-partlabel", hide = true)]
    partdir: std::path::PathBuf,
    #[arg(long, default_value = pinna_bootctl::slot::RUN_DIR, hide = true)]
    run_dir: std::path::PathBuf,
}

/// Blocking (no tokio): the download uses reqwest's blocking client.
fn update(args: UpdateArgs) -> Result<()> {
    let opts = pinna_update::Options {
        tries: args.tries,
        misc: args.misc,
        partdir: args.partdir,
        run_dir: args.run_dir,
        ..Default::default()
    };
    let installed = pinna_update::install(&opts, &args.manifest, &mut |line| println!("{line}"))?;
    if args.reboot {
        println!("rebooting into slot {}", installed.slot);
        let status = std::process::Command::new("systemctl")
            .arg("reboot")
            .status()?;
        anyhow::ensure!(status.success(), "systemctl reboot failed");
    }
    Ok(())
}

#[derive(Clone, Copy, ValueEnum)]
enum Switch {
    On,
    Off,
}

fn phase_name(s: &Status) -> &'static str {
    match s.phase.as_known() {
        Some(Phase::PHASE_IDLE) => "idle",
        Some(Phase::PHASE_LISTENING) => "listening",
        Some(Phase::PHASE_THINKING) => "thinking",
        Some(Phase::PHASE_SPEAKING) => "speaking",
        _ => "unknown",
    }
}

fn yes_no(b: bool) -> &'static str {
    if b { "yes" } else { "no" }
}

fn duration(secs: u64) -> String {
    let (h, m, s) = (secs / 3600, secs / 60 % 60, secs % 60);
    if h > 0 {
        format!("{h}h {m:02}m {s:02}s")
    } else if m > 0 {
        format!("{m}m {s:02}s")
    } else {
        format!("{s}s")
    }
}

fn muted_text(s: &Status) -> String {
    match (s.hardware_muted, s.software_muted) {
        (false, false) => "no".into(),
        (true, false) => "yes (hardware)".into(),
        (false, true) => "yes (software)".into(),
        (true, true) => "yes (hardware, software)".into(),
    }
}

fn render(s: &Status) -> String {
    let mut out = String::new();
    let wake = s.wake_word.as_option().cloned().unwrap_or_default();
    let _ = writeln!(out, "device:     {} ({})", s.name, s.mac);
    let _ = writeln!(out, "version:    {}", s.version);
    let _ = writeln!(out, "uptime:     {}", duration(s.uptime_seconds));
    let _ = writeln!(out, "phase:      {}", phase_name(s));
    let _ = writeln!(out, "muted:      {}", muted_text(s));
    let _ = writeln!(out, "volume:     {}%", s.volume);
    let _ = writeln!(
        out,
        "home assistant: {}, voice {}",
        if s.ha_connected {
            "connected"
        } else {
            "not connected"
        },
        if s.voice_subscribed {
            "subscribed"
        } else {
            "not subscribed"
        }
    );
    let _ = writeln!(
        out,
        "wake word:  \"{}\" ({})",
        wake.phrase,
        if wake.enabled { "armed" } else { "not armed" }
    );
    let _ = writeln!(out, "audio:      {}", s.audio_backend);
    let _ = writeln!(out, "leds:       {}", s.led_backend);
    if s.timers.is_empty() {
        let _ = writeln!(out, "timers:     none");
    } else {
        let _ = writeln!(out, "timers:");
        for t in &s.timers {
            let name = if t.name.is_empty() { &t.id } else { &t.name };
            let _ = writeln!(
                out,
                "  {name}: {} left of {}{}",
                duration(t.seconds_left.into()),
                duration(t.total_seconds.into()),
                if t.active { "" } else { " (paused)" }
            );
        }
    }
    out
}

/// One line for `watch`.
fn render_line(s: &Status) -> String {
    format!(
        "{:<9} volume {:>3}%  muted {}  ha {}  voice {}  timers {}",
        phase_name(s),
        s.volume,
        yes_no(s.muted),
        yes_no(s.ha_connected),
        yes_no(s.voice_subscribed),
        s.timers.len()
    )
}

fn print(s: &Status, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(s)?);
    } else {
        print!("{}", render(s));
    }
    Ok(())
}

async fn run(client: &Client, command: Command) -> Result<()> {
    match command {
        Command::Status { json } => {
            let resp = client.get_status(GetStatusRequest::default()).await?;
            let status = resp.into_view().to_owned_message().status.into_option();
            print(&status.unwrap_or_default(), json)?;
        }
        Command::Volume { percent } => {
            client
                .set_volume(SetVolumeRequest {
                    percent: percent.into(),
                    ..Default::default()
                })
                .await?;
            println!("volume {percent}%");
        }
        Command::Mute { state } => {
            let muted = matches!(state, Switch::On);
            client
                .set_mute(SetMuteRequest {
                    muted,
                    ..Default::default()
                })
                .await?;
            println!("software mute {}", if muted { "on" } else { "off" });
        }
        Command::Listen => {
            client
                .start_conversation(StartConversationRequest::default())
                .await?;
            println!("listening");
        }
        Command::Stop => {
            client
                .stop_conversation(StopConversationRequest::default())
                .await?;
            println!("stopped");
        }
        Command::LedTest => {
            client.led_test(LedTestRequest::default()).await?;
            println!("LED test started");
        }
        Command::Watch { json } => {
            let mut stream = client.watch_status(WatchStatusRequest::default()).await?;
            while let Some(msg) = stream.message::<WatchStatusResponse>().await? {
                let status = msg
                    .to_owned_message()
                    .status
                    .into_option()
                    .unwrap_or_default();
                if json {
                    println!("{}", serde_json::to_string(&status)?);
                } else {
                    println!("{}", render_line(&status));
                }
            }
        }
        Command::Update(_) => unreachable!("handled before the runtime starts"),
    }
    Ok(())
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    if let Command::Update(args) = cli.command {
        return update(args).context("update");
    }
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(api_main(cli))
}

async fn api_main(cli: Cli) -> Result<()> {
    let config = ClientConfig::new(
        cli.api
            .parse()
            .with_context(|| format!("invalid --api URL {:?}", cli.api))?,
    );
    let client = Client::new(HttpClient::plaintext(), config);
    run(&client, cli.command)
        .await
        .with_context(|| format!("pinnad API at {}", cli.api))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        assert_eq!(duration(5), "5s");
        assert_eq!(duration(65), "1m 05s");
        assert_eq!(duration(3725), "1h 02m 05s");
    }

    #[test]
    fn renders_defaults() {
        let out = render(&Status::default());
        assert!(out.contains("muted:      no"));
        assert!(out.contains("timers:     none"));
    }
}
