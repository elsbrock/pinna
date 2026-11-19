//! A Sendspin server built with snow, as Music Assistant's would be, to run sessions against
//! the player's [`Connection`].

#![allow(dead_code)]

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use pinna_sendspin::noise::PROTOCOL;
use pinna_sendspin::psk::SENTINEL_PSK;
use pinna_sendspin::*;
use serde_json::{Value, json};

pub const MS: i64 = 1_000;
pub const S: i64 = 1_000_000;
/// The server's clock minus the player's.
pub const OFFSET: i64 = 7 * S;

pub fn b64(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

pub fn unb64(text: &str) -> Vec<u8> {
    URL_SAFE_NO_PAD.decode(text).unwrap()
}

pub fn flac48() -> AudioFormat {
    AudioFormat {
        codec: Codec::Flac,
        channels: 2,
        sample_rate: 48000,
        bit_depth: 16,
    }
}

pub fn pcm48() -> AudioFormat {
    AudioFormat {
        codec: Codec::Pcm,
        channels: 2,
        sample_rate: 48000,
        bit_depth: 16,
    }
}

pub fn config(identity: Identity) -> Config {
    Config {
        identity,
        name: "Kitchen".into(),
        device_info: Some(DeviceInfo {
            product_name: Some("Test speaker".into()),
            manufacturer: None,
            software_version: Some("0.4.0".into()),
            mac_address: Some("aa:bb:cc:dd:ee:ff".into()),
        }),
        roles: vec![Role::Player, Role::Controller, Role::Metadata],
        formats: vec![flac48(), pcm48()],
        buffer_capacity: 1 << 20,
        unpaired_access: true,
    }
}

pub fn player_state() -> PlayerState {
    PlayerState {
        available: true,
        volume: 40,
        muted: false,
        required_lead_time_ms: 300,
        min_buffer_ms: 150,
        format: Some(flac48()),
        supported_commands: vec![PlayerCommandKind::Volume, PlayerCommandKind::Mute],
    }
}

/// What the player sent, decrypted.
#[derive(Debug, Clone, PartialEq)]
pub enum Out {
    Text(Value),
    Json(Value),
    Binary(u8, Vec<u8>),
    Close(CloseReason),
}

pub fn kinds(outs: &[Out]) -> Vec<String> {
    outs.iter()
        .map(|o| match o {
            Out::Text(v) | Out::Json(v) => v["type"].as_str().unwrap().to_string(),
            Out::Binary(k, _) => format!("binary {k}"),
            Out::Close(_) => "close".into(),
        })
        .collect()
}

/// The payload of the one message of type `kind`.
pub fn payload(outs: &[Out], kind: &str) -> Value {
    let found: Vec<&Value> = outs
        .iter()
        .filter_map(|o| match o {
            Out::Text(v) | Out::Json(v) if v["type"] == kind => Some(v),
            _ => None,
        })
        .collect();
    assert_eq!(found.len(), 1, "{kind} in {:?}", kinds(outs));
    found[0]["payload"].clone()
}

pub fn closed_with(outs: &[Out]) -> Option<CloseReason> {
    outs.iter().find_map(|o| match o {
        Out::Close(r) => Some(r.clone()),
        _ => None,
    })
}

pub fn write(hs: &mut snow::HandshakeState, payload: &[u8]) -> Vec<u8> {
    let mut out = vec![0u8; 1024];
    let n = hs.write_message(payload, &mut out).unwrap();
    out.truncate(n);
    out
}

pub struct Server {
    pub keys: snow::Keypair,
    client_key: Vec<u8>,
    transport: Option<snow::TransportState>,
    hash: Vec<u8>,
}

impl Server {
    pub fn new(client: &Identity) -> Self {
        Self {
            keys: snow::Builder::new(PROTOCOL.parse().unwrap())
                .generate_keypair()
                .unwrap(),
            client_key: client.public_key().to_vec(),
            transport: None,
            hash: Vec::new(),
        }
    }

    pub fn id(&self) -> String {
        b64(&self.keys.public)
    }

    fn initiator(&self, prologue: &[u8], psk: &Psk, ephemeral: &[u8; 32]) -> snow::HandshakeState {
        snow::Builder::new(PROTOCOL.parse().unwrap())
            .local_private_key(&self.keys.private)
            .unwrap()
            .remote_public_key(&self.client_key)
            .unwrap()
            .prologue(prologue)
            .unwrap()
            .psk(2, psk.bytes())
            .unwrap()
            .fixed_ephemeral_key_for_testing_only(ephemeral)
            .build_initiator()
            .unwrap()
    }

    /// The initial handshake. Message 1 names `named` as `category`; message 2 is checked with
    /// `verify`, as a server checks it again with the Sentinel key after a mismatch. Returns
    /// false when the player did not answer or its answer did not verify.
    pub fn handshake_as(
        &mut self,
        conn: &mut Connection,
        named: &Psk,
        category: &str,
        verify: &Psk,
        now: i64,
    ) -> bool {
        let Some(Transmit::Text(init)) = conn.poll_transmit() else {
            panic!("no client/init");
        };
        let server_init = json!({"type": "server/init",
            "payload": {"server_id": self.id(), "version": 1}})
        .to_string();
        let mut prologue = init.into_bytes();
        prologue.extend_from_slice(server_init.as_bytes());
        let ephemeral = *Psk::random().unwrap().bytes();
        let mut writer = self.initiator(&prologue, named, &ephemeral);
        let payload = json!({"psk_id": named.id(), "psk_category": category}).to_string();
        let msg1 = write(&mut writer, payload.as_bytes());
        // A second state with the same ephemeral key writes the same message 1 and checks
        // message 2 with the other key.
        let mut reader = if verify == named {
            writer
        } else {
            let mut reader = self.initiator(&prologue, verify, &ephemeral);
            assert_eq!(write(&mut reader, payload.as_bytes()), msg1);
            reader
        };

        conn.on_text(&server_init, now);
        conn.on_text(
            &json!({"type": "noise/handshake", "payload": {"data": b64(&msg1)}}).to_string(),
            now,
        );
        let Some(Transmit::Text(reply)) = conn.poll_transmit() else {
            return false;
        };
        let reply: Value = serde_json::from_str(&reply).unwrap();
        assert_eq!(reply["type"], "noise/handshake");
        let msg2 = unb64(reply["payload"]["data"].as_str().unwrap());
        let mut payload = vec![0u8; 256];
        let Ok(n) = reader.read_message(&msg2, &mut payload) else {
            return false;
        };
        assert_eq!(&payload[..n], b"{}");
        self.hash = reader.get_handshake_hash().to_vec();
        self.transport = Some(reader.into_transport_mode().unwrap());
        true
    }

    pub fn handshake(&mut self, conn: &mut Connection, psk: &Psk, category: &str, now: i64) {
        assert!(self.handshake_as(conn, psk, category, psk, now));
    }

    /// The server's half of an in-band re-handshake to `psk`. Returns false when the player
    /// did not answer.
    pub fn rehandshake(
        &mut self,
        conn: &mut Connection,
        psk: &Psk,
        category: &str,
        now: i64,
    ) -> bool {
        let ephemeral = *Psk::random().unwrap().bytes();
        let prologue = self.hash.clone();
        let mut hs = self.initiator(&prologue, psk, &ephemeral);
        let payload = json!({"psk_id": psk.id(), "psk_category": category}).to_string();
        let msg1 = write(&mut hs, payload.as_bytes());
        self.send(
            conn,
            json!({"type": "noise/handshake", "payload": {"data": b64(&msg1)}}),
            now,
        );
        // Message 2 comes under the old keys.
        let outs = self.recv(conn);
        let [Out::Json(reply)] = outs.as_slice() else {
            return false;
        };
        assert_eq!(reply["type"], "noise/handshake");
        let msg2 = unb64(reply["payload"]["data"].as_str().unwrap());
        let mut payload = vec![0u8; 256];
        let n = hs.read_message(&msg2, &mut payload).unwrap();
        assert_eq!(&payload[..n], b"{}");
        self.hash = hs.get_handshake_hash().to_vec();
        self.transport = Some(hs.into_transport_mode().unwrap());
        true
    }

    pub fn encrypt(&mut self, plain: &[u8]) -> Vec<u8> {
        let mut out = vec![0u8; plain.len() + 16];
        let n = self
            .transport
            .as_mut()
            .unwrap()
            .write_message(plain, &mut out)
            .unwrap();
        out.truncate(n);
        out
    }

    pub fn send_plain(&mut self, conn: &mut Connection, plain: &[u8], now: i64) {
        let message = self.encrypt(plain);
        conn.on_binary(&message, now);
    }

    pub fn send(&mut self, conn: &mut Connection, message: Value, now: i64) {
        let mut plain = vec![0u8];
        plain.extend_from_slice(message.to_string().as_bytes());
        self.send_plain(conn, &plain, now);
    }

    pub fn recv(&mut self, conn: &mut Connection) -> Vec<Out> {
        let mut outs = Vec::new();
        while let Some(t) = conn.poll_transmit() {
            outs.push(match t {
                Transmit::Text(text) => Out::Text(serde_json::from_str(&text).unwrap()),
                Transmit::Binary(message) => {
                    let mut plain = vec![0u8; message.len()];
                    let n = self
                        .transport
                        .as_mut()
                        .unwrap()
                        .read_message(&message, &mut plain)
                        .unwrap();
                    plain.truncate(n);
                    match plain[0] {
                        0 => Out::Json(serde_json::from_slice(&plain[1..]).unwrap()),
                        k => Out::Binary(k, plain[1..].to_vec()),
                    }
                }
                Transmit::Close(reason) => Out::Close(reason),
            });
        }
        outs
    }
}

pub fn hello() -> Value {
    json!({"type": "server/hello", "payload": {"name": "Music Assistant"}})
}

pub fn activate(activities: Value, roles: Value) -> Value {
    json!({"type": "server/activate", "payload": {"activities": activities, "active_roles": roles}})
}

pub fn all_roles() -> Value {
    json!(["player@v1", "controller@v1", "metadata@v1"])
}

pub fn audio(timestamp: i64, send_ahead: u32, data: &[u8]) -> Vec<u8> {
    let mut plain = vec![4u8];
    plain.extend_from_slice(&timestamp.to_be_bytes());
    plain.extend_from_slice(&send_ahead.to_be_bytes());
    plain.extend_from_slice(data);
    plain
}

pub fn flac_start() -> Value {
    json!({"type": "stream/start", "payload": {"server_transmitted": 1,
        "player": {"codec": "flac", "sample_rate": 48000, "channels": 2, "bit_depth": 16,
            "codec_header": "ZkxhQw=="}}})
}

/// A connection and its server, from the handshake on.
pub struct Session {
    pub conn: Connection,
    pub server: Server,
    pub now: i64,
}

impl Session {
    pub fn send(&mut self, message: Value) {
        self.server.send(&mut self.conn, message, self.now);
    }

    pub fn send_plain(&mut self, plain: &[u8]) {
        self.server.send_plain(&mut self.conn, plain, self.now);
    }

    pub fn recv(&mut self) -> Vec<Out> {
        self.server.recv(&mut self.conn)
    }

    pub fn events(&mut self) -> Vec<Event> {
        std::iter::from_fn(|| self.conn.poll_event()).collect()
    }

    pub fn tick(&mut self, now: i64) {
        self.now = now;
        self.conn.on_tick(now);
    }

    /// Answers `client/time` until the filter takes a measurement. Returns everything else
    /// the player sent meanwhile. Each exchange takes 400 µs, 10 µs of it in the server.
    pub fn sync(&mut self) -> Vec<Out> {
        self.sync_with(|t| t + OFFSET)
    }

    /// [`Session::sync`] with a server whose clock reads `server_time(t)` at the player's time
    /// `t`. Stops early if the connection closes.
    pub fn sync_with(&mut self, server_time: impl Fn(i64) -> i64) -> Vec<Out> {
        let before = self.conn.clock().count();
        let mut other = Vec::new();
        for _ in 0..100 {
            if self.conn.clock().count() > before || self.conn.is_closed() {
                other.extend(self.recv());
                return other;
            }
            let mut answered = false;
            for out in self.recv() {
                match &out {
                    Out::Json(v) if v["type"] == "client/time" => {
                        let t1 = v["payload"]["client_transmitted"].as_i64().unwrap();
                        self.now = t1 + 400;
                        self.send(json!({"type": "server/time", "payload": {
                            "client_transmitted": t1,
                            "server_received": server_time(t1 + 200),
                            "server_transmitted": server_time(t1 + 210)}}));
                        answered = true;
                    }
                    _ => other.push(out),
                }
            }
            if !answered {
                let at = self.conn.next_wakeup().expect("nothing scheduled");
                self.tick(at.max(self.now));
            }
        }
        panic!("the clock never took a measurement");
    }
}

/// A session through `server/hello` and `client/hello`, keyed with `psk` as `category`.
pub fn connect(
    identity: Identity,
    mut server: Server,
    store: &PairingStore,
    psk: &Psk,
    category: &str,
    adjust: impl FnOnce(&mut Config),
) -> Session {
    let mut config = config(identity);
    adjust(&mut config);
    let mut conn = Connection::new(config, store, 0).unwrap();
    server.handshake(&mut conn, psk, category, 0);
    let mut session = Session {
        conn,
        server,
        now: 0,
    };
    session.send(hello());
    assert_eq!(kinds(&session.recv()), ["client/hello"]);
    session
}

pub fn start(store: &PairingStore, psk: &Psk, category: &str) -> Session {
    let identity = Identity::generate().unwrap();
    let server = Server::new(&identity);
    connect(identity, server, store, psk, category, |_| {})
}

/// An unpaired session with every role active and playback declared.
pub fn playing(store: &PairingStore) -> Session {
    let mut s = start(store, &SENTINEL_PSK, "sn");
    s.send(activate(json!(["playback"]), all_roles()));
    s
}
