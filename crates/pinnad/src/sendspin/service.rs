//! The service's task (ADR 0027 §2–§15): the listener's life and the record, every
//! connection, the stream's timed source in the media lane, the player's state as servers hear
//! it, and the report.
//!
//! Everything a connection does happens here, between two awaits: a newcomer's activation is
//! judged against the admitted connection as it is at that moment (§3), as `pinna_sendspin`
//! requires. Nothing here waits on the speaker: audio is decoded and queued in the timed
//! source, which the speaker's own task drains.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use pinna_audio::chunk::{self, ChunkDecoder};
use pinna_audio::{EndReason, TimedState, Track};
use pinna_sendspin::messages::PlaybackState;
use pinna_sendspin::{
    ActiveRoles, AudioChunk, AudioFormat, CloseReason, Codec, Config, Connection,
    ControllerCommand, DeviceInfo, Event, GoodbyeReason, GroupUpdate, Metadata, PlayerCommandKind,
    PlayerState, PlayerStream, Role, Transmit,
};
use tokio::sync::{mpsc, watch};
use tokio::time::Instant;

use super::clock::{FilterClock, monotonic_us};
use super::lateness::Lateness;
use super::record::{self, Mdns};
use super::saved::{self, Keys, Saved, Saver};
use super::socket::{self, BindError, FromSocket, Listener, Shared, ToSocket};
use super::{Command, Control, Output, Report, Setup, Sink, State, Stats};

/// In priority order (ADR 0027 §8).
const ROLES: [Role; 3] = [Role::Player, Role::Controller, Role::Metadata];
/// About 5 s of PCM, 10 s of FLAC (ADR 0027 §8).
const BUFFER_CAPACITY: u32 = 1 << 20;
/// The speaker's 100 ms lead, aplay's 128 ms buffer and decoding, until measured (ADR 0027
/// §11). It also covers what plays on after a `stream/clear`, as the specification requires.
const REQUIRED_LEAD_MS: u32 = 300;
/// Half the airtime of PCM on a radio shared with Bluetooth (ADR 0027 §8).
const PREFERRED: AudioFormat = format(Codec::Flac, 48_000);
/// How often a listener that failed is tried again, while the player is on.
const RETRY: Duration = Duration::from_secs(5);
/// How often the report's stats are refreshed while a stream plays.
const STATS_EVERY: Duration = Duration::from_secs(5);
/// Messages from the sockets waiting for the service. A socket task that finds the queue full
/// waits, and the server's sending waits with it; each message keeps its receive time.
const QUEUE: usize = 64;
/// While Ready, after a server closed before `server/init`, as a server of the older
/// unencrypted wire format does (ADR 0027 §2).
const TOO_OLD: &str = "Music Assistant must be 2.10.7 or later";

const fn format(codec: Codec, sample_rate: u32) -> AudioFormat {
    AudioFormat {
        codec,
        channels: 2,
        sample_rate,
        bit_depth: 16,
    }
}

/// FLAC, then PCM, each at 48 kHz, then at 44.1 kHz (ADR 0027 §8).
fn formats() -> Vec<AudioFormat> {
    vec![
        PREFERRED,
        format(Codec::Pcm, 48_000),
        format(Codec::Flac, 44_100),
        format(Codec::Pcm, 44_100),
    ]
}

/// The specification's volume curve; muted is silent.
fn gain(volume: u8, muted: bool) -> f32 {
    if muted {
        0.0
    } else {
        (f32::from(volume.min(100)) / 100.0).powf(1.5)
    }
}

/// A server's text as the report shows it: without control characters, at most 64 bytes. The
/// log quotes it with `{:?}` on top.
fn display_name(name: &str) -> String {
    let name: String = name.chars().filter(|c| !c.is_control()).collect();
    record::cut(&name, 64).to_owned()
}

/// What the metadata role says is playing, for the dashboard.
fn track(metadata: Option<Metadata>) -> Track {
    let Some(m) = metadata else {
        return Track::default();
    };
    let text = |t: Option<String>| {
        t.map(|t| t.chars().filter(|c| !c.is_control()).collect())
            .unwrap_or_default()
    };
    Track {
        title: text(m.title),
        artist: text(m.artist),
        album: text(m.album),
        station: String::new(),
    }
}

