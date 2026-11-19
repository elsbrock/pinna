//! Whole sessions between the player's `Connection` and a server built with snow in the test.

mod common;

use common::*;
use pinna_sendspin::clock::{BURST_INTERVAL_US, BURST_LEN, REPLY_TIMEOUT_US};
use pinna_sendspin::connection::{MAX_AHEAD_US, STEP_TIMEOUT_US};
use pinna_sendspin::pairing::ATTEMPT_TIMEOUT_US;
use pinna_sendspin::psk::SENTINEL_PSK;
use pinna_sendspin::*;
use serde_json::{Value, json};

fn store() -> PairingStore {
    PairingStore::generate().unwrap()
}

fn activated(events: &[Event]) -> Option<(Activities, ActiveRoles, bool, bool)> {
    events.iter().find_map(|e| match e {
        Event::Activated {
            activities,
            roles,
            paired,
            displaces_rival,
        } => Some((*activities, *roles, *paired, *displaces_rival)),
        _ => None,
    })
}

fn closed(events: &[Event]) -> Option<CloseReason> {
    events.iter().find_map(|e| match e {
        Event::Closed(r) => Some(r.clone()),
        _ => None,
    })
}

const PLAYBACK: Activities = Activities {
    playback: true,
    pairing: false,
};

#[test]
fn sentinel_session_from_client_init_to_activation() {
    let identity = Identity::generate().unwrap();
    let mut server = Server::new(&identity);
    let store = store();
    let mut conn = Connection::new(config(identity.clone()), &store, 0).unwrap();

    // client/init goes first, in clear text, with exactly the defined fields.
    let Some(Transmit::Text(init)) = conn.poll_transmit() else {
        panic!()
    };
    let init: Value = serde_json::from_str(&init).unwrap();
    assert_eq!(
        init,
        json!({"type": "client/init", "payload": {"client_id": identity.client_id(),
            "version": 1, "suite": "25519_ChaChaPoly_SHA256"}})
    );
    // Put it back for the server's side of the handshake.
    let mut conn = Connection::new(config(identity.clone()), &store, 0).unwrap();
    server.handshake(&mut conn, &SENTINEL_PSK, "sn", 0);
    assert_eq!(conn.psk_category(), Some(PskCategory::Sentinel));
    assert_eq!(conn.server_id(), Some(server.id().as_str()));
    // Nothing until server/hello.
    assert!(server.recv(&mut conn).is_empty());

    server.send(&mut conn, hello(), 0);
    let outs = server.recv(&mut conn);
    assert_eq!(kinds(&outs), ["client/hello"]);
    assert_eq!(
        payload(&outs, "client/hello"),
        json!({
            "name": "Kitchen",
            "device_info": {"product_name": "Test speaker", "software_version": "0.4.0",
                "mac_address": "aa:bb:cc:dd:ee:ff"},
            "supported_roles": ["player@v1", "controller@v1", "metadata@v1"],
            "player@v1_support": {
                "supported_formats": [
                    {"codec": "flac", "channels": 2, "sample_rate": 48000, "bit_depth": 16},
                    {"codec": "pcm", "channels": 2, "sample_rate": 48000, "bit_depth": 16}],
                "buffer_capacity": 1048576},
            "supported_pair_methods": {"pairing_psk": {}},
            "unpaired_access": {"enabled": true}
        })
    );
    assert_eq!(
        std::iter::from_fn(|| conn.poll_event()).collect::<Vec<_>>(),
        [Event::Connected {
            server_id: server.id(),
            name: "Music Assistant".into()
        }]
    );
    // The player's state waits for the activation.
    conn.set_player_state(player_state()).unwrap();
    assert!(server.recv(&mut conn).is_empty());

    server.send(&mut conn, activate(json!(["playback"]), all_roles()), 5);
    let events: Vec<Event> = std::iter::from_fn(|| conn.poll_event()).collect();
    let all = ActiveRoles {
        player: true,
        controller: true,
        metadata: true,
    };
    assert_eq!(activated(&events), Some((PLAYBACK, all, false, false)));
    assert!(events.contains(&Event::LastPlaybackServer(server.id())));
    assert_eq!(
        conn.store().last_playback_server(),
        Some(server.id().as_str())
    );
    // The first burst starts at once; client/state waits for its measurement.
    let outs = server.recv(&mut conn);
    assert_eq!(kinds(&outs), ["client/time"]);
    assert_eq!(
        payload(&outs, "client/time"),
        json!({"client_transmitted": 5})
    );
}

#[test]
fn a_stored_long_term_key_makes_a_paired_session() {
    let identity = Identity::generate().unwrap();
    let server = Server::new(&identity);
    let mut store = store();
    let long_term = Psk::random().unwrap();
    store.insert(
        PairingRecord {
            server_id: server.id(),
            long_term_psk: long_term.clone(),
        },
        &[],
    );
    let mut s = connect(identity, server, &store, &long_term, "lt", |c| {
        c.unpaired_access = false;
    });
    assert_eq!(s.conn.psk_category(), Some(PskCategory::LongTerm));
    s.send(activate(json!(["playback"]), json!(["player@v1"])));
    let (activities, roles, paired, _) = activated(&s.events()).unwrap();
    assert_eq!(activities, PLAYBACK);
    assert!(roles.player && !roles.controller);
    assert!(paired);
}

#[test]
fn a_pairing_bound_to_another_server_fails_the_handshake_silently() {
    let identity = Identity::generate().unwrap();
    let mut server = Server::new(&identity);
    let mut store = store();
    let long_term = Psk::random().unwrap();
    store.insert(
        PairingRecord {
            server_id: "someone-else".into(),
            long_term_psk: long_term.clone(),
        },
        &[],
    );
    let mut conn = Connection::new(config(identity), &store, 0).unwrap();
    assert!(!server.handshake_as(&mut conn, &long_term, "lt", &long_term, 0));
    let events: Vec<Event> = std::iter::from_fn(|| conn.poll_event()).collect();
    assert!(matches!(closed(&events), Some(CloseReason::Silent(_))));
    assert_eq!(
        conn.poll_transmit(),
        None,
        "the close was the only thing queued"
    );
}

