//! The virtual Echo Dot (ADR 0014): hardware for the development VM, driven
//! from the dashboard over one WebSocket, `/virtual-device`.
//!
//! pinnad itself runs unchanged against it:
//! - buttons are a uinput device that pinna-hal's evdev scan finds;
//! - the microphone switch is a privacy latch file that pinna-hal polls; the
//!   microphone button flips it and sends KEY_POWER, like the Dot's PMIC;
//! - the microphone is the VM's PipeWire source `dev_mic`, which pinnad records
//!   by name (`[audio] capture_target`), fed from the browser through `dev_mic_in`;
//! - the speaker is the VM's PipeWire sink `dev_speaker` (default sink), whose
//!   monitor goes back to the browser.
//!
//! It has no LED ring: the dashboard draws WatchRing's frames, as for any
//! satellite.
//!
//! Socket protocol:
//! - browser -> pinnad, binary: microphone audio, S16LE mono at 16 kHz;
//! - browser -> pinnad, text: `{"button": "action" | "volume_up" |
//!   "volume_down" | "mute", "pressed": bool}`;
//! - pinnad -> browser, binary: speaker audio, S16LE mono at 48 kHz.
//!
//! One browser at a time: a new connection closes the previous one with code
//! 4000. While the microphone switch is on, microphone audio is dropped.

use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use axum::extract::State;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use futures::stream::SplitSink;
use futures::{SinkExt, StreamExt};
use pinna_hal::buttons::Button;
use pinna_hal::virt::{PrivacyLatch, VirtualButtons};
use serde::Deserialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, watch};

/// Where the browser's microphone audio is played (a sink looped into `dev_mic`).
pub const MIC_SINK: &str = "dev_mic_in";
/// The VM's speaker; its monitor goes to the browser.
pub const SPEAKER_SINK: &str = "dev_speaker";
pub const LATCH_PATH: &str = "/run/pinna/virtual/privacy_state";
pub const ROUTE: &str = "/virtual-device";

/// Largest frame accepted from the browser (about 2 s of microphone audio).
const MAX_MESSAGE: usize = 64 * 1024;
/// Queue depths in chunks (~20 ms each): late audio is dropped, never queued up.
const QUEUE: usize = 25;
/// Close code for a connection another browser took over.
const CLOSE_REPLACED: u16 = 4000;
/// Close code when the VM's audio graph is not there.
const CLOSE_NO_AUDIO: u16 = 4001;
/// A browser that takes longer than this to accept a message is gone.
const SEND_TIMEOUT: Duration = Duration::from_secs(2);

pub struct VirtualDevice {
    buttons: Mutex<VirtualButtons>,
    latch: PrivacyLatch,
    muted: AtomicBool,
    /// Bumped by each new connection; the older one sees it and ends.
    session: watch::Sender<u64>,
    /// Which connection holds each button down.
    owners: Mutex<HashMap<Button, u64>>,
}

#[derive(Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct ButtonMsg {
    button: ButtonName,
    pressed: bool,
}

#[derive(Debug, Deserialize, PartialEq, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum ButtonName {
    Action,
    VolumeUp,
    VolumeDown,
    Mute,
}

impl From<ButtonName> for Button {
    fn from(b: ButtonName) -> Self {
        match b {
            ButtonName::Action => Button::Action,
            ButtonName::VolumeUp => Button::VolumeUp,
            ButtonName::VolumeDown => Button::VolumeDown,
            ButtonName::Mute => Button::Mute,
        }
    }
}

impl VirtualDevice {
    /// The latch (open) and the buttons. Refused on real hardware.
    pub fn start() -> Result<Arc<Self>> {
        if !pinna_hal::ident::board_model().is_empty() {
            bail!("this is real hardware, not the development VM");
        }
        let latch = PrivacyLatch::create(LATCH_PATH).context("microphone switch")?;
        let buttons = VirtualButtons::create().context("buttons (/dev/uinput)")?;
        Ok(Arc::new(Self {
            buttons: Mutex::new(buttons),
            latch,
            muted: AtomicBool::new(false),
            session: watch::Sender::new(0),
            owners: Mutex::default(),
        }))
    }

    pub fn latch_path(&self) -> &Path {
        self.latch.path()
    }

    /// A press or release from connection `me`.
    fn button(&self, b: Button, pressed: bool, me: u64) {
        if !claim(&mut self.owners.lock().unwrap(), b, pressed, me) {
            return;
        }
        // The Dot's microphone button flips the latch in hardware as it goes down.
        if b == Button::Mute && pressed {
            let muted = !self.muted.load(Ordering::SeqCst);
            match self.latch.set(muted) {
                Ok(()) => self.muted.store(muted, Ordering::SeqCst),
                Err(e) => tracing::warn!("virtual microphone switch: {e}"),
            }
        }
        if let Err(e) = self.buttons.lock().unwrap().set(b, pressed) {
            tracing::warn!("virtual button {b:?}: {e}");
        }
    }