/// A decoder for the format a `stream/start` set.
fn decoder(start: &PlayerStream) -> Result<ChunkDecoder, String> {
    let f = start.format;
    let codec = match f.codec {
        Codec::Flac => chunk::Codec::Flac,
        Codec::Pcm => chunk::Codec::Pcm,
        Codec::Opus => return Err("Opus is not offered".into()),
    };
    ChunkDecoder::new(
        codec,
        f.sample_rate,
        f.channels,
        f.bit_depth,
        start.codec_header.clone(),
    )
    .map_err(|e| e.to_string())
}

/// One WebSocket and its connection.
struct Peer {
    id: u64,
    conn: Connection,
    out: mpsc::UnboundedSender<ToSocket>,
    /// Until the socket task says it closed.
    socket_open: bool,
    /// The connection's time filter, for its stream's timed source.
    clock: Arc<FilterClock>,
    /// The server's name, once it introduced itself.
    name: String,
    roles: ActiveRoles,
    group: Option<GroupUpdate>,
    /// The group this satellite paused through the controller role, until the group plays
    /// again or the satellite stops or leaves it. A group reports `stopped` whether it paused,
    /// stopped or ran out of queue, so only a pause sent from here shows it Paused (ADR 0027
    /// §14).
    paused_group: Option<String>,
}

/// A player stream, from its `stream/start` to its end.
struct Stream {
    peer: u64,
    /// `None` when the format can't be decoded: its audio is dropped and counted.
    decoder: Option<ChunkDecoder>,
    /// A decode error was logged for this stream.
    warned: bool,
}

/// The stream's timed source in the media lane, until it ends.
struct Sounding {
    peer: u64,
    sink: Box<dyn Sink>,
    state: watch::Receiver<TimedState>,
}

struct Service {
    output: Option<Arc<dyn Output>>,
    exact_backend: bool,
    device_info: DeviceInfo,
    listen: Vec<SocketAddr>,
    mdns: Mdns,
    saver: Saver,
    report: watch::Sender<Report>,
    shared: Arc<Shared>,
    from_sockets: mpsc::Receiver<FromSocket>,
    /// Made the first time the player is switched on.
    keys: Option<Keys>,
    volume: u8,
    muted: bool,
    /// The name servers list the player under, while it is on.
    name: Option<String>,
    listener: Option<Listener>,
    /// Why it isn't listening although it is on.
    failure: Option<String>,
    retry_at: Option<Instant>,
    /// Why the last server was turned away, until one is admitted.
    turned_away: Option<&'static str>,
    peers: Vec<Peer>,
    stream: Option<Stream>,
    sink: Option<Sounding>,
    lateness: Lateness,
    stats: Stats,
    /// The sink's stats say its timing comes from the hardware pointer, or there is none.
    exact: bool,
    stats_at: Option<Instant>,
    /// Counts of the timed sources that ended.
    ended_jumps: u64,
    ended_late_drops: u64,
    decode_drops: u64,
}

pub(super) async fn run(
    setup: Setup,
    mut commands: mpsc::UnboundedReceiver<Command>,
    report: watch::Sender<Report>,
) {
    let (to_service, from_sockets) = mpsc::channel(QUEUE);
    let path = setup.state_file.clone();
    let loaded = tokio::task::spawn_blocking(move || saved::load(&path))
        .await
        .ok()
        .flatten();
    let mut s = Service {
        output: setup.output,
        exact_backend: setup.exact_backend,
        device_info: setup.device_info,
        listen: setup.listen,
        mdns: setup.mdns,
        saver: Saver::new(setup.state_file),
        report,
        shared: Shared::new(to_service),
        from_sockets,
        volume: loaded.as_ref().map_or(saved::VOLUME_DEFAULT, |l| l.volume),
        muted: loaded.as_ref().is_some_and(|l| l.muted),
        keys: loaded.map(|l| l.keys),
        name: None,
        listener: None,
        failure: None,
        retry_at: None,
        turned_away: None,
        peers: Vec::new(),
        stream: None,
        sink: None,
        lateness: Lateness::default(),
        stats: Stats::default(),
        exact: true,
        stats_at: None,
        ended_jumps: 0,
        ended_late_drops: 0,
        decode_drops: 0,
    };
    s.publish();
    loop {
        let wake = s.next_wakeup();
        tokio::select! {
            command = commands.recv() => match command {
                Some(command) => s.command(command).await,
                // pinnad is going away. No goodbye: a connection dropped without one is a
                // restart, and the server connects again.
                None => return,
            },
            Some(event) = s.from_sockets.recv() => s.socket(event),
            () = tokio::time::sleep_until(wake) => s.tick().await,
            () = sink_changes(&mut s.sink) => s.sink_changed(),
        }
        s.pump();
        s.publish();
    }
}

