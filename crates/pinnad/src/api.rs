//! Local control API: ConnectRPC `pinna.v1.DeviceService`, served over plain HTTP.
//!
//! No authentication — bind it to loopback only. The service holds no state of
//! its own: reads come from the brain's status `watch`, commands go to the brain
//! as inputs and are answered with the state right after they were applied.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result};
use buffa::MessageField;
use connectrpc::{ConnectError, RequestContext, Response, Router, Server, ServiceRequest};
use connectrpc::{ServiceResult, ServiceStream};
use futures::{Stream, StreamExt};
use pinna_api::{
    Calibration, CalibrationEvent, CalibrationEventKind, ClearNotificationRequest,
    ClearNotificationResponse, DeviceService, GetNetworkRequest, GetNetworkResponse,
    GetStatusRequest, GetStatusResponse, GetUpdatesRequest, GetUpdatesResponse,
    InstallUpdateRequest, InstallUpdateResponse, LastBoot as LastBootMsg, LastBootOutcome,
    LedTestRequest, LedTestResponse, RebootRequest, RebootResponse, SetActiveSlotRequest,
    SetActiveSlotResponse, SetMuteRequest, SetMuteResponse, SetStreamVolumeRequest,
    SetStreamVolumeResponse, SetVolumeRequest, SetVolumeResponse, StartCalibrationRequest,
    StartCalibrationResponse, StartConversationRequest, StartConversationResponse, Status,
    StopCalibrationRequest, StopCalibrationResponse, StopConversationRequest,
    StopConversationResponse, Timer, UpdateSettingsRequest, UpdateSettingsResponse, WakeEvent,
    WakeWord, WakeWordOption, WatchInstallRequest, WatchInstallResponse, WatchLogsRequest,
    WatchLogsResponse, WatchRingRequest, WatchRingResponse, WatchStatusRequest,
    WatchStatusResponse,
};
use pinna_api::{
    FactoryResetRequest, FactoryResetResponse, ForgetWifiRequest, ForgetWifiResponse,
    JoinWifiRequest, JoinWifiResponse, KeepWifiRequest, KeepWifiResponse, ListWifiRequest,
    ListWifiResponse, ScanWifiRequest, ScanWifiResponse,
};
use pinna_api::{
    ForgetBluetoothDeviceRequest, ForgetBluetoothDeviceResponse, ReportBluetoothRequest,
    ReportBluetoothResponse, StartBluetoothPairingRequest, StartBluetoothPairingResponse,
    StopBluetoothPairingRequest, StopBluetoothPairingResponse,
};
use pinna_hal::led::{Frame, RING_PIXELS, Rgb};
use tokio::sync::{mpsc, oneshot, watch};

use crate::boot;
use crate::updates::{Event, Updater, UpdaterPaths, stage_of};
use crate::voice::{ApiCommand, ApiReply, Input, SettingsPatch, Snapshot};
use crate::wifi::{Wifi, WifiPaths};
use crate::{logs, network, wifi};

/// What does not change while pinnad runs.
#[derive(Debug, Clone)]
pub struct Info {
    /// The running image version (pinna-init's boot-version), or pinnad's own
    /// version where no image was booted.
    pub version: String,
    pub mac: String,
    /// The wake words on offer (ADR 0013).
    pub wake_words: Arc<crate::wakewords::Catalogue>,
    pub audio_backend: String,
    pub led_backend: String,
    pub model: String,
    /// The development VM's virtual Echo Dot (ADR 0014).
    pub virtual_device: bool,
    /// How the boot before this one ended (pstore, ADR 0012).
    pub last_boot: boot::LastBoot,
    /// The image carries the ESPHome key, so a factory reset keeps it.
    pub key_from_image: bool,
}

pub struct DeviceApi {
    info: Info,
    started: Instant,
    status: watch::Receiver<Snapshot>,
    /// The frames the ring shows (leds::run).
    ring: watch::Receiver<Frame>,
    inputs: mpsc::Sender<Input>,
    updater: Arc<Updater>,
    wifi: Arc<Wifi>,
}

/// WatchRing sends at most one frame per this interval.
const RING_INTERVAL: std::time::Duration = std::time::Duration::from_millis(40);

impl DeviceApi {
    pub fn new(info: Info, status: watch::Receiver<Snapshot>, inputs: mpsc::Sender<Input>) -> Self {
        Self::with_updater(info, status, inputs, Updater::new(UpdaterPaths::default()))
    }

    pub fn with_updater(
        info: Info,
        status: watch::Receiver<Snapshot>,
        inputs: mpsc::Sender<Input>,
        updater: Updater,
    ) -> Self {
        Self {
            info,
            started: Instant::now(),
            status,
            ring: watch::channel([Rgb::OFF; RING_PIXELS]).1,
            inputs,
            updater: Arc::new(updater),
            wifi: Arc::new(Wifi::new(WifiPaths::default())),
        }
    }

    /// A request reached the dashboard at this local address (ADR 0008: a
    /// network on trial is kept once someone reaches it there).
    pub fn reached(&self, local: IpAddr) {
        self.wifi.reached(local);
    }

    /// Undo a Wi-Fi trial that a stopped pinnad left behind.
    pub async fn recover_wifi(&self) {
        self.wifi.recover().await;
    }

    /// Stream the frames on `ring` to WatchRing.
    pub fn with_ring(mut self, ring: watch::Receiver<Frame>) -> Self {
        self.ring = ring;
        self
    }

    /// Talk to another wpa_supplicant (tests).
    #[cfg(test)]
    pub fn with_wifi(mut self, wifi: Wifi) -> Self {
        self.wifi = Arc::new(wifi);
        self
    }

    fn render(&self, s: &Snapshot) -> Status {
        render(&self.info, self.started, s)
    }

    /// The latest state, as it changes.
    pub fn info(&self) -> &Info {
        &self.info
    }

    pub fn watch(&self) -> watch::Receiver<Snapshot> {
        self.status.clone()
    }

    async fn command(
        &self,
        make: impl FnOnce(ApiReply) -> ApiCommand,
    ) -> Result<Snapshot, ConnectError> {
        let (tx, rx) = oneshot::channel();
        self.inputs
            .send(Input::Api(make(tx)))
            .await
            .map_err(|_| ConnectError::unavailable("pinnad is shutting down"))?;
        rx.await
            .map_err(|_| ConnectError::unavailable("pinnad is shutting down"))?
            .map_err(ConnectError::failed_precondition)
    }
}