#[test]
fn an_unknown_key_falls_back_to_the_sentinel_on_the_first_handshake() {
    let identity = Identity::generate().unwrap();
    let mut server = Server::new(&identity);
    let store = store();
    let lost = Psk::random().unwrap();
    let mut conn = Connection::new(config(identity), &store, 0).unwrap();
    // The server names a long-term key the player no longer holds; message 2 verifies only
    // with the Sentinel key.
    assert!(server.handshake_as(&mut conn, &lost, "lt", &SENTINEL_PSK, 0));
    assert_eq!(conn.psk_category(), Some(PskCategory::Sentinel));
    // The pairing key named under another category is a miss too.
    let identity = Identity::generate().unwrap();
    let mut server = Server::new(&identity);
    let mut conn = Connection::new(config(identity), &store, 0).unwrap();
    assert!(server.handshake_as(&mut conn, store.pairing_psk(), "lt", &SENTINEL_PSK, 0));
    assert_eq!(conn.psk_category(), Some(PskCategory::Sentinel));
}

#[test]
fn pairing_psk_flow_end_to_end() {
    let store = store();
    let mut s = start(&store, store.pairing_psk(), "pr");
    assert_eq!(s.conn.psk_category(), Some(PskCategory::Pairing));
    s.send(
        json!({"type": "server/activate", "payload": {"activities": ["pairing"],
        "active_roles": [], "pairing": {"method": "pairing_psk"}}}),
    );
    let (activities, roles, paired, _) = activated(&s.events()).unwrap();
    assert!(activities.pairing && !activities.playback);
    assert_eq!(roles, ActiveRoles::default());
    assert!(!paired);
    // client/pair-init, then client/pair-finalize without waiting; the clock burst starts too.
    let outs = s.recv();
    assert_eq!(
        kinds(&outs),
        ["client/pair-init", "client/pair-finalize", "client/time"]
    );
    assert_eq!(
        payload(&outs, "client/pair-init"),
        json!({"pairing_index": 1})
    );
    let fin = payload(&outs, "client/pair-finalize");
    assert_eq!(fin.as_object().unwrap().len(), 1);
    let long_term = Psk::from_base64url(fin["long_term_psk"].as_str().unwrap()).unwrap();
    assert_ne!(&long_term, store.pairing_psk());
    assert_ne!(long_term, SENTINEL_PSK);
    assert!(s.conn.as_rival().unwrap().pairing_attempt);
    // Nothing is kept until the server confirms.
    assert!(s.conn.store().pairings().is_empty());

    s.send(json!({"type": "server/pair-finalize", "payload": {}}));
    let record = PairingRecord {
        server_id: s.server.id(),
        long_term_psk: long_term.clone(),
    };
    assert_eq!(s.events(), [Event::PairingAdded(record.clone())]);
    assert_eq!(s.conn.store().pairings(), [record]);
    assert!(!s.conn.as_rival().unwrap().pairing_attempt);

    // The server re-handshakes to the new key and activates playback as a paired session.
    let now = s.now;
    assert!(s.server.rehandshake(&mut s.conn, &long_term, "lt", now));
    assert!(
        s.recv().is_empty(),
        "nothing between message 2 and server/activate"
    );
    s.send(activate(json!(["playback"]), json!(["player@v1"])));
    let (activities, _, paired, _) = activated(&s.events()).unwrap();
    assert_eq!(activities, PLAYBACK);
    assert!(paired);
    assert_eq!(s.conn.psk_category(), Some(PskCategory::LongTerm));
    // A later server/pair-finalize is out of sequence.
    s.send(json!({"type": "server/pair-finalize", "payload": {}}));
    assert!(matches!(
        closed(&s.events()),
        Some(CloseReason::Protocol(_))
    ));
}

#[test]
fn an_activation_in_place_of_server_pair_finalize_keeps_nothing() {
    let store = store();
    let mut s = start(&store, store.pairing_psk(), "pr");
    let pairing = json!({"type": "server/activate", "payload": {"activities": ["pairing"],
        "active_roles": [], "pairing": {"method": "pairing_psk"}}});
    s.send(pairing.clone());
    s.recv();
    s.send(activate(json!([]), json!([])));
    s.send(json!({"type": "server/pair-finalize", "payload": {}}));
    let events = s.events();
    assert!(!events.iter().any(|e| matches!(e, Event::PairingAdded(_))));
    assert!(matches!(closed(&events), Some(CloseReason::Protocol(_))));
    assert!(s.conn.store().pairings().is_empty());
}

#[test]
fn a_pairing_attempt_times_out_and_late_messages_are_dropped() {
    let store = store();
    let mut s = start(&store, store.pairing_psk(), "pr");
    let pairing = json!({"type": "server/activate", "payload": {"activities": ["pairing"],
        "active_roles": [], "pairing": {"method": "pairing_psk"}}});
    s.send(pairing.clone());
    s.recv();
    s.tick(ATTEMPT_TIMEOUT_US - 1);
    assert!(!kinds(&s.recv()).contains(&"pair/abort".to_string()));
    s.tick(ATTEMPT_TIMEOUT_US);
    let outs = s.recv();
    assert_eq!(
        payload(&outs, "pair/abort"),
        json!({"reason": "attempt_timeout"})
    );
    // Pairing messages in flight are dropped until the next activation.
    s.send(json!({"type": "server/pair-finalize", "payload": {}}));
    s.send(json!({"type": "server/pair-auth", "payload": {"pake_msg_1": "x"}}));
    let events = s.events();
    assert_eq!(closed(&events), None);
    assert!(!events.iter().any(|e| matches!(e, Event::PairingAdded(_))));
    // The next pairing activation starts a new attempt, counted since the handshake.
    s.send(pairing);
    assert_eq!(
        payload(&s.recv(), "client/pair-init"),
        json!({"pairing_index": 2})
    );
}

