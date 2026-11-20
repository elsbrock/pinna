//! The Sendspin player (ADR 0027): while the switch is on, pinnad listens on TCP 8928 at
//! `/sendspin`, publishes a `_sendspin._tcp` record, and plays what a server such as Music
//! Assistant streams on the server's timeline, through a timed source in the media lane.
//!
//! [`worker`] starts the service and returns a [`Handle`] and a watch of its [`Report`]. One
//! task runs every connection's [`pinna_sendspin::Connection`], because a server that connects
//! while another is admitted is judged against it (ADR 0027 §3); each WebSocket has a task of
//! its own that only moves frames ([`socket`]). The player's keys, pairings and volume live in
//! `sendspin.toml` ([`saved`]).
//!
//! The Brain decides when the player is on and under which name, routes pause, play and stop
//! to the group while it plays ([`Handle::control`], §14), and sets the stream's level from
//! Home Assistant and the dashboard ([`Handle::set_volume`], §15). When pinnad exits, the
//! connections drop without a goodbye: the specification takes that for a restart, and the
//! server connects again.

mod clock;
mod lateness;
mod record;
mod saved;
mod service;
mod socket;
#[cfg(test)]
mod tests;

use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;

use pinna_audio::{Refused, ServerClock, Speaker, TimedSink, TimedState, TimedStats, Track};
use pinna_sendspin::DeviceInfo;
use tokio::sync::{mpsc, oneshot, watch};

pub use record::Mdns;

/// The port the specification recommends, and the one the record names.
pub const PORT: u16 = 8928;
/// Where systemd-resolved reads the record from.
pub const RECORD: &str = record::DNSSD;
/// The WebSocket endpoint.
pub const PATH: &str = "/sendspin";
/// The media lane's name for the source (ADR 0027 §13).
pub const SOURCE: &str = "sendspin";

/// How the player is switched on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Params {
    /// The satellite's friendly name. Servers list the player under it, without `%` and cut
    /// to 64 bytes.
    pub name: String,
}

/// What the player is doing, as the dashboard shows it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum State {
    #[default]
    Off,
    /// Listening; no server is admitted.
    Ready,
    /// A server is admitted: its name.
    Connected(String),
    /// A server's stream plays.
    Playing,
    /// Switched on, but not listening: why, e.g. "port 8928 is in use".
    Failed(String),
    /// No speaker: the switch is refused.
    Unavailable,
}

/// How well the player keeps time, for the metrics (ADR 0027 §11, ADR 0020). The counts add up
/// over every stream since pinnad started.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Stats {
    /// The latest error between when audio sounds and when the server's clock says it should,
    /// in µs, while a stream sounds.
    pub sync_error_us: Option<f64>,
    /// The standard deviation of the clock filter's offset, in µs, once it has a measurement.
    pub filter_error_us: Option<f64>,
    /// The stream has kept time for 10 s or more (the specification's steady state).
    pub steady: bool,
    /// Starts at the right sample: a stream's first, and every one after a clear, an underrun
    /// or a large error.
    pub jumps: u64,
    /// Chunks dropped because their audio was already past.
    pub late_drops: u64,
    /// Chunks dropped because they did not decode.
    pub decode_drops: u64,
}

/// The player as it is.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Report {
    pub state: State,
    /// The name servers list it under, while it is on.
    pub name: String,
    /// The admitted server's name, while one is.
    pub server: String,
    /// Why it failed, or, while it is Ready, why the last server was turned away: a short
    /// lower-case phrase without a full stop. Empty otherwise.
    pub reason: String,
    /// The speaker's hardware pointer times the audio, so it plays in step with its group.
    pub exact_timing: bool,
    /// The admitted server's group is paused: it holds the player role, nothing sounds, and the
    /// group stopped after it played here (ADR 0027 §14). The media state keeps the source
    /// `sendspin` meanwhile.
    pub group_paused: bool,
    /// The stream's own level, 0 to 100 (ADR 0027 §15).
    pub volume: u8,
    pub muted: bool,
    /// Where it listens, while it does.
    pub listening: Option<SocketAddr>,
    /// Refreshed every 5 s while a stream plays, and with every other change: a watcher that
    /// acts only on state changes compares the rest.
    pub stats: Stats,
}

impl Report {
    /// The player before the service runs: Off, or Unavailable without a speaker.
    pub fn idle(speaker: bool) -> Self {
        Self {
            state: if speaker {
                State::Off
            } else {
                State::Unavailable
            },
            volume: saved::VOLUME_DEFAULT,
            ..Self::default()
        }
    }
}