    async fn serve(self: Arc<Self>, socket: WebSocket) {
        let mut me = 0;
        self.session.send_modify(|n| {
            *n += 1;
            me = *n;
        });
        let mut session = self.session.subscribe();
        tracing::info!("virtual device: browser connected");
        let (mut tx, mut rx) = socket.split();
        let audio = match Audio::spawn() {
            Ok(a) => a,
            Err(e) => {
                tracing::warn!("virtual device: no audio: {e:#}");
                send(&mut tx, close(CLOSE_NO_AUDIO, "no audio graph in the VM")).await;
                return;
            }
        };
        let Audio {
            mic,
            mut speaker,
            _children,
        } = audio;
        let mut odd_frames = 0u32;
        loop {
            tokio::select! {
                _ = session.changed() => {
                    if *session.borrow() != me {
                        send(&mut tx, close(CLOSE_REPLACED, "opened in another tab")).await;
                        break;
                    }
                }
                msg = rx.next() => match msg {
                    Some(Ok(Message::Binary(pcm))) => {
                        if pcm.len() % 2 != 0 {
                            odd_frames += 1;
                            if odd_frames == 1 {
                                tracing::warn!("virtual device: dropping microphone frames of odd length");
                            }
                        } else if !self.muted.load(Ordering::SeqCst) {
                            // Full: the VM is behind the browser's clock; drop.
                            let _ = mic.try_send(pcm);
                        }
                    }
                    Some(Ok(Message::Text(text))) => match serde_json::from_str::<ButtonMsg>(&text) {
                        Ok(m) => self.button(Button::from(m.button), m.pressed, me),
                        Err(e) => tracing::warn!("virtual device: ignoring message: {e}"),
                    },
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(_)) => {}
                    Some(Err(e)) => {
                        tracing::debug!("virtual device: {e}");
                        break;
                    }
                },
                chunk = speaker.recv() => match chunk {
                    Some(pcm) => {
                        if !send(&mut tx, Message::Binary(pcm)).await {
                            break;
                        }
                    }
                    None => {
                        send(&mut tx, close(CLOSE_NO_AUDIO, "the VM's audio graph stopped")).await;
                        break;
                    }
                },
            }
        }
        // A button held when the browser went away must not stay down
        // (volume would repeat to the end, the long press would fire). One a
        // newer connection holds now is its to release.
        let mine: Vec<Button> = self
            .owners
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, owner)| **owner == me)
            .map(|(b, _)| *b)
            .collect();
        for b in mine {
            self.button(b, false, me);
        }
        tracing::info!("virtual device: browser disconnected");
    }
}

/// Whether a press or release from connection `me` reaches the button: a
/// press always does (and `me` holds it from then on); a release only from
/// the connection that holds the button.
fn claim(owners: &mut HashMap<Button, u64>, b: Button, pressed: bool, me: u64) -> bool {
    if pressed {
        owners.insert(b, me);
        true
    } else if owners.get(&b) == Some(&me) {
        owners.remove(&b);
        true
    } else {
        false
    }
}

/// Send, or give up on a browser that does not take it in time.
async fn send(tx: &mut SplitSink<WebSocket, Message>, msg: Message) -> bool {
    matches!(
        tokio::time::timeout(SEND_TIMEOUT, tx.send(msg)).await,
        Ok(Ok(()))
    )
}

fn close(code: u16, reason: &'static str) -> Message {
    Message::Close(Some(CloseFrame {
        code,
        reason: reason.into(),
    }))
}

/// The two PipeWire streams of one connection. Dropping it ends them.
struct Audio {
    mic: mpsc::Sender<Bytes>,
    speaker: mpsc::Receiver<Bytes>,
    _children: [Child; 2],
}