#[test]
fn rehandshakes() {
    let store = store();
    let mut s = playing(&store);
    s.sync();
    s.conn.set_player_state(player_state()).unwrap();
    assert_eq!(kinds(&s.recv()), ["client/state"]);

    // To the pairing key from a Sentinel session: allowed, and the session's key changes.
    let now = s.now;
    let pairing = s.conn.store().pairing_psk().clone();
    assert!(s.server.rehandshake(&mut s.conn, &pairing, "pr", now));
    // Nothing may be sent until server/activate: not a state change, not a command.
    let mut state = player_state();
    state.volume = 41;
    s.conn.set_player_state(state).unwrap();
    assert_eq!(s.conn.leave(), Err(CommandError::NotActive));
    assert!(s.recv().is_empty());
    s.send(activate(json!(["playback"]), all_roles()));
    assert_eq!(s.conn.psk_category(), Some(PskCategory::Pairing));
    // The state change goes out now; the stream and the clock carried over.
    assert_eq!(payload(&s.recv(), "client/state")["player"]["volume"], 41);
    assert_eq!(s.conn.clock().count(), 1);

    // Anything but server/activate after message 1 is a protocol error.
    let now = s.now;
    assert!(s.server.rehandshake(&mut s.conn, &pairing, "pr", now));
    s.send(
        json!({"type": "group/update", "payload": {"playback_state": "playing",
        "group_id": "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA", "group_name": "x"}}),
    );
    assert!(matches!(
        closed(&s.events()),
        Some(CloseReason::Protocol(_))
    ));
}

#[test]
fn a_rehandshake_to_an_unknown_key_closes_silently() {
    let store = store();
    let mut s = playing(&store);
    s.recv();
    let now = s.now;
    assert!(
        !s.server
            .rehandshake(&mut s.conn, &Psk::random().unwrap(), "lt", now)
    );
    assert!(matches!(closed(&s.events()), Some(CloseReason::Silent(_))));
}

#[test]
fn a_rehandshake_must_be_finished_in_time() {
    let store = store();
    let mut s = playing(&store);
    s.recv();
    let now = s.now;
    assert!(s.server.rehandshake(&mut s.conn, &SENTINEL_PSK, "sn", now));
    assert_eq!(s.conn.next_wakeup(), Some(now + STEP_TIMEOUT_US));
    s.tick(now + STEP_TIMEOUT_US);
    assert!(matches!(closed(&s.events()), Some(CloseReason::Timeout(_))));
}

#[test]
fn a_legacy_server_is_recognised_and_closed() {
    let identity = Identity::generate().unwrap();
    let mut conn = Connection::new(config(identity), &store(), 0).unwrap();
    // The old wire format starts with the server's hello in clear text.
    conn.on_text(
        &json!({"type": "server/hello", "payload": {"server_id": "ma", "name": "Music Assistant",
            "version": 1, "active_roles": ["player@v1"], "connection_reason": "playback"}})
        .to_string(),
        0,
    );
    let events: Vec<Event> = std::iter::from_fn(|| conn.poll_event()).collect();
    assert_eq!(
        events,
        [
            Event::LegacyServer,
            Event::Closed(CloseReason::LegacyServer)
        ]
    );
    assert!(matches!(conn.poll_transmit(), Some(Transmit::Text(_))));
    assert_eq!(
        conn.poll_transmit(),
        Some(Transmit::Close(CloseReason::LegacyServer))
    );
    assert!(conn.is_closed());
}

