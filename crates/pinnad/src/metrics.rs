//! Metrics for Prometheus (ADR 0020): `GET /metrics` on the dashboard
//! listener, in the Prometheus text format.
//!
//! The host's numbers are read from /proc and /sys when scraped, named as
//! node_exporter names them. pinnad's come from its status, from counters the
//! brain keeps ([`Counters`]), from what standalone requests report
//! ([`StandaloneCounters`]) and from its own process. A file that can't be
//! read leaves its metrics out; the scrape still succeeds.

use std::borrow::Cow;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use pinna_assist::error::{ProviderError, Slot};
use pinna_assist::observe::{Guard, Observer, Spend};
use pinna_assist::presets::PresetId;
use procfs::FromRead;
use prometheus_client::collector::Collector;
use prometheus_client::encoding::{DescriptorEncoder, EncodeLabelSet, EncodeMetric};
use prometheus_client::metrics::MetricType;
use prometheus_client::metrics::counter::{ConstCounter, Counter};
use prometheus_client::metrics::family::Family;
use prometheus_client::metrics::gauge::ConstGauge;
use prometheus_client::metrics::histogram::Histogram;
use prometheus_client::registry::Registry;
use tokio::sync::watch;

use crate::airplay::State;
use crate::api::Info;
use crate::settings::Mode;
use crate::voice::Snapshot;

pub use prometheus_client::encoding::prometheus_text::CONTENT_TYPE;

/// Seconds from the end of speech to the reply's first audio.
const FIRST_AUDIO_BUCKETS: [f64; 10] = [0.25, 0.5, 0.75, 1.0, 1.5, 2.0, 3.0, 5.0, 8.0, 13.0];

/// Seconds a standalone stage took. An agent may think for two minutes.
const STAGE_BUCKETS: [f64; 12] = [
    0.25, 0.5, 1.0, 2.0, 3.0, 5.0, 8.0, 13.0, 20.0, 30.0, 60.0, 120.0,
];

