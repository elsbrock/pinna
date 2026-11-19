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
#[command(version = pinna_version::VERSION, about)]
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
    /// Manifest: http(s) URL, file:// URL or path. Its signature,
    /// manifest.json.sig, must be next to it (ADR 0022).
    manifest: String,
    /// Reboot into the new slot when installed.
    #[arg(long)]
    reboot: bool,
    /// Install as from this channel (stable or beta): only a newer version,
    /// and no beta on stable. Without it, an older version installs too.
    #[arg(long, value_parser = |s: &str| s.parse::<pinna_update::Channel>())]
    channel: Option<pinna_update::Channel>,
    /// Install only this version: the one a check found.
    #[arg(long)]
    expect_version: Option<String>,
    /// Only fetch the manifest and check its signature; print the result as
    /// one line of JSON and write nothing.
    #[arg(long, conflicts_with_all = ["reboot", "channel", "expect_version"])]
    check: bool,
    /// Boot attempts the new slot gets before rolling back.
    #[arg(long = "tries", default_value_t = pinna_bootctl::bcb::DEFAULT_ATTEMPTS,
          value_parser = clap::value_parser!(u8).range(1..=pinna_bootctl::bcb::MAX_ATTEMPTS as i64))]
    attempts: u8,
    #[arg(long, default_value = "/dev/disk/by-partlabel/misc", hide = true)]
    misc: std::path::PathBuf,
    #[arg(long, default_value = "/dev/disk/by-partlabel", hide = true)]
    partdir: std::path::PathBuf,
    #[arg(long, default_value = pinna_bootctl::slot::RUN_DIR, hide = true)]
    run_dir: std::path::PathBuf,
}

/// Blocking (no tokio): the download uses pinna-http's blocking client.
fn update(args: UpdateArgs) -> Result<()> {
    let model = pinna_hal::ident::board_model();
    let opts = pinna_update::Options {
        attempts: args.attempts,
        misc: args.misc,
        partdir: args.partdir,
        run_dir: args.run_dir,
        model: (!model.is_empty()).then(|| model.to_owned()),
        ..Default::default()
    };
    if args.check {
        return check(&opts, &args.manifest);
    }
    let rules = pinna_update::Rules {
        channel: args.channel,
        expect_version: args.expect_version,
    };
    let installed = pinna_update::install(&opts, &args.manifest, &rules, &mut |line| {
        println!("{line}")
    })?;
    if args.reboot {
        println!("rebooting into slot {}", installed.slot);
        // The next boot reports "restarted (update)" (ADR 0012). Best effort.
        let _ = pinna_bootctl::reboot::record_at(&opts.run_dir, "update");
        let status = std::process::Command::new("systemctl")
            .arg("reboot")
            .status()?;
        anyhow::ensure!(status.success(), "systemctl reboot failed");
    }
    Ok(())
}

/// `pinnactl update --check`: one line of JSON for pinnad (ADR 0022); exits
/// with 1 when the check failed.
fn check(opts: &pinna_update::Options, manifest: &str) -> Result<()> {
    let out = match pinna_update::check(opts, manifest) {
        Ok(c) => serde_json::json!({
            "ok": true,
            "url": c.fetched.url.as_str(),
            "manifest": c.fetched.manifest,
            "blocked": c.blocked.map(|b| serde_json::json!({
                "kind": b.kind(),
                "message": b.to_string(),
            })),
        }),
        Err(e) => {
            let (status, detail) = match &e {
                pinna_update::FetchError::Status { status, .. } => (Some(*status), String::new()),
                pinna_update::FetchError::NoAnswer { detail, .. }
                | pinna_update::FetchError::Unreadable { detail, .. } => (None, detail.clone()),
                _ => (None, String::new()),
            };
            println!(
                "{}",
                serde_json::json!({
                    "ok": false,
                    "kind": e.kind(),
                    "message": e.to_string(),
                    "status": status,
                    "detail": detail,
                })
            );
            std::process::exit(1);
        }
    };
    println!("{out}");
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
    if let Some(boot) = s.last_boot.as_option() {
        let _ = writeln!(out, "last start: {}", boot.summary);
    }
    let _ = writeln!(out, "phase:      {}", phase_name(s));
    let _ = writeln!(out, "muted:      {}", muted_text(s));
    let _ = writeln!(out, "volume:     {}%", s.volume);
    let standalone = s.mode.as_known() == Some(pinna_api::Mode::MODE_STANDALONE);
    if standalone {
        let assist = s.assistant.as_option().cloned().unwrap_or_default();
        let _ = writeln!(
            out,
            "mode:       standalone, {}",
            if assist.ready {
                "services ready"
            } else {
                "services not ready"
            }
        );
        if !assist.last_error.is_empty() {
            let _ = writeln!(out, "last failure: {}", assist.last_error);
        }
    }
    let _ = writeln!(
        out,
        "home assistant: {}{}",
        if s.ha_connected {
            "connected"
        } else {
            "not connected"
        },
        match (standalone, s.voice_subscribed) {
            // In standalone mode it has no voice to subscribe to.
            (true, _) => "",
            (false, true) => ", voice subscribed",
            (false, false) => ", voice not subscribed",
        }
    );
    let _ = writeln!(
        out,
        "wake word:  {} ({})",
        if wake.phrase.is_empty() {
            "none chosen"
        } else {
            &wake.phrase
        },
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
        assert!(out.contains("home assistant: not connected, voice not subscribed"));
        assert!(!out.contains("mode:"));
    }

    #[test]
    fn standalone_mode_shows_its_services_not_the_voice_subscription() {
        let s = Status {
            mode: pinna_api::Mode::MODE_STANDALONE.into(),
            assistant: buffa::MessageField::some(pinna_api::AssistantStatus {
                ready: true,
                last_error: "No credit is left on this account.".into(),
                ..Default::default()
            }),
            ..Default::default()
        };
        let out = render(&s);
        assert!(
            out.contains("mode:       standalone, services ready"),
            "{out}"
        );
        assert!(
            out.contains("last failure: No credit is left on this account."),
            "{out}"
        );
        assert!(out.contains("home assistant: not connected\n"), "{out}");
    }
}
