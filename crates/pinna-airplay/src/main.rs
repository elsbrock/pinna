//! pinna-airplay — runs shairport-sync and bridges it to pinnad.
//!
//! shairport-sync's PCM (stdout backend) becomes a session on pinnad's stream
//! socket, and the volume it announces on its metadata pipe is set through the
//! control API (ADR 0011).

mod metadata;

use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::{Context, Result, bail};
use clap::Parser;
use pinna_bridge::{Api, Connector, QUIET, VolumeApi, header};
use tokio::io::AsyncReadExt as _;
use tokio::net::unix::pipe;
use tokio::process::Command;

use metadata::{Item, pvol_percent};

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    /// shairport-sync configuration, written by pinnad.
    #[arg(long, default_value = "/run/pinna/airplay/shairport-sync.conf")]
    conf: PathBuf,
    /// pinnad's stream socket.
    #[arg(long, default_value = "/run/pinna/stream.sock")]
    socket: PathBuf,
    /// shairport-sync's metadata pipe.
    #[arg(long, default_value = "/run/pinna-airplay/metadata")]
    metadata: PathBuf,
    /// pinnad control API.
    #[arg(long, default_value = "http://127.0.0.1:7470")]
    api: String,
    #[arg(long, default_value = "/usr/bin/shairport-sync")]
    shairport_sync: PathBuf,
}

/// shairport-sync's stdout backend: S16_LE stereo at 44.1 kHz only.
const RATE: u32 = 44100;

/// What the metadata pipe means for the session.
async fn on_metadata<V: VolumeApi>(c: &mut Connector<V>, item: &Item) {
    if item.kind != "ssnc" {
        return;
    }
    match item.code.as_str() {
        "pvol" => {
            if let Some(percent) = pvol_percent(&item.data) {
                c.volume(percent).await;
            }
        }
        // The sender went quiet on purpose.
        "pfls" | "pend" => c.unblock(),
        _ => {}
    }
}

/// Creates the metadata FIFO if missing.
fn make_fifo(path: &Path) -> Result<()> {
    let c = std::ffi::CString::new(path.as_os_str().as_bytes())?;
    // SAFETY: `c` is a valid NUL-terminated path.
    if unsafe { libc::mkfifo(c.as_ptr(), 0o666) } != 0 {
        let e = std::io::Error::last_os_error();
        if e.kind() != std::io::ErrorKind::AlreadyExists {
            return Err(e).with_context(|| format!("mkfifo {}", path.display()));
        }
    }
    Ok(())
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    pinna_bridge::init_logging();
    let cli = Cli::parse();

    // shairport-sync opens the pipe write-only and non-blocking, which fails
    // while nobody reads: the read end comes first, read-write so it never sees EOF.
    make_fifo(&cli.metadata)?;
    let mut meta = pipe::OpenOptions::new()
        .read_write(true)
        .open_receiver(&cli.metadata)
        .with_context(|| format!("open {}", cli.metadata.display()))?;

    let mut child = Command::new(&cli.shairport_sync)
        .arg("-c")
        .arg(&cli.conf)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("spawn {}", cli.shairport_sync.display()))?;
    let mut pcm = child.stdout.take().context("shairport-sync stdout")?;

    let api = Api::new(&cli.api)?;
    let mut connector = Connector::new(cli.socket, header(RATE), api);
    let mut parser = metadata::Parser::default();
    let mut pcm_buf = vec![0u8; 16 * 1024];
    let mut meta_buf = vec![0u8; 4096];

    // Quiet counts from the last audio, whatever else arrives meanwhile.
    let mut last_pcm = tokio::time::Instant::now();
    loop {
        tokio::select! {
            status = child.wait() => bail!("shairport-sync exited: {}", status?),
            n = pcm.read(&mut pcm_buf) => match n {
                Ok(0) => bail!("shairport-sync closed its output"),
                Ok(n) => {
                    last_pcm = tokio::time::Instant::now();
                    connector.data(&pcm_buf[..n]).await;
                }
                Err(e) => return Err(e).context("read shairport-sync output"),
            },
            () = tokio::time::sleep_until(last_pcm + QUIET) => {
                connector.quiet();
                last_pcm = tokio::time::Instant::now();
            }
            n = meta.read(&mut meta_buf) => {
                let n = n.context("read metadata")?;
                for item in parser.push(&String::from_utf8_lossy(&meta_buf[..n])) {
                    on_metadata(&mut connector, &item).await;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    #[derive(Clone, Default)]
    struct Recorder(Arc<Mutex<Vec<u8>>>);

    impl VolumeApi for Recorder {
        async fn set_stream_volume(&self, percent: u8) -> anyhow::Result<()> {
            self.0.lock().unwrap().push(percent);
            Ok(())
        }
    }

    fn item(code: &str, data: &str) -> Item {
        Item {
            kind: "ssnc".into(),
            code: code.into(),
            data: data.as_bytes().to_vec(),
        }
    }

    #[tokio::test]
    async fn pvol_sets_the_volume() {
        let rec = Recorder::default();
        let mut c = Connector::new("/nonexistent".into(), header(RATE), rec.clone());
        on_metadata(&mut c, &item("pvol", "-15.00,-15.00,-96.30,0.00")).await;
        assert_eq!(*rec.0.lock().unwrap(), vec![51]);
        on_metadata(&mut c, &item("prgr", "")).await;
        assert_eq!(rec.0.lock().unwrap().len(), 1);
    }
}