fn render(info: &Info, started: Instant, s: &Snapshot) -> Status {
    let now = Instant::now();
    Status {
        version: info.version.clone(),
        name: s.name.clone(),
        mac: info.mac.clone(),
        model: info.model.clone(),
        uptime_seconds: started.elapsed().as_secs(),
        phase: s.phase.into(),
        muted: s.hw_muted || s.sw_muted,
        hardware_muted: s.hw_muted,
        software_muted: s.sw_muted,
        volume: s.volume.into(),
        ha_connected: s.ha_connected,
        voice_subscribed: s.voice_subscribed,
        wake_word: MessageField::some(WakeWord {
            phrase: info.wake_words.phrases(&s.active_wake_words),
            enabled: s.wake_enabled,
            threshold: s.settings.wake_threshold(),
            recent: s
                .recent_wakes
                .iter()
                .map(|w| WakeEvent {
                    unix_ms: w.unix_ms,
                    woke: w.woke,
                    score: w.score,
                    wakes_from: pinna_wakeword::WakeConfig::min_sensitivity_for(w.score)
                        .map(u32::from),
                    reached_threshold: w.reached_threshold,
                    ..Default::default()
                })
                .collect(),
            calibration: s.calibration.as_ref().map_or_else(MessageField::none, |c| {
                MessageField::some(Calibration {
                    started_from: c.started_from.into(),
                    events: c
                        .events
                        .iter()
                        .map(|e| CalibrationEvent {
                            id: e.id,
                            kind: calibration_kind(e.kind).into(),
                            raise: e.raise,
                            score: e.score,
                            from: e.from.into(),
                            to: e.to.into(),
                            ..Default::default()
                        })
                        .collect(),
                    ..Default::default()
                })
            }),
            available: info
                .wake_words
                .words()
                .iter()
                .map(|w| WakeWordOption {
                    id: w.id.clone(),
                    phrase: w.phrase.clone(),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        }),
        timers: s
            .timers
            .iter()
            .map(|t| Timer {
                id: t.id.clone(),
                name: t.name.clone(),
                seconds_left: if t.active {
                    let elapsed = now.saturating_duration_since(t.at).as_secs() as u32;
                    t.left.saturating_sub(elapsed)
                } else {
                    t.left
                },
                total_seconds: t.total,
                active: t.active,
                ..Default::default()
            })
            .collect(),
        audio_backend: info.audio_backend.clone(),
        led_backend: info.led_backend.clone(),
        alarm_ringing: s.alarm_ringing,
        notification: s.settings.notification,
        virtual_device: info.virtual_device,
        last_boot: MessageField::some(last_boot(&info.last_boot)),
        airplay: MessageField::some(airplay(&s.airplay)),
        bluetooth: MessageField::some(bluetooth(&s.bluetooth)),
        settings: MessageField::some(pinna_api::Settings {
            wake_word: s.settings.wake_word,
            wake_sensitivity: s.settings.wake_sensitivity.into(),
            led_brightness: s.settings.led_brightness.into(),
            wake_words: s.active_wake_words.clone(),
            friendly_name: s.friendly_name.clone(),
            default_name: s.default_name,
            wake_sound: s.wake_sound,
            allowed_hosts: s.settings.allowed_hosts.clone().unwrap_or_default(),
            airplay: s.settings.airplay,
            bluetooth: s.settings.bluetooth,
            ..Default::default()
        }),
        api_encrypted: s.encrypted,
        api_key_from_image: info.key_from_image,
        led_test: s.led_test,
        ..Default::default()
    }
}

/// The proto kind of a calibration event.
fn calibration_kind(kind: crate::calibration::EventKind) -> CalibrationEventKind {
    use crate::calibration::{EventKind, Unchanged};
    match kind {
        EventKind::Woke => CalibrationEventKind::CALIBRATION_EVENT_KIND_WOKE,
        EventKind::Raised => CalibrationEventKind::CALIBRATION_EVENT_KIND_RAISED,
        EventKind::Lowered => CalibrationEventKind::CALIBRATION_EVENT_KIND_LOWERED,
        EventKind::Unchanged(Unchanged::NothingHeard) => {
            CalibrationEventKind::CALIBRATION_EVENT_KIND_NOTHING_HEARD
        }
        EventKind::Unchanged(Unchanged::NoSettingHelps) => {
            CalibrationEventKind::CALIBRATION_EVENT_KIND_NO_SETTING_HELPS
        }
        EventKind::Unchanged(Unchanged::WouldUndo) => {
            CalibrationEventKind::CALIBRATION_EVENT_KIND_WOULD_UNDO
        }
    }
}

/// The previous boot's verdict, as the API reports it.
fn airplay(r: &crate::airplay::Report) -> pinna_api::AirPlay {
    use crate::airplay::State;
    use pinna_api::AirPlayState as S;
    let (state, reason) = match &r.state {
        State::Off => (S::AIRPLAY_STATE_OFF, ""),
        State::Starting => (S::AIRPLAY_STATE_STARTING, ""),
        State::Ready => (S::AIRPLAY_STATE_READY, ""),
        State::Failed(why) => (S::AIRPLAY_STATE_FAILED, why.as_str()),
        State::Unavailable => (S::AIRPLAY_STATE_UNAVAILABLE, ""),
    };
    pinna_api::AirPlay {
        state: buffa::EnumValue::Known(state),
        name: r.name.clone(),
        reason: reason.into(),
        ..Default::default()
    }
}

/// Bluetooth as the API reports it.
fn bluetooth(r: &crate::bluetooth::Report) -> pinna_api::Bluetooth {
    use crate::airplay::State;
    use pinna_api::BluetoothState as S;
    let (state, reason) = match &r.state {
        State::Off => (S::BLUETOOTH_STATE_OFF, ""),
        State::Starting => (S::BLUETOOTH_STATE_STARTING, ""),
        State::Ready => (S::BLUETOOTH_STATE_READY, ""),
        State::Failed(why) => (S::BLUETOOTH_STATE_FAILED, why.as_str()),
        State::Unavailable => (S::BLUETOOTH_STATE_UNAVAILABLE, ""),
    };
    pinna_api::Bluetooth {
        state: buffa::EnumValue::Known(state),
        name: r.name.clone(),
        address: r.address.clone(),
        pairing_until_ms: r.pairing_until_ms,
        pairing_seconds_left: r.pairing_until.map_or(0, |u| {
            u.saturating_duration_since(std::time::Instant::now())
                .as_secs_f64()
                .ceil() as u32
        }),
        devices: r
            .devices
            .iter()
            .map(|d| pinna_api::BluetoothDevice {
                address: d.address.clone(),
                name: d.name.clone(),
                connected: d.connected,
                ..Default::default()
            })
            .collect(),
        reason: reason.into(),
        ..Default::default()
    }
}

fn last_boot(lb: &boot::LastBoot) -> LastBootMsg {
    let outcome = match lb.outcome {
        boot::Outcome::Cold => LastBootOutcome::LAST_BOOT_OUTCOME_COLD_START,
        boot::Outcome::Clean => LastBootOutcome::LAST_BOOT_OUTCOME_RESTARTED,
        boot::Outcome::Rescue => LastBootOutcome::LAST_BOOT_OUTCOME_RESCUE,
        boot::Outcome::Crashed => LastBootOutcome::LAST_BOOT_OUTCOME_CRASHED,
        boot::Outcome::Reset => LastBootOutcome::LAST_BOOT_OUTCOME_RESET,
    };
    LastBootMsg {
        outcome: buffa::EnumValue::Known(outcome),
        reason: lb.reason.clone().unwrap_or_default(),
        summary: lb.summary(),
        ..Default::default()
    }
}

/// A request from the device itself (a bridge on loopback), not the network.
fn from_device(peer: Option<SocketAddr>) -> bool {
    peer.is_some_and(|a| a.ip().to_canonical().is_loopback())
}

#[allow(refining_impl_trait)]
impl DeviceService for DeviceApi {
    async fn get_status(
        &self,
        _ctx: RequestContext,
        _request: ServiceRequest<'_, GetStatusRequest>,
    ) -> ServiceResult<GetStatusResponse> {
        let status = self.render(&self.status.borrow());
        Response::ok(GetStatusResponse {
            status: MessageField::some(status),
            ..Default::default()
        })
    }

    async fn set_volume(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, SetVolumeRequest>,
    ) -> ServiceResult<SetVolumeResponse> {
        let percent = request.percent;
        if percent > 100 {
            return Err(ConnectError::invalid_argument(
                "percent must be between 0 and 100",
            ));
        }
        let snap = self
            .command(|r| ApiCommand::SetVolume(percent as u8, r))
            .await?;
        Response::ok(SetVolumeResponse {
            status: MessageField::some(self.render(&snap)),
            ..Default::default()
        })
    }

    async fn set_stream_volume(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, SetStreamVolumeRequest>,
    ) -> ServiceResult<SetStreamVolumeResponse> {
        if !from_device(ctx.peer_addr()) {
            return Err(ConnectError::permission_denied(
                "only the device's own bridges set the stream volume",
            ));
        }
        let percent = request.percent;
        if percent > 100 {
            return Err(ConnectError::invalid_argument(
                "percent must be between 0 and 100",
            ));
        }
        let snap = self
            .command(|r| ApiCommand::SetStreamVolume(percent as u8, r))
            .await?;
        Response::ok(SetStreamVolumeResponse {
            status: MessageField::some(self.render(&snap)),
            ..Default::default()
        })
    }

    async fn set_mute(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, SetMuteRequest>,
    ) -> ServiceResult<SetMuteResponse> {
        let muted = request.muted;
        let snap = self.command(|r| ApiCommand::SetMute(muted, r)).await?;
        Response::ok(SetMuteResponse {
            status: MessageField::some(self.render(&snap)),
            ..Default::default()
        })
    }

    async fn start_conversation(
        &self,
        _ctx: RequestContext,
        _request: ServiceRequest<'_, StartConversationRequest>,
    ) -> ServiceResult<StartConversationResponse> {
        let snap = self.command(ApiCommand::StartConversation).await?;
        Response::ok(StartConversationResponse {
            status: MessageField::some(self.render(&snap)),
            ..Default::default()
        })
    }

    async fn stop_conversation(
        &self,
        _ctx: RequestContext,
        _request: ServiceRequest<'_, StopConversationRequest>,
    ) -> ServiceResult<StopConversationResponse> {
        let snap = self.command(ApiCommand::StopConversation).await?;
        Response::ok(StopConversationResponse {
            status: MessageField::some(self.render(&snap)),
            ..Default::default()
        })
    }

    async fn led_test(
        &self,
        _ctx: RequestContext,
        _request: ServiceRequest<'_, LedTestRequest>,
    ) -> ServiceResult<LedTestResponse> {
        self.command(ApiCommand::LedTest).await?;
        Response::ok(LedTestResponse::default())
    }

    async fn clear_notification(
        &self,
        _ctx: RequestContext,
        _request: ServiceRequest<'_, ClearNotificationRequest>,
    ) -> ServiceResult<ClearNotificationResponse> {
        let snap = self.command(ApiCommand::ClearNotification).await?;
        Response::ok(ClearNotificationResponse {
            status: MessageField::some(self.render(&snap)),
            ..Default::default()
        })
    }

    async fn watch_status(
        &self,
        _ctx: RequestContext,
        _request: ServiceRequest<'_, WatchStatusRequest>,
    ) -> ServiceResult<ServiceStream<WatchStatusResponse>> {
        let info = self.info.clone();
        let started = self.started;
        let mut rx = self.status.clone();
        // The current state first, then one message per change until the brain is gone.
        rx.mark_changed();
        let stream = futures::stream::unfold(rx, move |mut rx| {
            let info = info.clone();
            async move {
                rx.changed().await.ok()?;
                let status = render(&info, started, &rx.borrow_and_update());
                let msg = WatchStatusResponse {
                    status: MessageField::some(status),
                    ..Default::default()
                };
                Some((Ok(msg), rx))
            }
        });
        Response::ok(Box::pin(stream))
    }

    async fn watch_ring(
        &self,
        _ctx: RequestContext,
        _request: ServiceRequest<'_, WatchRingRequest>,
    ) -> ServiceResult<ServiceStream<WatchRingResponse>> {
        let mut rx = self.ring.clone();
        // The current frame first, then the latest one after each interval
        // in which the ring changed.
        rx.mark_changed();
        let stream = futures::stream::unfold((rx, None), |(mut rx, sent)| async move {
            if let Some(sent) = sent {
                tokio::time::sleep_until(sent + RING_INTERVAL).await;
            }
            rx.changed().await.ok()?;
            let frame = *rx.borrow_and_update();
            let msg = WatchRingResponse {
                frame: frame.iter().flat_map(|p| [p.r, p.g, p.b]).collect(),
                ..Default::default()
            };
            Some((Ok(msg), (rx, Some(tokio::time::Instant::now()))))
        });
        Response::ok(Box::pin(stream))
    }

    async fn update_settings(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, UpdateSettingsRequest>,
    ) -> ServiceResult<UpdateSettingsResponse> {
        let percent = |name: &str, v: Option<u32>| match v {
            Some(v) if v > 100 => Err(ConnectError::invalid_argument(format!(
                "{name} must be between 0 and 100"
            ))),
            v => Ok(v.map(|v| v as u8)),
        };
        let wake_words = match request.wake_words.as_option() {
            Some(w) => {
                let ids: Vec<String> = w.ids.iter().map(|id| (*id).to_owned()).collect();
                self.info
                    .wake_words
                    .validate(&ids)
                    .map_err(ConnectError::invalid_argument)?;
                Some(ids)
            }
            None => None,
        };
        let friendly_name = request
            .friendly_name
            .map(crate::settings::check_friendly_name)
            .transpose()
            .map_err(ConnectError::invalid_argument)?;
        let hostname = request
            .hostname
            .map(crate::settings::check_hostname)
            .transpose()
            .map_err(ConnectError::invalid_argument)?;
        let allowed_hosts = match request.allowed_hosts.as_option() {
            Some(h) => {
                let names: Vec<String> = h.names.iter().map(|n| (*n).to_owned()).collect();
                Some(
                    crate::settings::check_host_names(&names)
                        .map_err(ConnectError::invalid_argument)?,
                )
            }
            None => None,
        };
        let patch = SettingsPatch {
            wake_word: request.wake_word,
            wake_sensitivity: percent("wake_sensitivity", request.wake_sensitivity)?,
            led_brightness: percent("led_brightness", request.led_brightness)?,
            wake_words,
            friendly_name,
            hostname,
            wake_sound: request.wake_sound,
            allowed_hosts,
            airplay: request.airplay,
            bluetooth: request.bluetooth,
        };
        let snap = self
            .command(|r| ApiCommand::UpdateSettings(patch, r))
            .await?;
        Response::ok(UpdateSettingsResponse {
            status: MessageField::some(self.render(&snap)),
            ..Default::default()
        })
    }

    async fn start_calibration(
        &self,
        _ctx: RequestContext,
        _request: ServiceRequest<'_, StartCalibrationRequest>,
    ) -> ServiceResult<StartCalibrationResponse> {
        let snap = self.command(ApiCommand::StartCalibration).await?;
        Response::ok(StartCalibrationResponse {
            status: MessageField::some(self.render(&snap)),
            ..Default::default()
        })
    }

    async fn start_bluetooth_pairing(
        &self,
        _ctx: RequestContext,
        _request: ServiceRequest<'_, StartBluetoothPairingRequest>,
    ) -> ServiceResult<StartBluetoothPairingResponse> {
        let snap = self.command(ApiCommand::StartBluetoothPairing).await?;
        Response::ok(StartBluetoothPairingResponse {
            status: MessageField::some(self.render(&snap)),
            ..Default::default()
        })
    }

    async fn stop_bluetooth_pairing(
        &self,
        _ctx: RequestContext,
        _request: ServiceRequest<'_, StopBluetoothPairingRequest>,
    ) -> ServiceResult<StopBluetoothPairingResponse> {
        let snap = self.command(ApiCommand::StopBluetoothPairing).await?;
        Response::ok(StopBluetoothPairingResponse {
            status: MessageField::some(self.render(&snap)),
            ..Default::default()
        })
    }

    async fn forget_bluetooth_device(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, ForgetBluetoothDeviceRequest>,
    ) -> ServiceResult<ForgetBluetoothDeviceResponse> {
        let address = request.address.to_ascii_uppercase();
        if !crate::bluetooth::valid_address(&address) {
            return Err(ConnectError::invalid_argument(
                "address must look like 44:63:B6:7A:85:8B",
            ));
        }
        let snap = self
            .command(|r| ApiCommand::ForgetBluetoothDevice(address, r))
            .await?;
        Response::ok(ForgetBluetoothDeviceResponse {
            status: MessageField::some(self.render(&snap)),
            ..Default::default()
        })
    }

    async fn report_bluetooth(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, ReportBluetoothRequest>,
    ) -> ServiceResult<ReportBluetoothResponse> {
        if !from_device(ctx.peer_addr()) {
            return Err(ConnectError::permission_denied(
                "only the device's own Bluetooth bridge reports",
            ));
        }
        let bridge = crate::bluetooth::Bridge {
            ready: request.ready,
            address: request.address.to_owned(),
            devices: request
                .devices
                .iter()
                .map(|d| crate::bluetooth::Device {
                    address: d.address.to_owned(),
                    name: d.name.to_owned(),
                    connected: d.connected,
                })
                .collect(),
            error: request.error.to_owned(),
        };
        self.command(|r| ApiCommand::ReportBluetooth(bridge, r))
            .await?;
        Response::ok(ReportBluetoothResponse::default())
    }

    async fn stop_calibration(
        &self,
        _ctx: RequestContext,
        _request: ServiceRequest<'_, StopCalibrationRequest>,
    ) -> ServiceResult<StopCalibrationResponse> {
        let snap = self.command(ApiCommand::StopCalibration).await?;
        Response::ok(StopCalibrationResponse {
            status: MessageField::some(self.render(&snap)),
            ..Default::default()
        })
    }

    async fn get_network(
        &self,
        _ctx: RequestContext,
        _request: ServiceRequest<'_, GetNetworkRequest>,
    ) -> ServiceResult<GetNetworkResponse> {
        let interfaces = network::interfaces()
            .await
            .map_err(|e| ConnectError::unavailable(format!("reading network interfaces: {e:#}")))?;
        Response::ok(GetNetworkResponse {
            hostname: network::hostname(),
            interfaces,
            ..Default::default()
        })
    }

    async fn watch_logs(
        &self,
        _ctx: RequestContext,
        _request: ServiceRequest<'_, WatchLogsRequest>,
    ) -> ServiceResult<ServiceStream<WatchLogsResponse>> {
        let lines = logs::watch()
            .map_err(|e| ConnectError::unavailable(format!("journalctl is not available: {e}")))?;
        let stream = lines.map(|line| {
            Ok(WatchLogsResponse {
                line: MessageField::some(line),
                ..Default::default()
            })
        });
        Response::ok(Box::pin(stream))
    }

    async fn get_updates(
        &self,
        ctx: RequestContext,
        _request: ServiceRequest<'_, GetUpdatesRequest>,
    ) -> ServiceResult<GetUpdatesResponse> {
        let mut resp = self.updater.get_updates().await;
        // The browser's own address is where a release is probably served.
        resp.suggested_manifest = match ctx.peer_addr().map(|a| a.ip().to_canonical()) {
            Some(IpAddr::V4(ip)) => format!("http://{ip}:8000/manifest.json"),
            Some(IpAddr::V6(ip)) => format!("http://[{ip}]:8000/manifest.json"),
            None => String::new(),
        };
        Response::ok(resp)
    }

    async fn install_update(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, InstallUpdateRequest>,
    ) -> ServiceResult<InstallUpdateResponse> {
        let manifest = request.manifest.trim();
        let valid = url::Url::parse(manifest).is_ok_and(|u| matches!(u.scheme(), "http" | "https"));
        if !valid {
            return Err(ConnectError::invalid_argument(
                "the manifest must be an http:// or https:// URL",
            ));
        }
        self.updater.install(manifest)?;
        Response::ok(InstallUpdateResponse::default())
    }

    async fn watch_install(
        &self,
        _ctx: RequestContext,
        _request: ServiceRequest<'_, WatchInstallRequest>,
    ) -> ServiceResult<ServiceStream<WatchInstallResponse>> {
        let progress = self.updater.progress();
        let line = |line: String| WatchInstallResponse {
            stage: stage_of(&line).into(),
            line,
            ..Default::default()
        };
        let done = |ok: bool| WatchInstallResponse {
            done: true,
            ok,
            ..Default::default()
        };
        let history = futures::stream::iter(progress.history.into_iter().map(line));
        // Live lines until the done marker; `Lagged` just skips ahead.
        let tail: std::pin::Pin<Box<dyn Stream<Item = WatchInstallResponse> + Send>> =
            match (progress.live, progress.finished) {
                (Some(rx), _) => {
                    Box::pin(futures::stream::unfold(Some(rx), move |rx| async move {
                        let mut rx = rx?;
                        loop {
                            match rx.recv().await {
                                Ok(Event::Line(l)) => return Some((line(l), Some(rx))),
                                Ok(Event::Done { ok }) => return Some((done(ok), None)),
                                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                                    continue;
                                }
                                Err(_) => return None,
                            }
                        }
                    }))
                }
                (None, Some(ok)) => Box::pin(futures::stream::once(async move { done(ok) })),
                (None, None) => Box::pin(futures::stream::empty()),
            };
        Response::ok(Box::pin(history.chain(tail).map(Ok)))
    }

    async fn scan_wifi(
        &self,
        _ctx: RequestContext,
        _request: ServiceRequest<'_, ScanWifiRequest>,
    ) -> ServiceResult<ScanWifiResponse> {
        Response::ok(ScanWifiResponse {
            networks: self.wifi.scan().await?,
            ..Default::default()
        })
    }

    async fn list_wifi(
        &self,
        _ctx: RequestContext,
        _request: ServiceRequest<'_, ListWifiRequest>,
    ) -> ServiceResult<ListWifiResponse> {
        Response::ok(self.wifi.list().await?)
    }

    async fn join_wifi(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, JoinWifiRequest>,
    ) -> ServiceResult<ServiceStream<JoinWifiResponse>> {
        let req = request.to_owned_message();
        let join = wifi::check_join(&req).map_err(ConnectError::invalid_argument)?;
        let live = self.wifi.join(join)?;
        // Until the final step; a lagging reader just skips ahead.
        let stream = futures::stream::unfold(Some(live), |rx| async move {
            let mut rx = rx?;
            loop {
                match rx.recv().await {
                    Ok(step) => {
                        let done = wifi::done(&step);
                        return Some((Ok(step), (!done).then_some(rx)));
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => return None,
                }
            }
        });
        Response::ok(Box::pin(stream))
    }

    async fn keep_wifi(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, KeepWifiRequest>,
    ) -> ServiceResult<KeepWifiResponse> {
        Response::ok(KeepWifiResponse {
            result: MessageField::some(self.wifi.keep(request.keep).await?),
            ..Default::default()
        })
    }

    async fn forget_wifi(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, ForgetWifiRequest>,
    ) -> ServiceResult<ForgetWifiResponse> {
        Response::ok(ForgetWifiResponse {
            saved: self.wifi.forget(request.ssid).await?,
            ..Default::default()
        })
    }

    async fn set_active_slot(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, SetActiveSlotRequest>,
    ) -> ServiceResult<SetActiveSlotResponse> {
        let slot = pinna_bootctl::slot::Slot::parse(request.slot)
            .map_err(|e| ConnectError::invalid_argument(format!("{e:#}")))?;
        self.updater.set_active(slot).await?;
        Response::ok(SetActiveSlotResponse::default())
    }

    async fn reboot(
        &self,
        _ctx: RequestContext,
        _request: ServiceRequest<'_, RebootRequest>,
    ) -> ServiceResult<RebootResponse> {
        self.updater
            .reboot("user")
            .await
            .map_err(|e| ConnectError::internal(format!("rebooting: {e:#}")))?;
        Response::ok(RebootResponse::default())
    }

    async fn factory_reset(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, FactoryResetRequest>,
    ) -> ServiceResult<FactoryResetResponse> {
        // The person typed the name: a reset never happens by a stray call.
        let (friendly, name) = {
            let s = self.status.borrow();
            (s.friendly_name.clone(), s.name.clone())
        };
        let typed = request.name.trim();
        let matches = |n: &str| !n.is_empty() && typed.eq_ignore_ascii_case(n.trim());
        if !matches(&friendly) && !matches(&name) {
            return Err(ConnectError::invalid_argument(format!(
                "Type the satellite's name, {friendly}, to confirm."
            )));
        }
        self.updater
            .factory_reset()
            .await
            .map_err(|e| ConnectError::internal(format!("factory reset: {e:#}")))?;
        Response::ok(FactoryResetResponse::default())
    }
}

/// The ConnectRPC routes; shared by the loopback API and the dashboard listener.
pub fn router(api: Arc<DeviceApi>) -> Router {
    Router::new().add_service(api)
}

/// Bind `listen` and serve the API in the background; returns the bound address.
pub async fn start(api: Arc<DeviceApi>, listen: &str) -> Result<SocketAddr> {
    let router = router(api);
    let server = Server::bind(listen)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))
        .with_context(|| format!("binding the API on {listen}"))?;
    let addr = server.local_addr()?;
    tokio::spawn(async move {
        if let Err(e) = server.serve(router).await {
            tracing::error!("API server stopped: {e}");
        }
    });
    Ok(addr)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use connectrpc::client::{ClientConfig, HttpClient};
    use pinna_api::{DeviceServiceClient, Phase};
    use pinna_esphome::{DeviceConfig, HaCommand, TimerEventType};

    use super::*;
    use crate::leds::LedState;
    use crate::voice::{Brain, Options, Shared};

    type Client = DeviceServiceClient<HttpClient>;

    static NEXT_DEVICE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

    /// The real brain without audio or LED backends, served on an ephemeral port.
    async fn start_device() -> (Client, mpsc::Sender<Input>) {
        start_device_with(Updater::new(UpdaterPaths::default())).await
    }

    async fn start_device_with(updater: Updater) -> (Client, mpsc::Sender<Input>) {
        start_device_inner(updater, None).await
    }

    async fn start_device_inner(
        updater: Updater,
        wifi: Option<crate::wifi::Wifi>,
    ) -> (Client, mpsc::Sender<Input>) {
        let mut dc = DeviceConfig::new("pinna-test", [2, 0, 0, 0xaa, 0xbb, 0xcc]);
        dc.port = 0;
        let (esp, _ha) = pinna_esphome::serve(dc).await.unwrap();
        let (led_tx, _led_rx) = watch::channel(LedState::default());
        let (status_tx, status_rx) = watch::channel(Snapshot::default());
        let (in_tx, in_rx) = mpsc::channel(64);
        let brain = Brain::new(
            esp,
            None,
            Arc::new(Shared::default()),
            led_tx,
            status_tx,
            in_tx.clone(),
            Options {
                key_file: std::env::temp_dir().join("pinnad-api-test-key"),
                wake_words: crate::wakewords::test_catalogue(),
                active_wake_words: vec!["alexa_v0.1".into()],
                keywords: None,
                defaults: crate::voice::Defaults {
                    node_name: "pinna-test".into(),
                    friendly_name: "Echo Dot aabbcc".into(),
                    default_name: true,
                    wake_sound: false,
                },
                mdns: None,
                encrypted: false,
                has_hw_privacy: false,
                // One file per device: tests run in parallel, and a failed
                // save is an error.
                settings_file: std::env::temp_dir().join(format!(
                    "pinnad-api-test-{}-{}/settings.toml",
                    std::process::id(),
                    NEXT_DEVICE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                )),
                airplay: false,
                bluetooth: false,
                settings: crate::settings::Settings {
                    volume: 40,
                    ..Default::default()
                },
            },
        );
        tokio::spawn(brain.run(in_rx));
        let info = Info {
            version: "0.1.0".into(),
            mac: "02:00:00:AA:BB:CC".into(),
            wake_words: crate::wakewords::test_catalogue(),
            audio_backend: "none".into(),
            led_backend: "none".into(),
            model: "echo-dot-2".into(),
            virtual_device: false,
            last_boot: boot::LastBoot::cold(),
            key_from_image: false,
        };
        let mut api = DeviceApi::with_updater(info, status_rx, in_tx.clone(), updater);
        if let Some(w) = wifi {
            api = api.with_wifi(w);
        }
        let addr = start(Arc::new(api), "127.0.0.1:0").await.unwrap();
        let config = ClientConfig::new(format!("http://{addr}").parse().unwrap());
        (Client::new(HttpClient::plaintext(), config), in_tx)
    }

    async fn status(c: &Client) -> Status {
        let resp = c.get_status(GetStatusRequest::default()).await.unwrap();
        resp.into_view()
            .to_owned_message()
            .status
            .into_option()
            .unwrap()
    }

    #[tokio::test]
    async fn every_rpc_takes_effect() {
        let (c, inputs) = start_device().await;

        let s = status(&c).await;
        assert_eq!(s.name, "pinna-test");
        assert_eq!(s.mac, "02:00:00:AA:BB:CC");
        assert_eq!(s.volume, 40);
        assert_eq!(s.phase, Phase::PHASE_IDLE);
        assert_eq!(s.audio_backend, "none");
        assert_eq!(s.wake_word.as_option().unwrap().phrase, "Alexa");
        assert!(!s.muted && !s.ha_connected && !s.voice_subscribed);

        c.set_volume(pinna_api::SetVolumeRequest {
            percent: 73,
            ..Default::default()
        })
        .await
        .unwrap();
        assert_eq!(status(&c).await.volume, 73);
        let err = c
            .set_volume(pinna_api::SetVolumeRequest {
                percent: 101,
                ..Default::default()
            })
            .await
            .unwrap_err();
        assert_eq!(err.code, connectrpc::ErrorCode::InvalidArgument);

        c.set_mute(pinna_api::SetMuteRequest {
            muted: true,
            ..Default::default()
        })
        .await
        .unwrap();
        let s = status(&c).await;
        assert!(s.muted && s.software_muted && !s.hardware_muted);
        c.set_mute(pinna_api::SetMuteRequest {
            muted: false,
            ..Default::default()
        })
        .await
        .unwrap();
        assert!(!status(&c).await.muted);

        // Without a Home Assistant voice subscription there is nothing to start.
        let err = c
            .start_conversation(StartConversationRequest::default())
            .await
            .unwrap_err();
        assert_eq!(err.code, connectrpc::ErrorCode::FailedPrecondition);

        inputs
            .send(Input::Ha(HaCommand::ClientConnected {
                peer: "test".into(),
            }))
            .await
            .unwrap();
        inputs
            .send(Input::Ha(HaCommand::VoiceSubscribed(true)))
            .await
            .unwrap();
        c.start_conversation(StartConversationRequest::default())
            .await
            .unwrap();
        let s = status(&c).await;
        assert!(s.ha_connected && s.voice_subscribed);
        assert_eq!(s.phase, Phase::PHASE_LISTENING);

        c.stop_conversation(StopConversationRequest::default())
            .await
            .unwrap();
        assert_eq!(status(&c).await.phase, Phase::PHASE_IDLE);

        c.led_test(LedTestRequest::default()).await.unwrap();
    }

    /// Set the software mute through the brain: also orders after earlier inputs.
    async fn set_mute(c: &Client, muted: bool) -> Status {
        c.set_mute(pinna_api::SetMuteRequest {
            muted,
            ..Default::default()
        })
        .await
        .unwrap()
        .into_view()
        .to_owned_message()
        .status
        .into_option()
        .unwrap()
    }

    #[tokio::test]
    async fn notification_clears_on_what_the_ring_shows() {
        use pinna_hal::buttons::{Button, ButtonEvent};
        let (c, inputs) = start_device().await;
        let on = Input::Ha(HaCommand::Switch {
            object_id: "notification".into(),
            on: true,
        });
        let action = || Input::Button(ButtonEvent::Press(Button::Action));

        inputs.send(on).await.unwrap();
        assert!(set_mute(&c, false).await.notification);

        // Muted, the ring is red: the press doesn't clear what it hides.
        set_mute(&c, true).await;
        inputs.send(action()).await.unwrap();
        assert!(set_mute(&c, false).await.notification);

        // A running timer's arc shows above it: the press doesn't clear it.
        inputs
            .send(Input::Ha(HaCommand::Timer {
                event: TimerEventType::Started,
                id: "t1".into(),
                name: String::new(),
                total_seconds: 60,
                seconds_left: 60,
                is_active: true,
            }))
            .await
            .unwrap();
        inputs.send(action()).await.unwrap();
        assert!(set_mute(&c, false).await.notification);
        inputs
            .send(Input::Ha(HaCommand::Timer {
                event: TimerEventType::Cancelled,
                id: "t1".into(),
                name: String::new(),
                total_seconds: 60,
                seconds_left: 60,
                is_active: false,
            }))
            .await
            .unwrap();

        // Shown: the press clears it instead of starting a request.
        inputs.send(action()).await.unwrap();
        let s = set_mute(&c, false).await;
        assert!(!s.notification);
        assert_eq!(s.phase, Phase::PHASE_IDLE);

        // From the dashboard.
        inputs
            .send(Input::Ha(HaCommand::Switch {
                object_id: "notification".into(),
                on: true,
            }))
            .await
            .unwrap();
        let s = c
            .clear_notification(ClearNotificationRequest::default())
            .await
            .unwrap()
            .into_view()
            .to_owned_message()
            .status
            .into_option()
            .unwrap();
        assert!(!s.notification);
    }

    async fn next<B>(
        stream: &mut connectrpc::client::ServerStream<
            B,
            pinna_api::WatchStatusResponseView<'static>,
        >,
    ) -> Status
    where
        B: connectrpc::http_body::Body<Data = buffa::bytes::Bytes> + Unpin,
        B::Error: std::fmt::Display,
    {
        let msg = tokio::time::timeout(
            Duration::from_secs(5),
            stream.message::<WatchStatusResponse>(),
        )
        .await
        .expect("no status update")
        .unwrap()
        .expect("stream ended");
        msg.to_owned_message().status.into_option().unwrap()
    }

    #[tokio::test]
    async fn wake_words_from_the_dashboard_and_home_assistant() {
        let (c, inputs) = start_device().await;
        let words = |ids: &[&str]| pinna_api::UpdateSettingsRequest {
            wake_words: MessageField::some(pinna_api::WakeWordIds {
                ids: ids.iter().map(|s| s.to_string()).collect(),
                ..Default::default()
            }),
            ..Default::default()
        };
        let update = async |req: pinna_api::UpdateSettingsRequest| {
            c.update_settings(req).await.map(|r| {
                r.into_view()
                    .to_owned_message()
                    .status
                    .into_option()
                    .unwrap()
            })
        };
        let active = |s: &Status| s.settings.as_option().unwrap().wake_words.clone();
        let phrase = |s: &Status| s.wake_word.as_option().unwrap().phrase.clone();

        let s = status(&c).await;
        assert_eq!(active(&s), ["alexa_v0.1"]);
        assert_eq!(phrase(&s), "Alexa");
        let offered: Vec<_> = s
            .wake_word
            .as_option()
            .unwrap()
            .available
            .iter()
            .map(|w| (w.id.clone(), w.phrase.clone()))
            .collect();
        assert_eq!(
            offered,
            [
                ("alexa_v0.1".to_string(), "Alexa".to_string()),
                ("hey_jarvis_v0.1".to_string(), "Hey Jarvis".to_string())
            ]
        );

        // Without Home Assistant, the dashboard chooses.
        let s = update(words(&["hey_jarvis_v0.1", "alexa_v0.1"]))
            .await
            .unwrap();
        assert_eq!(active(&s), ["hey_jarvis_v0.1", "alexa_v0.1"]);
        assert_eq!(phrase(&s), "Hey Jarvis or Alexa");
        let s = update(words(&[])).await.unwrap();
        assert!(active(&s).is_empty());
        assert_eq!(phrase(&s), "");
        // Leaving the field out leaves the words alone.
        let s = update(Default::default()).await.unwrap();
        assert!(active(&s).is_empty());

        for bad in [
            &["gone"][..],
            &["alexa_v0.1", "alexa_v0.1"],
            &["alexa_v0.1", "hey_jarvis_v0.1", "alexa_v0.1"],
        ] {
            let err = update(words(bad)).await.unwrap_err();
            assert_eq!(err.code, connectrpc::ErrorCode::InvalidArgument, "{bad:?}");
        }

        // While Home Assistant is connected, it chooses.
        inputs
            .send(Input::Ha(HaCommand::ClientConnected { peer: "ha".into() }))
            .await
            .unwrap();
        let err = update(words(&["alexa_v0.1"])).await.unwrap_err();
        assert_eq!(err.code, connectrpc::ErrorCode::FailedPrecondition);
        assert!(
            err.message
                .unwrap_or_default()
                .contains("Home Assistant chooses")
        );
        inputs
            .send(Input::Ha(HaCommand::SetActiveWakeWords(vec![
                "alexa_v0.1".into(),
                "hey_jarvis_v0.1".into(),
            ])))
            .await
            .unwrap();
        // An empty update goes through the same queue, after Home Assistant's choice.
        let s = update(Default::default()).await.unwrap();
        assert_eq!(active(&s), ["alexa_v0.1", "hey_jarvis_v0.1"]);
    }

    #[tokio::test]
    async fn update_settings_round_trip() {
        let (c, _inputs) = start_device().await;
        let settings = |s: Status| s.settings.into_option().unwrap();
        let before = settings(status(&c).await);
        assert!(before.wake_word);

        let s = c
            .update_settings(pinna_api::UpdateSettingsRequest {
                wake_sensitivity: Some(70),
                led_brightness: Some(40),
                wake_word: Some(false),
                ..Default::default()
            })
            .await
            .unwrap()
            .into_view()
            .to_owned_message()
            .status
            .into_option()
            .unwrap();
        let after = settings(s);
        assert_eq!(
            (
                after.wake_sensitivity,
                after.led_brightness,
                after.wake_word
            ),
            (70, 40, false)
        );

        let err = c
            .update_settings(pinna_api::UpdateSettingsRequest {
                led_brightness: Some(101),
                ..Default::default()
            })
            .await
            .unwrap_err();
        assert_eq!(err.code, connectrpc::ErrorCode::InvalidArgument);
        assert_eq!(settings(status(&c).await).led_brightness, 40);
    }

    #[tokio::test]
    async fn name_wake_sound_and_hosts_round_trip() {
        let (c, _inputs) = start_device().await;
        let settings = |s: Status| s.settings.into_option().unwrap();
        let before = settings(status(&c).await);
        assert_eq!(before.friendly_name, "Echo Dot aabbcc");
        assert!(before.default_name && !before.wake_sound);
        assert!(before.allowed_hosts.is_empty());

        let update = async |req: pinna_api::UpdateSettingsRequest| {
            c.update_settings(req).await.map(|r| {
                settings(
                    r.into_view()
                        .to_owned_message()
                        .status
                        .into_option()
                        .unwrap(),
                )
            })
        };
        let after = update(pinna_api::UpdateSettingsRequest {
            friendly_name: Some("  Küche ".into()),
            wake_sound: Some(true),
            allowed_hosts: MessageField::some(pinna_api::HostNames {
                names: vec!["Kitchen.home.arpa.".into(), "kitchen.home.arpa".into()],
                ..Default::default()
            }),
            ..Default::default()
        })
        .await
        .unwrap();
        assert_eq!(after.friendly_name, "Küche");
        assert!(!after.default_name && after.wake_sound);
        assert_eq!(after.allowed_hosts, ["kitchen.home.arpa"]);

        for bad in [
            pinna_api::UpdateSettingsRequest {
                friendly_name: Some(" ".into()),
                ..Default::default()
            },
            pinna_api::UpdateSettingsRequest {
                allowed_hosts: MessageField::some(pinna_api::HostNames {
                    names: vec!["pinna.local".into()],
                    ..Default::default()
                }),
                ..Default::default()
            },
        ] {
            let err = update(bad).await.unwrap_err();
            assert_eq!(err.code, connectrpc::ErrorCode::InvalidArgument);
        }
        // An empty list clears the dashboard's names; the rest stays.
        let cleared = update(pinna_api::UpdateSettingsRequest {
            allowed_hosts: MessageField::some(pinna_api::HostNames::default()),
            ..Default::default()
        })
        .await
        .unwrap();
        assert!(cleared.allowed_hosts.is_empty());
        assert_eq!(cleared.friendly_name, "Küche");
    }

    #[tokio::test]
    async fn hostname_renames_the_node() {
        let (c, _inputs) = start_device().await;
        assert_eq!(status(&c).await.name, "pinna-test");
        let rename = async |name: &str| {
            c.update_settings(pinna_api::UpdateSettingsRequest {
                hostname: Some(name.into()),
                ..Default::default()
            })
            .await
        };
        let r = rename(" Kitchen-Dot ").await.unwrap();
        let renamed = r
            .into_view()
            .to_owned_message()
            .status
            .into_option()
            .unwrap();
        assert_eq!(renamed.name, "kitchen-dot");
        assert_eq!(status(&c).await.name, "kitchen-dot");
        for bad in ["", "kitchen.local", "-x", "a b"] {
            let err = rename(bad).await.unwrap_err();
            assert_eq!(err.code, connectrpc::ErrorCode::InvalidArgument, "{bad:?}");
        }
        assert_eq!(status(&c).await.name, "kitchen-dot");
    }

    #[tokio::test]
    async fn recent_wakes_carry_the_setting_that_wakes_on_them() {
        let (c, inputs) = start_device().await;
        let near = |score, reached_threshold| Input::NearMiss {
            score,
            reached_threshold,
        };
        inputs.send(near(0.48, false)).await.unwrap();
        inputs.send(near(0.6, true)).await.unwrap();
        inputs.send(near(0.3, false)).await.unwrap();
        inputs
            .send(Input::Wake {
                id: "alexa_v0.1".into(),
                score: 0.62,
            })
            .await
            .unwrap();
        let wake = set_mute(&c, false).await.wake_word.into_option().unwrap();
        // The default sensitivity, 68.
        assert!((wake.threshold - 0.5508).abs() < 1e-4);
        let got: Vec<_> = wake
            .recent
            .iter()
            .map(|e| (e.woke, e.wakes_from, e.reached_threshold))
            .collect();
        assert_eq!(
            got,
            [
                (false, Some(85), false),
                (false, Some(57), true),
                (false, None, false),
                (true, Some(53), false),
            ]
        );
        assert!(wake.recent.iter().all(|e| e.unix_ms > 1_700_000_000_000));

        // Only the last 20 are kept.
        for i in 0..30 {
            inputs
                .send(near(0.2 + i as f32 / 100.0, false))
                .await
                .unwrap();
        }
        let recent = set_mute(&c, false)
            .await
            .wake_word
            .into_option()
            .unwrap()
            .recent;
        assert_eq!(recent.len(), 20);
        assert!((recent[0].score - 0.3).abs() < 1e-6);
        assert!((recent[19].score - 0.49).abs() < 1e-6);
    }

    #[tokio::test]
    async fn calibration_tunes_the_sensitivity_from_the_buttons() {
        use pinna_hal::buttons::{Button, ButtonEvent};
        let (c, inputs) = start_device().await;
        let press = |b| Input::Button(ButtonEvent::Press(b));
        let calibration = |s: &Status| {
            s.wake_word
                .as_option()
                .unwrap()
                .calibration
                .as_option()
                .cloned()
        };
        let sensitivity = |s: &Status| s.settings.as_option().unwrap().wake_sensitivity;

        // Nothing to tune while the microphones are off.
        set_mute(&c, true).await;
        let err = c
            .start_calibration(StartCalibrationRequest::default())
            .await
            .unwrap_err();
        assert_eq!(err.code, connectrpc::ErrorCode::FailedPrecondition);
        set_mute(&c, false).await;

        let s = c
            .start_calibration(StartCalibrationRequest::default())
            .await
            .unwrap()
            .into_view()
            .to_owned_message()
            .status
            .into_option()
            .unwrap();
        let cal = calibration(&s).unwrap();
        assert_eq!(cal.started_from, 68);
        assert!(cal.events.is_empty());

        // A near miss, then + : raise just enough to catch it.
        inputs
            .send(Input::NearMiss {
                score: 0.5,
                reached_threshold: false,
            })
            .await
            .unwrap();
        inputs.send(press(Button::VolumeUp)).await.unwrap();
        let s = set_mute(&c, false).await;
        assert_eq!(sensitivity(&s), 85);
        let cal = calibration(&s).unwrap();
        let last = cal.events.last().unwrap();
        assert_eq!(
            last.kind,
            CalibrationEventKind::CALIBRATION_EVENT_KIND_RAISED
        );
        assert_eq!((last.from, last.to), (68, 85));
        assert!((last.score.unwrap() - 0.5).abs() < 1e-6);

        // Action ends it; the setting stays.
        inputs.send(press(Button::Action)).await.unwrap();
        let s = set_mute(&c, false).await;
        assert!(calibration(&s).is_none());
        assert_eq!(sensitivity(&s), 85);

        // A fresh calibration: a wake, then − lowers to ignore it. The wake
        // starts no request.
        c.start_calibration(StartCalibrationRequest::default())
            .await
            .unwrap();
        inputs
            .send(Input::Wake {
                id: "alexa_v0.1".into(),
                score: 0.62,
            })
            .await
            .unwrap();
        inputs.send(press(Button::VolumeDown)).await.unwrap();
        let s = set_mute(&c, false).await;
        assert_eq!(sensitivity(&s), 49);
        assert_eq!(s.phase, Phase::PHASE_IDLE);
        let cal = calibration(&s).unwrap();
        let last = cal.events.last().unwrap();
        assert_eq!(
            last.kind,
            CalibrationEventKind::CALIBRATION_EVENT_KIND_LOWERED
        );
        assert_eq!((last.from, last.to), (85, 49));
        c.stop_calibration(StopCalibrationRequest::default())
            .await
            .unwrap();
        assert!(calibration(&set_mute(&c, false).await).is_none());

        // The Home Assistant switch starts and stops it; the wake word being
        // off refuses a start.
        inputs
            .send(Input::Ha(HaCommand::Switch {
                object_id: "wake_word_calibration".into(),
                on: true,
            }))
            .await
            .unwrap();
        assert!(calibration(&set_mute(&c, false).await).is_some());
        c.stop_calibration(StopCalibrationRequest::default())
            .await
            .unwrap();
        assert!(calibration(&set_mute(&c, false).await).is_none());
        inputs
            .send(Input::Ha(HaCommand::Switch {
                object_id: "wake_word".into(),
                on: false,
            }))
            .await
            .unwrap();
        let err = c
            .start_calibration(StartCalibrationRequest::default())
            .await
            .unwrap_err();
        assert_eq!(err.code, connectrpc::ErrorCode::FailedPrecondition);
    }

    #[tokio::test]
    async fn watch_ring_streams_the_latest_frame_at_most_25_a_second() {
        let info = Info {
            version: "0.1.0".into(),
            mac: String::new(),
            wake_words: crate::wakewords::test_catalogue(),
            audio_backend: "none".into(),
            led_backend: "none".into(),
            model: "echo-dot-2".into(),
            virtual_device: false,
            last_boot: boot::LastBoot::cold(),
            key_from_image: false,
        };
        let (_status_tx, status_rx) = watch::channel(Snapshot::default());
        let (in_tx, _in_rx) = mpsc::channel(1);
        let (ring_tx, ring_rx) = watch::channel([Rgb::OFF; RING_PIXELS]);
        let api = DeviceApi::new(info, status_rx, in_tx).with_ring(ring_rx);
        let addr = start(Arc::new(api), "127.0.0.1:0").await.unwrap();
        let config = ClientConfig::new(format!("http://{addr}").parse().unwrap());
        let c = Client::new(HttpClient::plaintext(), config);

        let mut stream = c.watch_ring(WatchRingRequest::default()).await.unwrap();
        let mut next = async || {
            tokio::time::timeout(
                Duration::from_secs(5),
                stream.message::<WatchRingResponse>(),
            )
            .await
            .expect("no frame")
            .unwrap()
            .expect("stream ended")
            .to_owned_message()
            .frame
        };
        // The current frame first: 12 pixels, 3 bytes each.
        assert_eq!(next().await, vec![0; RING_PIXELS * 3]);

        // Frames faster than the interval: the latest one wins.
        let lit = |r: u8| {
            let mut f = [Rgb::OFF; RING_PIXELS];
            f[0] = Rgb::new(r, 0x80, 0xff);
            f
        };
        let started = Instant::now();
        for r in 1..=10 {
            ring_tx.send_replace(lit(r));
        }
        let frame = next().await;
        assert_eq!(&frame[..3], &[10, 0x80, 0xff]);
        assert_eq!(&frame[3..], &[0; (RING_PIXELS - 1) * 3]);
        assert!(
            started.elapsed() >= RING_INTERVAL / 2,
            "{:?}",
            started.elapsed()
        );

        // Only changes are sent.
        ring_tx.send_replace(lit(10));
        ring_tx.send_replace(lit(11));
        assert_eq!(next().await[0], 11);
    }

    #[test]
    fn render_reports_a_ringing_alarm() {
        let info = Info {
            version: "0.1.0".into(),
            mac: String::new(),
            wake_words: crate::wakewords::test_catalogue(),
            audio_backend: "none".into(),
            led_backend: "none".into(),
            model: "echo-dot-2".into(),
            virtual_device: false,
            last_boot: boot::LastBoot::cold(),
            key_from_image: false,
        };
        let mut snap = Snapshot::default();
        assert!(!render(&info, Instant::now(), &snap).alarm_ringing);
        snap.alarm_ringing = true;
        assert!(render(&info, Instant::now(), &snap).alarm_ringing);
        assert!(!render(&info, Instant::now(), &snap).led_test);
        snap.led_test = true;
        assert!(render(&info, Instant::now(), &snap).led_test);
    }

    #[tokio::test]
    async fn airplay_stays_off_without_a_stream_socket() {
        let (c, inputs) = start_device().await;
        let airplay = |s: &Status| s.settings.as_option().unwrap().airplay;
        assert!(!airplay(&status(&c).await), "off by default");
        let state = status(&c).await.airplay.as_option().unwrap().state;
        assert_eq!(
            state,
            buffa::EnumValue::Known(pinna_api::AirPlayState::AIRPLAY_STATE_UNAVAILABLE)
        );
        let err = c
            .update_settings(UpdateSettingsRequest {
                airplay: Some(true),
                ..Default::default()
            })
            .await
            .unwrap_err();
        assert_eq!(err.code, connectrpc::ErrorCode::FailedPrecondition);
        inputs
            .send(Input::Ha(HaCommand::Switch {
                object_id: "airplay".into(),
                on: true,
            }))
            .await
            .unwrap();
        // Off from the dashboard is always fine, and orders after the switch.
        c.update_settings(UpdateSettingsRequest {
            airplay: Some(false),
            ..Default::default()
        })
        .await
        .unwrap();
        assert!(!airplay(&status(&c).await));
    }

    #[tokio::test]
    async fn bluetooth_stays_off_without_a_controller() {
        let (c, inputs) = start_device().await;
        let on = |s: &Status| s.settings.as_option().unwrap().bluetooth;
        let st = status(&c).await;
        assert!(!on(&st), "off by default");
        assert_eq!(
            st.bluetooth.as_option().unwrap().state,
            buffa::EnumValue::Known(pinna_api::BluetoothState::BLUETOOTH_STATE_UNAVAILABLE)
        );
        let err = c
            .update_settings(UpdateSettingsRequest {
                bluetooth: Some(true),
                ..Default::default()
            })
            .await
            .unwrap_err();
        assert_eq!(err.code, connectrpc::ErrorCode::FailedPrecondition);
        inputs
            .send(Input::Ha(HaCommand::Switch {
                object_id: "bluetooth".into(),
                on: true,
            }))
            .await
            .unwrap();
        let err = c
            .start_bluetooth_pairing(pinna_api::StartBluetoothPairingRequest::default())
            .await
            .unwrap_err();
        assert_eq!(err.code, connectrpc::ErrorCode::FailedPrecondition);
        let err = c
            .forget_bluetooth_device(pinna_api::ForgetBluetoothDeviceRequest {
                address: "../../x".into(),
                ..Default::default()
            })
            .await
            .unwrap_err();
        assert_eq!(err.code, connectrpc::ErrorCode::InvalidArgument);
        let st = status(&c).await;
        assert!(!on(&st));
        assert_eq!(st.bluetooth.as_option().unwrap().pairing_until_ms, 0);
    }

    #[test]
    fn stream_volume_only_from_the_device() {
        for local in ["127.0.0.1:1", "[::1]:1", "[::ffff:127.0.0.1]:1"] {
            assert!(from_device(Some(local.parse().unwrap())), "{local}");
        }
        assert!(!from_device(Some("192.168.1.5:1".parse().unwrap())));
        assert!(!from_device(None));
    }

    #[tokio::test]
    async fn home_assistant_sets_the_stream_volume_while_a_stream_plays() {
        let (c, inputs) = start_device().await;
        let stream_volume = |percent| pinna_api::SetStreamVolumeRequest {
            percent,
            ..Default::default()
        };
        let ha_volume = |v| {
            Input::Ha(HaCommand::MediaPlayer {
                command: None,
                volume: Some(v),
                media_url: None,
                announcement: false,
            })
        };
        let err = c.set_stream_volume(stream_volume(101)).await.unwrap_err();
        assert_eq!(err.code, connectrpc::ErrorCode::InvalidArgument);
        c.set_stream_volume(stream_volume(30)).await.unwrap();
        assert_eq!(status(&c).await.volume, 40, "the speaker volume stays");

        let playing = pinna_audio::MediaState {
            phase: pinna_audio::MediaPhase::Playing,
            url: Some(pinna_audio::STREAM_URL.into()),
            ..Default::default()
        };
        inputs.send(Input::Media(playing)).await.unwrap();
        inputs.send(ha_volume(0.6)).await.unwrap();
        // Inputs are handled in order: this answer comes after both.
        c.set_stream_volume(stream_volume(60)).await.unwrap();
        assert_eq!(status(&c).await.volume, 40, "went to the stream instead");

        inputs
            .send(Input::Media(pinna_audio::MediaState::default()))
            .await
            .unwrap();
        inputs.send(ha_volume(0.6)).await.unwrap();
        c.set_stream_volume(stream_volume(60)).await.unwrap();
        assert_eq!(status(&c).await.volume, 60, "no stream: the speaker volume");
    }

    #[tokio::test]
    async fn watch_status_streams_changes() {
        let (c, _inputs) = start_device().await;
        let mut stream = c.watch_status(WatchStatusRequest::default()).await.unwrap();
        let first = next(&mut stream).await;
        assert_eq!(first.volume, 40);

        c.set_volume(pinna_api::SetVolumeRequest {
            percent: 12,
            ..Default::default()
        })
        .await
        .unwrap();
        let changed = next(&mut stream).await;
        assert_eq!(changed.volume, 12);
    }

    /// An updater over a temp dir with a fake `pinnactl` that runs `body`.
    struct Rig {
        dir: std::path::PathBuf,
        paths: UpdaterPaths,
    }

    impl Rig {
        fn new(name: &str, body: &str) -> Self {
            use std::os::unix::fs::PermissionsExt;
            let dir =
                std::env::temp_dir().join(format!("pinnad-upd-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            for d in ["run", "parts"] {
                std::fs::create_dir_all(dir.join(d)).unwrap();
            }
            let script = dir.join("pinnactl");
            std::fs::write(&script, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
            let paths = UpdaterPaths {
                misc: dir.join("misc"),
                partdir: dir.join("parts"),
                run_dir: dir.join("run"),
                pinnactl: script,
                reboot: vec!["true".into()],
                reset_mark: dir.join("factory-reset"),
                key: slot_key().verifying_key(),
            };
            Self { dir, paths }
        }

        /// Booted from `booted`, with `state` in misc and version `b_version`
        /// sealed into slot b (signed with `key`).
        fn slots(&self, booted: &str, state: SlotState, b_version: &str, key: &SigningKey) {
            std::fs::write(self.paths.run_dir.join("boot-slot"), format!("{booted}\n")).unwrap();
            std::fs::write(self.paths.run_dir.join("boot-version"), "1.0\n").unwrap();
            std::fs::write(&self.paths.misc, vec![0u8; 0x80000]).unwrap();
            pinna_bootctl::slot::write_state(&self.paths.misc, &state).unwrap();
            let b = self.paths.partdir.join("system_b");
            std::fs::write(b, sealed(b_version, key)).unwrap();
        }

        fn state(&self) -> Option<SlotState> {
            pinna_bootctl::slot::read_state(&self.paths.misc).unwrap()
        }

        fn reboot_reason(&self) -> Option<String> {
            std::fs::read_to_string(self.paths.run_dir.join("reboot-reason"))
                .ok()
                .map(|r| r.trim().to_owned())
        }

        fn updater(&self) -> Updater {
            Updater::new(self.paths.clone())
        }
    }

    impl Drop for Rig {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    use pinna_bootctl::slot::{Slot, SlotInfo, SlotState};
    use pinna_image::keys::SigningKey;

    fn slot_key() -> SigningKey {
        SigningKey::from_bytes(&[42; 32])
    }

    /// A signed slot image: a small erofs-shaped root, footer and hash tree.
    fn sealed(version: &str, key: &SigningKey) -> Vec<u8> {
        let blocks = 16u32;
        let mut img = vec![0u8; blocks as usize * 4096];
        img[1024..1028].copy_from_slice(&0xE0F5_E1E2u32.to_le_bytes());
        img[1024 + 12] = 12;
        img[1024 + 36..1024 + 40].copy_from_slice(&blocks.to_le_bytes());
        let mut out = Vec::new();
        pinna_image::seal(&img[..], version, [7; 32], key, &mut out).unwrap();
        out
    }

    fn st(active: Slot, a: (u8, bool), b: (u8, bool)) -> SlotState {
        let info = |(tries_left, successful)| SlotInfo {
            tries_left,
            successful,
        };
        SlotState {
            active,
            slots: [info(a), info(b)],
        }
    }

    fn slot_request(slot: &str) -> SetActiveSlotRequest {
        SetActiveSlotRequest {
            slot: slot.into(),
            ..Default::default()
        }
    }

    async fn updates(c: &Client) -> GetUpdatesResponse {
        c.get_updates(GetUpdatesRequest::default())
            .await
            .unwrap()
            .into_view()
            .to_owned_message()
    }

    fn install_request(manifest: &str) -> InstallUpdateRequest {
        InstallUpdateRequest {
            manifest: manifest.into(),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn wifi_scan_and_a_streamed_join() {
        use crate::wifi::fake::{Fake, Outcome};
        use pinna_api::JoinStage;
        let fake = Fake::start(Outcome::Joins);
        let wifi = crate::wifi::Wifi::with_timeouts(
            fake.paths(),
            Duration::from_secs(3),
            Duration::from_secs(60),
        );
        let (c, _inputs) =
            start_device_inner(Updater::new(UpdaterPaths::default()), Some(wifi)).await;

        let scan = c.scan_wifi(ScanWifiRequest::default()).await.unwrap();
        let names: Vec<String> = scan
            .into_view()
            .to_owned_message()
            .networks
            .into_iter()
            .map(|n| n.ssid)
            .collect();
        assert_eq!(names, ["Work", "Home", "Café", "Neighbour"]);

        let join = |pass: &str| JoinWifiRequest {
            ssid: "Café".into(),
            security: pinna_api::WifiSecurity::WIFI_SECURITY_WPA_PSK.into(),
            passphrase: pass.into(),
            ..Default::default()
        };
        // Checked before anything reaches wpa_supplicant; a streaming call
        // reports it on the stream.
        let err = match c.join_wifi(join("short")).await {
            Err(e) => e,
            Ok(mut s) => s
                .message::<JoinWifiResponse>()
                .await
                .map(|_| ())
                .expect_err("refused"),
        };
        assert_eq!(err.code, connectrpc::ErrorCode::InvalidArgument);

        let mut stream = c.join_wifi(join("long enough")).await.unwrap();
        let mut stages = Vec::new();
        while let Some(m) = tokio::time::timeout(
            Duration::from_secs(10),
            stream.message::<JoinWifiResponse>(),
        )
        .await
        .expect("JoinWifi stalled")
        .unwrap()
        {
            stages.push(m.to_owned_message().stage.as_known().unwrap());
        }
        assert_eq!(
            stages,
            [
                JoinStage::JOIN_STAGE_CONNECTING,
                JoinStage::JOIN_STAGE_ADDRESS,
                JoinStage::JOIN_STAGE_TRIAL
            ]
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
        let list = c.list_wifi(ListWifiRequest::default()).await.unwrap();
        let list = list.into_view().to_owned_message();
        assert!(!list.joining);
        assert_eq!(list.trial.as_option().unwrap().ssid, "Café");
        let kept = c
            .keep_wifi(KeepWifiRequest {
                keep: true,
                ..Default::default()
            })
            .await
            .unwrap();
        let kept = kept
            .into_view()
            .to_owned_message()
            .result
            .into_option()
            .unwrap();
        assert_eq!(kept.stage.as_known(), Some(JoinStage::JOIN_STAGE_JOINED));
    }

    /// Every message of a WatchInstall until it ends.
    async fn watch_install_all(c: &Client) -> Vec<WatchInstallResponse> {
        let mut stream = c
            .watch_install(WatchInstallRequest::default())
            .await
            .unwrap();
        let mut out = Vec::new();
        while let Some(m) = tokio::time::timeout(
            Duration::from_secs(10),
            stream.message::<WatchInstallResponse>(),
        )
        .await
        .expect("WatchInstall stalled")
        .unwrap()
        {
            out.push(m.to_owned_message());
        }
        out
    }

    async fn wait_until_idle(c: &Client) -> GetUpdatesResponse {
        for _ in 0..100 {
            let u = updates(c).await;
            if !u.installing {
                return u;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("the install did not finish");
    }

    #[tokio::test]
    async fn get_updates_without_pinna_init_is_unavailable() {
        let rig = Rig::new("noinit", "");
        let (c, _) = start_device_with(rig.updater()).await;
        let u = updates(&c).await;
        assert!(!u.available);
        assert!(
            u.unavailable_reason.contains("pinna-init"),
            "{}",
            u.unavailable_reason
        );
        assert_eq!(u.suggested_manifest, "http://127.0.0.1:8000/manifest.json");
    }

    #[tokio::test]
    async fn get_updates_reads_the_slots() {
        let rig = Rig::new("slots", "");
        std::fs::write(rig.paths.run_dir.join("boot-slot"), "a\n").unwrap();
        std::fs::write(rig.paths.run_dir.join("boot-version"), "1.2.3\n").unwrap();
        std::fs::write(&rig.paths.misc, vec![0u8; 0x40000 + 512]).unwrap();
        let (c, _) = start_device_with(rig.updater()).await;
        let u = updates(&c).await;
        assert!(u.available, "{}", u.unavailable_reason);
        assert_eq!(u.booted_version, "1.2.3");
        assert_eq!(u.slots.len(), 2);
        let (a, b) = (&u.slots[0], &u.slots[1]);
        assert_eq!((a.name.as_str(), a.state.as_str()), ("a", "good"));
        assert!(a.booted && a.active && a.version.is_empty());
        assert_eq!((b.name.as_str(), b.state.as_str()), ("b", "unbootable"));
        assert!(!b.booted && !b.active && b.version.is_empty());
        assert!(!u.installing && u.last_install.as_option().is_none());
    }

    #[tokio::test]
    async fn install_streams_progress_and_records_the_result() {
        let rig = Rig::new("ok", "echo one; echo two >&2; echo three; exit 0");
        let (c, _) = start_device_with(rig.updater()).await;
        // Nothing has run yet.
        assert!(watch_install_all(&c).await.is_empty());

        c.install_update(install_request("http://10.0.0.2:8000/manifest.json"))
            .await
            .unwrap();
        let msgs = watch_install_all(&c).await;
        let last = msgs.last().unwrap();
        assert!(last.done && last.ok);
        let mut lines: Vec<_> = msgs
            .iter()
            .filter(|m| !m.done)
            .map(|m| m.line.as_str())
            .collect();
        lines.sort_unstable();
        assert_eq!(lines, ["one", "three", "two"]);

        let u = wait_until_idle(&c).await;
        let r = u.last_install.as_option().unwrap();
        assert!(r.ok);
        assert_eq!(r.manifest, "http://10.0.0.2:8000/manifest.json");
        assert!(r.finished_unix_ms > 0);

        // A watcher that comes late gets the history and the result.
        let msgs = watch_install_all(&c).await;
        assert_eq!(msgs.len(), 4);
        assert!(msgs[3].done && msgs[3].ok);
    }

    #[tokio::test]
    async fn failed_install_reports_its_last_line() {
        let rig = Rig::new("fail", "echo working; echo boom; exit 1");
        let (c, _) = start_device_with(rig.updater()).await;
        c.install_update(install_request("https://example.org/manifest.json"))
            .await
            .unwrap();
        let msgs = watch_install_all(&c).await;
        let last = msgs.last().unwrap();
        assert!(last.done && !last.ok);
        let u = wait_until_idle(&c).await;
        let r = u.last_install.as_option().unwrap();
        assert!(!r.ok);
        assert_eq!(r.last_line, "boom");
    }

    #[tokio::test]
    async fn second_install_is_refused_while_one_runs() {
        let rig = Rig::new("busy", "echo start; sleep 1; echo end");
        let (c, _) = start_device_with(rig.updater()).await;
        c.install_update(install_request("http://h/m.json"))
            .await
            .unwrap();
        assert!(updates(&c).await.installing);
        let err = c
            .install_update(install_request("http://h/m.json"))
            .await
            .unwrap_err();
        assert_eq!(err.code, connectrpc::ErrorCode::FailedPrecondition);
        assert!(
            !wait_until_idle(&c)
                .await
                .last_install
                .as_option()
                .unwrap()
                .last_line
                .is_empty()
        );
    }

    #[tokio::test]
    async fn install_rejects_other_schemes() {
        let rig = Rig::new("scheme", "exit 0");
        let (c, _) = start_device_with(rig.updater()).await;
        for bad in ["file:///x", "ftp://h/m.json", "not a url", ""] {
            let err = c.install_update(install_request(bad)).await.unwrap_err();
            assert_eq!(err.code, connectrpc::ErrorCode::InvalidArgument, "{bad}");
        }
    }

    #[tokio::test]
    async fn install_goes_on_when_the_watcher_leaves() {
        let rig = Rig::new("leave", "echo start; sleep 1; echo end");
        let (c, _) = start_device_with(rig.updater()).await;
        c.install_update(install_request("http://h/m.json"))
            .await
            .unwrap();
        let mut stream = c
            .watch_install(WatchInstallRequest::default())
            .await
            .unwrap();
        let first = stream
            .message::<WatchInstallResponse>()
            .await
            .unwrap()
            .unwrap();
        assert_eq!(first.line(), "start");
        drop(stream);
        let u = wait_until_idle(&c).await;
        let r = u.last_install.as_option().unwrap();
        assert!(r.ok);
        assert_eq!(r.last_line, "end");
    }

    #[tokio::test]
    async fn factory_reset_needs_the_name_typed() {
        let rig = Rig::new("reset", "");
        let (c, _) = start_device_with(rig.updater()).await;
        let reset = async |name: &str| {
            c.factory_reset(pinna_api::FactoryResetRequest {
                name: name.into(),
                ..Default::default()
            })
            .await
        };
        let err = reset("").await.unwrap_err();
        assert_eq!(err.code, connectrpc::ErrorCode::InvalidArgument);
        assert!(err.message.unwrap_or_default().contains("Echo Dot aabbcc"));
        assert!(reset("Kitchen").await.is_err());
        assert!(!rig.paths.reset_mark.exists());
        // The name Home Assistant shows, or the node name, in any case.
        reset("  echo dot AABBCC ").await.unwrap();
        assert!(rig.paths.reset_mark.exists());
        std::fs::remove_file(&rig.paths.reset_mark).unwrap();
        reset("pinna-test").await.unwrap();
        assert!(rig.paths.reset_mark.exists());
        let reason = std::fs::read_to_string(rig.paths.run_dir.join("reboot-reason")).unwrap();
        assert_eq!(reason.trim(), "factory-reset");
    }

    #[tokio::test]
    async fn set_active_slot_switches_and_restarts() {
        let rig = Rig::new("switch", "");
        let before = st(Slot::A, (0, true), (0, true));
        rig.slots("a", before, "0.9", &slot_key());
        let (c, _) = start_device_with(rig.updater()).await;
        assert_eq!(updates(&c).await.slots[1].version, "0.9");

        c.set_active_slot(slot_request("b")).await.unwrap();
        assert_eq!(rig.state(), Some(st(Slot::B, (0, true), (3, false))));
        assert_eq!(rig.reboot_reason().as_deref(), Some("switch-slot"));
    }

    #[tokio::test]
    async fn set_active_slot_puts_the_state_back_when_the_restart_fails() {
        let rig = Rig::new("switch-noreboot", "");
        let before = st(Slot::A, (0, true), (0, true));
        rig.slots("a", before, "0.9", &slot_key());
        let mut paths = rig.paths.clone();
        paths.reboot = vec!["false".into()];
        let (c, _) = start_device_with(Updater::new(paths)).await;

        let err = c.set_active_slot(slot_request("b")).await.unwrap_err();
        assert_eq!(err.code, connectrpc::ErrorCode::Internal);
        assert_eq!(rig.state(), Some(before));
        assert_eq!(rig.reboot_reason(), None);
    }

    #[tokio::test]
    async fn set_active_slot_keeps_the_running_slot() {
        let rig = Rig::new("keep", "");
        // An installed update waits in slot b.
        rig.slots("a", st(Slot::B, (0, true), (3, false)), "2.0", &slot_key());
        let (c, _) = start_device_with(rig.updater()).await;

        c.set_active_slot(slot_request("a")).await.unwrap();
        assert_eq!(rig.state(), Some(st(Slot::A, (0, true), (3, false))));
        assert_eq!(rig.reboot_reason(), None, "keeping doesn't restart");

        // Slot b hasn't been started: it can still be switched to.
        c.set_active_slot(slot_request("b")).await.unwrap();
        assert_eq!(rig.state(), Some(st(Slot::B, (0, true), (3, false))));
    }

    #[tokio::test]
    async fn set_active_slot_refusals_change_nothing() {
        use connectrpc::ErrorCode::{FailedPrecondition, InvalidArgument};
        let refused = async |rig: &Rig, slot: &str, code| {
            let before = rig.state();
            let (c, _) = start_device_with(rig.updater()).await;
            let err = c.set_active_slot(slot_request(slot)).await.unwrap_err();
            assert_eq!(
                err.code,
                code,
                "{slot}: {}",
                err.message.unwrap_or_default()
            );
            assert_eq!(rig.state(), before);
            assert_eq!(rig.reboot_reason(), None);
        };
        let good = st(Slot::A, (0, true), (0, true));

        let rig = Rig::new("refuse-name", "");
        rig.slots("a", good, "0.9", &slot_key());
        refused(&rig, "c", InvalidArgument).await;

        let rig = Rig::new("refuse-no-init", "");
        std::fs::write(&rig.paths.misc, vec![0u8; 0x80000]).unwrap();
        refused(&rig, "b", FailedPrecondition).await;

        // The running slot is on trial: slot a is its only fallback.
        let rig = Rig::new("refuse-trial", "");
        rig.slots("b", st(Slot::B, (0, true), (2, false)), "0.9", &slot_key());
        refused(&rig, "a", FailedPrecondition).await;

        // Slot b failed its trial.
        let rig = Rig::new("refuse-failed", "");
        rig.slots("a", st(Slot::A, (0, true), (0, false)), "0.9", &slot_key());
        refused(&rig, "b", FailedPrecondition).await;

        // Signed with another key.
        let rig = Rig::new("refuse-key", "");
        rig.slots("a", good, "0.9", &SigningKey::from_bytes(&[1; 32]));
        refused(&rig, "b", FailedPrecondition).await;

        // A corrupted top hash block.
        let rig = Rig::new("refuse-hash", "");
        rig.slots("a", good, "0.9", &slot_key());
        let b = rig.paths.partdir.join("system_b");
        let mut img = std::fs::read(&b).unwrap();
        let last = img.len() - 1;
        img[last] ^= 0xff;
        std::fs::write(&b, img).unwrap();
        refused(&rig, "b", FailedPrecondition).await;
    }

    #[tokio::test]
    async fn set_active_slot_waits_for_an_install() {
        let rig = Rig::new("switch-installing", "sleep 2");
        rig.slots("a", st(Slot::A, (0, true), (0, true)), "0.9", &slot_key());
        let before = rig.state();
        let (c, _) = start_device_with(rig.updater()).await;
        c.install_update(install_request("http://10.0.0.2:8000/manifest.json"))
            .await
            .unwrap();
        let err = c.set_active_slot(slot_request("b")).await.unwrap_err();
        assert_eq!(err.code, connectrpc::ErrorCode::FailedPrecondition);
        assert_eq!(rig.state(), before);
    }

    #[tokio::test]
    async fn reboot_runs_the_command() {
        let rig = Rig::new("reboot", "");
        let (c, _) = start_device_with(rig.updater()).await;
        c.reboot(RebootRequest::default()).await.unwrap();

        let mut paths = rig.paths.clone();
        paths.reboot = vec!["false".into()];
        let (c, _) = start_device_with(Updater::new(paths)).await;
        let err = c.reboot(RebootRequest::default()).await.unwrap_err();
        assert_eq!(err.code, connectrpc::ErrorCode::Internal);
    }
}
