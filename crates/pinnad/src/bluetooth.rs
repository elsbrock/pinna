//! Bluetooth (ADR 0011): BlueZ, BlueALSA as an A2DP sink and the `pinna-bt`
//! bridge, started together as `pinna-bluetooth.target` while the switch is
//! on. bluetoothd powers the adapter when it starts and off when it stops, so
//! nothing switches the chip's Bluetooth function on while the switch is off.
//! The bridge names the adapter, opens the pairing window that pinnad keeps,
//! carries the audio into the stream socket and reports the paired phones.

use std::io::Write as _;
use std::path::Path;
use std::time::Duration;

use tokio::sync::{mpsc, watch};

use crate::airplay::{State, systemctl, unit_state};

const TARGET: &str = "pinna-bluetooth.target";
const BRIDGE: &str = "pinna-bt.service";
/// Written before Bluetooth starts, with the boot's ID, and removed once it
/// has run for [`SETTLE`] with the Wi-Fi up: a start from an earlier boot
/// that never got there is skipped, so Bluetooth can't keep costing the
/// network (ADR 0011).
pub const MARKER: &str = "/var/lib/pinna/bluetooth-starting";
/// How long Bluetooth must run with the Wi-Fi up before its start counts.
pub const SETTLE: Duration = Duration::from_secs(30);
/// How long a pairing window stays open.
pub const PAIRING: Duration = Duration::from_secs(120);
/// How often the worker looks at the bridge while Bluetooth is on.
const POLL: Duration = Duration::from_secs(2);

/// The device has a Bluetooth controller (the kernel registers it at boot,
/// unpowered).
pub fn present() -> bool {
    Path::new("/sys/class/bluetooth/hci0").exists()
}

/// The Wi-Fi is up. A missing wlan0 counts as down: on the Echo, the chip
/// that Bluetooth shares has lost it.
pub fn wifi_ok() -> bool {
    std::fs::read_to_string("/sys/class/net/wlan0/operstate").is_ok_and(|s| s.trim() == "up")
}

fn boot_id() -> String {
    std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .map(|s| s.trim().to_owned())
        .unwrap_or_default()
}

/// The marker, written durably: it matters exactly when the device hangs or
/// loses power right after.
fn write_marker() -> std::io::Result<()> {
    let path = Path::new(MARKER);
    let mut f = std::fs::File::create(path)?;
    f.write_all(boot_id().as_bytes())?;
    f.sync_all()?;
    if let Some(dir) = path.parent() {
        std::fs::File::open(dir)?.sync_all()?;
    }
    Ok(())
}

/// A start from an earlier boot never got to running with the Wi-Fi up. A
/// marker from this boot only means pinnad restarted meanwhile.
pub fn left_from_last_boot() -> bool {
    match std::fs::read_to_string(MARKER) {
        Ok(id) => id.trim() != boot_id(),
        Err(_) => false,
    }
}

/// Whether a start counts yet: the bridge ready, with the Wi-Fi up, for
/// [`SETTLE`]. Once per start: the bridge repeats its report every 15 s, and
/// each repeat used to start the wait again, so pinnad logged the start as
/// counted every 31 s for as long as Bluetooth ran.
#[derive(Debug, Default)]
pub struct Settle {
    since: Option<std::time::Instant>,
    done: bool,
}

impl Settle {
    /// Bluetooth started or was switched: the next start has to count again.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// The bridge's report: ready (and without an error) or not.
    pub fn report(&mut self, ready: bool, now: std::time::Instant) {
        if self.done {
            return;
        }
        if ready {
            self.since.get_or_insert(now);
        } else {
            self.since = None;
        }
    }

    /// True once, when the start has run for [`SETTLE`] with the Wi-Fi up.
    /// The Wi-Fi going down starts the wait again with the next report.
    pub fn tick(&mut self, wifi_up: bool, now: std::time::Instant) -> bool {
        let Some(since) = self.since else {
            return false;
        };
        if !wifi_up {
            self.since = None;
            return false;
        }
        if now.saturating_duration_since(since) < SETTLE {
            return false;
        }
        self.since = None;
        self.done = true;
        true
    }
}

/// A paired phone, as the bridge reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    pub address: String,
    pub name: String,
    pub connected: bool,
}

/// What the bridge last reported.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Bridge {
    pub ready: bool,
    pub address: String,
    pub devices: Vec<Device>,
    pub error: String,
}

