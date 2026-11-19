//! Runs the pinnactl binary against a fake in-process DeviceService.

use std::sync::{Arc, Mutex};

use buffa::MessageField;
use connectrpc::{
    ConnectError, RequestContext, Response, Router, Server, ServiceRequest, ServiceResult,
    ServiceStream,
};
use pinna_api::*;

#[derive(Default)]
struct Fake {
    state: Mutex<(u32, bool)>,
}

impl Fake {
    fn status(&self) -> Status {
        let (volume, muted) = *self.state.lock().unwrap();
        Status {
            name: "pinna-fake".into(),
            volume,
            muted,
            software_muted: muted,
            phase: Phase::PHASE_IDLE.into(),
            wake_word: MessageField::some(WakeWord {
                phrase: "Alexa".into(),
                ..Default::default()
            }),
            ..Default::default()
        }
    }
}

macro_rules! status_reply {
    ($ty:ident, $self:ident) => {
        Response::ok($ty {
            status: MessageField::some($self.status()),
            ..Default::default()
        })
    };
}

#[allow(refining_impl_trait)]
impl DeviceService for Fake {
    async fn get_status(
        &self,
        _: RequestContext,
        _: ServiceRequest<'_, GetStatusRequest>,
    ) -> ServiceResult<GetStatusResponse> {
        status_reply!(GetStatusResponse, self)
    }
    async fn set_volume(
        &self,
        _: RequestContext,
        r: ServiceRequest<'_, SetVolumeRequest>,
    ) -> ServiceResult<SetVolumeResponse> {
        self.state.lock().unwrap().0 = r.percent;
        status_reply!(SetVolumeResponse, self)
    }
    async fn set_stream_volume(
        &self,
        _: RequestContext,
        _: ServiceRequest<'_, SetStreamVolumeRequest>,
    ) -> ServiceResult<SetStreamVolumeResponse> {
        status_reply!(SetStreamVolumeResponse, self)
    }
    async fn set_stream_metadata(
        &self,
        _: RequestContext,
        _: ServiceRequest<'_, SetStreamMetadataRequest>,
    ) -> ServiceResult<SetStreamMetadataResponse> {
        status_reply!(SetStreamMetadataResponse, self)
    }
    async fn start_bluetooth_pairing(
        &self,
        _: RequestContext,
        _: ServiceRequest<'_, StartBluetoothPairingRequest>,
    ) -> ServiceResult<StartBluetoothPairingResponse> {
        status_reply!(StartBluetoothPairingResponse, self)
    }
    async fn stop_bluetooth_pairing(
        &self,
        _: RequestContext,
        _: ServiceRequest<'_, StopBluetoothPairingRequest>,
    ) -> ServiceResult<StopBluetoothPairingResponse> {
        status_reply!(StopBluetoothPairingResponse, self)
    }
    async fn forget_bluetooth_device(
        &self,
        _: RequestContext,
        _: ServiceRequest<'_, ForgetBluetoothDeviceRequest>,
    ) -> ServiceResult<ForgetBluetoothDeviceResponse> {
        status_reply!(ForgetBluetoothDeviceResponse, self)
    }
    async fn set_ring_light(
        &self,
        _: RequestContext,
        _: ServiceRequest<'_, SetRingLightRequest>,
    ) -> ServiceResult<SetRingLightResponse> {
        status_reply!(SetRingLightResponse, self)
    }
    async fn control_media(
        &self,
        _: RequestContext,
        _: ServiceRequest<'_, ControlMediaRequest>,
    ) -> ServiceResult<ControlMediaResponse> {
        status_reply!(ControlMediaResponse, self)
    }
    async fn play_media(
        &self,
        _: RequestContext,
        _: ServiceRequest<'_, PlayMediaRequest>,
    ) -> ServiceResult<PlayMediaResponse> {
        status_reply!(PlayMediaResponse, self)
    }
    async fn report_bluetooth(
        &self,
        _: RequestContext,
        _: ServiceRequest<'_, ReportBluetoothRequest>,
    ) -> ServiceResult<ReportBluetoothResponse> {
        Response::ok(ReportBluetoothResponse::default())
    }
    async fn set_mute(
        &self,
        _: RequestContext,
        r: ServiceRequest<'_, SetMuteRequest>,
    ) -> ServiceResult<SetMuteResponse> {
        self.state.lock().unwrap().1 = r.muted;
        status_reply!(SetMuteResponse, self)
    }
    async fn start_conversation(
        &self,
        _: RequestContext,
        _: ServiceRequest<'_, StartConversationRequest>,
    ) -> ServiceResult<StartConversationResponse> {
        Err(ConnectError::failed_precondition("the microphone is muted"))
    }
    async fn stop_conversation(
        &self,
        _: RequestContext,
        _: ServiceRequest<'_, StopConversationRequest>,
    ) -> ServiceResult<StopConversationResponse> {
        status_reply!(StopConversationResponse, self)
    }
    async fn led_test(
        &self,
        _: RequestContext,
        _: ServiceRequest<'_, LedTestRequest>,
    ) -> ServiceResult<LedTestResponse> {
        Response::ok(LedTestResponse::default())
    }
    async fn clear_notification(
        &self,
        _: RequestContext,
        _: ServiceRequest<'_, ClearNotificationRequest>,
    ) -> ServiceResult<ClearNotificationResponse> {
        status_reply!(ClearNotificationResponse, self)
    }
    async fn watch_status(
        &self,
        _: RequestContext,
        _: ServiceRequest<'_, WatchStatusRequest>,
    ) -> ServiceResult<ServiceStream<WatchStatusResponse>> {
        let msg = |volume| {
            Ok(WatchStatusResponse {
                status: MessageField::some(Status {
                    volume,
                    ..Default::default()
                }),
                ..Default::default()
            })
        };
        Response::ok(Box::pin(futures::stream::iter([msg(10), msg(20)])))
    }

