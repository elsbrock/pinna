//! Local control API: ConnectRPC `pinna.v1.DeviceService`, served over plain HTTP.
//!
//! No authentication — bind it to loopback only. The service holds no state of
//! its own: reads come from the brain's status `watch`, commands go to the brain
//! as inputs and are answered with the state right after they were applied.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result};
use buffa::MessageField;
use connectrpc::{ConnectError, RequestContext, Response, Router, Server, ServiceRequest};
use connectrpc::{ServiceResult, ServiceStream};
use pinna_api::{
    DeviceService, GetStatusRequest, GetStatusResponse, LedTestRequest, LedTestResponse,
    SetMuteRequest, SetMuteResponse, SetVolumeRequest, SetVolumeResponse, StartConversationRequest,
    StartConversationResponse, Status, StopConversationRequest, StopConversationResponse, Timer,
    WakeWord, WatchStatusRequest, WatchStatusResponse,
};
use tokio::sync::{mpsc, oneshot, watch};

use crate::voice::{ApiCommand, ApiReply, Input, Snapshot};

/// What does not change while pinnad runs.
#[derive(Debug, Clone)]
pub struct Info {
    pub name: String,
    pub mac: String,
    pub wake_phrase: String,
    pub audio_backend: String,
    pub led_backend: String,
}

pub struct DeviceApi {
    info: Info,
    started: Instant,
    status: watch::Receiver<Snapshot>,
    inputs: mpsc::Sender<Input>,
}

impl DeviceApi {
    pub fn new(info: Info, status: watch::Receiver<Snapshot>, inputs: mpsc::Sender<Input>) -> Self {
        Self {
            info,
            started: Instant::now(),
            status,
            inputs,
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
        version: env!("CARGO_PKG_VERSION").into(),
        name: info.name.clone(),
        mac: info.mac.clone(),
        uptime_seconds: started.elapsed().as_secs(),
        phase: s.phase.into(),
        muted: s.hw_muted || s.sw_muted,
        hardware_muted: s.hw_muted,
        software_muted: s.sw_muted,
        volume: s.volume.into(),
        ha_connected: s.ha_connected,
        voice_subscribed: s.voice_subscribed,
        wake_word: MessageField::some(WakeWord {
            phrase: info.wake_phrase.clone(),
            enabled: s.wake_enabled,
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
    use pinna_esphome::{DeviceConfig, HaCommand};

    use super::*;
    use crate::leds::LedState;
    use crate::voice::{Brain, Options, Shared};

    type Client = DeviceServiceClient<HttpClient>;

    /// The real brain without audio or LED backends, served on an ephemeral port.
    async fn start_device() -> (Client, mpsc::Sender<Input>) {
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
                wake_phrase: "Alexa".into(),
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
            name: "pinna-test".into(),
            mac: "02:00:00:AA:BB:CC".into(),
            wake_phrase: "Alexa".into(),
            audio_backend: "none".into(),
            led_backend: "none".into(),
        };
        let addr = start(
            Arc::new(DeviceApi::new(info, status_rx, in_tx.clone())),
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

    #[test]
    fn render_reports_a_ringing_alarm() {
        let info = Info {
            name: "pinna-test".into(),
            mac: String::new(),
            wake_phrase: "Alexa".into(),
            audio_backend: "none".into(),
            led_backend: "none".into(),
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
}