/// The standalone slots in `AssistStatus::slots` order.
const SLOTS: [Slot; 3] = [Slot::Stt, Slot::Chat, Slot::Tts];

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct Word {
    pub word: String,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct Code {
    pub code: &'static str,
}

/// Home Assistant's Assist error codes worth telling apart. Any other code
/// counts as `other`, so text from Home Assistant can't add series.
const ERROR_CODES: [&str; 8] = [
    "stt-no-text-recognized",
    "stt-stream-failed",
    "stt-provider-missing",
    "intent-failed",
    "intent-not-supported",
    "tts-failed",
    "tts-not-supported",
    "pipeline-not-found",
];

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
struct Failure {
    slot: &'static str,
    reason: &'static str,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
struct Stage {
    slot: &'static str,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
struct Preset {
    preset: &'static str,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
struct Tokens {
    preset: &'static str,
    direction: &'static str,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
struct GuardKind {
    guard: &'static str,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
struct ToolUse {
    tool: Cow<'static, str>,
    outcome: &'static str,
    via: &'static str,
}

/// What the brain counts. Clones share the counts.
#[derive(Clone, Debug)]
pub struct Counters {
    pub wakes: Family<Word, Counter>,
    pub near_misses: Counter,
    pub runs: Counter,
    errors: Family<Code, Counter>,
    pub first_audio: Histogram,
    pub standalone: StandaloneCounters,
    tool_calls: Family<ToolUse, Counter>,
    mic_frames_dropped: Counter,
}

/// Who asked for a tool call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Via {
    /// A standalone run's tool loop. Its label stays "brain".
    Run,
    /// A client of Pinna's MCP server.
    Mcp,
}

impl Counters {
    /// An Assist run ended with an error.
    pub fn error(&self, code: &str) {
        let code = ERROR_CODES
            .iter()
            .find(|c| **c == code)
            .copied()
            .unwrap_or("other");
        self.errors.get_or_create(&Code { code }).inc();
    }

    /// A tool call ended. `tool` is the name as called; `server` is the
    /// owner's MCP server that answered it, if one did.
    pub fn tool_call(&self, tool: &str, server: Option<&str>, via: Via, ok: bool) {
        self.tool_calls
            .get_or_create(&ToolUse {
                tool: tool_label(tool, server),
                outcome: if ok { "ok" } else { "error" },
                via: match via {
                    Via::Run => "brain",
                    Via::Mcp => "mcp",
                },
            })
            .inc();
    }

    /// The capture loop's running total of microphone frames a standalone
    /// request did not take in time. There is one capture loop, so the
    /// counter itself holds the total already counted.
    pub fn mic_dropped(&self, total: u64) {
        let counted = self.mic_frames_dropped.get();
        if total > counted {
            self.mic_frames_dropped.inc_by(total - counted);
        }
    }
}

impl Default for Counters {
    fn default() -> Self {
        Self {
            wakes: Family::default(),
            near_misses: Counter::default(),
            runs: Counter::default(),
            errors: Family::default(),
            first_audio: Histogram::new(FIRST_AUDIO_BUCKETS),
            standalone: StandaloneCounters::default(),
            tool_calls: Family::default(),
            mic_frames_dropped: Counter::default(),
        }
    }
}

/// The `tool` label of a call: a device tool's own name, else the owner's
/// MCP server that answered (a configured name, one of at most eight), else
/// `other`. A name the brain made up or an MCP client sent never becomes a
/// label.
fn tool_label(tool: &str, server: Option<&str>) -> Cow<'static, str> {
    if let Some(own) = crate::tools::device_tool_name(tool) {
        return Cow::Borrowed(own);
    }
    server.map_or(Cow::Borrowed("other"), |server| {
        Cow::Owned(server.to_owned())
    })
}

/// What standalone requests count (ADR 0017). Runs report through
/// [`Observer`]; the brain counts its own guards and failures here as well.
/// Clones share the counts.
#[derive(Clone, Debug)]
pub struct StandaloneCounters {
    errors: Family<Failure, Counter>,
    stages: Family<Stage, Histogram>,
    audio: Family<Preset, Counter<f64, AtomicU64>>,
    tokens: Family<Tokens, Counter>,
    characters: Family<Preset, Counter>,
    guards: Family<GuardKind, Counter>,
}

impl Default for StandaloneCounters {
    fn default() -> Self {
        Self {
            errors: Family::default(),
            stages: Family::new_with_constructor(|| Histogram::new(STAGE_BUCKETS)),
            audio: Family::default(),
            tokens: Family::default(),
            characters: Family::default(),
            guards: Family::default(),
        }
    }
}

#[cfg(test)]
impl StandaloneCounters {
    /// How often `guard` was counted.
    pub fn guard_total(&self, guard: Guard) -> u64 {
        self.guards
            .get_or_create(&GuardKind {
                guard: guard.label(),
            })
            .get()
    }

    /// How often a failure in `slot` for `reason` was counted.
    pub fn failure_total(&self, slot: Slot, reason: pinna_assist::error::Reason) -> u64 {
        self.errors
            .get_or_create(&Failure {
                slot: slot.label(),
                reason: reason.label(),
            })
            .get()
    }
}

impl Observer for StandaloneCounters {
    fn stage(&self, slot: Slot, seconds: f64) {
        self.stages
            .get_or_create(&Stage { slot: slot.label() })
            .observe(seconds);
    }

    fn spent(&self, preset: PresetId, spend: Spend) {
        let preset = preset.label();
        match spend {
            Spend::AudioSeconds(seconds) => {
                // A float counter would go down on a negative value.
                if seconds.is_finite() && seconds > 0.0 {
                    self.audio
                        .get_or_create(&Preset { preset })
                        .inc_by(f64::from(seconds));
                }
            }
            Spend::Tokens { input, output } => {
                for (direction, n) in [("input", input), ("output", output)] {
                    self.tokens
                        .get_or_create(&Tokens { preset, direction })
                        .inc_by(n);
                }
            }
            Spend::Characters(n) => {
                self.characters.get_or_create(&Preset { preset }).inc_by(n);
            }
        }
    }

    fn guard(&self, guard: Guard) {
        tracing::debug!("standalone guard: {}", guard.label());
        self.guards
            .get_or_create(&GuardKind {
                guard: guard.label(),
            })
            .inc();
    }

    fn failed(&self, error: &ProviderError) {
        self.errors
            .get_or_create(&Failure {
                slot: error.slot.label(),
                reason: error.reason.label(),
            })
            .inc();
    }
}

/// Where the collectors read from: the real paths on the satellite, a test
/// directory in tests.
#[derive(Debug, Clone)]
pub struct Sources {
    pub proc: PathBuf,
    pub sys: PathBuf,
    /// pinna-init's `boot-slot` and `boot-version`.
    pub run_dir: PathBuf,
}

impl Default for Sources {
    fn default() -> Self {
        Self {
            proc: "/proc".into(),
            sys: "/sys".into(),
            run_dir: pinna_bootctl::slot::RUN_DIR.into(),
        }
    }
}

/// Everything `/metrics` shows.
pub fn registry(
    info: Arc<Info>,
    status: watch::Receiver<Snapshot>,
    counters: &Counters,
    sources: Sources,
) -> Registry {
    let mut r = Registry::default();
    // The encoder ends registered metrics' help with a full stop.
    r.register(
        "pinna_wakes",
        "Wake word detections, by word",
        counters.wakes.clone(),
    );
    r.register(
        "pinna_wake_near_misses",
        "Utterances that scored close to a wake without one",
        counters.near_misses.clone(),
    );
    r.register(
        "pinna_assist_runs",
        "Assist runs started (wake word, button, dashboard or Home Assistant)",
        counters.runs.clone(),
    );
    r.register(
        "pinna_assist_errors",
        "Assist runs that Home Assistant ended with an error, by its error code",
        counters.errors.clone(),
    );
    r.register(
        "pinna_reply_first_audio_seconds",
        "Time from the end of speech to the reply's first audio",
        counters.first_audio.clone(),
    );
    let s = &counters.standalone;
    r.register(
        "pinna_standalone_errors",
        "Standalone requests that failed, by slot and reason",
        s.errors.clone(),
    );
    r.register(
        "pinna_standalone_stage_seconds",
        "Time a standalone stage took: speech-to-text, the brain's first text, text-to-speech's first audio",
        s.stages.clone(),
    );
    r.register(
        "pinna_standalone_audio_seconds",
        "Seconds of speech sent to speech-to-text, by preset",
        s.audio.clone(),
    );
    r.register(
        "pinna_standalone_tokens",
        "Tokens the brain read (input) and wrote (output), by preset",
        s.tokens.clone(),
    );
    r.register(
        "pinna_standalone_characters",
        "Characters sent to text-to-speech, by preset",
        s.characters.clone(),
    );
    r.register(
        "pinna_standalone_guards",
        "Times a standalone guard acted: no speech, an invented transcript, a follow-up, an acknowledgment, or a stop by the wake word or the button",
        s.guards.clone(),
    );
    r.register(
        "pinna_tool_calls",
        "Tool calls from the brain or an MCP client, by tool and outcome",
        counters.tool_calls.clone(),
    );
    r.register(
        "pinna_mic_frames_dropped",
        "Microphone frames a standalone request did not take in time",
        counters.mic_frames_dropped.clone(),
    );
    r.register_collector(Box::new(Status {
        info,
        status,
        run_dir: sources.run_dir.clone(),
    }));
    r.register_collector(Box::new(Host(sources)));
    r.register_collector(Box::new(Process));
    r
}

#[cfg(test)]
impl Counters {
    /// The `pinna_tool_calls` family as `/metrics` renders it, for tests of
    /// the callers.
    pub fn tool_calls_text(&self) -> String {
        let mut r = Registry::default();
        r.register("pinna_tool_calls", "", self.tool_calls.clone());
        render(&r)
    }
}

/// The text `/metrics` answers with.
pub fn render(registry: &Registry) -> String {
    let mut out = String::new();
    // Writing to a String only fails when a collector returns an error, which
    // ours don't.
    if let Err(e) = prometheus_client::encoding::prometheus_text::encode(&mut out, registry) {
        tracing::warn!("metrics: encoding failed: {e}");
    }
    out
}

// --- writing ------------------------------------------------------------------

type Labels<'a> = Vec<(&'static str, &'a str)>;

/// One metric family with any number of samples.
fn family<'a>(
    enc: &mut DescriptorEncoder,
    name: &str,
    help: &str,
    kind: MetricType,
    samples: impl IntoIterator<Item = (Labels<'a>, f64)>,
) -> fmt::Result {
    let mut samples = samples.into_iter().peekable();
    if samples.peek().is_none() {
        return Ok(());
    }
    let mut m = enc.encode_descriptor(name, help, None, kind)?;
    for (labels, v) in samples {
        let e = m.encode_family(&labels)?;
        match kind {
            MetricType::Counter => ConstCounter::new(v).encode(e)?,
            _ => ConstGauge::new(v).encode(e)?,
        }
    }
    Ok(())
}

fn gauge<'a>(
    enc: &mut DescriptorEncoder,
    name: &str,
    help: &str,
    samples: impl IntoIterator<Item = (Labels<'a>, f64)>,
) -> fmt::Result {
    family(enc, name, help, MetricType::Gauge, samples)
}

/// A counter: `name` without `_total`, which the encoder appends.
fn counter<'a>(
    enc: &mut DescriptorEncoder,
    name: &str,
    help: &str,
    samples: impl IntoIterator<Item = (Labels<'a>, f64)>,
) -> fmt::Result {
    family(enc, name, help, MetricType::Counter, samples)
}

fn one(v: f64) -> Option<(Labels<'static>, f64)> {
    Some((Vec::new(), v))
}

fn flag(b: bool) -> f64 {
    if b { 1.0 } else { 0.0 }
}

fn read(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

fn parse<T: FromRead>(path: &Path) -> Option<T> {
    T::from_file(path).ok()
}

// --- pinnad's status ----------------------------------------------------------

#[derive(Debug)]
struct Status {
    info: Arc<Info>,
    status: watch::Receiver<Snapshot>,
    run_dir: PathBuf,
}

fn state_name(s: &State) -> &'static str {
    match s {
        State::Off => "off",
        State::Starting => "starting",
        State::Ready => "ready",
        State::Failed(_) => "failed",
        State::Unavailable => "unavailable",
    }
}

const STATES: [&str; 5] = ["off", "starting", "ready", "failed", "unavailable"];

impl Collector for Status {
    fn encode(&self, mut enc: DescriptorEncoder) -> fmt::Result {
        let s = self.status.borrow().clone();
        let slot = pinna_bootctl::slot::booted_slot(&self.run_dir)
            .ok()
            .flatten()
            .map(|s| s.to_string())
            .unwrap_or_default();
        gauge(
            &mut enc,
            "pinna_build_info",
            "The running image: version, device model and A/B slot.",
            [(
                vec![
                    ("version", self.info.version.as_str()),
                    ("model", self.info.model.as_str()),
                    ("slot", slot.as_str()),
                ],
                1.0,
            )],
        )?;
        let outcome = format!("{:?}", self.info.last_boot.outcome).to_lowercase();
        gauge(
            &mut enc,
            "pinna_last_boot_info",
            "How the boot before this one ended: cold, clean, rescue, crashed or reset.",
            [(vec![("outcome", outcome.as_str())], 1.0)],
        )?;
        gauge(
            &mut enc,
            "pinna_microphone_muted",
            "The microphones are muted, by the hardware switch or the software one.",
            [
                (vec![("switch", "hardware")], flag(s.hw_muted)),
                (vec![("switch", "software")], flag(s.sw_muted)),
            ],
        )?;
        gauge(
            &mut enc,
            "pinna_home_assistant_connected",
            "An ESPHome API client (Home Assistant) is connected.",
            one(flag(s.ha_connected)),
        )?;
        gauge(
            &mut enc,
            "pinna_voice_subscribed",
            "Home Assistant subscribed to the voice assistant.",
            one(flag(s.voice_subscribed)),
        )?;
        gauge(
            &mut enc,
            "pinna_standalone_mode",
            "Pinna answers requests itself (standalone mode), not through Home Assistant.",
            one(flag(s.mode == Mode::Standalone)),
        )?;
        gauge(
            &mut enc,
            "pinna_standalone_slot_ready",
            "A standalone slot (speech-to-text, brain, text-to-speech) is set up and its check passed.",
            SLOTS.iter().map(|slot| {
                (
                    vec![("slot", slot.label())],
                    flag(s.assist.slots.get(*slot)),
                )
            }),
        )?;
        gauge(
            &mut enc,
            "pinna_wake_word_enabled",
            "The wake word is on and at least one word is chosen.",
            one(flag(
                s.settings.wake_word && !s.active_wake_words.is_empty(),
            )),
        )?;
        gauge(
            &mut enc,
            "pinna_timers",
            "Timers set on the satellite, running or paused.",
            [true, false].map(|running| {
                let n = s.timers.iter().filter(|t| t.active == running).count();
                (
                    vec![("state", if running { "running" } else { "paused" })],
                    n as f64,
                )
            }),
        )?;
        gauge(
            &mut enc,
            "pinna_alarm_ringing",
            "A finished timer's alarm is sounding.",
            one(flag(s.alarm_ringing)),
        )?;
        for (name, help, state) in [
            (
                "pinna_airplay_state",
                "1 for the state the AirPlay receiver is in.",
                &s.airplay.state,
            ),
            (
                "pinna_bluetooth_state",
                "1 for the state the Bluetooth bridge is in.",
                &s.bluetooth.state,
            ),
        ] {
            let now = state_name(state);
            gauge(
                &mut enc,
                name,
                help,
                STATES.map(|st| (vec![("state", st)], flag(st == now))),
            )?;
        }
        gauge(
            &mut enc,
            "pinna_bluetooth_paired_devices",
            "Phones paired over Bluetooth.",
            one(s.bluetooth.devices.len() as f64),
        )?;
        counter(
            &mut enc,
            "pinna_speaker_underruns",
            "Times the speaker ran out of audio and restarted.",
            one(pinna_audio::speaker::underruns() as f64),
        )
    }
}

// --- the host -----------------------------------------------------------------

#[derive(Debug)]
struct Host(Sources);

/// File systems worth reporting: those on a block device.
const REAL_FS: [&str; 6] = ["ext4", "f2fs", "erofs", "vfat", "btrfs", "xfs"];

impl Collector for Host {
    fn encode(&self, mut enc: DescriptorEncoder) -> fmt::Result {
        let proc = &self.0.proc;
        let sys = &self.0.sys;
        uname(&mut enc)?;
        if let Ok(now) = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
            gauge(
                &mut enc,
                "node_time_seconds",
                "System time in seconds since epoch (1970).",
                one(now.as_secs_f64()),
            )?;
        }
        if let Some(stat) = read(&proc.join("stat")) {
            cpu(&mut enc, &stat, procfs::ticks_per_second() as f64)?;
        }
        if let Some(l) = parse::<procfs::LoadAverage>(&proc.join("loadavg")) {
            for (name, v) in [
                ("node_load1", l.one),
                ("node_load5", l.five),
                ("node_load15", l.fifteen),
            ] {
                gauge(&mut enc, name, "Load average.", one(f64::from(v)))?;
            }
        }
        if let Some(m) = parse::<procfs::Meminfo>(&proc.join("meminfo")) {
            memory(&mut enc, &m)?;
        }
        pressure(&mut enc, &proc.join("pressure"))?;
        if let Some(mounts) = read(&proc.join("mounts")) {
            filesystems(&mut enc, &mounts)?;
        }
        if let Some(dev) = read(&proc.join("net/dev")) {
            network(&mut enc, &dev)?;
        }
        if let Some(w) = read(&proc.join("net/wireless")) {
            gauge(
                &mut enc,
                "pinna_wifi_signal_dbm",
                "Wi-Fi signal level of the connected network.",
                parse_wireless(&w)
                    .iter()
                    .map(|(dev, dbm)| (vec![("device", dev.as_str())], *dbm)),
            )?;
        }
        thermal(&mut enc, &sys.join("class/thermal"))?;
        units(&mut enc, &sys.join("fs/cgroup/system.slice"))
    }
}

/// `node_uname_info`: Grafana's node dashboards pick hosts by it.
fn uname(enc: &mut DescriptorEncoder) -> fmt::Result {
    let mut u = std::mem::MaybeUninit::<libc::utsname>::uninit();
    // SAFETY: uname fills the buffer on success.
    if unsafe { libc::uname(u.as_mut_ptr()) } != 0 {
        return Ok(());
    }
    // SAFETY: uname returned 0, so the buffer is filled.
    let u = unsafe { u.assume_init() };
    let field = |f: &[libc::c_char]| {
        // SAFETY: utsname fields are NUL-terminated.
        unsafe { std::ffi::CStr::from_ptr(f.as_ptr()) }
            .to_string_lossy()
            .into_owned()
    };
    let (sysname, release, version, machine, nodename, domainname) = (
        field(&u.sysname),
        field(&u.release),
        field(&u.version),
        field(&u.machine),
        field(&u.nodename),
        field(&u.domainname),
    );
    gauge(
        enc,
        "node_uname_info",
        "Labeled system information as provided by the uname system call.",
        [(
            vec![
                ("sysname", sysname.as_str()),
                ("release", release.as_str()),
                ("version", version.as_str()),
                ("machine", machine.as_str()),
                ("nodename", nodename.as_str()),
                ("domainname", domainname.as_str()),
            ],
            1.0,
        )],
    )
}

/// `node_cpu_seconds_total`, `node_boot_time_seconds`, from /proc/stat. Read
/// here rather than through procfs, which drops the CPU numbers.
fn cpu(enc: &mut DescriptorEncoder, stat: &str, ticks: f64) -> fmt::Result {
    const MODES: [&str; 8] = [
        "user", "nice", "system", "idle", "iowait", "irq", "softirq", "steal",
    ];
    let mut samples = Vec::new();
    let mut boot = None;
    for line in stat.lines() {
        let mut f = line.split_whitespace();
        let Some(key) = f.next() else { continue };
        if key == "btime" {
            boot = f.next().and_then(|v| v.parse::<f64>().ok());
        }
        let Some(n) = key.strip_prefix("cpu").filter(|n| !n.is_empty()) else {
            continue;
        };
        for (mode, v) in MODES.iter().zip(f) {
            if let Ok(v) = v.parse::<f64>() {
                samples.push((n.to_owned(), *mode, v / ticks));
            }
        }
    }
    counter(
        enc,
        "node_cpu_seconds",
        "Seconds the CPUs spent in each mode.",
        samples
            .iter()
            .map(|(n, mode, v)| (vec![("cpu", n.as_str()), ("mode", *mode)], *v)),
    )?;
    gauge(
        enc,
        "node_boot_time_seconds",
        "Node boot time, in unixtime.",
        boot.and_then(one),
    )
}

fn memory(enc: &mut DescriptorEncoder, m: &procfs::Meminfo) -> fmt::Result {
    for (name, v) in [
        ("MemTotal", Some(m.mem_total)),
        ("MemFree", Some(m.mem_free)),
        ("MemAvailable", m.mem_available),
        ("Buffers", Some(m.buffers)),
        ("Cached", Some(m.cached)),
        ("SwapTotal", Some(m.swap_total)),
        ("SwapFree", Some(m.swap_free)),
    ] {
        if let Some(v) = v {
            gauge(
                enc,
                &format!("node_memory_{name}_bytes"),
                &format!("Memory information field {name}_bytes."),
                one(v as f64),
            )?;
        }
    }
    Ok(())
}

/// Pressure stall information: seconds some (or all) tasks waited.
fn pressure(enc: &mut DescriptorEncoder, dir: &Path) -> fmt::Result {
    let secs = |r: &procfs::PressureRecord| r.total as f64 / 1e6;
    if let Some(p) = parse::<procfs::CpuPressure>(&dir.join("cpu")) {
        counter(
            enc,
            "node_pressure_cpu_waiting_seconds",
            "Total time in seconds that processes have waited for CPU time.",
            one(secs(&p.some)),
        )?;
    }
    if let Some(p) = parse::<procfs::MemoryPressure>(&dir.join("memory")) {
        counter(
            enc,
            "node_pressure_memory_waiting_seconds",
            "Total time in seconds that processes have waited for memory.",
            one(secs(&p.some)),
        )?;
        counter(
            enc,
            "node_pressure_memory_stalled_seconds",
            "Total time in seconds no process could make progress due to memory congestion.",
            one(secs(&p.full)),
        )?;
    }
    if let Some(p) = parse::<procfs::IoPressure>(&dir.join("io")) {
        counter(
            enc,
            "node_pressure_io_waiting_seconds",
            "Total time in seconds that processes have waited due to IO congestion.",
            one(secs(&p.some)),
        )?;
        counter(
            enc,
            "node_pressure_io_stalled_seconds",
            "Total time in seconds no process could make progress due to IO congestion.",
            one(secs(&p.full)),
        )?;
    }
    Ok(())
}

/// Size and free space of the file systems on block devices.
fn filesystems(enc: &mut DescriptorEncoder, mounts: &str) -> fmt::Result {
    let mut sizes = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for line in mounts.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        let [dev, mnt, fstype, ..] = f[..] else {
            continue;
        };
        if !REAL_FS.contains(&fstype) || !seen.insert(mnt) {
            continue;
        }
        // /proc/mounts escapes spaces in paths as \040.
        let path = mnt.replace("\\040", " ");
        if let Some((size, avail)) = statvfs(&path) {
            sizes.push((dev, mnt, fstype, size, avail));
        }
    }
    fn labels<'a>(d: &'a str, m: &'a str, t: &'a str) -> Labels<'a> {
        vec![("device", d), ("mountpoint", m), ("fstype", t)]
    }
    gauge(
        enc,
        "node_filesystem_size_bytes",
        "Filesystem size in bytes.",
        sizes
            .iter()
            .map(|(d, m, t, size, _)| (labels(d, m, t), *size as f64)),
    )?;
    gauge(
        enc,
        "node_filesystem_avail_bytes",
        "Filesystem space available to non-root users in bytes.",
        sizes
            .iter()
            .map(|(d, m, t, _, avail)| (labels(d, m, t), *avail as f64)),
    )
}