/// A name, and the text messages a server sends.
type Case = (&'static str, Box<dyn Fn(&Server) -> Vec<String>>);

#[test]
fn handshake_failures_close_without_a_message() {
    let cases: Vec<Case> = vec![
        (
            "server/error",
            Box::new(|_| {
                vec![
                    json!({"type": "server/error", "payload": {"reason": "unsupported_version"}})
                        .to_string(),
                ]
            }),
        ),
        (
            "another version",
            Box::new(|s| {
                vec![
                    json!({"type": "server/init", "payload": {"server_id": s.id(), "version": 2}})
                        .to_string(),
                ]
            }),
        ),
        (
            "a server_id that is not a key",
            Box::new(|_| {
                vec![
                    json!({"type": "server/init", "payload": {"server_id": "short", "version": 1}})
                        .to_string(),
                ]
            }),
        ),
        ("not JSON", Box::new(|_| vec!["hello".to_string()])),
        (
            "message 1 first",
            Box::new(|_| {
                vec![json!({"type": "noise/handshake", "payload": {"data": "AAAA"}}).to_string()]
            }),
        ),
        (
            "garbage message 1",
            Box::new(|s| {
                vec![
                    json!({"type": "server/init", "payload": {"server_id": s.id(), "version": 1}})
                        .to_string(),
                    json!({"type": "noise/handshake", "payload": {"data": b64(&[7; 120])}})
                        .to_string(),
                ]
            }),
        ),
        (
            "server/hello after server/init",
            Box::new(|s| {
                vec![
                    json!({"type": "server/init", "payload": {"server_id": s.id(), "version": 1}})
                        .to_string(),
                    hello().to_string(),
                ]
            }),
        ),
    ];
    for (name, texts) in cases {
        let identity = Identity::generate().unwrap();
        let server = Server::new(&identity);
        let mut conn = Connection::new(config(identity), &store(), 0).unwrap();
        assert!(matches!(conn.poll_transmit(), Some(Transmit::Text(_))));
        for text in texts(&server) {
            conn.on_text(&text, 0);
        }
        assert!(conn.is_closed(), "{name}");
        assert!(
            matches!(conn.poll_transmit(), Some(Transmit::Close(_))),
            "{name}: nothing but the close"
        );
        assert_eq!(conn.poll_transmit(), None, "{name}");
    }
    // The reason from server/error is kept for the log.
    let mut conn = Connection::new(config(Identity::generate().unwrap()), &store(), 0).unwrap();
    conn.on_text(
        r#"{"type":"server/error","payload":{"reason":"unsupported_suite"}}"#,
        0,
    );
    assert_eq!(
        closed(&std::iter::from_fn(|| conn.poll_event()).collect::<Vec<_>>()),
        Some(CloseReason::ServerError("unsupported_suite".into()))
    );
    // A binary message during the handshake.
    let mut conn = Connection::new(config(Identity::generate().unwrap()), &store(), 0).unwrap();
    conn.on_binary(&[0, 1, 2], 0);
    assert!(conn.is_closed());
}

#[test]
fn after_the_handshake_only_encrypted_binary_messages_count() {
    let store = store();
    let mut s = playing(&store);
    s.conn.on_text(&hello().to_string(), s.now);
    assert!(matches!(closed(&s.events()), Some(CloseReason::Silent(_))));
    let mut s = playing(&store);
    s.recv();
    s.conn.on_binary(&[0; 40], s.now);
    assert!(matches!(closed(&s.events()), Some(CloseReason::Silent(_))));
    assert!(matches!(s.recv().as_slice(), [Out::Close(_)]));
    // Once closed, input is ignored.
    s.send(hello());
    assert_eq!(s.recv(), []);
    assert_eq!(s.events(), []);
}

#[test]
fn message_order_before_activation() {
    let store = store();
    // server/activate before server/hello.
    let identity = Identity::generate().unwrap();
    let mut server = Server::new(&identity);
    let mut conn = Connection::new(config(identity), &store, 0).unwrap();
    server.handshake(&mut conn, &SENTINEL_PSK, "sn", 0);
    server.send(&mut conn, activate(json!([]), json!([])), 0);
    assert!(conn.is_closed());
    // Audio before activation.
    let mut s = start(&store, &SENTINEL_PSK, "sn");
    s.send_plain(&audio(0, 0, b"x"));
    assert!(s.conn.is_closed());
    // A second server/hello, a client's message, and handshake messages after activation.
    for message in [
        hello(),
        json!({"type": "client/state", "payload": {"available": true}}),
        json!({"type": "server/init", "payload": {"server_id": "x", "version": 1}}),
    ] {
        let mut s = playing(&store);
        s.send(message.clone());
        assert!(
            matches!(closed(&s.events()), Some(CloseReason::Protocol(_))),
            "{message}"
        );
    }
    // Unknown types and binary types are ignored.
    let mut s = playing(&store);
    s.send(json!({"type": "server/something-new", "payload": {"x": [1, 2]}}));
    s.send_plain(&[200, 1, 2, 3]);
    assert!(!s.conn.is_closed());
}

#[test]
fn the_handshake_and_the_first_activation_time_out() {
    let store = store();
    let identity = Identity::generate().unwrap();
    let mut conn = Connection::new(config(identity), &store, 1000).unwrap();
    assert_eq!(conn.next_wakeup(), Some(1000 + STEP_TIMEOUT_US));
    conn.on_tick(1000 + STEP_TIMEOUT_US - 1);
    assert!(!conn.is_closed());
    conn.on_tick(1000 + STEP_TIMEOUT_US);
    assert_eq!(
        closed(&std::iter::from_fn(|| conn.poll_event()).collect::<Vec<_>>()),
        Some(CloseReason::Timeout("server/init"))
    );

    let mut s = start(&store, &SENTINEL_PSK, "sn");
    assert_eq!(s.conn.next_wakeup(), Some(STEP_TIMEOUT_US));
    s.tick(STEP_TIMEOUT_US);
    assert_eq!(
        closed(&s.events()),
        Some(CloseReason::Timeout("server/activate"))
    );

    // After the activation, only the clock wakes the connection.
    let s = playing(&store);
    assert_eq!(s.conn.next_wakeup(), Some(REPLY_TIMEOUT_US));
}

#[test]
fn clock_bursts_through_the_connection() {
    let store = store();
    let mut s = playing(&store);
    s.events();
    let mut sent = 0;
    // Replies come after 400 µs; each one lets the next request go.
    while s.conn.clock().count() == 0 {
        let outs = s.recv();
        assert_eq!(kinds(&outs), ["client/time"], "request {sent}");
        let t1 = payload(&outs, "client/time")["client_transmitted"]
            .as_i64()
            .unwrap();
        sent += 1;
        s.now = t1 + 400;
        s.send(
            json!({"type": "server/time", "payload": {"client_transmitted": t1,
            "server_received": t1 + 200 + OFFSET, "server_transmitted": t1 + 210 + OFFSET}}),
        );
    }
    assert_eq!(sent, BURST_LEN);
    assert!(s.events().contains(&Event::ClockUpdated));
    assert_eq!(s.conn.clock().offset_us(), (OFFSET + 5) as f64);
    assert_eq!(s.conn.next_wakeup(), Some(BURST_INTERVAL_US));
    // The next burst, BURST_INTERVAL_US after the first began. Without replies, one request
    // every 500 ms.
    s.tick(BURST_INTERVAL_US);
    assert_eq!(kinds(&s.recv()), ["client/time"]);
    s.tick(BURST_INTERVAL_US + REPLY_TIMEOUT_US - 1);
    assert!(s.recv().is_empty());
    s.tick(BURST_INTERVAL_US + REPLY_TIMEOUT_US);
    assert_eq!(kinds(&s.recv()), ["client/time"]);
    // Conversions follow the server's clock.
    let server = s.conn.clock().compute_server_time(s.now);
    assert_eq!(server, s.now + OFFSET + 5);
    assert_eq!(s.conn.clock().compute_client_time(server), s.now);
}

#[test]
fn client_state_is_sent_only_when_allowed_and_changed() {
    let store = store();
    let mut s = playing(&store);
    s.conn.set_player_state(player_state()).unwrap();
    // The player role is active: nothing before the first clock measurement.
    assert_eq!(kinds(&s.recv()), ["client/time"]);
    let outs = s.sync();
    assert_eq!(kinds(&outs), ["client/state"]);
    assert_eq!(
        payload(&outs, "client/state"),
        json!({"available": true, "player": {"volume": 40, "muted": false,
            "output_delay_ms": 0, "required_lead_time_ms": 300, "min_buffer_ms": 150,
            "supported_commands": ["volume", "mute"],
            "format": {"codec": "flac", "channels": 2, "sample_rate": 48000, "bit_depth": 16}}})
    );
    // The same state again: nothing. A change: the full state again.
    s.conn.set_player_state(player_state()).unwrap();
    assert!(s.recv().is_empty());
    let mut state = player_state();
    state.format = None;
    s.conn.set_player_state(state.clone()).unwrap();
    let sent = payload(&s.recv(), "client/state");
    assert_eq!(sent["player"]["volume"], 40);
    assert!(sent["player"].get("format").is_none());

    // Without the player role, the state carries no player object, and only changes go out.
    s.send(activate(json!(["playback"]), json!(["metadata@v1"])));
    assert!(s.recv().is_empty());
    state.available = false;
    s.conn.set_player_state(state.clone()).unwrap();
    assert_eq!(
        payload(&s.recv(), "client/state"),
        json!({"available": false})
    );
    // The player role back: its object goes out in full at once.
    state.available = true;
    s.conn.set_player_state(state.clone()).unwrap();
    s.recv();
    s.send(activate(json!(["playback"]), json!(["player@v1"])));
    let sent = payload(&s.recv(), "client/state");
    assert_eq!(sent["player"]["volume"], 40);

    // Out-of-range values are refused locally.
    state.volume = 101;
    assert_eq!(
        s.conn.set_player_state(state.clone()),
        Err(StateError::Volume)
    );
    state.volume = 0;
    state.min_buffer_ms = 5001;
    assert_eq!(
        s.conn.set_player_state(state.clone()),
        Err(StateError::MinBuffer)
    );
    state.min_buffer_ms = 5000;
    state.format = Some(AudioFormat {
        sample_rate: 96000,
        ..flac48()
    });
    assert_eq!(s.conn.set_player_state(state), Err(StateError::Format));
}

#[test]
fn a_connection_without_the_player_role_reports_without_waiting_for_the_clock() {
    let store = store();
    let mut s = start(&store, &SENTINEL_PSK, "sn");
    s.conn.set_player_state(player_state()).unwrap();
    s.send(activate(json!([]), json!(["controller@v1"])));
    let outs = s.recv();
    assert_eq!(kinds(&outs), ["client/time", "client/state"]);
    assert_eq!(payload(&outs, "client/state"), json!({"available": true}));
}

#[test]
fn server_commands_for_the_player() {
    let store = store();
    let mut s = playing(&store);
    s.sync();
    // Before the player reported anything, commands are ignored.
    s.send(json!({"type": "server/command", "payload": {"player": {"command": "volume", "volume": 10}}}));
    assert!(s.events().iter().all(|e| !matches!(e, Event::Volume(_))));
    let mut state = player_state();
    state.supported_commands = vec![PlayerCommandKind::Volume];
    s.conn.set_player_state(state).unwrap();
    s.recv();
    // Not offered: ignored, value unchecked.
    s.send(
        json!({"type": "server/command", "payload": {"player": {"command": "mute", "mute": 3}}}),
    );
    s.send(
        json!({"type": "server/command", "payload": {"player": {"command": "set_output_delay",
        "output_delay_ms": 99999}}}),
    );
    assert!(s.recv().is_empty());
    assert_eq!(s.events(), []);
    // Offered: applied, reported back.
    s.send(json!({"type": "server/command", "payload": {"player": {"command": "volume", "volume": 55}}}));
    assert_eq!(s.events(), [Event::Volume(55)]);
    let sent = payload(&s.recv(), "client/state");
    assert_eq!(sent["player"]["volume"], 55);
    assert_eq!(sent["player"]["muted"], false);
    // Out of range or missing: protocol error.
    for bad in [
        json!({"command": "volume", "volume": 101}),
        json!({"command": "volume"}),
    ] {
        let mut s = playing(&store);
        s.sync();
        s.conn.set_player_state(player_state()).unwrap();
        s.send(json!({"type": "server/command", "payload": {"player": bad}}));
        assert!(
            matches!(closed(&s.events()), Some(CloseReason::Protocol(_))),
            "{bad}"
        );
    }
    // A player object while the role is inactive is left unread.
    let mut s = start(&store, &SENTINEL_PSK, "sn");
    s.send(activate(json!(["playback"]), json!(["metadata@v1"])));
    s.send(json!({"type": "server/command", "payload": {"player": {"command": 7}}}));
    assert!(!s.conn.is_closed());
}

#[test]
fn controller_commands() {
    let store = store();
    let mut s = playing(&store);
    s.events();
    assert_eq!(
        s.conn.command(ControllerCommand::Pause),
        Err(CommandError::NotSupported(
            messages::ControllerCommandKind::Pause
        ))
    );
    s.send(json!({"type": "server/state", "payload": {"controller": {
        "supported_commands": ["play", "pause", "seek", "volume", "future_thing"],
        "volume": 50, "muted": false, "repeat": "off", "shuffle": false, "seek_max_ms": 1000}}}));
    let Some(Event::ControllerState(Some(state))) = s.events().pop() else {
        panic!()
    };
    assert_eq!(state.volume, 50);
    s.recv();
    assert_eq!(s.conn.command(ControllerCommand::Pause), Ok(()));
    assert_eq!(
        payload(&s.recv(), "client/command"),
        json!({"controller": {"command": "pause"}})
    );
    assert_eq!(
        s.conn.command(ControllerCommand::Next),
        Err(CommandError::NotSupported(
            messages::ControllerCommandKind::Next
        ))
    );
    assert_eq!(
        s.conn
            .command(ControllerCommand::Seek { position_ms: 1001 }),
        Err(CommandError::OutOfRange)
    );
    assert_eq!(
        s.conn.command(ControllerCommand::Volume(101)),
        Err(CommandError::OutOfRange)
    );
    assert_eq!(
        s.conn
            .command(ControllerCommand::Seek { position_ms: 1000 }),
        Ok(())
    );
    assert_eq!(
        payload(&s.recv(), "client/command"),
        json!({"controller": {"command": "seek", "position_ms": 1000}})
    );
    // An out-of-range controller state is a protocol error.
    let mut bad = playing(&store);
    bad.send(json!({"type": "server/state", "payload": {"controller": {
        "supported_commands": [], "volume": 101, "muted": false, "repeat": "off", "shuffle": false}}}));
    assert!(bad.conn.is_closed());
    // The role removed: its state goes, and commands with it.
    s.send(activate(json!(["playback"]), json!(["player@v1"])));
    assert!(s.events().contains(&Event::ControllerState(None)));
    assert_eq!(
        s.conn.command(ControllerCommand::Play),
        Err(CommandError::NotActive)
    );
}

#[test]
fn metadata_now_and_scheduled() {
    let store = store();
    let mut s = playing(&store);
    s.sync();
    s.events();
    let metadata = |timestamp: i64, title: &str| {
        json!({"type": "server/state", "payload": {"metadata": {"timestamp": timestamp,
            "title": title, "progress": {"track_progress": 0, "track_duration": 0,
            "playback_speed": 1000}}}})
    };
    let server_now = s.conn.clock().compute_server_time(s.now);
    s.send(metadata(server_now, "Now"));
    let Some(Event::Metadata(Some(m))) = s.events().pop() else {
        panic!()
    };
    assert_eq!(m.title.as_deref(), Some("Now"));
    // A future timestamp waits for its moment on the player's clock.
    s.send(metadata(server_now + 2 * S, "Next"));
    assert_eq!(s.events(), []);
    let due = s.conn.clock().compute_client_time(server_now + 2 * S);
    assert_eq!(s.conn.next_wakeup(), Some(due));
    s.tick(due - 1);
    assert_eq!(s.events(), []);
    s.tick(due);
    let Some(Event::Metadata(Some(m))) = s.events().pop() else {
        panic!()
    };
    assert_eq!(m.title.as_deref(), Some("Next"));
    // A present update discards the pending one.
    let server_now = s.conn.clock().compute_server_time(s.now);
    s.send(metadata(server_now + 5 * S, "Never"));
    s.send(metadata(server_now, "Instead"));
    s.tick(s.now + 6 * S);
    let titles: Vec<_> = s
        .events()
        .into_iter()
        .filter_map(|e| match e {
            Event::Metadata(Some(m)) => m.title,
            _ => None,
        })
        .collect();
    assert_eq!(titles, ["Instead"]);
    // Limits close the connection.
    let mut bad = playing(&store);
    bad.send(metadata(0, &"x".repeat(256)));
    assert!(bad.conn.is_closed());
    // The role removed: what was shown goes.
    s.send(activate(json!(["playback"]), json!(["player@v1"])));
    assert!(s.events().contains(&Event::Metadata(None)));
    // Metadata for an inactive role is left unread.
    s.send(json!({"type": "server/state", "payload": {"metadata": {"title": 5}}}));
    assert!(!s.conn.is_closed());
}

#[test]
fn group_updates_are_checked_and_passed_on() {
    let store = store();
    let mut s = playing(&store);
    s.events();
    let id = "A".repeat(42) + "_";
    s.send(
        json!({"type": "group/update", "payload": {"playback_state": "stopped",
        "group_id": id, "group_name": "Downstairs"}}),
    );
    assert_eq!(
        s.events(),
        [Event::Group(GroupUpdate {
            playback_state: messages::PlaybackState::Stopped,
            group_id: id,
            group_name: "Downstairs".into()
        })]
    );
    s.send(
        json!({"type": "group/update", "payload": {"playback_state": "stopped",
        "group_id": "A".repeat(42), "group_name": "Downstairs"}}),
    );
    assert!(s.conn.is_closed());
}

#[test]
fn streams_and_audio() {
    let store = store();
    let mut s = playing(&store);
    s.sync();
    s.events();
    s.send(flac_start());
    assert_eq!(
        s.events(),
        [Event::StreamStart(PlayerStream {
            format: flac48(),
            codec_header: Some(b"fLaC".to_vec()),
            in_place: false,
            server_transmitted_us: 1
        })]
    );
    let server_now = s.conn.clock().compute_server_time(s.now);
    let ts = server_now + 500 * MS;
    s.send_plain(&audio(ts, 480_000, b"frame"));
    assert_eq!(
        s.events(),
        [Event::Audio(AudioChunk {
            timestamp_us: ts,
            send_ahead_us: Some(480_000),
            arrival_us: s.now,
            data: b"frame".to_vec()
        })]
    );
    // Saturated send_ahead values measured nothing.
    s.send_plain(&audio(ts + 20 * MS, 0, b"a"));
    s.send_plain(&audio(ts + 40 * MS, u32::MAX, b"b"));
    let leads: Vec<_> = s
        .events()
        .into_iter()
        .map(|e| match e {
            Event::Audio(a) => a.send_ahead_us,
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(leads, [None, None]);
    // Audio due more than 30 s ahead is dropped.
    s.send_plain(&audio(server_now + MAX_AHEAD_US + S, 1, b"far"));
    assert_eq!(s.events(), []);
    // A format change in place.
    s.send(
        json!({"type": "stream/start", "payload": {"server_transmitted": 2,
        "player": {"codec": "pcm", "sample_rate": 48000, "channels": 2, "bit_depth": 16}}}),
    );
    let Some(Event::StreamStart(stream)) = s.events().pop() else {
        panic!()
    };
    assert!(stream.in_place);
    assert_eq!(stream.format, pcm48());
    // Clear from a time, clear all, then end.
    s.send(
        json!({"type": "stream/clear", "payload": {"server_transmitted": 3,
        "from_timestamp": ts, "roles": ["player"]}}),
    );
    s.send(
        json!({"type": "stream/clear", "payload": {"server_transmitted": 4,
        "roles": ["visualizer"]}}),
    );
    s.send(json!({"type": "stream/clear", "payload": {"server_transmitted": 5}}));
    assert_eq!(
        s.events(),
        [
            Event::StreamClear {
                from_timestamp: Some(ts)
            },
            Event::StreamClear {
                from_timestamp: None
            }
        ]
    );
    s.send(json!({"type": "stream/end", "payload": {"roles": ["artwork"]}}));
    assert_eq!(s.events(), []);
    s.send(json!({"type": "stream/end", "payload": {}}));
    assert_eq!(s.events(), [Event::StreamEnd]);
    // Audio after the end is dropped, and a second end changes nothing.
    s.send_plain(&audio(ts, 1, b"late"));
    s.send(json!({"type": "stream/end", "payload": {}}));
    assert_eq!(s.events(), []);
    assert!(!s.conn.is_closed());
}

#[test]
fn audio_and_stream_checks() {
    let store = store();
    let synced = || {
        let mut s = playing(&store);
        s.sync();
        s.send(flac_start());
        s.events();
        s
    };
    // A short audio message is a protocol error, with or without a stream.
    let mut s = synced();
    s.send_plain(&[4; 12]);
    assert!(matches!(
        closed(&s.events()),
        Some(CloseReason::Protocol(_))
    ));
    // A format the player did not offer.
    let mut s = synced();
    s.send(
        json!({"type": "stream/start", "payload": {"server_transmitted": 1,
        "player": {"codec": "pcm", "sample_rate": 44100, "channels": 2, "bit_depth": 16}}}),
    );
    assert!(s.conn.is_closed());
    // An empty roles list.
    let mut s = synced();
    s.send(json!({"type": "stream/clear", "payload": {"server_transmitted": 1, "roles": []}}));
    assert!(s.conn.is_closed());
    // More outstanding audio than buffer_capacity, plus one Noise message of slack.
    let identity = Identity::generate().unwrap();
    let server = Server::new(&identity);
    let mut s = connect(identity, server, &store, &SENTINEL_PSK, "sn", |c| {
        c.buffer_capacity = 70_000;
    });
    s.send(activate(json!(["playback"]), json!(["player@v1"])));
    s.sync();
    s.send(flac_start());
    let ts = s.conn.clock().compute_server_time(s.now) + 5 * S;
    for i in 0..4 {
        s.send_plain(&audio(ts + i * 150 * MS, 1, &[0; 30_000]));
    }
    assert!(!s.conn.is_closed());
    s.send_plain(&audio(ts + 600 * MS, 1, &[0; 30_000]));
    assert!(matches!(
        closed(&s.events()),
        Some(CloseReason::Protocol(_))
    ));
}

#[test]
fn fragmented_audio_is_reassembled() {
    let store = store();
    let mut s = playing(&store);
    s.sync();
    s.send(flac_start());
    s.events();
    let ts = s.conn.clock().compute_server_time(s.now) + S;
    let data: Vec<u8> = (0..100_000u32).map(|i| i as u8).collect();
    let whole = audio(ts, 7, &data);
    let (first, rest) = whole[1..].split_at(60_000);
    let mut fragment = vec![1, 0b10, 4];
    fragment.extend_from_slice(first);
    s.send_plain(&fragment);
    assert_eq!(s.events(), []);
    let mut fragment = vec![1, 0b01];
    fragment.extend_from_slice(rest);
    s.send_plain(&fragment);
    let Some(Event::Audio(chunk)) = s.events().pop() else {
        panic!()
    };
    assert_eq!(chunk.timestamp_us, ts);
    assert_eq!(chunk.data, data);
    // A JSON message cannot interrupt a fragmented one.
    s.send_plain(&[1, 0b10, 4, 0, 0]);
    s.send(json!({"type": "stream/end", "payload": {}}));
    assert!(matches!(
        closed(&s.events()),
        Some(CloseReason::Protocol(_))
    ));
}

#[test]
fn without_playback_the_player_stream_is_ignored() {
    let store = store();
    let mut s = start(&store, &SENTINEL_PSK, "sn");
    s.send(activate(json!([]), json!(["player@v1"])));
    s.sync();
    s.events();
    // Not even checked.
    s.send(
        json!({"type": "stream/start", "payload": {"server_transmitted": 1,
        "player": {"codec": "mp3"}}}),
    );
    s.send_plain(&audio(0, 1, b"x"));
    assert_eq!(s.events(), []);
    assert!(!s.conn.is_closed());
    // Dropping playback ends a running stream even without stream/end.
    s.send(activate(json!(["playback"]), json!(["player@v1"])));
    s.send(flac_start());
    s.events();
    s.send(activate(json!([]), json!(["player@v1"])));
    let events = s.events();
    assert_eq!(events[0], Event::StreamEnd);
}

#[test]
fn admission_through_the_connection() {
    let store = store();
    // Unpaired access off: playback on an unpaired session needs pairing.
    let identity = Identity::generate().unwrap();
    let server = Server::new(&identity);
    let mut s = connect(identity, server, &store, &SENTINEL_PSK, "sn", |c| {
        c.unpaired_access = false;
    });
    s.send(activate(json!(["playback"]), json!(["player@v1"])));
    let outs = s.recv();
    assert_eq!(kinds(&outs), ["client/goodbye", "close"]);
    assert_eq!(
        payload(&outs, "client/goodbye"),
        json!({"reason": "pairing_required"})
    );
    assert_eq!(
        closed_with(&outs),
        Some(CloseReason::Goodbye(GoodbyeReason::PairingRequired))
    );

    // A paired session never pairs.
    let identity = Identity::generate().unwrap();
    let server = Server::new(&identity);
    let mut paired = store.clone();
    let long_term = Psk::random().unwrap();
    paired.insert(
        PairingRecord {
            server_id: server.id(),
            long_term_psk: long_term.clone(),
        },
        &[],
    );
    let mut s = connect(identity, server, &paired, &long_term, "lt", |_| {});
    s.send(
        json!({"type": "server/activate", "payload": {"activities": ["pairing"],
        "pairing": {"method": "pairing_psk"}}}),
    );
    assert_eq!(
        payload(&s.recv(), "client/goodbye"),
        json!({"reason": "unauthorized"})
    );

    // Pairing PSK on a Sentinel session: refused with pair/abort, the connection stays open
    // and the activation is not applied.
    let mut s = start(&store, &SENTINEL_PSK, "sn");
    s.send(
        json!({"type": "server/activate", "payload": {"activities": ["pairing"],
        "active_roles": [], "pairing": {"method": "pairing_psk"}}}),
    );
    let outs = s.recv();
    assert_eq!(kinds(&outs), ["pair/abort"]);
    assert_eq!(
        payload(&outs, "pair/abort"),
        json!({"reason": "method_not_supported"})
    );
    assert!(!s.conn.is_activated());
    s.send(activate(json!(["playback"]), json!(["player@v1"])));
    assert!(activated(&s.events()).is_some());
}

#[test]
fn unpairing() {
    let identity = Identity::generate().unwrap();
    let server = Server::new(&identity);
    let mut store = store();
    let long_term = Psk::random().unwrap();
    store.insert(
        PairingRecord {
            server_id: server.id(),
            long_term_psk: long_term.clone(),
        },
        &[],
    );
    let mut s = connect(identity, server, &store, &long_term, "lt", |_| {});
    s.send(activate(json!(["playback"]), json!(["player@v1"])));
    s.events();
    s.recv();
    s.send(json!({"type": "server/unpair", "payload": {}}));
    let events = s.events();
    assert_eq!(
        events,
        [
            Event::Unpaired(s.server.id()),
            Event::Closed(CloseReason::Goodbye(GoodbyeReason::Unpaired))
        ]
    );
    assert_eq!(
        payload(&s.recv(), "client/goodbye"),
        json!({"reason": "unpaired"})
    );
    assert!(s.conn.store().pairings().is_empty());

    // On an unpaired session it changes nothing.
    let mut s = playing(&PairingStore::generate().unwrap());
    s.events();
    s.send(json!({"type": "server/unpair", "payload": {}}));
    assert_eq!(s.events(), []);
    assert!(!s.conn.is_closed());
}

#[test]
fn leaving_and_goodbye() {
    let store = store();
    let mut s = playing(&store);
    s.recv();
    assert_eq!(s.conn.leave(), Ok(()));
    let outs = s.recv();
    assert_eq!(kinds(&outs), ["client/leave"]);
    assert_eq!(payload(&outs, "client/leave"), json!({}));
    s.conn.goodbye(GoodbyeReason::Shutdown);
    let outs = s.recv();
    assert_eq!(kinds(&outs), ["client/goodbye", "close"]);
    assert_eq!(
        payload(&outs, "client/goodbye"),
        json!({"reason": "shutdown"})
    );
    assert_eq!(s.conn.leave(), Err(CommandError::NotActive));
    assert_eq!(s.conn.next_wakeup(), None);

    // A goodbye is allowed once the handshake is done, before any activation.
    let mut s = start(&store, &SENTINEL_PSK, "sn");
    s.conn.goodbye(GoodbyeReason::Restart);
    assert_eq!(kinds(&s.recv()), ["client/goodbye", "close"]);
    // Before it, the connection just closes.
    let mut conn = Connection::new(config(Identity::generate().unwrap()), &store, 0).unwrap();
    conn.poll_transmit();
    conn.goodbye(GoodbyeReason::Shutdown);
    assert_eq!(
        conn.poll_transmit(),
        Some(Transmit::Close(CloseReason::Goodbye(
            GoodbyeReason::Shutdown
        )))
    );
    assert_eq!(conn.leave(), Err(CommandError::NotActive));
}

#[test]
fn a_newcomer_against_the_admitted_connection() {
    let store = store();
    let mut admitted = playing(&store);
    let rival = admitted.conn.as_rival().unwrap();
    assert_eq!(rival.activities, PLAYBACK);

    // Lower rank loses.
    let mut newcomer = start(&store, &SENTINEL_PSK, "sn");
    newcomer.conn.set_rival(Some(rival.clone()));
    newcomer.send(activate(json!([]), json!([])));
    let outs = newcomer.recv();
    assert_eq!(
        payload(&outs, "client/goodbye"),
        json!({"reason": "concurrent_attempt"})
    );
    assert!(activated(&newcomer.events()).is_none());

    // A rejected pairing gets pair/abort instead.
    let mut newcomer = start(&store, store.pairing_psk(), "pr");
    newcomer.conn.set_rival(Some(rival.clone()));
    newcomer.send(
        json!({"type": "server/activate", "payload": {"activities": ["pairing"],
        "active_roles": [], "pairing": {"method": "pairing_psk"}}}),
    );
    let outs = newcomer.recv();
    assert_eq!(kinds(&outs), ["pair/abort", "close"]);
    assert_eq!(
        payload(&outs, "pair/abort"),
        json!({"reason": "concurrent_attempt"})
    );

    // Equal rank wins, and the admitted connection is displaced.
    let mut newcomer = start(&store, &SENTINEL_PSK, "sn");
    newcomer.conn.set_rival(Some(rival));
    newcomer.send(activate(json!(["playback"]), json!(["player@v1"])));
    assert_eq!(
        activated(&newcomer.events()).map(|a| a.3),
        Some(true),
        "displaces_rival"
    );
    admitted.recv();
    admitted.conn.displace();
    assert_eq!(
        payload(&admitted.recv(), "client/goodbye"),
        json!({"reason": "another_server"})
    );

    // A pairing attempt in progress is never displaced.
    let mut pairing = start(&store, store.pairing_psk(), "pr");
    pairing.send(
        json!({"type": "server/activate", "payload": {"activities": ["pairing"],
        "active_roles": [], "pairing": {"method": "pairing_psk"}}}),
    );
    let rival = pairing.conn.as_rival().unwrap();
    assert!(rival.pairing_attempt);
    let mut newcomer = start(&store, &SENTINEL_PSK, "sn");
    newcomer.conn.set_rival(Some(rival));
    newcomer.send(activate(json!(["playback"]), json!(["player@v1"])));
    assert!(newcomer.conn.is_closed());
    // Displacing it aborts the pairing.
    pairing.recv();
    pairing.conn.displace();
    assert_eq!(
        payload(&pairing.recv(), "pair/abort"),
        json!({"reason": "concurrent_attempt"})
    );
}

#[test]
fn between_idle_connections_the_last_playback_server_wins() {
    let identity = Identity::generate().unwrap();
    let server = Server::new(&identity);
    let mut store = store();
    store.set_last_playback_server(&server.id());
    let idle = Rival {
        server_id: "other".into(),
        activities: Activities::default(),
        pairing_attempt: false,
    };
    let mut newcomer = connect(identity, server, &store, &SENTINEL_PSK, "sn", |_| {});
    newcomer.conn.set_rival(Some(idle.clone()));
    newcomer.send(activate(json!([]), json!([])));
    assert_eq!(activated(&newcomer.events()).map(|a| a.3), Some(true));

    // Any other server loses against an idle connection.
    let mut stranger = start(&store, &SENTINEL_PSK, "sn");
    stranger.conn.set_rival(Some(idle));
    stranger.send(activate(json!([]), json!([])));
    assert!(stranger.conn.is_closed());
}