/// Resolves when the sink's state changes; never without a sink.
async fn sink_changes(sink: &mut Option<Sounding>) {
    if let Some(s) = sink
        && s.state.changed().await.is_ok()
    {
        return;
    }
    // No sink; or none that can change: the sink holds the sender.
    std::future::pending().await
}

impl Service {
    fn next_wakeup(&self) -> Instant {
        let now = Instant::now();
        let now_us = monotonic_us();
        let connections = self
            .peers
            .iter()
            .filter_map(|p| p.conn.next_wakeup())
            .min()
            .map(|at| {
                now + Duration::from_micros(u64::try_from(at.saturating_sub(now_us)).unwrap_or(0))
            });
        [connections, self.retry_at, self.stats_at]
            .into_iter()
            .flatten()
            .min()
            .unwrap_or(now + Duration::from_secs(3600))
    }

    async fn tick(&mut self) {
        let now = Instant::now();
        let now_us = monotonic_us();
        for i in 0..self.peers.len() {
            self.set_rival(i);
            self.peers[i].conn.on_tick(now_us);
        }
        if self.retry_at.is_some_and(|at| at <= now) {
            self.retry_at = None;
            self.start().await;
        }
        if self.stats_at.is_some_and(|at| at <= now) {
            self.stats_at = self.sink.as_ref().map(|_| now + STATS_EVERY);
            self.refresh_stats();
        }
    }

    // Commands.

    async fn command(&mut self, command: Command) {
        match command {
            Command::Enable(Some(params)) => self.on(&params.name).await,
            Command::Enable(None) => self.off().await,
            Command::Rename(name) => {
                if self.name.is_some() {
                    self.rename(record::player_name(&name)).await;
                }
            }
            Command::Control(control, reply) => {
                let _ = reply.send(self.control(control));
            }
            Command::SetVolume(volume) => {
                self.volume = volume.min(100);
                self.level_changed();
            }
            Command::SetMuted(muted) => {
                self.muted = muted;
                self.level_changed();
            }
            Command::AnotherSource => self.leave_group(),
        }
    }

    async fn on(&mut self, friendly: &str) {
        if self.output.is_none() {
            tracing::warn!("sendspin: no speaker, so it stays off");
            return;
        }
        let name = record::player_name(friendly);
        match &self.name {
            Some(current) if *current == name => {}
            Some(_) => self.rename(name).await,
            None => {
                self.name = Some(name);
                self.start().await;
            }
        }
    }

    /// Keys first, then the listener, then the record: a server that finds the record can
    /// connect, and learns an identity that is already kept.
    async fn start(&mut self) {
        let Some(name) = self.name.clone() else {
            return;
        };
        if self.keys.is_none() {
            let made = match Keys::generate() {
                Ok(keys) => self.saver.save_now(self.saved(&keys)).await.map(|()| keys),
                Err(e) => Err(e),
            };
            match made {
                Ok(keys) => self.keys = Some(keys),
                Err(e) => return self.fail("couldn't save the player's keys", &format!("{e:#}")),
            }
        }
        match socket::listen(&self.listen, self.shared.clone()).await {
            Ok(listener) => {
                let port = listener.addr().port();
                self.listener = Some(listener);
                self.failure = None;
                publish_record(self.mdns.clone(), name, port).await;
            }
            Err(e @ BindError::InUse(_)) => self.fail(&e.to_string(), ""),
            Err(BindError::Other { port, error }) => self.fail(
                &format!("couldn't listen on port {port}"),
                &error.to_string(),
            ),
        }
    }

