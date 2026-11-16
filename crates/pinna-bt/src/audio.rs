//! The audio task: a phone's PCM from BlueALSA becomes sessions on pinnad's
//! stream socket.

use std::os::fd::OwnedFd;
use std::path::PathBuf;

use pinna_bridge::{Api, Connector, QUIET, header};
use tokio::io::AsyncReadExt as _;
use tokio::net::unix::pipe;
use tokio::sync::mpsc;
use tokio::time::Instant;

pub enum Command {
    /// A phone's PCM, opened: read it from now on, in place of any other.
    Open {
        path: String,
        pcm: OwnedFd,
        /// BlueALSA's control socket for the PCM: the PCM stays open with it.
        ctrl: OwnedFd,
        rate: u32,
    },
    /// The phone's volume, 0..=100.
    Volume(u8),
    /// The PCM is gone (the phone disconnected).
    Closed,
}

struct Open {
    path: String,
    pcm: pipe::Receiver,
    _ctrl: OwnedFd,
}

pub fn spawn(socket: PathBuf, api: Api) -> mpsc::UnboundedSender<Command> {
    let (tx, mut rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        let mut connector = Connector::new(socket, header(44100), api);
        let mut open: Option<Open> = None;
        let mut buf = vec![0u8; 16 * 1024];
        let mut last_pcm = Instant::now();
        loop {
            tokio::select! {
                cmd = rx.recv() => match cmd {
                    None => return,
                    Some(Command::Open { path, pcm, ctrl, rate }) => {
                        match pipe::Receiver::from_owned_fd(pcm) {
                            Ok(pcm) => {
                                tracing::info!("reading {path} ({rate} Hz)");
                                connector.set_header(header(rate));
                                open = Some(Open { path, pcm, _ctrl: ctrl });
                            }
                            Err(e) => tracing::warn!("{path}: {e}"),
                        }
                    }
                    Some(Command::Volume(percent)) => connector.volume(percent).await,
                    Some(Command::Closed) => {
                        if let Some(o) = open.take() {
                            tracing::info!("{} closed", o.path);
                        }
                        connector.quiet();
                    }
                },
                n = async { open.as_mut().expect("guarded").pcm.read(&mut buf).await }, if open.is_some() => {
                    match n {
                        Ok(n) if n > 0 => {
                            last_pcm = Instant::now();
                            connector.data(&buf[..n]).await;
                        }
                        end => {
                            if let Some(o) = open.take() {
                                tracing::info!("{} ended ({end:?})", o.path);
                            }
                            connector.quiet();
                        }
                    }
                }
                // Quiet counts from the last audio: the phone paused.
                () = tokio::time::sleep_until(last_pcm + QUIET) => {
                    connector.quiet();
                    last_pcm = Instant::now();
                }
            }
        }
    });
    tx
}