/// Bluetooth as the dashboard shows it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    pub state: State,
    pub name: String,
    pub address: String,
    pub pairing_until_ms: u64,
    /// When the window closes on this device's monotonic clock: Status
    /// carries the seconds left from it, counted when rendered, so screens
    /// count down whatever their clock says (as for timers).
    pub pairing_until: Option<std::time::Instant>,
    pub devices: Vec<Device>,
}

/// A task that starts or stops the target in order, one change at a time
/// (changes that pile up collapse into the latest), and while Bluetooth is on
/// reports the bridge's unit: `Ready` there only means it runs.
pub fn worker(unit: watch::Sender<State>) -> mpsc::UnboundedSender<bool> {
    let (tx, mut rx) = mpsc::unbounded_channel::<bool>();
    tokio::spawn(async move {
        let mut on = false;
        let mut poll = tokio::time::interval(POLL);
        loop {
            tokio::select! {
                next = rx.recv() => {
                    let Some(mut next) = next else { return };
                    while let Ok(later) = rx.try_recv() {
                        next = later;
                    }
                    on = next;
                    unit.send_replace(if on { State::Starting } else { State::Off });
                    let applied = tokio::task::spawn_blocking(move || {
                        if on {
                            if let Err(e) = write_marker() {
                                // Without it, a start that costs the network
                                // would be tried again on every boot.
                                tracing::warn!("bluetooth: {MARKER}: {e}; not starting");
                                return Err(format!("can't write {MARKER}"));
                            }
                            // A failed unit, or one past its start limit, starts afresh.
                            systemctl(&[
                                "reset-failed",
                                "bluetooth.service",
                                "bluealsa.service",
                                BRIDGE,
                            ]);
                            systemctl(&["start", TARGET]);
                            tracing::info!("bluetooth: on");
                        } else {
                            systemctl(&["stop", TARGET]);
                            let _ = std::fs::remove_file(MARKER);
                            tracing::info!("bluetooth: off");
                        }
                        Ok(())
                    });
                    if let Ok(Err(why)) = applied.await {
                        on = false;
                        unit.send_replace(State::Failed(why));
                    }
                }
                _ = poll.tick(), if on => {
                    if let Ok(state) = tokio::task::spawn_blocking(|| unit_state(BRIDGE)).await {
                        unit.send_if_modified(|u| {
                            let changed = *u != state;
                            if changed && let State::Failed(why) = &state {
                                tracing::warn!(
                                    "bluetooth: the bridge stopped ({why}); \
                                     journalctl -u pinna-bt says why"
                                );
                            }
                            *u = state;
                            changed
                        });
                    }
                }
            }
        }
    });
    tx
}