    /// Not listening although on: `reason` for the status, `detail` for the log, which has it
    /// once, not at every try.
    fn fail(&mut self, reason: &str, detail: &str) {
        if self.failure.as_deref() != Some(reason) && !detail.is_empty() {
            tracing::warn!("sendspin: {reason}: {detail}");
        }
        self.failure = Some(reason.to_owned());
        self.retry_at = Some(Instant::now() + RETRY);
    }

    /// The record goes first, so servers stop listing the player at once; then each
    /// connection says goodbye; then the listener closes (ADR 0027 §2). The record goes even
    /// when the player is already off: one may be left over from before a restart.
    async fn off(&mut self) {
        let mdns = self.mdns.clone();
        let _ = tokio::task::spawn_blocking(move || mdns.withdraw()).await;
        if self.name.take().is_none() {
            return;
        }
        for p in &mut self.peers {
            p.conn.goodbye(GoodbyeReason::Shutdown);
        }
        self.pump();
        self.listener = None;
        self.failure = None;
        self.retry_at = None;
        self.turned_away = None;
    }

    /// The record first, then a goodbye that has the servers connect again and read the new
    /// name (ADR 0027 §2).
    async fn rename(&mut self, name: String) {
        if self.name.as_deref() == Some(name.as_str()) {
            return;
        }
        self.name = Some(name.clone());
        if let Some(listener) = &self.listener {
            let port = listener.addr().port();
            publish_record(self.mdns.clone(), name, port).await;
        }
        for p in &mut self.peers {
            p.conn.goodbye(GoodbyeReason::Restart);
        }
    }

    fn control(&mut self, control: Control) -> bool {
        let command = match control {
            Control::Pause => ControllerCommand::Pause,
            Control::Play => ControllerCommand::Play,
            Control::Stop => ControllerCommand::Stop,
        };
        let Some(i) = self.admitted() else {
            return false;
        };
        let p = &mut self.peers[i];
        if p.conn.command(command).is_err() {
            return false;
        }
        match control {
            Control::Pause => p.paused_group = p.group.as_ref().map(|g| g.group_id.clone()),
            // A group stopped from here is not waiting to resume (ADR 0027 §14).
            Control::Stop => p.paused_group = None,
            // Paused until the group says it plays.
            Control::Play => {}
        }
        true
    }

    /// Another source took the lane while the group waited paused from here: the group plays
    /// on without the satellite when it resumes (ADR 0027 §14).
    fn leave_group(&mut self) {
        if let Some(i) = self.admitted() {
            let p = &mut self.peers[i];
            if p.conn.leave().is_ok() {
                tracing::info!("sendspin: another source plays; left {:?}'s group", p.name);
            }
            p.paused_group = None;
        }
    }

    /// The stream's level changed, here or at the server: the sink follows, the file keeps
    /// it, and every server hears it.
    fn level_changed(&mut self) {
        if let Some(s) = &self.sink {
            s.sink.set_gain(gain(self.volume, self.muted));
        }
        if let Some(keys) = &self.keys {
            self.saver.save(self.saved(keys));
        }
        self.send_player_state();
    }

    fn saved(&self, keys: &Keys) -> Saved {
        Saved {
            volume: self.volume,
            muted: self.muted,
            keys: keys.clone(),
        }
    }

    fn save_keys(&self) {
        if let Some(keys) = &self.keys {
            self.saver.save(self.saved(keys));
        }
    }

    fn player_state(&self) -> PlayerState {
        PlayerState {
            available: true,
            volume: self.volume,
            muted: self.muted,
            required_lead_time_ms: REQUIRED_LEAD_MS,
            min_buffer_ms: self.lateness.reported_ms(),
            format: Some(PREFERRED),
            // No set_output_delay: the speaker is built in (ADR 0027 §15).
            supported_commands: vec![PlayerCommandKind::Volume, PlayerCommandKind::Mute],
        }
    }