/// A command for the admitted server's group, through the controller role (ADR 0027 §14).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Control {
    Pause,
    Play,
    Stop,
}

/// Where the stream sounds: the speaker's media lane, or a test's stand-in.
pub(crate) trait Output: Send + Sync + 'static {
    /// A new timed source in the lane, on `clock`.
    fn begin(&self, clock: Arc<dyn ServerClock>) -> Box<dyn Sink>;
    /// What the stream plays; the default track clears it.
    fn set_track(&self, track: Track);
}

/// The service's end of a timed source: [`TimedSink`], or a test's stand-in.
pub(crate) trait Sink: Send {
    fn push(&self, server_ts_us: i64, rate: u32, pcm: Vec<i16>) -> Result<(), Refused>;
    fn clear(&self, from_us: Option<i64>);
    fn end(&self);
    fn set_gain(&self, gain: f32);
    fn state(&self) -> watch::Receiver<TimedState>;
    fn stats(&self) -> TimedStats;
}

impl Output for Speaker {
    fn begin(&self, clock: Arc<dyn ServerClock>) -> Box<dyn Sink> {
        Box::new(self.media().begin_timed(SOURCE, clock))
    }

    fn set_track(&self, track: Track) {
        self.set_stream_track(SOURCE, track);
    }
}

impl Sink for TimedSink {
    fn push(&self, server_ts_us: i64, rate: u32, pcm: Vec<i16>) -> Result<(), Refused> {
        TimedSink::push(self, server_ts_us, rate, pcm)
    }

    fn clear(&self, from_us: Option<i64>) {
        TimedSink::clear(self, from_us);
    }

    fn end(&self) {
        TimedSink::end(self);
    }

    fn set_gain(&self, gain: f32) {
        TimedSink::set_gain(self, gain);
    }

    fn state(&self) -> watch::Receiver<TimedState> {
        TimedSink::state(self)
    }

    fn stats(&self) -> TimedStats {
        TimedSink::stats(self)
    }
}

/// What the service runs with.
pub struct Setup {
    /// `None`: no speaker, so the player can't run (Unavailable).
    pub(crate) output: Option<Arc<dyn Output>>,
    /// The speaker's backend reads a hardware pointer (`Backend::echo_dot_mixer`): on PipeWire
    /// or a file the stream plays at the pacing clock, not in exact step (ADR 0027 §12).
    pub exact_backend: bool,
    /// `sendspin.toml`, beside the settings.
    pub state_file: PathBuf,
    /// What `client/hello` says about the device ([`device_info`]).
    pub device_info: DeviceInfo,
    /// Tried in order until one binds.
    pub listen: Vec<SocketAddr>,
    pub mdns: Mdns,
}

impl Setup {
    /// There is a speaker to play on: without one the switch is refused.
    pub fn has_speaker(&self) -> bool {
        self.output.is_some()
    }

    /// The player on `speaker`, listening on every address at [`PORT`] and publishing its
    /// record through systemd-resolved.
    pub fn new(
        speaker: Option<Speaker>,
        exact_backend: bool,
        state_file: PathBuf,
        device_info: DeviceInfo,
    ) -> Self {
        Self {
            output: speaker.map(|s| Arc::new(s) as Arc<dyn Output>),
            exact_backend,
            state_file,
            device_info,
            listen: every_address(PORT),
            mdns: Mdns::default(),
        }
    }

    /// Another port, and the record in another file: `pinnad.toml`'s `[sendspin]`, for a
    /// developer. The record names the port the player listens on.
    pub fn at(self, port: u16, record: PathBuf) -> Self {
        Self {
            listen: every_address(port),
            mdns: Mdns::at(record),
            ..self
        }
    }
}

/// `[::]` takes IPv4 too; `0.0.0.0` is for a host without IPv6.
fn every_address(port: u16) -> Vec<SocketAddr> {
    vec![
        SocketAddr::from((Ipv6Addr::UNSPECIFIED, port)),
        SocketAddr::from((Ipv4Addr::UNSPECIFIED, port)),
    ]
}