// The field types differ by target: 32 bits on the satellite's armv7.
#[allow(clippy::unnecessary_cast)]
fn statvfs(path: &str) -> Option<(u64, u64)> {
    let c = std::ffi::CString::new(path).ok()?;
    let mut s = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: a valid C string and a buffer statvfs fills on success.
    if unsafe { libc::statvfs(c.as_ptr(), s.as_mut_ptr()) } != 0 {
        return None;
    }
    // SAFETY: statvfs returned 0, so it filled the buffer.
    let s = unsafe { s.assume_init() };
    let frsize = s.f_frsize as u64;
    Some((s.f_blocks as u64 * frsize, s.f_bavail as u64 * frsize))
}

/// Traffic and errors per interface, without loopback.
fn network(enc: &mut DescriptorEncoder, dev: &str) -> fmt::Result {
    let Ok(stats) = procfs::net::InterfaceDeviceStatus::from_read(dev.as_bytes()) else {
        return Ok(());
    };
    let mut ifs: Vec<_> = stats.0.values().filter(|d| d.name != "lo").collect();
    ifs.sort_by(|a, b| a.name.cmp(&b.name));
    type Field = fn(&procfs::net::DeviceStatus) -> u64;
    let fields: [(&str, &str, Field); 6] = [
        ("node_network_receive_bytes", "Bytes received.", |d| {
            d.recv_bytes
        }),
        ("node_network_transmit_bytes", "Bytes sent.", |d| {
            d.sent_bytes
        }),
        ("node_network_receive_errs", "Receive errors.", |d| {
            d.recv_errs
        }),
        ("node_network_transmit_errs", "Transmit errors.", |d| {
            d.sent_errs
        }),
        (
            "node_network_receive_drop",
            "Received packets dropped.",
            |d| d.recv_drop,
        ),
        ("node_network_transmit_drop", "Sent packets dropped.", |d| {
            d.sent_drop
        }),
    ];
    for (name, help, get) in fields {
        counter(
            enc,
            name,
            help,
            ifs.iter()
                .map(|d| (vec![("device", d.name.as_str())], get(d) as f64)),
        )?;
    }
    Ok(())
}