    /// Each connection sends `client/state` once it may, and only when something changed.
    fn send_player_state(&mut self) {
        let state = self.player_state();
        for p in &mut self.peers {
            let _ = p.conn.set_player_state(state.clone());
        }
    }

    // Sockets.

    fn index(&self, id: u64) -> Option<usize> {
        self.peers.iter().position(|p| p.id == id)
    }

    /// The admitted connection: activated and open. There is at most one.
    fn admitted(&self) -> Option<usize> {
        self.peers
            .iter()
            .position(|p| p.conn.is_activated() && !p.conn.is_closed())
    }

    /// A connection not yet activated is judged against the admitted one as it is now, and
    /// the last server that played (ADR 0027 §3).
    fn set_rival(&mut self, i: usize) {
        if self.peers[i].conn.is_activated() {
            return;
        }
        let id = self.peers[i].id;
        let rival = self
            .peers
            .iter()
            .filter(|p| p.id != id)
            .find_map(|p| p.conn.as_rival());
        let last = self
            .keys
            .as_ref()
            .and_then(|k| k.pairing.last_playback_server())
            .map(str::to_owned);
        self.peers[i].conn.set_rival(rival, last.as_deref());
    }

    fn socket(&mut self, event: FromSocket) {
        match event {
            FromSocket::Opened { id, out } => self.opened(id, out),
            FromSocket::Text { id, text, at } => {
                if let Some(i) = self.index(id) {
                    self.set_rival(i);
                    self.peers[i].conn.on_text(&text, at);
                }
            }
            FromSocket::Binary { id, bytes, at } => {
                if let Some(i) = self.index(id) {
                    self.set_rival(i);
                    self.peers[i].conn.on_binary(&bytes, at);
                }
            }
            FromSocket::Closed { id } => {
                if let Some(i) = self.index(id) {
                    self.peers[i].socket_open = false;
                    self.peers[i].conn.on_peer_closed();
                }
            }
        }
    }

    fn opened(&mut self, id: u64, out: mpsc::UnboundedSender<ToSocket>) {
        // A socket that opened as the player went off: dropping `out` closes it.
        let (Some(name), Some(keys)) = (&self.name, &self.keys) else {
            return;
        };
        let config = Config {
            identity: keys.identity.clone(),
            name: name.clone(),
            device_info: Some(self.device_info.clone()),
            roles: ROLES.to_vec(),
            formats: formats(),
            buffer_capacity: BUFFER_CAPACITY,
            unpaired_access: true,
        };
        let mut conn = match Connection::new(config, &keys.pairing, monotonic_us()) {
            Ok(conn) => conn,
            Err(e) => {
                tracing::warn!("sendspin: {e}");
                return;
            }
        };
        // Held until the first activation and clock measurement allow it.
        let _ = conn.set_player_state(self.player_state());
        tracing::debug!("sendspin: connection {id} opened");
        self.peers.push(Peer {
            id,
            conn,
            out,
            socket_open: true,
            clock: Arc::new(FilterClock::new()),
            name: String::new(),
            roles: ActiveRoles::default(),
            group: None,
            paused_group: None,
        });
    }

    /// Sends what the connections queued and acts on their events, until none has more:
    /// acting on one connection's event can make another send (a displaced connection says
    /// goodbye).
    fn pump(&mut self) {
        loop {
            let mut busy = false;
            for i in 0..self.peers.len() {
                while let Some(t) = self.peers[i].conn.poll_transmit() {
                    busy = true;
                    let message = match t {
                        Transmit::Text(text) => ToSocket::Text(text),
                        Transmit::Binary(bytes) => ToSocket::Binary(bytes),
                        Transmit::Close(_) => ToSocket::Close,
                    };
                    let _ = self.peers[i].out.send(message);
                }
                while let Some(event) = self.peers[i].conn.poll_event() {
                    busy = true;
                    let id = self.peers[i].id;
                    self.event(id, event);
                }
            }
            if !busy {
                break;
            }
        }
        self.peers.retain(|p| p.socket_open);
    }

    // Events.