/// What `client/hello` says about the device: the model as Pinna names it (the host's name on
/// other hardware), Pinna, pinnad's version, and the Wi-Fi interface's MAC when it is known.
/// `model` is the dashboard's id (`pinna_hal::ident::board_model`).
pub fn device_info(model: &str, mac: Option<[u8; 6]>) -> DeviceInfo {
    let product = match model {
        "echo-dot-2" => "Echo Dot (2nd gen)".to_owned(),
        "echo-2" => "Echo (2nd gen)".to_owned(),
        _ => crate::network::hostname(),
    };
    let field = |text: &str| {
        let text = record::cut(text, 64);
        (!text.is_empty()).then(|| text.to_owned())
    };
    DeviceInfo {
        product_name: field(&product),
        manufacturer: Some("Pinna".into()),
        software_version: field(pinna_version::VERSION),
        mac_address: mac.map(|m| {
            m.iter()
                .map(|b| format!("{b:02x}"))
                .collect::<Vec<_>>()
                .join(":")
        }),
    }
}

enum Command {
    Enable(Option<Params>),
    Rename(String),
    Control(Control, oneshot::Sender<bool>),
    SetVolume(u8),
    SetMuted(bool),
    AnotherSource,
}

/// The Brain's end of the service. Commands are carried out in order.
#[derive(Clone)]
pub struct Handle {
    tx: mpsc::UnboundedSender<Command>,
}

impl Handle {
    /// On under `params.name`: listen, then publish the record. Off (`None`): the record goes
    /// first, then every connection ends with `client/goodbye` (`shutdown`), then the listener
    /// closes (ADR 0027 §2). Refused without a speaker. Off also removes a record left over
    /// from before a restart.
    pub fn enable(&self, params: Option<Params>) {
        let _ = self.tx.send(Command::Enable(params));
    }

    /// A new friendly name: the record is written again, then every connection ends with
    /// `client/goodbye` (`restart`), so the servers connect again and read the new name.
    pub fn rename(&self, name: &str) {
        let _ = self.tx.send(Command::Rename(name.to_owned()));
    }

    /// Pause, play or stop the admitted server's group (ADR 0027 §14). Whether it could be
    /// sent: the server must have admitted the controller role and list the command as one
    /// it accepts now.
    pub async fn control(&self, control: Control) -> bool {
        let (tx, rx) = oneshot::channel();
        if self.tx.send(Command::Control(control, tx)).is_err() {
            return false;
        }
        rx.await.unwrap_or(false)
    }

    /// The stream's own level, 0 to 100, from Home Assistant or the dashboard (ADR 0027 §15):
    /// applied with the specification's curve, kept, and reported to the server.
    pub fn set_volume(&self, volume: u8) {
        let _ = self.tx.send(Command::SetVolume(volume.min(100)));
    }

    pub fn set_muted(&self, muted: bool) {
        let _ = self.tx.send(Command::SetMuted(muted));
    }

    /// Another source took the lane while the group was paused: leave the group, which plays
    /// on without the satellite when it resumes (ADR 0027 §14).
    pub fn another_source(&self) {
        let _ = self.tx.send(Command::AnotherSource);
    }
}

/// Starts the service: Off, or Unavailable without a speaker, until [`Handle::enable`]. Must be
/// called inside a tokio runtime. Dropping every handle stops it without a goodbye.
pub fn worker(setup: Setup) -> (Handle, watch::Receiver<Report>) {
    let (tx, rx) = mpsc::unbounded_channel();
    let (report_tx, report_rx) = watch::channel(Report::idle(setup.has_speaker()));
    tokio::spawn(service::run(setup, rx, report_tx));
    (Handle { tx }, report_rx)
}

/// What the Brain asked of a [`fake`] handle.
#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Asked {
    Enable(Option<String>),
    Rename(String),
    Control(Control),
    Volume(u8),
    Muted(bool),
    AnotherSource,
}

/// A stand-in for the service, for the Brain's tests: [`Handle::control`] answers `controls`,
/// every command arrives in order as [`Asked`], and the report is the test's to set. Must be
/// called inside a tokio runtime.
#[cfg(test)]
pub(crate) fn fake(
    controls: bool,
) -> (
    Handle,
    watch::Sender<Report>,
    mpsc::UnboundedReceiver<Asked>,
) {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let (asked_tx, asked) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        while let Some(command) = rx.recv().await {
            let asked = match command {
                Command::Enable(params) => Asked::Enable(params.map(|p| p.name)),
                Command::Rename(name) => Asked::Rename(name),
                Command::Control(control, reply) => {
                    let _ = reply.send(controls);
                    Asked::Control(control)
                }
                Command::SetVolume(volume) => Asked::Volume(volume),
                Command::SetMuted(muted) => Asked::Muted(muted),
                Command::AnotherSource => Asked::AnotherSource,
            };
            if asked_tx.send(asked).is_err() {
                return;
            }
        }
    });
    let (report, _) = watch::channel(Report::idle(true));
    (Handle { tx }, report, asked)
}