    // pinnactl does not use the dashboard's RPCs.
    async fn update_settings(
        &self,
        _: RequestContext,
        _: ServiceRequest<'_, UpdateSettingsRequest>,
    ) -> ServiceResult<UpdateSettingsResponse> {
        Err(ConnectError::unimplemented("fake"))
    }

    async fn start_calibration(
        &self,
        _: RequestContext,
        _: ServiceRequest<'_, StartCalibrationRequest>,
    ) -> ServiceResult<StartCalibrationResponse> {
        Err(ConnectError::unimplemented("fake"))
    }

    async fn stop_calibration(
        &self,
        _: RequestContext,
        _: ServiceRequest<'_, StopCalibrationRequest>,
    ) -> ServiceResult<StopCalibrationResponse> {
        Err(ConnectError::unimplemented("fake"))
    }

    async fn add_wake_word(
        &self,
        _: RequestContext,
        _: ServiceRequest<'_, AddWakeWordRequest>,
    ) -> ServiceResult<AddWakeWordResponse> {
        Err(ConnectError::unimplemented("fake"))
    }

    async fn remove_wake_word(
        &self,
        _: RequestContext,
        _: ServiceRequest<'_, RemoveWakeWordRequest>,
    ) -> ServiceResult<RemoveWakeWordResponse> {
        Err(ConnectError::unimplemented("fake"))
    }

    async fn get_network(
        &self,
        _: RequestContext,
        _: ServiceRequest<'_, GetNetworkRequest>,
    ) -> ServiceResult<GetNetworkResponse> {
        Err(ConnectError::unimplemented("fake"))
    }

    async fn watch_ring(
        &self,
        _: RequestContext,
        _: ServiceRequest<'_, WatchRingRequest>,
    ) -> ServiceResult<ServiceStream<WatchRingResponse>> {
        Err(ConnectError::unimplemented("fake"))
    }

    async fn watch_logs(
        &self,
        _: RequestContext,
        _: ServiceRequest<'_, WatchLogsRequest>,
    ) -> ServiceResult<ServiceStream<WatchLogsResponse>> {
        Err(ConnectError::unimplemented("fake"))
    }

    async fn get_updates(
        &self,
        _: RequestContext,
        _: ServiceRequest<'_, GetUpdatesRequest>,
    ) -> ServiceResult<GetUpdatesResponse> {
        Err(ConnectError::unimplemented("fake"))
    }

    async fn check_for_updates(
        &self,
        _: RequestContext,
        _: ServiceRequest<'_, CheckForUpdatesRequest>,
    ) -> ServiceResult<CheckForUpdatesResponse> {
        Err(ConnectError::unimplemented("fake"))
    }

    async fn install_update(
        &self,
        _: RequestContext,
        _: ServiceRequest<'_, InstallUpdateRequest>,
    ) -> ServiceResult<InstallUpdateResponse> {
        Err(ConnectError::unimplemented("fake"))
    }

    async fn watch_install(
        &self,
        _: RequestContext,
        _: ServiceRequest<'_, WatchInstallRequest>,
    ) -> ServiceResult<ServiceStream<WatchInstallResponse>> {
        Err(ConnectError::unimplemented("fake"))
    }

    async fn set_active_slot(
        &self,
        _: RequestContext,
        _: ServiceRequest<'_, SetActiveSlotRequest>,
    ) -> ServiceResult<SetActiveSlotResponse> {
        Err(ConnectError::unimplemented("fake"))
    }