    fn streams_from(&self, id: u64) -> bool {
        self.stream.as_ref().is_some_and(|s| s.peer == id)
    }

    fn event(&mut self, id: u64, event: Event) {
        let Some(i) = self.index(id) else {
            return;
        };
        match event {
            Event::Connected { name, .. } => {
                self.peers[i].name = display_name(&name);
                tracing::debug!(
                    "sendspin: connection {id} is {:?}",
                    self.peers[i].name.as_str()
                );
            }
            Event::Activated {
                roles,
                displaces_rival,
                ..
            } => {
                if displaces_rival {
                    for p in self
                        .peers
                        .iter_mut()
                        .filter(|p| p.id != id && p.conn.is_activated())
                    {
                        p.conn.displace();
                    }
                }
                self.peers[i].roles = roles;
                self.turned_away = None;
            }
            Event::StreamStart(start) => self.stream_start(i, &start),
            Event::StreamClear { from_timestamp } => {
                if let Some(s) = self.sink.as_ref().filter(|s| s.peer == id) {
                    s.sink.clear(from_timestamp);
                }
            }
            Event::StreamEnd => {
                if self.streams_from(id) {
                    self.end_stream();
                }
            }
            Event::Audio(chunk) => self.audio(i, chunk),
            Event::Volume(volume) => {
                self.volume = volume;
                self.level_changed();
            }
            Event::Mute(muted) => {
                self.muted = muted;
                self.level_changed();
            }
            Event::Metadata(metadata) => {
                if let Some(output) = &self.output {
                    output.set_track(track(metadata));
                }
            }
            // The connection keeps it, and checks each command against it.
            Event::ControllerState(_) => {}
            Event::Group(group) => {
                let p = &mut self.peers[i];
                if group.playback_state == PlaybackState::Playing {
                    p.paused_group = None;
                }
                p.group = Some(group);
            }
            Event::ClockUpdated => {
                let p = &self.peers[i];
                p.clock.update(p.conn.clock());
                // Once a burst: the filter's error is a metric.
                self.refresh_stats();
            }
            Event::PairingAdded(pairing) => self.paired(i, pairing),
            Event::Unpaired(server_id) => {
                if let Some(keys) = &mut self.keys
                    && keys.pairing.remove(&server_id)
                {
                    tracing::info!("sendspin: {:?} removed its pairing", self.peers[i].name);
                    self.save_keys();
                }
            }
            Event::LastPlaybackServer(server_id) => {
                if let Some(keys) = &mut self.keys
                    && keys.pairing.set_last_playback_server(&server_id)
                {
                    self.save_keys();
                }
            }
            Event::Closed(reason) => self.closed(i, &reason),
        }
    }

    /// Keeps a new pairing without evicting the record of any open connection.
    fn paired(&mut self, i: usize, pairing: pinna_sendspin::PairingRecord) {
        let open: Vec<String> = self
            .peers
            .iter()
            .filter(|p| !p.conn.is_closed())
            .filter_map(|p| p.conn.server_id().map(str::to_owned))
            .collect();
        let open: Vec<&str> = open.iter().map(String::as_str).collect();
        let Some(keys) = &mut self.keys else {
            return;
        };
        let evicted = keys.pairing.insert(pairing, &open);
        tracing::info!("sendspin: paired with {:?}", self.peers[i].name);
        if !evicted.is_empty() {
            tracing::info!(
                "sendspin: forgot the {} oldest pairing(s) to make room",
                evicted.len()
            );
        }
        self.save_keys();
    }

    fn closed(&mut self, i: usize, reason: &CloseReason) {
        let id = self.peers[i].id;
        if self.streams_from(id) {
            self.end_stream();
        }
        let p = &self.peers[i];
        if p.conn.is_activated() {
            tracing::info!("sendspin: {:?} disconnected: {reason}", p.name);
            if p.roles.metadata
                && let Some(output) = &self.output
            {
                output.set_track(Track::default());
            }
        } else {
            tracing::debug!("sendspin: connection {id} closed: {reason}");
        }
        if *reason == CloseReason::ClosedBeforeServerInit && self.turned_away != Some(TOO_OLD) {
            tracing::warn!(
                "sendspin: a server closed the connection before server/init, as one that \
                 speaks only the protocol from before 1.0.0-rc1 does: {TOO_OLD}"
            );
            self.turned_away = Some(TOO_OLD);
        }
    }