/// The signal level of each connected interface in /proc/net/wireless.
fn parse_wireless(text: &str) -> Vec<(String, f64)> {
    text.lines()
        .skip(2)
        .filter_map(|line| {
            let (dev, rest) = line.split_once(':')?;
            // status, link quality, signal level (dBm), noise, ...
            let level = rest.split_whitespace().nth(2)?;
            let level = level.trim_end_matches('.').parse::<f64>().ok()?;
            // The MT8163 Wi-Fi driver reports dBm as an unsigned byte without
            // IW_QUAL_DBM, so the kernel prints it unsigned: 208 and 210 on the
            // Echo Dot (2nd gen) and the Echo (2nd gen) while `iw` showed -48
            // and -46 dBm. Like wireless-tools, read 64 and above as negative.
            let dbm = if level >= 64.0 { level - 256.0 } else { level };
            // A level of 0 or above is no measurement (no link), not a signal.
            (dbm < 0.0).then(|| (dev.trim().to_owned(), dbm))
        })
        .collect()
}

/// `node_thermal_zone_temp`: each thermal zone, in °C.
fn thermal(enc: &mut DescriptorEncoder, dir: &Path) -> fmt::Result {
    let mut zones = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let Some(n) = name.strip_prefix("thermal_zone") else {
                continue;
            };
            let p = e.path();
            let Some(temp) = read(&p.join("temp")).and_then(|t| t.trim().parse::<f64>().ok())
            else {
                continue;
            };
            let kind = read(&p.join("type")).unwrap_or_default();
            zones.push((n.to_owned(), kind.trim().to_owned(), temp / 1000.0));
        }
    }
    zones.sort_by(|a, b| a.0.cmp(&b.0));
    gauge(
        enc,
        "node_thermal_zone_temp",
        "Zone temperature in Celsius.",
        zones
            .iter()
            .map(|(z, t, c)| (vec![("zone", z.as_str()), ("type", t.as_str())], *c)),
    )
}

