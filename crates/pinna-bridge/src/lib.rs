//! Shared parts of the media bridges (ADR 0011): a source's PCM becomes
//! sessions on pinnad's stream socket, and its volume is set through the
//! loopback control API.

use std::io;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use tokio::io::AsyncWriteExt as _;
use tokio::net::UnixStream;

use connectrpc::client::{ClientConfig, HttpClient};
use pinna_api::{DeviceServiceClient, SetStreamVolumeRequest};
/// Matches pinnad's `STREAM_IDLE`: without data for this long, pinnad ends the session.
pub const QUIET: Duration = Duration::from_secs(2);
const CONNECT_WARN_EVERY: Duration = Duration::from_secs(10);

/// Sets the stream volume in pinnad.
pub trait VolumeApi {
    fn set_stream_volume(&self, percent: u8) -> impl Future<Output = anyhow::Result<()>>;
}

/// The header line of a pinnad stream session: 16-bit little-endian stereo
/// at `rate` (44100 or 48000).
pub fn header(rate: u32) -> Vec<u8> {
    format!("s16le {rate} 2\n").into_bytes()
}

pub struct Connector<V> {
    socket: PathBuf,
    header: Vec<u8>,
    api: V,
    conn: Option<UnixStream>,
    /// pinnad closed the session; no reconnect until the sender goes quiet.
    blocked: bool,
    volume: Option<u8>,
    last_connect_warn: Option<Instant>,
}

impl<V: VolumeApi> Connector<V> {
    pub fn new(socket: PathBuf, header: Vec<u8>, api: V) -> Self {
        Self {
            socket,
            header,
            api,
            conn: None,
            blocked: false,
            volume: None,
            last_connect_warn: None,
        }
    }

    pub fn blocked(&self) -> bool {
        self.blocked
    }

    /// The source's format changed: the next session starts with this header.
    pub fn set_header(&mut self, header: Vec<u8>) {
        if header != self.header {
            self.header = header;
            self.conn = None;
        }
    }

    /// PCM from the source.
    pub async fn data(&mut self, chunk: &[u8]) {
        if self.blocked {
            return;
        }
        if self.conn.is_none() {
            // The volume goes first: a bridge sets it before it connects.
            if let Some(percent) = self.volume {
                self.set_volume(percent).await;
            }
            match self.connect().await {
                Ok(conn) => self.conn = Some(conn),
                Err(e) => {
                    let due = self
                        .last_connect_warn
                        .is_none_or(|t| t.elapsed() >= CONNECT_WARN_EVERY);
                    if due {
                        self.last_connect_warn = Some(Instant::now());
                        tracing::warn!("cannot connect to {}: {e}", self.socket.display());
                    }
                    return;
                }
            }
        }
        if let Some(conn) = &mut self.conn
            && conn.write_all(chunk).await.is_err()
        {
            // pinnad closed it: stopped, paused or replaced by another source.
            self.conn = None;
            self.blocked = true;
            tracing::info!("pinnad ended the stream; waiting for the sender to go quiet");
        }
    }

    async fn connect(&self) -> io::Result<UnixStream> {
        let mut conn = UnixStream::connect(&self.socket).await?;
        conn.write_all(&self.header).await?;
        Ok(conn)
    }

    /// No data for [`QUIET`]: pinnad fades out on EOF.
    pub fn quiet(&mut self) {
        self.conn = None;
        self.blocked = false;
    }

    /// The sender's volume, 0..=100 on the master curve. Set now, and again
    /// before each new session.
    pub async fn volume(&mut self, percent: u8) {
        self.volume = Some(percent);
        self.set_volume(percent).await;
    }

    /// The sender went quiet on purpose (paused, flushed, stopped): the next
    /// audio may start a session again.
    pub fn unblock(&mut self) {
        self.blocked = false;
    }

    async fn set_volume(&self, percent: u8) {
        if let Err(e) = self.api.set_stream_volume(percent).await {
            tracing::warn!("cannot set the stream volume: {e:#}");
        }
    }
}

/// pinnad's control API on loopback.
pub struct Api(pub DeviceServiceClient<HttpClient>);

/// The audio loop waits for these calls: a slow answer must not stall the
/// source's output.
pub const API_TIMEOUT: Duration = Duration::from_millis(500);

impl Api {
    pub fn new(url: &str) -> anyhow::Result<Self> {
        let config = ClientConfig::new(url.parse().map_err(|e| anyhow::anyhow!("--api: {e}"))?);
        Ok(Self(DeviceServiceClient::new(
            HttpClient::plaintext(),
            config,
        )))
    }
}

