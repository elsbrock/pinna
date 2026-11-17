//! pinna-bt — bridges BlueZ and BlueALSA to pinnad (ADR 0011).
//!
//! pinnad keeps the switch and the pairing window; this bridge carries them
//! out. It names the adapter after the device, makes it discoverable and
//! pairable only while the window is open, accepts pairings only then, trusts
//! a phone once it paired, and reports the paired phones. A phone's A2DP audio
//! (BlueALSA's PCM) becomes a session on pinnad's stream socket, and the
//! phone's volume sets the stream volume.

mod agent;
mod audio;
mod objects;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use clap::Parser;
use pinna_api::{BluetoothDevice, GetStatusRequest, ReportBluetoothRequest};
use pinna_bridge::{API_TIMEOUT, Api};
use zbus::Connection;
use zbus::zvariant::{ObjectPath, OwnedFd, Value};

use objects::{ADAPTER, Objects};

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    /// pinnad's stream socket.
    #[arg(long, default_value = "/run/pinna/stream.sock")]
    socket: PathBuf,
    /// pinnad control API.
    #[arg(long, default_value = "http://127.0.0.1:7470")]
    api: String,
}

/// How often the bridge looks at BlueZ, BlueALSA and pinnad.
const TICK: Duration = Duration::from_millis(500);
/// A report that hasn't changed is sent again this often, for a pinnad that
/// restarted.
const REPORT_AGAIN: Duration = Duration::from_secs(15);
/// Without an adapter this long after start, the bridge reports why.
const ADAPTER_GRACE: Duration = Duration::from_secs(30);

const BLUEZ: &str = "org.bluez";
const BLUEALSA: &str = "org.bluealsa";

async fn managed_objects(conn: &Connection, service: &str, path: &str) -> zbus::Result<Objects> {
    let reply = conn
        .call_method(
            Some(service),
            path,
            Some("org.freedesktop.DBus.ObjectManager"),
            "GetManagedObjects",
            &(),
        )
        .await?;
    reply.body().deserialize()
}

async fn set_property(
    conn: &Connection,
    path: &str,
    iface: &str,
    name: &str,
    value: Value<'_>,
) -> zbus::Result<()> {
    conn.call_method(
        Some(BLUEZ),
        path,
        Some("org.freedesktop.DBus.Properties"),
        "Set",
        &(iface, name, value),
    )
    .await?;
    Ok(())
}

async fn name_owner(conn: &Connection, name: &str) -> Option<String> {
    let reply = conn
        .call_method(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            Some("org.freedesktop.DBus"),
            "GetNameOwner",
            &name,
        )
        .await
        .ok()?;
    reply.body().deserialize::<String>().ok()
}

async fn register_agent(conn: &Connection) -> zbus::Result<()> {
    let path = ObjectPath::try_from(agent::PATH)?;
    let manager = Some("org.bluez.AgentManager1");
    conn.call_method(
        Some(BLUEZ),
        "/org/bluez",
        manager,
        "RegisterAgent",
        &(&path, agent::CAPABILITY),
    )
    .await?;
    conn.call_method(
        Some(BLUEZ),
        "/org/bluez",
        manager,
        "RequestDefaultAgent",
        &path,
    )
    .await?;
    Ok(())
}