/// Memory and CPU of each systemd service, from its cgroup.
fn units(enc: &mut DescriptorEncoder, slice: &Path) -> fmt::Result {
    let mut found = Vec::new();
    if let Ok(entries) = std::fs::read_dir(slice) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if !name.ends_with(".service") {
                continue;
            }
            let p = e.path();
            let mem = read(&p.join("memory.current")).and_then(|v| v.trim().parse::<f64>().ok());
            let cpu = read(&p.join("cpu.stat")).and_then(|s| {
                s.lines()
                    .find_map(|l| l.strip_prefix("usage_usec "))
                    .and_then(|v| v.trim().parse::<f64>().ok())
                    .map(|us| us / 1e6)
            });
            let max = read(&p.join("memory.max")).and_then(|v| v.trim().parse::<f64>().ok());
            let ooms = read(&p.join("memory.events")).and_then(|s| {
                s.lines()
                    .find_map(|l| l.strip_prefix("oom_kill "))
                    .and_then(|v| v.trim().parse::<f64>().ok())
            });
            found.push(Unit {
                name,
                mem,
                max,
                cpu,
                ooms,
            });
        }
    }
    found.sort_by(|a, b| a.name.cmp(&b.name));
    type Field = fn(&Unit) -> Option<f64>;
    let fields: [(&str, &str, MetricType, Field); 4] = [
        (
            "pinna_unit_memory_bytes",
            "Memory a systemd service uses, from its cgroup.",
            MetricType::Gauge,
            |u| u.mem,
        ),
        (
            "pinna_unit_memory_max_bytes",
            "A systemd service's memory limit (MemoryMax=), where it has one.",
            MetricType::Gauge,
            |u| u.max,
        ),
        (
            "pinna_unit_cpu_seconds",
            "CPU time a systemd service used, from its cgroup.",
            MetricType::Counter,
            |u| u.cpu,
        ),
        (
            "pinna_unit_oom_kills",
            "Processes of a systemd service the kernel's OOM killer stopped, at the service's limit or with the device out of memory.",
            MetricType::Counter,
            |u| u.ooms,
        ),
    ];
    for (name, help, kind, get) in fields {
        family(
            enc,
            name,
            help,
            kind,
            found
                .iter()
                .filter_map(|u| Some((vec![("unit", u.name.as_str())], get(u)?))),
        )?;
    }
    Ok(())
}

struct Unit {
    name: String,
    mem: Option<f64>,
    /// `None` without a limit (`max`).
    max: Option<f64>,
    cpu: Option<f64>,
    ooms: Option<f64>,
}

// --- pinnad's own process -----------------------------------------------------

#[derive(Debug)]
struct Process;