impl Audio {
    fn spawn() -> Result<Self> {
        let mut play = Command::new("pw-play")
            .args(pw_args(MIC_SINK, 16_000))
            .arg("-")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .context("pw-play")?;
        let mut record = Command::new("pw-record")
            .args(pw_args(SPEAKER_SINK, 48_000))
            // The sink's monitor: what pinnad plays.
            .args(["-P", "{ stream.capture.sink = true }", "-"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .context("pw-record")?;

        let (mic, mut mic_rx) = mpsc::channel::<Bytes>(QUEUE);
        let mut stdin = play.stdin.take().expect("piped");
        tokio::spawn(async move {
            while let Some(pcm) = mic_rx.recv().await {
                if stdin.write_all(&pcm).await.is_err() {
                    break;
                }
            }
        });

        let (speaker_tx, speaker) = mpsc::channel::<Bytes>(QUEUE);
        let mut stdout = record.stdout.take().expect("piped");
        tokio::spawn(async move {
            // 20 ms at 48 kHz mono S16LE; read continuously so pw-record never
            // blocks, and drop what a slow browser cannot take.
            let mut buf = vec![0u8; 1920];
            while stdout.read_exact(&mut buf).await.is_ok() {
                if let Err(mpsc::error::TrySendError::Closed(_)) =
                    speaker_tx.try_send(Bytes::copy_from_slice(&buf))
                {
                    break;
                }
            }
        });
        Ok(Self {
            mic,
            speaker,
            _children: [play, record],
        })
    }
}

fn pw_args(target: &str, rate: u32) -> Vec<String> {
    [
        "--raw",
        "--rate",
        &rate.to_string(),
        "--channels",
        "1",
        "--format",
        "s16",
        "--latency",
        "40ms",
        "--target",
        target,
    ]
    .map(String::from)
    .to_vec()
}

/// Browsers send no CORS preflight for a WebSocket, so check where the page
/// that opens one comes from: the dashboard itself (same host), or a page on
/// this computer (the Vite dev server). Clients that are not browsers send no
/// Origin.
pub fn origin_allowed(headers: &HeaderMap) -> bool {
    let Some(origin) = headers.get(header::ORIGIN) else {
        return true;
    };
    let Some(origin) = origin.to_str().ok().and_then(|o| url::Url::parse(o).ok()) else {
        return false;
    };
    let Some(origin_host) = origin.host_str() else {
        return false;
    };
    let host = headers
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    let origin_authority = match origin.port() {
        Some(p) => format!("{origin_host}:{p}"),
        None => origin_host.to_string(),
    };
    origin_authority.eq_ignore_ascii_case(host)
        || matches!(origin_host, "localhost" | "127.0.0.1" | "[::1]")
}

pub async fn socket(
    State(device): State<Arc<VirtualDevice>>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    if !origin_allowed(&headers) {
        return (StatusCode::FORBIDDEN, "cross-site WebSocket refused\n").into_response();
    }
    ws.max_message_size(MAX_MESSAGE)
        .on_upgrade(move |s| device.serve(s))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(origin: Option<&str>, host: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(header::HOST, host.parse().unwrap());
        if let Some(o) = origin {
            h.insert(header::ORIGIN, o.parse().unwrap());
        }
        h
    }

    #[test]
    fn origin_check() {
        for (origin, host) in [
            (None, "127.0.0.1:18090"),
            (Some("http://127.0.0.1:18090"), "127.0.0.1:18090"),
            (Some("http://pinna-vm.local"), "pinna-vm.local"),
            (Some("http://localhost:5173"), "127.0.0.1:18090"),
            (Some("http://[::1]:5173"), "127.0.0.1:18090"),
            // Any page served from this computer, whatever its port.
            (Some("http://127.0.0.1:18091"), "127.0.0.1:18090"),
        ] {
            assert!(origin_allowed(&headers(origin, host)), "{origin:?} {host}");
        }
        for (origin, host) in [
            (Some("https://evil.example"), "127.0.0.1:18090"),
            (Some("http://192.168.1.9"), "127.0.0.1:18090"),
            (Some("http://192.168.1.9:5173"), "192.168.1.5"),
            (Some("null"), "127.0.0.1:18090"),
        ] {
            assert!(!origin_allowed(&headers(origin, host)), "{origin:?} {host}");
        }
    }

    #[test]
    fn a_release_only_counts_from_the_holder() {
        let mut owners = HashMap::new();
        assert!(claim(&mut owners, Button::VolumeUp, true, 1));
        // Taken over: the new tab presses the same button, then the old
        // connection's cleanup must not release it.
        assert!(claim(&mut owners, Button::VolumeUp, true, 2));
        assert!(!claim(&mut owners, Button::VolumeUp, false, 1));
        assert!(claim(&mut owners, Button::VolumeUp, false, 2));
        // A release without a press goes nowhere.
        assert!(!claim(&mut owners, Button::Action, false, 2));
        assert!(owners.is_empty());
    }

    #[test]
    fn parses_button_messages() {
        let m: ButtonMsg =
            serde_json::from_str(r#"{"button": "volume_up", "pressed": true}"#).unwrap();
        assert_eq!(
            m,
            ButtonMsg {
                button: ButtonName::VolumeUp,
                pressed: true
            }
        );
        assert_eq!(Button::from(ButtonName::Mute), Button::Mute);
        for bad in [
            r#"{"button": "power", "pressed": true}"#,
            r#"{"button": "action"}"#,
            r#"{"button": "action", "pressed": true, "x": 1}"#,
            "not json",
        ] {
            assert!(serde_json::from_str::<ButtonMsg>(bad).is_err(), "{bad}");
        }
    }
}
