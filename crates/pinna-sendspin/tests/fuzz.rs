//! Pseudo-random input from the server, which must never make the player panic: into the
//! parsers directly, many times, and into whole sessions, through valid Noise encryption so it
//! reaches the parsers behind it.

mod common;

use common::*;
use pinna_sendspin::frame::{Reassembler, split_audio};
use pinna_sendspin::messages::{
    self, ControllerState, Metadata, PairingParams, ServerMessage, StreamFormat,
};
use pinna_sendspin::*;
use serde_json::{Value, json};

/// xorshift64*: enough to spread inputs, and the same on every run.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| self.next() as u8).collect()
    }

    fn value(&mut self, depth: u32) -> Value {
        match self.below(if depth > 3 { 6 } else { 8 }) {
            0 => Value::Null,
            1 => json!(self.below(2) == 1),
            2 => json!(
                [0i64, -1, 1, 100, 101, 5000, 5001, i64::MAX, i64::MIN][self.below(9) as usize]
            ),
            3 => json!(1e300),
            4 => {
                let n = self.below(80) as usize;
                json!(String::from_utf8_lossy(&self.bytes(n)))
            }
            5 => json!(
                [
                    "playback",
                    "pairing",
                    "player@v1",
                    "volume",
                    "flac",
                    "pcm",
                    "lt",
                    "player"
                ][self.below(8) as usize]
            ),
            6 => Value::Array((0..self.below(4)).map(|_| self.value(depth + 1)).collect()),
            _ => {
                let keys = ["player", "volume", "codec", "timestamp", "activities", "x"];
                Value::Object(
                    (0..self.below(4))
                        .map(|_| {
                            (
                                keys[self.below(6) as usize].to_string(),
                                self.value(depth + 1),
                            )
                        })
                        .collect(),
                )
            }
        }
    }

    /// Replaces one value somewhere inside `v`.
    fn mutate(&mut self, v: &mut Value) {
        match v {
            Value::Object(map) if !map.is_empty() && self.below(4) != 0 => {
                let i = self.below(map.len() as u64) as usize;
                if let Some((_, child)) = map.iter_mut().nth(i) {
                    self.mutate(child);
                }
            }
            Value::Array(items) if !items.is_empty() && self.below(4) != 0 => {
                let i = self.below(items.len() as u64) as usize;
                self.mutate(&mut items[i]);
            }
            _ => *v = self.value(0),
        }
    }

    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len() as u64) as usize]
    }
}

/// Valid messages that an activated session survives.
fn corpus() -> Vec<Value> {
    vec![
        json!({"type": "server/time", "payload": {"client_transmitted": 1,
            "server_received": 2, "server_transmitted": 3}}),
        json!({"type": "server/state", "payload": {"metadata": {"timestamp": 5, "title": "t",
            "artist": "a", "album": "b", "artwork_url": "u", "year": 2024, "track": 2,
            "progress": {"track_progress": 1, "track_duration": 2, "playback_speed": 1000}},
            "controller": {"supported_commands": ["play"], "volume": 5, "muted": true,
            "repeat": "one", "shuffle": false, "seek_max_ms": 7}}}),
        json!({"type": "server/command", "payload": {"player": {"command": "volume", "volume": 7}}}),
        json!({"type": "server/command", "payload": {"player": {"command": "mute", "mute": true}}}),
        json!({"type": "group/update", "payload": {"playback_state": "playing",
            "group_id": "A".repeat(43), "group_name": "g"}}),
        flac_start(),
        json!({"type": "stream/clear", "payload": {"server_transmitted": 1,
            "from_timestamp": 9, "roles": ["player"]}}),
        json!({"type": "stream/end", "payload": {"roles": ["player"]}}),
        activate(json!(["playback"]), all_roles()),
        json!({"type": "server/activate", "payload": {"activities": ["pairing"],
            "pairing": {"method": "pairing_psk"}}}),
        json!({"type": "pair/abort", "payload": {"reason": "user_cancelled"}}),
        json!({"type": "server/unpair", "payload": {}}),
        json!({"type": "server/new-thing", "payload": {"x": 1}}),
    ]
}

/// Messages that end most sessions, mixed in less often.
fn fatal() -> Vec<Value> {
    vec![
        json!({"type": "server/pair-finalize", "payload": {}}),
        json!({"type": "noise/handshake", "payload": {"data": "AAAA"}}),
        json!({"type": "server/hello", "payload": {"name": "n", "languages": ["en"]}}),
    ]
}

fn json_message(value: &Value) -> Vec<u8> {
    let mut plain = vec![0];
    plain.extend(value.to_string().into_bytes());
    plain
}