impl Collector for Process {
    fn encode(&self, mut enc: DescriptorEncoder) -> fmt::Result {
        let Ok(me) = procfs::process::Process::myself() else {
            return Ok(());
        };
        if let Ok(stat) = me.stat() {
            let ticks = procfs::ticks_per_second() as f64;
            counter(
                &mut enc,
                "process_cpu_seconds",
                "Total user and system CPU time spent in seconds.",
                one((stat.utime + stat.stime) as f64 / ticks),
            )?;
            gauge(
                &mut enc,
                "process_resident_memory_bytes",
                "Resident memory size in bytes.",
                one((stat.rss * procfs::page_size()) as f64),
            )?;
            gauge(
                &mut enc,
                "process_virtual_memory_bytes",
                "Virtual memory size in bytes.",
                one(stat.vsize as f64),
            )?;
            gauge(
                &mut enc,
                "process_threads",
                "Number of OS threads in the process.",
                one(stat.num_threads as f64),
            )?;
            if let Ok(boot) = procfs::boot_time_secs() {
                gauge(
                    &mut enc,
                    "process_start_time_seconds",
                    "Start time of the process since unix epoch in seconds.",
                    one(boot as f64 + stat.starttime as f64 / ticks),
                )?;
            }
        }
        if let Ok(n) = me.fd_count() {
            gauge(
                &mut enc,
                "process_open_fds",
                "Number of open file descriptors.",
                one(n as f64),
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::boot;

    const STAT: &str = "\
cpu  1000 20 300 5000 40 0 6 0 0 0
cpu0 250 5 75 1250 10 0 2 0 0 0
cpu2 250 5 75 1250 10 0 1 0 0 0
intr 12345
ctxt 6789
btime 1763300000
processes 400
";

    const MEMINFO: &str = "\
MemTotal:         481000 kB
MemFree:           52000 kB
MemAvailable:     240000 kB
Buffers:            8000 kB
Cached:           160000 kB
SwapCached:            0 kB
Active:           120000 kB
Inactive:         150000 kB
SwapTotal:             0 kB
SwapFree:              0 kB
Dirty:                 4 kB
Writeback:             0 kB
Mapped:            60000 kB
Slab:              20000 kB
Committed_AS:     300000 kB
VmallocTotal:     245760 kB
VmallocUsed:        5000 kB
VmallocChunk:          0 kB
";

    const NET_DEV: &str = "\
Inter-|   Receive                                                |  Transmit
 face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed
    lo:    1000      10    0    0    0     0          0         0     1000      10    0    0    0     0       0          0
 wlan0: 5000000    4000    3    7    0     0          0        12  2000000    3000    1    2    0     0       0          0
";

    const WIRELESS: &str = "\
Inter-| sta-|   Quality        |   Discarded packets               | Missed | WE
 face | tus | link level noise |  nwid  crypt   frag  retry   misc | beacon | 22
 wlan0: 0000   54.  -56.  -256        0      0      0      0      0        0
  p2p0: 0000    0.    0.  -256        0      0      0      0      0        0
";

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    /// A /proc and /sys as the satellite has them, and a mount at `dir`.
    fn sources(dir: &Path) -> Sources {
        let (proc, sys) = (dir.join("proc"), dir.join("sys"));
        write(&proc.join("stat"), STAT);
        write(&proc.join("loadavg"), "0.52 0.41 0.30 2/180 1234\n");
        write(&proc.join("meminfo"), MEMINFO);
        write(
            &proc.join("pressure/memory"),
            "some avg10=0.00 avg60=0.00 avg300=0.00 total=2500000\n\
             full avg10=0.00 avg60=0.00 avg300=0.00 total=500000\n",
        );
        write(&proc.join("net/dev"), NET_DEV);
        write(&proc.join("net/wireless"), WIRELESS);
        write(
            &proc.join("mounts"),
            &format!(
                "proc /proc proc rw 0 0\n\
                 /dev/mmcblk0p20 {} ext4 rw 0 0\n\
                 tmpfs /run tmpfs rw 0 0\n",
                dir.display()
            ),
        );
        let zone = sys.join("class/thermal/thermal_zone0");
        write(&zone.join("temp"), "47500\n");
        write(&zone.join("type"), "cpu_thermal\n");
        let unit = sys.join("fs/cgroup/system.slice/pinna-bt.service");
        write(&unit.join("memory.current"), "9437184\n");
        write(&unit.join("memory.max"), "33554432\n");
        write(
            &unit.join("cpu.stat"),
            "usage_usec 2500000\nuser_usec 2000000\n",
        );
        write(
            &unit.join("memory.events"),
            "low 0\nhigh 0\nmax 4\noom 1\noom_kill 1\n",
        );
        let unit = sys.join("fs/cgroup/system.slice/pinnad.service");
        write(&unit.join("memory.current"), "41943040\n");
        write(&unit.join("memory.max"), "max\n");
        Sources {
            proc,
            sys,
            run_dir: dir.join("run"),
        }
    }

    fn info() -> Info {
        Info {
            version: "1.2.3".into(),
            mac: String::new(),
            time_zones: Arc::new(vec!["UTC".into()]),
            wake_words: crate::wakewords::test_catalogue(),
            audio_backend: "none".into(),
            led_backend: "none".into(),
            model: "echo-dot-2".into(),
            virtual_device: false,
            last_boot: boot::LastBoot::cold(),
            key_from_image: false,
        }
    }

    fn scrape(snapshot: Snapshot, counters: &Counters, dir: &Path) -> String {
        let (_tx, rx) = watch::channel(snapshot);
        render(&registry(Arc::new(info()), rx, counters, sources(dir)))
    }

    fn has(text: &str, line: &str) {
        assert!(text.lines().any(|l| l == line), "no {line:?} in\n{text}");
    }

    #[test]
    fn reads_the_host_like_node_exporter() {
        let dir = tempfile::tempdir().unwrap();
        let t = scrape(Snapshot::default(), &Counters::default(), dir.path());
        let ticks = procfs::ticks_per_second() as f64;
        has(
            &t,
            &format!(
                "node_cpu_seconds_total{{cpu=\"2\",mode=\"user\"}} {:?}",
                250.0 / ticks
            ),
        );
        // The total line is not a CPU.
        assert!(!t.contains("cpu=\"\""), "{t}");
        has(&t, "node_boot_time_seconds 1763300000.0");
        has(&t, "node_load1 0.5199999809265137");
        has(&t, "node_memory_MemAvailable_bytes 245760000.0");
        has(&t, "node_pressure_memory_waiting_seconds_total 2.5");
        has(&t, "node_pressure_memory_stalled_seconds_total 0.5");
        // No CPU or IO pressure files: no such metrics, and no failure.
        assert!(!t.contains("node_pressure_cpu"), "{t}");
        has(
            &t,
            "node_network_receive_bytes_total{device=\"wlan0\"} 5000000.0",
        );
        has(&t, "node_network_transmit_drop_total{device=\"wlan0\"} 2.0");
        assert!(!t.contains("device=\"lo\""), "{t}");
        let fs = format!(
            "{{device=\"/dev/mmcblk0p20\",mountpoint=\"{}\",fstype=\"ext4\"}}",
            dir.path().display()
        );
        assert!(
            t.contains(&format!("node_filesystem_size_bytes{fs} ")),
            "{t}"
        );
        assert!(
            t.contains(&format!("node_filesystem_avail_bytes{fs} ")),
            "{t}"
        );
        assert!(!t.contains("tmpfs"), "{t}");
        has(
            &t,
            "node_thermal_zone_temp{zone=\"0\",type=\"cpu_thermal\"} 47.5",
        );
        assert!(t.contains("node_uname_info{sysname=\"Linux\""), "{t}");
        assert!(t.contains("\nnode_time_seconds 1"), "{t}");
    }

    #[test]
    fn reads_the_services_cgroups() {
        let dir = tempfile::tempdir().unwrap();
        let t = scrape(Snapshot::default(), &Counters::default(), dir.path());
        has(
            &t,
            "pinna_unit_memory_bytes{unit=\"pinna-bt.service\"} 9437184.0",
        );
        has(
            &t,
            "pinna_unit_memory_bytes{unit=\"pinnad.service\"} 41943040.0",
        );
        has(
            &t,
            "pinna_unit_memory_max_bytes{unit=\"pinna-bt.service\"} 33554432.0",
        );
        // No limit: no series, rather than an infinite one.
        assert!(
            !t.contains("pinna_unit_memory_max_bytes{unit=\"pinnad.service\"}"),
            "{t}"
        );
        has(
            &t,
            "pinna_unit_cpu_seconds_total{unit=\"pinna-bt.service\"} 2.5",
        );
        has(
            &t,
            "pinna_unit_oom_kills_total{unit=\"pinna-bt.service\"} 1.0",
        );
    }

    #[test]
    fn wifi_signal_only_with_a_link() {
        assert_eq!(parse_wireless(WIRELESS), vec![("wlan0".into(), -56.0)]);
        let dir = tempfile::tempdir().unwrap();
        let t = scrape(Snapshot::default(), &Counters::default(), dir.path());
        has(&t, "pinna_wifi_signal_dbm{device=\"wlan0\"} -56.0");
        assert!(!t.contains("p2p0"), "{t}");
    }

    #[test]
    fn wifi_signal_from_an_unsigned_level() {
        // The Echo Dot (2nd gen), while `iw dev wlan0 link` showed -48 dBm.
        let dot = "\
Inter-| sta-|   Quality        |   Discarded packets               | Missed | WE
 face | tus | link level noise |  nwid  crypt   frag  retry   misc | beacon | 22
 wlan0: 0000    0.  208.    0.       0      0      0      0      0        0
";
        assert_eq!(parse_wireless(dot), vec![("wlan0".into(), -48.0)]);
    }

    #[test]
    fn missing_files_leave_metrics_out() {
        let dir = tempfile::tempdir().unwrap();
        let s = Sources {
            proc: dir.path().join("none"),
            sys: dir.path().join("none"),
            run_dir: dir.path().join("none"),
        };
        let (_tx, rx) = watch::channel(Snapshot::default());
        let t = render(&registry(Arc::new(info()), rx, &Counters::default(), s));
        assert!(!t.contains("node_cpu"), "{t}");
        assert!(!t.contains("node_memory"), "{t}");
        assert!(!t.contains("pinna_unit"), "{t}");
        // pinnad's own metrics stay.
        has(&t, "pinna_home_assistant_connected 0.0");
    }

    #[test]
    fn reports_pinnad() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("run/boot-slot"), "b\n");
        let s = Snapshot {
            hw_muted: true,
            ha_connected: true,
            airplay: crate::airplay::Report {
                state: State::Failed("no socket".into()),
                ..Default::default()
            },
            ..Default::default()
        };
        let c = Counters::default();
        c.wakes
            .get_or_create(&Word {
                word: "alexa_v0.1".into(),
            })
            .inc();
        c.runs.inc();
        c.error("stt-no-text-recognized");
        c.error("made up <script>");
        c.first_audio.observe(1.2);
        let t = scrape(s, &c, dir.path());
        has(
            &t,
            "pinna_build_info{version=\"1.2.3\",model=\"echo-dot-2\",slot=\"b\"} 1.0",
        );
        has(&t, "pinna_last_boot_info{outcome=\"cold\"} 1.0");
        has(&t, "pinna_microphone_muted{switch=\"hardware\"} 1.0");
        has(&t, "pinna_microphone_muted{switch=\"software\"} 0.0");
        has(&t, "pinna_home_assistant_connected 1.0");
        // The setting, not the detector's state, which drops during a request.
        has(&t, "pinna_wake_word_enabled 0.0");
        has(&t, "pinna_airplay_state{state=\"failed\"} 1.0");
        has(&t, "pinna_airplay_state{state=\"off\"} 0.0");
        has(&t, "pinna_wakes_total{word=\"alexa_v0.1\"} 1");
        has(&t, "pinna_assist_runs_total 1");
        has(
            &t,
            "pinna_assist_errors_total{code=\"stt-no-text-recognized\"} 1",
        );
        has(&t, "pinna_assist_errors_total{code=\"other\"} 1");
        assert!(!t.contains("made up"), "{t}");
        has(&t, "pinna_reply_first_audio_seconds_bucket{le=\"1.5\"} 1");
        has(&t, "pinna_reply_first_audio_seconds_bucket{le=\"1\"} 0");
        has(&t, "# TYPE pinna_speaker_underruns_total counter");
        // What changes faster than a scrape, and says who is home, is left out.
        assert!(!t.contains("pinna_phase"), "{t}");
        assert!(!t.contains("pinna_volume"), "{t}");
        // pinnad's own process.
        assert!(t.contains("\nprocess_resident_memory_bytes "), "{t}");
        assert!(t.contains("\nprocess_open_fds "), "{t}");
    }

    #[test]
    fn standalone_families_render() {
        use pinna_assist::error::Reason;

        let dir = tempfile::tempdir().unwrap();
        let c = Counters::default();
        let o: &dyn Observer = &c.standalone;
        o.failed(&ProviderError::new(
            Slot::Chat,
            PresetId::OpenRouter,
            Reason::Http(402),
        ));
        o.failed(&ProviderError::new(
            Slot::Stt,
            PresetId::OpenAi,
            Reason::Timeout,
        ));
        o.stage(Slot::Stt, 0.8);
        o.stage(Slot::Chat, 1.5);
        o.stage(Slot::Tts, 0.4);
        o.spent(PresetId::OpenAi, Spend::AudioSeconds(2.5));
        o.spent(PresetId::OpenAi, Spend::AudioSeconds(1.0));
        o.spent(
            PresetId::Anthropic,
            Spend::Tokens {
                input: 1200,
                output: 80,
            },
        );
        o.spent(PresetId::ElevenLabs, Spend::Characters(42));
        for g in [
            Guard::NoSpeech,
            Guard::Invented,
            Guard::FollowUp,
            Guard::Ack,
            Guard::StoppedByWake,
            Guard::StoppedByButton,
            Guard::Ack,
        ] {
            o.guard(g);
        }
        c.tool_call("set_volume", None, Via::Run, true);
        c.tool_call("set_volume", None, Via::Run, true);
        c.tool_call("play_url", None, Via::Mcp, false);
        c.mic_dropped(3);
        let s = Snapshot {
            mode: Mode::Standalone,
            assist: crate::voice::AssistStatus {
                slots: pinna_assist::presets::Slots {
                    chat: false,
                    ..pinna_assist::presets::Slots::all(true)
                },
                ..Default::default()
            },
            ..Default::default()
        };
        let t = scrape(s, &c, dir.path());

        has(
            &t,
            "pinna_standalone_errors_total{slot=\"brain\",reason=\"http_402\"} 1",
        );
        has(
            &t,
            "pinna_standalone_errors_total{slot=\"stt\",reason=\"timeout\"} 1",
        );
        has(
            &t,
            "pinna_standalone_stage_seconds_bucket{slot=\"stt\",le=\"0.5\"} 0",
        );
        has(
            &t,
            "pinna_standalone_stage_seconds_bucket{slot=\"stt\",le=\"1\"} 1",
        );
        has(
            &t,
            "pinna_standalone_stage_seconds_bucket{slot=\"brain\",le=\"120\"} 1",
        );
        has(&t, "pinna_standalone_stage_seconds_count{slot=\"tts\"} 1");
        has(
            &t,
            "pinna_standalone_audio_seconds_total{preset=\"openai\"} 3.5",
        );
        has(
            &t,
            "pinna_standalone_tokens_total{preset=\"anthropic\",direction=\"input\"} 1200",
        );
        has(
            &t,
            "pinna_standalone_tokens_total{preset=\"anthropic\",direction=\"output\"} 80",
        );
        has(
            &t,
            "pinna_standalone_characters_total{preset=\"elevenlabs\"} 42",
        );
        for guard in [
            "no_speech",
            "invented",
            "follow_up",
            "stopped_by_wake",
            "stopped_by_button",
        ] {
            has(
                &t,
                &format!("pinna_standalone_guards_total{{guard=\"{guard}\"}} 1"),
            );
        }
        has(&t, "pinna_standalone_guards_total{guard=\"ack\"} 2");
        has(
            &t,
            "pinna_tool_calls_total{tool=\"set_volume\",outcome=\"ok\",via=\"brain\"} 2",
        );
        has(
            &t,
            "pinna_tool_calls_total{tool=\"play_url\",outcome=\"error\",via=\"mcp\"} 1",
        );
        has(&t, "pinna_mic_frames_dropped_total 3");
        has(&t, "pinna_standalone_mode 1.0");
        has(&t, "pinna_standalone_slot_ready{slot=\"stt\"} 1.0");
        has(&t, "pinna_standalone_slot_ready{slot=\"brain\"} 0.0");
        has(&t, "pinna_standalone_slot_ready{slot=\"tts\"} 1.0");
        // Help text gets one full stop, from the encoder.
        has(
            &t,
            "# HELP pinna_standalone_errors_total Standalone requests that failed, by slot and reason.",
        );
        has(&t, "# TYPE pinna_standalone_stage_seconds histogram");
    }

    #[test]
    fn standalone_gauges_in_home_assistant_mode() {
        let dir = tempfile::tempdir().unwrap();
        let t = scrape(Snapshot::default(), &Counters::default(), dir.path());
        has(&t, "pinna_standalone_mode 0.0");
        for slot in ["stt", "brain", "tts"] {
            has(
                &t,
                &format!("pinna_standalone_slot_ready{{slot=\"{slot}\"}} 0.0"),
            );
        }
        // A family with nothing counted yet is left out.
        assert!(!t.contains("pinna_standalone_errors"), "{t}");
        assert!(!t.contains("pinna_tool_calls"), "{t}");
        has(&t, "pinna_mic_frames_dropped_total 0");
    }

    #[test]
    fn unknown_labels_become_custom_or_other() {
        assert_eq!(tool_label("set_timer", None), "set_timer");
        // A name a brain made up, or an MCP client sent.
        for name in ["", "rm -rf /", "https://example.com/x", "SET_VOLUME"] {
            assert_eq!(tool_label(name, None), "other", "{name:?}");
        }
        // An owner MCP server's tools count as the server that answered.
        assert_eq!(tool_label("my_ha_lights_on", Some("my-ha")), "my-ha");
        // A device tool keeps its own name.
        assert_eq!(tool_label("set_volume", Some("set")), "set_volume");

        let dir = tempfile::tempdir().unwrap();
        let c = Counters::default();
        c.tool_call("made up", None, Via::Mcp, false);
        // The owner's own URL is the Custom preset, so it never shows.
        c.standalone.spent(PresetId::Custom, Spend::Characters(5));
        let t = scrape(Snapshot::default(), &c, dir.path());
        has(
            &t,
            "pinna_tool_calls_total{tool=\"other\",outcome=\"error\",via=\"mcp\"} 1",
        );
        assert!(!t.contains("made up"), "{t}");
        has(&t, "pinna_standalone_characters_total{preset=\"custom\"} 5");
    }

    #[test]
    fn audio_seconds_never_go_down() {
        let dir = tempfile::tempdir().unwrap();
        let c = Counters::default();
        for seconds in [1.5, -2.0, f32::NAN, f32::INFINITY, 0.0] {
            c.standalone
                .spent(PresetId::Groq, Spend::AudioSeconds(seconds));
        }
        let t = scrape(Snapshot::default(), &c, dir.path());
        has(
            &t,
            "pinna_standalone_audio_seconds_total{preset=\"groq\"} 1.5",
        );
    }

    #[test]
    fn mic_drops_follow_the_capture_loops_total() {
        let c = Counters::default();
        c.mic_dropped(0);
        assert_eq!(c.mic_frames_dropped.get(), 0);
        c.mic_dropped(3);
        c.mic_dropped(3);
        assert_eq!(c.mic_frames_dropped.get(), 3);
        c.mic_dropped(5);
        assert_eq!(c.mic_frames_dropped.get(), 5);
    }

    #[test]
    fn home_assistant_errors_render_as_before() {
        use pinna_assist::error::Reason;

        let dir = tempfile::tempdir().unwrap();
        let c = Counters::default();
        c.error("tts-failed");
        c.error("tts-failed");
        // Standalone failures have their own family.
        c.standalone.failed(&ProviderError::new(
            Slot::Tts,
            PresetId::OpenAi,
            Reason::Http(500),
        ));
        let t = scrape(Snapshot::default(), &c, dir.path());
        let lines: Vec<&str> = t
            .lines()
            .filter(|l| l.contains("pinna_assist_errors"))
            .collect();
        // As rendered before standalone mode existed.
        assert_eq!(
            lines,
            [
                "# HELP pinna_assist_errors_total Assist runs that Home Assistant ended with an error, by its error code.",
                "# TYPE pinna_assist_errors_total counter",
                "pinna_assist_errors_total{code=\"tts-failed\"} 2",
            ]
        );
    }
}