    // The stream.

    fn stream_start(&mut self, i: usize, start: &PlayerStream) {
        let id = self.peers[i].id;
        if !start.in_place || !self.streams_from(id) {
            if self.stream.is_some() {
                self.end_stream();
            }
            self.stream = Some(Stream {
                peer: id,
                decoder: None,
                warned: false,
            });
        }
        if let Some(stream) = &mut self.stream {
            match decoder(start) {
                Ok(d) => stream.decoder = Some(d),
                Err(e) => {
                    stream.decoder = None;
                    if !stream.warned {
                        stream.warned = true;
                        tracing::warn!("sendspin: the stream's audio can't be decoded: {e}");
                    }
                }
            }
        }
        // A format changed in place carries on in the sink there is, if any: a source that
        // took the lane keeps it.
        if start.in_place {
            return;
        }
        if self.sink.is_none()
            && let Some(output) = &self.output
        {
            let sink = output.begin(self.peers[i].clock.clone());
            sink.set_gain(gain(self.volume, self.muted));
            let state = sink.state();
            self.sink = Some(Sounding {
                peer: id,
                sink,
                state,
            });
            self.stats_at = Some(Instant::now() + STATS_EVERY);
            self.refresh_stats();
        }
    }

    /// The stream ended: its timed source fades out.
    fn end_stream(&mut self) {
        self.stream = None;
        if let Some(s) = self.sink.take() {
            s.sink.end();
            self.sink_gone(&s);
        }
    }

    fn sink_gone(&mut self, s: &Sounding) {
        let stats = s.sink.stats();
        self.ended_jumps += stats.jumps;
        self.ended_late_drops += stats.late_drops;
        self.stats_at = None;
        self.refresh_stats();
    }

    fn audio(&mut self, i: usize, chunk: AudioChunk) {
        let id = self.peers[i].id;
        if let Some(ahead) = chunk.send_ahead_us {
            let sent = self.peers[i]
                .conn
                .clock()
                .compute_client_time(chunk.timestamp_us.saturating_sub(i64::from(ahead)));
            let delay = chunk.arrival_us.saturating_sub(sent);
            if self.lateness.add(chunk.arrival_us, delay).is_some() {
                self.send_player_state();
            }
        }
        // Nothing to play into once another source took the lane: not even decoded.
        let Some(sounding) = self.sink.as_ref().filter(|s| s.peer == id) else {
            return;
        };
        let Some(stream) = self.stream.as_mut().filter(|s| s.peer == id) else {
            return;
        };
        let Some(decoder) = stream.decoder.as_mut() else {
            self.decode_drops += 1;
            return;
        };
        match decoder.decode(&chunk.data) {
            // Refused audio (due too far ahead, or after the source ended) is the timed
            // source's to count; an end shows in its state.
            Ok(pcm) => {
                let _ = sounding
                    .sink
                    .push(chunk.timestamp_us, decoder.sample_rate(), pcm);
            }
            Err(e) => {
                self.decode_drops += 1;
                if !stream.warned {
                    stream.warned = true;
                    tracing::warn!("sendspin: dropping audio that does not decode: {e}");
                }
            }
        }
    }

    fn sink_changed(&mut self) {
        let Some(s) = &mut self.sink else {
            return;
        };
        let state = *s.state.borrow_and_update();
        let TimedState::Ended(why) = state else {
            // Waiting or playing: the report's timing follows.
            return self.refresh_stats();
        };
        let peer = s.peer;
        if let Some(s) = self.sink.take() {
            self.sink_gone(&s);
        }
        match why {
            // Another source took the lane, or the lane stopped or paused it, although the
            // Brain routes pause and stop to the group: the group plays on without the
            // satellite (ADR 0027 §14).
            EndReason::Replaced | EndReason::Stopped | EndReason::Paused => {
                if let Some(i) = self.index(peer) {
                    let p = &mut self.peers[i];
                    if p.conn.leave().is_ok() {
                        tracing::info!(
                            "sendspin: the speaker went to another source; left {:?}'s group",
                            p.name
                        );
                    }
                    p.paused_group = None;
                }
            }
            EndReason::SpeakerFailed => {
                tracing::warn!("sendspin: the speaker failed; the next stream starts it again");
            }
            EndReason::Ended => {}
        }
    }

