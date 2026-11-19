//! Testing an uploaded wake word model in a process of its own (ADR 0013).
//! pinnad runs itself with `--check-wake-word`, so a model that exhausts
//! memory or crashes the inference engine takes down only the test.

use std::ffi::OsString;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tokio::io::AsyncWriteExt;

const MEMORY_LIMIT: u64 = 256 * 1024 * 1024;
const CPU_LIMIT_SECONDS: u64 = 30;
const WALL_LIMIT: Duration = Duration::from_secs(60);
/// The lowest CPU priority, so the test doesn't take time from the audio path.
const NICE: libc::c_int = 19;

pub const BUSY: &str =
    "The satellite is testing another wake word. Try again when it has finished.";
const TIMED_OUT: &str = "Testing this model took longer than 60 s, so the satellite won't use it.";
const CRASHED: &str = "Testing this model crashed or needed more than 256 MiB of memory or 30 s \
                       of processor time, so the satellite won't use it.";

/// Tests uploads, one at a time.
pub struct Checker {
    dir: PathBuf,
    busy: AtomicBool,
    runner: Runner,
}

enum Runner {
    /// Run pinnad again with these arguments (`--config` and the like).
    Process(Vec<OsString>),
    Disabled,
    #[cfg(test)]
    Fake {
        result: Result<(), String>,
        delay: Duration,
    },
}

struct Busy<'a>(&'a AtomicBool);

impl Drop for Busy<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

impl Checker {
    /// Stage uploads in `dir` and test them with `pinnad <args> --check-wake-word`.
    pub fn process(dir: PathBuf, args: Vec<OsString>) -> Self {
        Self::new(dir, Runner::Process(args))
    }

    /// A checker that refuses everything, for a [`DeviceApi`](crate::api::DeviceApi)
    /// that isn't given one.
    pub fn disabled() -> Self {
        Self::new(PathBuf::new(), Runner::Disabled)
    }

    #[cfg(test)]
    pub fn fake(dir: PathBuf, result: Result<(), String>, delay: Duration) -> Self {
        Self::new(dir, Runner::Fake { result, delay })
    }

    fn new(dir: PathBuf, runner: Runner) -> Self {
        Self {
            dir,
            busy: AtomicBool::new(false),
            runner,
        }
    }

    /// Write `model` to a staged file and test it at `threshold`. The file is
    /// the caller's when the model passes; a refusal deletes it. The error is
    /// a sentence for the dashboard.
    pub async fn check(&self, model: &[u8], threshold: f32) -> Result<PathBuf, String> {
        if self.busy.swap(true, Ordering::SeqCst) {
            return Err(BUSY.into());
        }
        let _busy = Busy(&self.busy);
        if matches!(self.runner, Runner::Disabled) {
            return Err("This satellite can't test wake word models.".into());
        }
        let staged = self.stage(model).await?;
        let result = match &self.runner {
            Runner::Process(args) => run_process(args, &staged, threshold).await,
            Runner::Disabled => unreachable!("refused above"),
            #[cfg(test)]
            Runner::Fake { result, delay } => {
                tokio::time::sleep(*delay).await;
                result.clone()
            }
        };
        match result {
            Ok(()) => Ok(staged),
            Err(why) => {
                let _ = std::fs::remove_file(&staged);
                Err(why)
            }
        }
    }

    async fn stage(&self, model: &[u8]) -> Result<PathBuf, String> {
        let fail = |e: std::io::Error| {
            tracing::error!("wake word upload: staging in {}: {e}", self.dir.display());
            "The satellite couldn't save the model to test it.".to_string()
        };
        std::fs::create_dir_all(&self.dir).map_err(fail)?;
        let path = self.dir.join(format!(".upload-{:016x}", fastrand::u64(..)));
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .map_err(fail)?;
        let mut file = tokio::fs::File::from_std(file);
        let written = async {
            file.write_all(model).await?;
            file.sync_all().await
        }
        .await;
        if let Err(e) = written {
            let _ = std::fs::remove_file(&path);
            return Err(fail(e));
        }
        Ok(path)
    }
}