#[test]
fn parsers_never_panic() {
    let mut rng = Rng(0x1234_5678_9abc_def1);
    let corpus = corpus();
    let offered = [flac48(), pcm48()];
    for _ in 0..20_000 {
        let mut v = rng.pick(&corpus).clone();
        rng.mutate(&mut v);
        let mut bytes = v.to_string().into_bytes();
        if rng.below(4) == 0 && !bytes.is_empty() {
            let at = rng.below(bytes.len() as u64) as usize;
            bytes[at] = rng.next() as u8;
        }
        if let Ok(envelope) = messages::envelope(&bytes) {
            let _ = ServerMessage::from_envelope(envelope);
        }
        let random = rng.value(0);
        let _ = Metadata::from_value(random.clone());
        let _ = ControllerState::from_value(random.clone());
        let _ = StreamFormat::from_value(random.clone(), &offered);
        let _ = PairingParams::from_value(random);
        if let Some(m) = Metadata::from_value(v["payload"]["metadata"].clone())
            .ok()
            .filter(|_| rng.below(2) == 0)
        {
            let _ = m.position_ms(rng.next() as i64);
        }
    }
    for _ in 0..2000 {
        let mut r = Reassembler::new(rng.below(70_000) as usize);
        for _ in 0..rng.below(12) {
            let n = rng.below(40) as usize;
            let mut plain = rng.bytes(n);
            if rng.below(2) == 0 && !plain.is_empty() {
                plain[0] = 1;
            }
            match r.push(plain) {
                Ok(Some(m)) if m.kind == 4 => {
                    let _ = split_audio(&m.body);
                }
                Err(_) => break,
                _ => {}
            }
        }
    }
}

#[test]
fn random_input_never_panics_a_connection() {
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let store = PairingStore::generate().unwrap();
    let identity = Identity::generate().unwrap();
    // Raw bytes during the handshake.
    for i in 0..1000 {
        let mut conn = Connection::new(config(identity.clone()), &store, 0).unwrap();
        if i % 3 == 2 {
            conn.on_text(
                &json!({"type": "server/init", "payload": {"server_id": b64(&[9; 32]),
                    "version": 1}})
                .to_string(),
                0,
            );
        }
        let n = rng.below(200) as usize;
        let bytes = rng.bytes(n);
        if i % 2 == 0 {
            conn.on_binary(&bytes, 0);
        } else {
            conn.on_text(&String::from_utf8_lossy(&bytes), 0);
        }
        conn.on_tick(rng.next() as i64);
        while conn.poll_transmit().is_some() {}
        while conn.poll_event().is_some() {}
    }
    // Encrypted messages into sessions at three stages, until each closes.
    let corpus = corpus();
    let fatal = fatal();
    for round in 0..60 {
        let server = Server::new(&identity);
        let mut s = connect(
            identity.clone(),
            server,
            &store,
            store.pairing_psk(),
            "pr",
            |_| {},
        );
        if round % 3 != 0 {
            s.send(activate(json!(["playback"]), all_roles()));
            s.conn.set_player_state(player_state()).unwrap();
        }
        if round % 3 == 2 {
            s.sync();
            s.send(flac_start());
        }
        for _ in 0..60 {
            let plain = match rng.below(20) {
                0..=9 => json_message(rng.pick(&corpus)),
                10 => json_message(rng.pick(&fatal)),
                11 | 12 => {
                    let mut v = rng.pick(&corpus).clone();
                    rng.mutate(&mut v);
                    json_message(&v)
                }
                13 => {
                    let mut plain = json_message(rng.pick(&corpus));
                    let at = rng.below(plain.len() as u64) as usize;
                    plain[at] = rng.next() as u8;
                    plain
                }
                14..=16 => {
                    let ahead = rng.below(40 * S as u64) as i64;
                    let ts = s.conn.clock().compute_server_time(s.now) + ahead;
                    let n = rng.below(2000) as usize;
                    audio(ts, rng.next() as u32, &rng.bytes(n))
                }
                17 => {
                    let mut plain = vec![1, rng.next() as u8 & 0b111];
                    let n = rng.below(20) as usize;
                    plain.extend(rng.bytes(n));
                    plain
                }
                18 => {
                    let n = rng.below(64) as usize;
                    rng.bytes(n)
                }
                _ => json_message(&rng.value(0)),
            };
            s.send_plain(&plain);
            if rng.below(4) == 0 {
                let now = s.now + rng.below(3 * S as u64) as i64;
                s.tick(now);
            }
            if rng.below(8) == 0 {
                let _ = s.conn.command(ControllerCommand::Seek {
                    position_ms: rng.next() as i64,
                });
                let _ = s.conn.leave();
            }
            while s.conn.poll_transmit().is_some() {}
            while s.conn.poll_event().is_some() {}
            if s.conn.is_closed() {
                break;
            }
        }
    }
}