/// Opens a phone's PCM: its audio pipe and its control socket.
async fn open_pcm(conn: &Connection, path: &str) -> zbus::Result<(OwnedFd, OwnedFd)> {
    let reply = conn
        .call_method(Some(BLUEALSA), path, Some("org.bluealsa.PCM1"), "Open", &())
        .await?;
    reply.body().deserialize()
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// What pinnad last said: the device's name, the pairing window, and
/// whether it already counts Bluetooth as ready.
#[derive(Default)]
struct Wanted {
    name: String,
    pairing_until_ms: u64,
    ready: bool,
}

async fn wanted(api: &Api) -> Option<Wanted> {
    let call = api.0.get_status(GetStatusRequest::default());
    let status = tokio::time::timeout(API_TIMEOUT, call).await.ok()?.ok()?;
    let bt = status
        .into_view()
        .to_owned_message()
        .status
        .into_option()?
        .bluetooth
        .into_option()?;
    Some(Wanted {
        ready: bt.state
            == buffa::EnumValue::Known(pinna_api::BluetoothState::BLUETOOTH_STATE_READY),
        name: bt.name,
        pairing_until_ms: bt.pairing_until_ms,
    })
}

struct Bridge {
    conn: Connection,
    api: Api,
    audio: tokio::sync::mpsc::UnboundedSender<audio::Command>,
    window: Arc<AtomicBool>,
    started: Instant,
    /// bluetoothd's bus name owner the agent is registered with.
    agent_owner: Option<String>,
    /// The window as last applied to the adapter.
    applied_window: Option<bool>,
    /// When pinnad last said the window closes: it closes then even if
    /// pinnad stops answering.
    pairing_until_ms: u64,
    /// The PCM being read, and the volume and track last sent.
    pcm: Option<String>,
    volume: Option<u8>,
    track: pinna_bridge::Track,
    last_report: Option<(ReportBluetoothRequest, Instant)>,
    /// Logged once until it changes.
    logged: HashMap<&'static str, String>,
}

impl Bridge {
    fn note(&mut self, key: &'static str, msg: String) {
        if self.logged.get(key) != Some(&msg) {
            tracing::warn!("{msg}");
            self.logged.insert(key, msg);
        }
    }

    async fn tick(&mut self) {
        let mut error = String::new();
        let owner = name_owner(&self.conn, BLUEZ).await;
        if owner.is_some() && owner != self.agent_owner {
            match register_agent(&self.conn).await {
                Ok(()) => {
                    tracing::info!("pairing agent registered");
                    self.agent_owner = owner.clone();
                    self.applied_window = None;
                }
                Err(e) => self.note("agent", format!("registering the pairing agent: {e}")),
            }
        }
        let bluez = managed_objects(&self.conn, BLUEZ, "/")
            .await
            .unwrap_or_default();
        let adapter = objects::adapter(&bluez);
        let wanted = wanted(&self.api).await;
        if let Some(w) = &wanted {
            self.pairing_until_ms = w.pairing_until_ms;
        }
        let open = self.pairing_until_ms > now_ms();
        self.window.store(open, Ordering::Relaxed);

        if let Some(a) = &adapter {
            if let Some(w) = &wanted
                && !w.name.is_empty()
                && a.alias != w.name
            {
                tracing::info!("named {:?}", w.name);
                if let Err(e) = set_property(
                    &self.conn,
                    ADAPTER,
                    "org.bluez.Adapter1",
                    "Alias",
                    Value::from(w.name.as_str()),
                )
                .await
                {
                    self.note("alias", format!("naming the adapter: {e}"));
                }
            }
            let drifted = a.powered && (a.discoverable != open || a.pairable != open);
            if self.applied_window != Some(open) || drifted {
                self.apply_window(open).await;
            }
            for d in objects::devices(&bluez) {
                // Bonded inside the window only: a phone paired then.
                if open && !d.trusted {
                    // Paired just now: it reconnects later without asking.
                    tracing::info!("paired with {} ({})", d.name, d.address);
                    if let Err(e) = set_property(
                        &self.conn,
                        &d.path,
                        "org.bluez.Device1",
                        "Trusted",
                        Value::from(true),
                    )
                    .await
                    {
                        self.note("trust", format!("trusting {}: {e}", d.address));
                    }
                }
            }
        } else if owner.is_none() {
            error = "bluetoothd isn't running".into();
        } else {
            error = "the Bluetooth adapter didn't power up".into();
        }

        let alsa = managed_objects(&self.conn, BLUEALSA, "/org/bluealsa").await;
        if alsa.is_err() && error.is_empty() {
            error = "BlueALSA isn't running".into();
        }
        self.follow_pcm(&alsa.unwrap_or_default()).await;
        let track = self
            .pcm
            .as_deref()
            .map(|p| objects::track(&bluez, p))
            .unwrap_or_default();
        if track != self.track {
            self.track = track.clone();
            let _ = self.audio.send(audio::Command::Track(track));
        }

        let ready = error.is_empty()
            && adapter.as_ref().is_some_and(|a| a.powered)
            && self.agent_owner.is_some();
        if !ready && self.started.elapsed() < ADAPTER_GRACE {
            // Still starting: bluetoothd powers the adapter up.
            error.clear();
        }
        let report = ReportBluetoothRequest {
            ready,
            address: adapter
                .as_ref()
                .map(|a| a.address.clone())
                .unwrap_or_default(),
            devices: objects::devices(&bluez)
                .into_iter()
                .map(|d| BluetoothDevice {
                    address: d.address,
                    name: d.name,
                    connected: d.connected,
                    ..Default::default()
                })
                .collect(),
            error,
            ..Default::default()
        };
        // pinnad restarted and doesn't know yet: tell it now.
        let resend = report.ready && wanted.as_ref().is_some_and(|w| !w.ready);
        self.report(report, resend).await;
    }

    async fn apply_window(&mut self, open: bool) {
        tracing::info!(
            "pairing window {}",
            if open {
                "open: discoverable and pairable"
            } else {
                "closed"
            }
        );
        let iface = "org.bluez.Adapter1";
        let mut ok = true;
        for (name, value) in [
            ("DiscoverableTimeout", Value::from(0u32)),
            ("PairableTimeout", Value::from(0u32)),
            ("Pairable", Value::from(open)),
            ("Discoverable", Value::from(open)),
        ] {
            if let Err(e) = set_property(&self.conn, ADAPTER, iface, name, value).await {
                self.note("window", format!("setting {name}: {e}"));
                ok = false;
            }
        }
        if ok {
            self.applied_window = Some(open);
        }
    }

    /// Reads the newest phone's PCM; follows its volume.
    async fn follow_pcm(&mut self, alsa: &Objects) {
        let pcms = objects::pcms(alsa);
        let newest = pcms
            .iter()
            .rev()
            .find(|p| objects::stream_rate(p).is_some());
        if let Some(cur) = &self.pcm
            && !pcms.iter().any(|p| &p.path == cur)
        {
            self.pcm = None;
            let _ = self.audio.send(audio::Command::Closed);
        }
        let Some(p) = newest else {
            for p in &pcms {
                self.note(
                    "format",
                    format!(
                        "{}: format {:#06x}, {} channel(s), {} Hz isn't playable",
                        p.path, p.format, p.channels, p.sampling
                    ),
                );
            }
            return;
        };
        let percent = objects::volume_percent(p.volume);
        if self.volume != Some(percent) {
            self.volume = Some(percent);
            let _ = self.audio.send(audio::Command::Volume(percent));
        }
        if self.pcm.as_deref() == Some(p.path.as_str()) {
            return;
        }
        match open_pcm(&self.conn, &p.path).await {
            Ok((pcm, ctrl)) => {
                self.pcm = Some(p.path.clone());
                let _ = self.audio.send(audio::Command::Open {
                    path: p.path.clone(),
                    pcm: pcm.into(),
                    ctrl: ctrl.into(),
                    rate: objects::stream_rate(p).unwrap_or(44100),
                });
            }
            Err(e) => self.note("open", format!("opening {}: {e}", p.path)),
        }
    }

    async fn report(&mut self, report: ReportBluetoothRequest, resend: bool) {
        let due = match &self.last_report {
            Some((last, at)) => resend || *last != report || at.elapsed() >= REPORT_AGAIN,
            None => true,
        };
        if !due {
            return;
        }
        let call = self.api.0.report_bluetooth(report.clone());
        match tokio::time::timeout(API_TIMEOUT, call).await {
            Ok(Ok(_)) => self.last_report = Some((report, Instant::now())),
            Ok(Err(e)) => self.note("report", format!("reporting to pinnad: {e}")),
            Err(_) => self.note("report", "reporting to pinnad: no answer".into()),
        }
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    pinna_bridge::init_logging();
    let cli = Cli::parse();

    let conn = Connection::system().await.context("system bus")?;
    let window = Arc::new(AtomicBool::new(false));
    conn.object_server()
        .at(
            agent::PATH,
            agent::Agent {
                window: window.clone(),
            },
        )
        .await
        .context("export the pairing agent")?;

    let mut bridge = Bridge {
        conn,
        api: Api::new(&cli.api)?,
        audio: audio::spawn(cli.socket, Api::new(&cli.api)?),
        window,
        started: Instant::now(),
        agent_owner: None,
        applied_window: None,
        pairing_until_ms: 0,
        pcm: None,
        volume: None,
        track: pinna_bridge::Track::default(),
        last_report: None,
        logged: HashMap::new(),
    };
    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = tick.tick() => bridge.tick().await,
            _ = tokio::signal::ctrl_c() => break,
        }
    }
    Ok(())
}