    async fn reboot(
        &self,
        _: RequestContext,
        _: ServiceRequest<'_, RebootRequest>,
    ) -> ServiceResult<RebootResponse> {
        Err(ConnectError::unimplemented("fake"))
    }

    async fn factory_reset(
        &self,
        _: RequestContext,
        _: ServiceRequest<'_, FactoryResetRequest>,
    ) -> ServiceResult<FactoryResetResponse> {
        Err(ConnectError::unimplemented("fake"))
    }

    async fn scan_wifi(
        &self,
        _: RequestContext,
        _: ServiceRequest<'_, ScanWifiRequest>,
    ) -> ServiceResult<ScanWifiResponse> {
        Err(ConnectError::unimplemented("fake"))
    }

    async fn list_time_zones(
        &self,
        _: RequestContext,
        _: ServiceRequest<'_, ListTimeZonesRequest>,
    ) -> ServiceResult<ListTimeZonesResponse> {
        Err(ConnectError::unimplemented("fake"))
    }

    async fn list_satellites(
        &self,
        _: RequestContext,
        _: ServiceRequest<'_, ListSatellitesRequest>,
    ) -> ServiceResult<ListSatellitesResponse> {
        Err(ConnectError::unimplemented("fake"))
    }

    async fn list_wifi(
        &self,
        _: RequestContext,
        _: ServiceRequest<'_, ListWifiRequest>,
    ) -> ServiceResult<ListWifiResponse> {
        Err(ConnectError::unimplemented("fake"))
    }

    async fn join_wifi(
        &self,
        _: RequestContext,
        _: ServiceRequest<'_, JoinWifiRequest>,
    ) -> ServiceResult<ServiceStream<JoinWifiResponse>> {
        Err(ConnectError::unimplemented("fake"))
    }

    async fn keep_wifi(
        &self,
        _: RequestContext,
        _: ServiceRequest<'_, KeepWifiRequest>,
    ) -> ServiceResult<KeepWifiResponse> {
        Err(ConnectError::unimplemented("fake"))
    }

    async fn forget_wifi(
        &self,
        _: RequestContext,
        _: ServiceRequest<'_, ForgetWifiRequest>,
    ) -> ServiceResult<ForgetWifiResponse> {
        Err(ConnectError::unimplemented("fake"))
    }
}

async fn pinnactl(addr: std::net::SocketAddr, args: &[&str]) -> (bool, String, String) {
    let out = tokio::process::Command::new(env!("CARGO_BIN_EXE_pinnactl"))
        .arg("--api")
        .arg(format!("http://{addr}"))
        .args(args)
        .output()
        .await
        .unwrap();
    (
        out.status.success(),
        String::from_utf8(out.stdout).unwrap(),
        String::from_utf8(out.stderr).unwrap(),
    )
}

#[tokio::test]
async fn commands_against_a_server() {
    let router = Router::new().add_service(Arc::new(Fake::default()));
    let server = Server::bind("127.0.0.1:0").await.unwrap();
    let addr = server.local_addr().unwrap();
    tokio::spawn(server.serve(router));

    let (ok, out, _) = pinnactl(addr, &["status"]).await;
    assert!(ok);
    assert!(out.contains("pinna-fake") && out.contains("phase:      idle"));

    let (ok, out, _) = pinnactl(addr, &["volume", "35"]).await;
    assert!(ok && out.contains("35%"));
    let (ok, out, _) = pinnactl(addr, &["mute", "on"]).await;
    assert!(ok && out.contains("on"));
    let (_, out, _) = pinnactl(addr, &["status"]).await;
    assert!(out.contains("volume:     35%") && out.contains("yes (software)"));

    let (ok, out, _) = pinnactl(addr, &["status", "--json"]).await;
    assert!(ok);
    let json: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(json["volume"], 35);
    assert_eq!(json["name"], "pinna-fake");

    let (ok, _, _) = pinnactl(addr, &["stop"]).await;
    assert!(ok);
    let (ok, _, _) = pinnactl(addr, &["led-test"]).await;
    assert!(ok);

    let (ok, _, err) = pinnactl(addr, &["listen"]).await;
    assert!(!ok && err.contains("the microphone is muted"), "{err}");

    let (ok, out, _) = pinnactl(addr, &["watch"]).await;
    assert!(ok);
    assert_eq!(out.lines().count(), 2);
    assert!(out.contains("volume  10%") && out.contains("volume  20%"));

    let (ok, _, _) = pinnactl(addr, &["volume", "101"]).await;
    assert!(!ok);
}

#[tokio::test]
async fn fails_when_nothing_listens() {
    // Health check use: a dead API must be a non-zero exit.
    let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = free.local_addr().unwrap();
    drop(free);
    let (ok, _, err) = pinnactl(addr, &["status"]).await;
    assert!(!ok && err.contains("pinnad API"), "{err}");
}
