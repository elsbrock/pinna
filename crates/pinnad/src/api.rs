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
    LedTestRequest, LedTestResponse, RebootRequest, RebootResponse, SetMuteRequest,
    SetMuteResponse, SetVolumeRequest, SetVolumeResponse, StartCalibrationRequest,
    StartCalibrationResponse, StartConversationRequest, StartConversationResponse, Status,
    StopCalibrationRequest, StopCalibrationResponse, StopConversationRequest,
    StopConversationResponse, Timer, UpdateSettingsRequest, UpdateSettingsResponse, WakeEvent,
    WakeWord, WakeWordOption, WatchInstallRequest, WatchInstallResponse, WatchLogsRequest,
    WatchLogsResponse, WatchStatusRequest, WatchStatusResponse,
};
use tokio::sync::{mpsc, oneshot, watch};

use crate::boot;
use crate::updates::{Event, Updater, UpdaterPaths, stage_of};
use crate::voice::{ApiCommand, ApiReply, Input, SettingsPatch, Snapshot};
use crate::{logs, network};

/// What does not change while pinnad runs.
#[derive(Debug, Clone)]
pub struct Info {
    /// The running image version (pinna-init's boot-version), or pinnad's own
    /// version where no image was booted.
    pub version: String,
    pub name: String,
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
}

pub struct DeviceApi {
    info: Info,
    started: Instant,
    status: watch::Receiver<Snapshot>,
    inputs: mpsc::Sender<Input>,
    updater: Arc<Updater>,
}

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
            inputs,
            updater: Arc::new(updater),
        }
    }

    fn render(&self, s: &Snapshot) -> Status {
        render(&self.info, self.started, s)
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
        name: info.name.clone(),
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
        settings: MessageField::some(pinna_api::Settings {
            wake_word: s.settings.wake_word,
            wake_sensitivity: s.settings.wake_sensitivity.into(),
            led_brightness: s.settings.led_brightness.into(),
            wake_words: s.active_wake_words.clone(),
            ..Default::default()
        }),
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
        let patch = SettingsPatch {
            wake_word: request.wake_word,
            wake_sensitivity: percent("wake_sensitivity", request.wake_sensitivity)?,
            led_brightness: percent("led_brightness", request.led_brightness)?,
            wake_words,
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

    async fn reboot(
        &self,
        _ctx: RequestContext,
        _request: ServiceRequest<'_, RebootRequest>,
    ) -> ServiceResult<RebootResponse> {
        self.updater
            .reboot()
            .await
            .map_err(|e| ConnectError::internal(format!("rebooting: {e:#}")))?;
        Response::ok(RebootResponse::default())
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

    /// The real brain without audio or LED backends, served on an ephemeral port.
    async fn start_device() -> (Client, mpsc::Sender<Input>) {
        start_device_with(Updater::new(UpdaterPaths::default())).await
    }

    async fn start_device_with(updater: Updater) -> (Client, mpsc::Sender<Input>) {
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
                wake_sound: false,
                has_hw_privacy: false,
                settings_file: std::env::temp_dir().join(format!(
                    "pinnad-api-test-settings-{}.toml",
                    std::process::id()
                )),
                settings: crate::settings::Settings {
                    volume: 40,
                    ..Default::default()
                },
            },
        );
        tokio::spawn(brain.run(in_rx));
        let info = Info {
            version: "0.1.0".into(),
            name: "pinna-test".into(),
            mac: "02:00:00:AA:BB:CC".into(),
            wake_words: crate::wakewords::test_catalogue(),
            audio_backend: "none".into(),
            led_backend: "none".into(),
            model: "echo-dot-2".into(),
            virtual_device: false,
            last_boot: boot::LastBoot::cold(),
        };
        let addr = start(
            Arc::new(DeviceApi::with_updater(
                info,
                status_rx,
                in_tx.clone(),
                updater,
            )),
            "127.0.0.1:0",
        )
        .await
        .unwrap();
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

    #[test]
    fn render_reports_a_ringing_alarm() {
        let info = Info {
            version: "0.1.0".into(),
            name: "pinna-test".into(),
            mac: String::new(),
            wake_words: crate::wakewords::test_catalogue(),
            audio_backend: "none".into(),
            led_backend: "none".into(),
            model: "echo-dot-2".into(),
            virtual_device: false,
            last_boot: boot::LastBoot::cold(),
        };
        let mut snap = Snapshot::default();
        assert!(!render(&info, Instant::now(), &snap).alarm_ringing);
        snap.alarm_ringing = true;
        assert!(render(&info, Instant::now(), &snap).alarm_ringing);
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
            };
            Self { dir, paths }
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