    // The report.

    fn refresh_stats(&mut self) {
        let sink = self
            .sink
            .as_ref()
            .map(|s| (s.sink.stats(), *s.state.borrow()));
        let clock = self
            .sink
            .as_ref()
            .map(|s| s.peer)
            .and_then(|id| self.index(id))
            .or_else(|| self.admitted())
            .map(|i| self.peers[i].conn.clock().error_us())
            .filter(|e| e.is_finite());
        let stats = sink.map(|(stats, _)| stats).unwrap_or_default();
        self.stats = Stats {
            sync_error_us: stats.sync_error_us,
            filter_error_us: clock,
            steady: stats.steady,
            jumps: self.ended_jumps + stats.jumps,
            late_drops: self.ended_late_drops + stats.late_drops,
            decode_drops: self.decode_drops,
        };
        // A sink's stats say how it is timed once it plays.
        self.exact =
            sink.is_none_or(|(stats, state)| state != TimedState::Playing || stats.exact_timing);
    }

    fn compose(&self) -> Report {
        let admitted = self.admitted().map(|i| &self.peers[i]);
        let server = admitted.map(|p| p.name.clone()).unwrap_or_default();
        let state = if self.output.is_none() {
            State::Unavailable
        } else if self.name.is_none() {
            State::Off
        } else if let Some(why) = &self.failure {
            State::Failed(why.clone())
        } else if self.sink.is_some() {
            State::Playing
        } else if admitted.is_some() {
            State::Connected(server.clone())
        } else {
            State::Ready
        };
        let reason = match &state {
            State::Failed(why) => why.clone(),
            State::Ready => self.turned_away.unwrap_or_default().to_owned(),
            _ => String::new(),
        };
        let group_paused = self.sink.is_none()
            && admitted.is_some_and(|p| {
                p.roles.player
                    && p.group.as_ref().is_some_and(|g| {
                        g.playback_state == PlaybackState::Stopped
                            && p.paused_group.as_deref() == Some(g.group_id.as_str())
                    })
            });
        Report {
            state,
            name: self.name.clone().unwrap_or_default(),
            server,
            reason,
            exact_timing: self.exact_backend && self.exact,
            group_paused,
            volume: self.volume,
            muted: self.muted,
            listening: self.listener.as_ref().map(Listener::addr),
            stats: self.stats,
        }
    }

    fn publish(&mut self) {
        let next = self.compose();
        if next.state != self.report.borrow().state {
            self.refresh_stats();
            log_state(&next);
        }
        let next = Report {
            stats: self.stats,
            exact_timing: self.exact_backend && self.exact,
            ..next
        };
        self.report.send_if_modified(|r| {
            let changed = *r != next;
            if changed {
                *r = next;
            }
            changed
        });
    }
}

async fn publish_record(mdns: Mdns, name: String, port: u16) {
    match tokio::task::spawn_blocking(move || mdns.publish(&name, port)).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => tracing::warn!("sendspin: publishing the record: {e:#}"),
        Err(e) => tracing::warn!("sendspin: publishing the record: {e}"),
    }
}

fn log_state(r: &Report) {
    match &r.state {
        State::Off => tracing::info!("sendspin: off"),
        State::Ready => match r.listening {
            Some(addr) => tracing::info!("sendspin: on as {:?}, listening on {addr}", r.name),
            None => tracing::info!("sendspin: on as {:?}", r.name),
        },
        State::Connected(server) => tracing::info!("sendspin: connected to {server:?}"),
        State::Playing => tracing::info!("sendspin: playing from {:?}", r.server),
        State::Failed(why) => tracing::warn!("sendspin: {why}; trying again every 5 s"),
        State::Unavailable => tracing::info!("sendspin: no speaker, so it can't run"),
    }
}
