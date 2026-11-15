//! Generated ESPHome native API messages plus the wire-id table.

#![allow(clippy::all, missing_docs)]

/// Messages generated from the vendored `api.proto` (package-less upstream).
pub mod proto {
    include!(concat!(env!("OUT_DIR"), "/_.rs"));
}

/// A protobuf message that has a wire id in the ESPHome native API.
pub trait ApiMessage: prost::Message + Default {
    /// Value of the `(id)` message option.
    const ID: u32;
}

include!(concat!(env!("OUT_DIR"), "/ids.rs"));

/// Name of the message with the given wire id, if any.
pub fn message_name(id: u32) -> Option<&'static str> {
    MESSAGE_TABLE
        .binary_search_by_key(&id, |(i, _)| *i)
        .ok()
        .map(|i| MESSAGE_TABLE[i].1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proto::*;

    #[test]
    fn ids_match_upstream() {
        assert_eq!(HelloRequest::ID, 1);
        assert_eq!(HelloResponse::ID, 2);
        assert_eq!(DisconnectRequest::ID, 5);
        assert_eq!(PingRequest::ID, 7);
        assert_eq!(PingResponse::ID, 8);
        assert_eq!(DeviceInfoResponse::ID, 10);
        assert_eq!(ListEntitiesLightResponse::ID, 15);
        assert_eq!(ListEntitiesSwitchResponse::ID, 17);
        assert_eq!(ListEntitiesDoneResponse::ID, 19);
        assert_eq!(LightStateResponse::ID, 24);
        assert_eq!(SwitchStateResponse::ID, 26);
        assert_eq!(LightCommandRequest::ID, 32);
        assert_eq!(SwitchCommandRequest::ID, 33);
        assert_eq!(GetTimeRequest::ID, 36);
        assert_eq!(GetTimeResponse::ID, 37);
        assert_eq!(SubscribeVoiceAssistantRequest::ID, 89);
        assert_eq!(VoiceAssistantRequest::ID, 90);
        assert_eq!(VoiceAssistantResponse::ID, 91);
        assert_eq!(VoiceAssistantEventResponse::ID, 92);
        assert_eq!(VoiceAssistantAudio::ID, 106);
        assert_eq!(VoiceAssistantTimerEventResponse::ID, 115);
        assert_eq!(VoiceAssistantAnnounceRequest::ID, 119);
        assert_eq!(VoiceAssistantAnnounceFinished::ID, 120);
        assert_eq!(VoiceAssistantConfigurationRequest::ID, 121);
        assert_eq!(VoiceAssistantConfigurationResponse::ID, 122);
        assert_eq!(VoiceAssistantSetConfiguration::ID, 123);
    }

    #[test]
    fn table_lookup() {
        assert_eq!(message_name(90), Some("VoiceAssistantRequest"));
        assert_eq!(message_name(0), None);
        assert!(MESSAGE_TABLE.windows(2).all(|w| w[0].0 < w[1].0));
    }
}