/// Forget a paired phone: BlueZ drops its keys, so it has to pair again.
/// Blocks (busctl: pinnad links no D-Bus library).
pub fn forget(address: &str) -> Result<(), String> {
    let path = format!("/org/bluez/hci0/dev_{}", address.replace(':', "_"));
    let out = std::process::Command::new("busctl")
        .args([
            "call",
            "org.bluez",
            "/org/bluez/hci0",
            "org.bluez.Adapter1",
            "RemoveDevice",
            "o",
            &path,
        ])
        .output()
        .map_err(|e| format!("busctl: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        let why = String::from_utf8_lossy(&out.stderr).trim().to_owned();
        tracing::warn!("bluetooth: forgetting {address}: {why}");
        Err(format!("BlueZ didn't forget the phone: {why}"))
    }
}

/// An address as BlueZ writes it: six hex pairs, colon-separated.
pub fn valid_address(a: &str) -> bool {
    let parts: Vec<&str> = a.split(':').collect();
    parts.len() == 6
        && parts
            .iter()
            .all(|p| p.len() == 2 && p.chars().all(|c| c.is_ascii_hexdigit()))
}

/// What the dashboard and the bridge see, from the switch, the unit, the
/// bridge's last report and the pairing window.
pub fn report(
    available: bool,
    on: bool,
    skipped: Option<&str>,
    unit: &State,
    bridge: &Bridge,
    name: &str,
    (pairing_until_ms, pairing_until): (u64, Option<std::time::Instant>),
) -> Report {
    let state = if !available {
        State::Unavailable
    } else if !on {
        State::Off
    } else if let Some(why) = skipped {
        State::Failed(why.into())
    } else {
        match unit {
            State::Failed(why) => State::Failed(why.clone()),
            State::Ready if !bridge.error.is_empty() => State::Failed(bridge.error.clone()),
            State::Ready if bridge.ready => State::Ready,
            _ => State::Starting,
        }
    };
    let mut r = Report {
        state,
        ..Default::default()
    };
    if on && available {
        // The bridge names the adapter with it before it is ready.
        r.name = name.into();
    }
    if r.state == State::Ready {
        // A bridge that stopped leaves its last report: not shown as current.
        r.devices = bridge.devices.clone();
        r.address = bridge.address.clone();
        r.pairing_until_ms = pairing_until_ms;
        r.pairing_until = pairing_until;
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bridge(ready: bool) -> Bridge {
        Bridge {
            ready,
            address: "68:37:E9:41:FF:4F".into(),
            devices: vec![Device {
                address: "44:63:B6:7A:85:8B".into(),
                name: "Phone".into(),
                connected: true,
            }],
            error: String::new(),
        }
    }

    #[test]
    fn ready_needs_the_unit_and_the_bridge() {
        let r =
            |unit: State, b: &Bridge| report(true, true, None, &unit, b, "Dot", (5, None)).state;
        assert_eq!(r(State::Ready, &bridge(true)), State::Ready);
        assert_eq!(r(State::Ready, &bridge(false)), State::Starting);
        assert_eq!(r(State::Starting, &bridge(true)), State::Starting);
        assert_eq!(
            r(State::Failed("exit-code".into()), &bridge(true)),
            State::Failed("exit-code".into())
        );
        let mut broken = bridge(false);
        broken.error = "no adapter".into();
        assert_eq!(r(State::Ready, &broken), State::Failed("no adapter".into()));
    }

    #[test]
    fn off_unavailable_and_skipped_win() {
        let b = bridge(true);
        assert_eq!(
            report(false, true, None, &State::Ready, &b, "Dot", (0, None)).state,
            State::Unavailable
        );
        let off = report(true, false, None, &State::Ready, &b, "Dot", (0, None));
        assert_eq!(off.state, State::Off);
        assert!(off.devices.is_empty() && off.name.is_empty());
        let starting = report(true, true, None, &State::Starting, &b, "Dot", (7, None));
        assert_eq!(starting.name, "Dot", "the bridge names the adapter first");
        assert!(
            starting.devices.is_empty(),
            "a stopped bridge's phones aren't current"
        );
        assert_eq!(starting.pairing_until_ms, 0);
        assert_eq!(
            report(
                true,
                true,
                Some("skipped"),
                &State::Off,
                &b,
                "Dot",
                (0, None)
            )
            .state,
            State::Failed("skipped".into())
        );
    }

    #[test]
    fn ready_carries_name_address_window_and_devices() {
        let r = report(
            true,
            true,
            None,
            &State::Ready,
            &bridge(true),
            "Büro",
            (42, Some(std::time::Instant::now())),
        );
        assert_eq!(
            (r.name.as_str(), r.address.as_str()),
            ("Büro", "68:37:E9:41:FF:4F")
        );
        assert_eq!(r.pairing_until_ms, 42);
        assert!(r.pairing_until.is_some());
        assert_eq!(r.devices.len(), 1);
    }

    #[test]
    fn a_start_counts_once_however_often_the_bridge_reports() {
        let t0 = std::time::Instant::now();
        let at = |s: u64| t0 + Duration::from_secs(s);
        let mut s = Settle::default();
        s.report(true, at(0));
        assert!(!s.tick(true, at(29)));
        assert!(
            s.tick(true, at(30)),
            "counts after SETTLE with the Wi-Fi up"
        );
        // The bridge repeats its report every 15 s.
        for k in 1..20 {
            s.report(true, at(30 + 15 * k));
            assert!(
                !s.tick(true, at(30 + 15 * k + 30)),
                "counted again at report {k}"
            );
        }
        // Switched off and on: the new start has to count.
        s.reset();
        s.report(true, at(400));
        assert!(s.tick(true, at(430)));
    }

    #[test]
    fn a_start_counts_only_with_the_wifi_up_and_the_bridge_ready() {
        let t0 = std::time::Instant::now();
        let at = |s: u64| t0 + Duration::from_secs(s);
        let mut s = Settle::default();
        s.report(true, at(0));
        assert!(!s.tick(false, at(5)), "Wi-Fi down");
        assert!(!s.tick(true, at(40)), "the wait starts again with a report");
        s.report(true, at(45));
        s.report(false, at(60));
        assert!(!s.tick(true, at(80)), "the bridge stopped being ready");
        s.report(true, at(90));
        assert!(s.tick(true, at(120)));
    }

    #[test]
    fn addresses_are_checked() {
        assert!(valid_address("44:63:B6:7A:85:8B"));
        assert!(!valid_address("44:63:B6:7A:85"));
        assert!(!valid_address("44:63:B6:7A:85:8G"));
        assert!(!valid_address("../../x"));
    }
}