async fn run_process(args: &[OsString], staged: &Path, threshold: f32) -> Result<(), String> {
    let crashed = |e: std::io::Error| {
        tracing::error!("wake word test: {e}");
        "The satellite couldn't start the test.".to_string()
    };
    let exe = std::env::current_exe().map_err(crashed)?;
    let mut cmd = tokio::process::Command::new(exe);
    cmd.args(args)
        .arg("--check-wake-word")
        .arg(staged)
        .arg("--check-threshold")
        .arg(threshold.to_string())
        // No log line may mix into the one line the test prints.
        .env("RUST_LOG", "off")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    // SAFETY: only setrlimit and setpriority run between fork and exec; both
    // are async-signal-safe.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setrlimit(libc::RLIMIT_AS, &rlimit(MEMORY_LIMIT)) != 0
                || libc::setrlimit(libc::RLIMIT_CPU, &rlimit(CPU_LIMIT_SECONDS)) != 0
                || libc::setpriority(libc::PRIO_PROCESS, 0, NICE) != 0
            {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = cmd.spawn().map_err(crashed)?;
    // Dropping the future on a timeout kills the child (kill_on_drop).
    let Ok(output) = tokio::time::timeout(WALL_LIMIT, child.wait_with_output()).await else {
        return Err(TIMED_OUT.into());
    };
    let output = output.map_err(crashed)?;
    let line = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    match output.status.code() {
        Some(0) if line == "ok" => Ok(()),
        Some(2) if !line.is_empty() => {
            let detail = String::from_utf8_lossy(&output.stderr);
            tracing::info!("wake word test refused the model: {}", detail.trim());
            Err(line)
        }
        _ => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let tail: String = stderr
                .chars()
                .rev()
                .take(500)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            tracing::warn!(
                "wake word test ended with {}: {}",
                output.status,
                tail.trim()
            );
            Err(CRASHED.into())
        }
    }
}

fn rlimit(value: u64) -> libc::rlimit {
    libc::rlimit {
        rlim_cur: value as libc::rlim_t,
        rlim_max: value as libc::rlim_t,
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    fn dir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    fn staged_files(dir: &Path) -> usize {
        std::fs::read_dir(dir).unwrap().count()
    }

    #[tokio::test]
    async fn a_passing_model_stays_staged_and_a_refused_one_is_deleted() {
        let d = dir();
        let ok = Checker::fake(d.path().into(), Ok(()), Duration::ZERO);
        let staged = ok.check(b"model", 0.5).await.unwrap();
        assert_eq!(std::fs::read(&staged).unwrap(), b"model");
        assert_eq!(
            std::fs::metadata(&staged).unwrap().permissions().mode() & 0o777,
            0o600
        );

        let refused = Checker::fake(d.path().into(), Err("No.".into()), Duration::ZERO);
        assert_eq!(refused.check(b"model", 0.5).await, Err("No.".into()));
        assert_eq!(staged_files(d.path()), 1);
    }

    #[tokio::test]
    async fn a_second_test_is_refused_while_one_runs() {
        let d = dir();
        let c = Checker::fake(d.path().into(), Ok(()), Duration::from_millis(200));
        let (first, second) = tokio::join!(c.check(b"a", 0.5), async {
            tokio::time::sleep(Duration::from_millis(50)).await;
            c.check(b"b", 0.5).await
        });
        assert!(first.is_ok());
        assert_eq!(second, Err(BUSY.into()));
        // It can test again once the first has finished.
        assert!(c.check(b"c", 0.5).await.is_ok());
    }

    #[tokio::test]
    async fn without_a_runner_everything_is_refused() {
        assert!(Checker::disabled().check(b"a", 0.5).await.is_err());
    }
}