impl VolumeApi for Api {
    async fn set_stream_volume(&self, percent: u8) -> anyhow::Result<()> {
        let call = self.0.set_stream_volume(SetStreamVolumeRequest {
            percent: percent.into(),
            ..Default::default()
        });
        tokio::time::timeout(API_TIMEOUT, call)
            .await
            .map_err(|_| anyhow::anyhow!("no answer"))??;
        Ok(())
    }
}

/// Logs to the journal without timestamps, or to a terminal with them.
pub fn init_logging() {
    let fmt = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_target(false);
    // journald timestamps lines itself.
    if std::env::var_os("JOURNAL_STREAM").is_some() {
        fmt.without_time().with_ansi(false).init();
    } else {
        fmt.init();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use tokio::io::AsyncReadExt as _;
    use tokio::net::UnixListener;

    use super::*;

    /// Records volume calls, and a marker for each accepted connection's order.
    #[derive(Clone, Default)]
    struct Recorder(Arc<Mutex<Vec<u8>>>);

    impl VolumeApi for Recorder {
        async fn set_stream_volume(&self, percent: u8) -> anyhow::Result<()> {
            self.0.lock().unwrap().push(percent);
            Ok(())
        }
    }

    fn setup() -> (
        tempfile::TempDir,
        UnixListener,
        Connector<Recorder>,
        Recorder,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stream.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let rec = Recorder::default();
        (
            dir,
            listener,
            Connector::new(path, header(44100), rec.clone()),
            rec,
        )
    }

    #[tokio::test]
    async fn first_data_connects_with_header_after_volume() {
        let (_dir, listener, mut c, rec) = setup();
        c.volume(51).await;
        assert_eq!(*rec.0.lock().unwrap(), vec![51]);
        c.data(&[1, 2, 3, 4]).await;
        // The volume is set again before the connect.
        assert_eq!(*rec.0.lock().unwrap(), vec![51, 51]);
        let (mut srv, _) = listener.accept().await.unwrap();
        c.data(&[5, 6]).await;
        drop(c);
        let mut got = Vec::new();
        srv.read_to_end(&mut got).await.unwrap();
        assert_eq!(got, b"s16le 44100 2\n\x01\x02\x03\x04\x05\x06");
    }

    #[tokio::test]
    async fn closed_by_server_blocks_reconnect() {
        let (_dir, listener, mut c, _rec) = setup();
        c.data(&[0; 64]).await;
        let (srv, _) = listener.accept().await.unwrap();
        drop(srv);
        for _ in 0..50 {
            if c.blocked() {
                break;
            }
            c.data(&[0; 64]).await;
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(c.blocked());
        c.data(&[0; 64]).await;
        let pending = tokio::time::timeout(Duration::from_millis(100), listener.accept()).await;
        assert!(pending.is_err(), "reconnected while blocked");
    }

    #[tokio::test]
    async fn quiet_then_data_reconnects() {
        let (_dir, listener, mut c, _rec) = setup();
        c.data(&[1, 1]).await;
        let (mut first, _) = listener.accept().await.unwrap();
        c.quiet();
        let mut got = Vec::new();
        first.read_to_end(&mut got).await.unwrap();
        assert_eq!(got, b"s16le 44100 2\n\x01\x01");
        c.data(&[2, 2]).await;
        let (mut second, _) = listener.accept().await.unwrap();
        drop(c);
        got.clear();
        second.read_to_end(&mut got).await.unwrap();
        assert_eq!(got, b"s16le 44100 2\n\x02\x02");
    }

    #[tokio::test]
    async fn unblock_does_not_connect() {
        let (_dir, listener, mut c, _rec) = setup();
        c.blocked = true;
        c.data(&[9; 8]).await;
        let none = || tokio::time::timeout(Duration::from_millis(50), listener.accept());
        assert!(none().await.is_err());
        c.unblock();
        assert!(!c.blocked());
        assert!(c.conn.is_none());
        assert!(none().await.is_err());
    }

    #[tokio::test]
    async fn a_new_header_starts_a_new_session() {
        let (_dir, listener, mut c, _rec) = setup();
        c.data(&[1, 1]).await;
        let (mut first, _) = listener.accept().await.unwrap();
        c.set_header(header(48000));
        c.data(&[2, 2]).await;
        let (mut second, _) = listener.accept().await.unwrap();
        drop(c);
        let mut got = Vec::new();
        first.read_to_end(&mut got).await.unwrap();
        assert_eq!(got, b"s16le 44100 2\n\x01\x01");
        got.clear();
        second.read_to_end(&mut got).await.unwrap();
        assert_eq!(got, b"s16le 48000 2\n\x02\x02");
    }
}
