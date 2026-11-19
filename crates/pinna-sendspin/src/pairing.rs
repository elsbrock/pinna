//! What the player keeps of its pairings, and the Pairing PSK method (ADR 0027 §5, §7).
//!
//! The player's own pairing key lives as long as its identity: an owner hands it to a server
//! as a token, and the two then hold a long-term key of their own. Up to eight such pairings are
//! kept; a ninth evicts the oldest that no open connection uses. A server whose pairing was
//! evicted lands on the Sentinel key at its next handshake and can offer re-pairing.

use serde::{Deserialize, Serialize};

use crate::identity::Identity;
use crate::noise::Candidate;
use crate::psk::{Psk, PskCategory, RandomError, SENTINEL_PSK};

/// Pairings kept at once. The specification asks for at least five.
pub const MAX_PAIRINGS: usize = 8;

/// How long a pairing attempt may take, from `client/pair-init`: the specification's
/// recommendation.
pub const ATTEMPT_TIMEOUT_US: i64 = 120_000_000;

/// A server the player is paired with, and the key they share.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairingRecord {
    pub server_id: String,
    pub long_term_psk: Psk,
}

/// Everything about pairing that outlives a connection. `pinnad` keeps it in
/// `/var/lib/pinna/sendspin.toml` (ADR 0027 §7). Its Debug output shows no key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairingStore {
    pairing_psk: Psk,
    /// The server that last held a connection with `playback` among its activities.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_playback_server: Option<String>,
    /// Oldest first.
    #[serde(default)]
    pairings: Vec<PairingRecord>,
}

impl PairingStore {
    /// A store with a new pairing key and no pairings.
    pub fn generate() -> Result<Self, RandomError> {
        Ok(Self::with_pairing_psk(Psk::random()?))
    }

    pub fn with_pairing_psk(pairing_psk: Psk) -> Self {
        Self {
            pairing_psk,
            last_playback_server: None,
            pairings: Vec::new(),
        }
    }

    pub fn pairing_psk(&self) -> &Psk {
        &self.pairing_psk
    }

    /// Oldest first.
    pub fn pairings(&self) -> &[PairingRecord] {
        &self.pairings
    }

    /// Stores `record`, replacing the server's earlier one. At capacity the oldest records whose
    /// server is not in `open` go, and are returned.
    ///
    /// `open` holds the server of every open connection, provisional or admitted: the
    /// specification forbids evicting a record that backs one. A provisional connection can be
    /// keyed with the oldest record while the admitted one completes a pairing. The
    /// specification also has `pinnad` keep fewer paired connections open than
    /// [`MAX_PAIRINGS`], so there is always a record to evict. If there is none, the store
    /// goes over capacity rather than evict a record in use or fail the pairing.
    pub fn insert(&mut self, record: PairingRecord, open: &[&str]) -> Vec<PairingRecord> {
        self.pairings.retain(|r| r.server_id != record.server_id);
        let mut evicted = Vec::new();
        while self.pairings.len() >= MAX_PAIRINGS {
            let Some(i) = self
                .pairings
                .iter()
                .position(|r| !open.contains(&r.server_id.as_str()))
            else {
                break;
            };
            evicted.push(self.pairings.remove(i));
        }
        self.pairings.push(record);
        evicted
    }

    /// Removes the server's record. Returns whether there was one.
    pub fn remove(&mut self, server_id: &str) -> bool {
        let before = self.pairings.len();
        self.pairings.retain(|r| r.server_id != server_id);
        self.pairings.len() != before
    }

    pub fn last_playback_server(&self) -> Option<&str> {
        self.last_playback_server.as_deref()
    }

    /// Returns whether it changed.
    pub fn set_last_playback_server(&mut self, server_id: &str) -> bool {
        if self.last_playback_server.as_deref() == Some(server_id) {
            return false;
        }
        self.last_playback_server = Some(server_id.to_string());
        true
    }

    /// The pairing token, version 0: `SP:0` and the base32 of the public key and the pairing
    /// key. Anyone who holds it can pair a server with this player.
    pub fn token(&self, identity: &Identity) -> String {
        let mut payload = [0u8; 64];
        payload[..32].copy_from_slice(identity.public_key());
        payload[32..].copy_from_slice(self.pairing_psk.bytes());
        format!("SP:0{}", base32(&payload).replace('2', "9"))
    }

    /// Every key a handshake can complete with: the pairings, the pairing key, which must
    /// always be among them, and the Sentinel key.
    pub fn candidates(&self) -> Vec<Candidate<'_>> {
        let mut candidates: Vec<Candidate<'_>> = self
            .pairings
            .iter()
            .map(|r| Candidate {
                category: PskCategory::LongTerm,
                psk: &r.long_term_psk,
                server_id: Some(&r.server_id),
            })
            .collect();
        candidates.push(Candidate {
            category: PskCategory::Pairing,
            psk: &self.pairing_psk,
            server_id: None,
        });
        candidates.push(Candidate {
            category: PskCategory::Sentinel,
            psk: &SENTINEL_PSK,
            server_id: None,
        });
        candidates
    }
}

/// RFC 4648 base32, `A`–`Z` and `2`–`7`, without padding.
fn base32(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let mut out = String::with_capacity(bytes.len().div_ceil(5) * 8);
    let (mut buffer, mut bits) = (0u32, 0u32);
    for &b in bytes {
        buffer = (buffer << 8) | u32::from(b);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(char::from(ALPHABET[((buffer >> bits) & 31) as usize]));
        }
        buffer &= (1 << bits) - 1;
    }
    if bits > 0 {
        out.push(char::from(ALPHABET[((buffer << (5 - bits)) & 31) as usize]));
    }
    out
}

/// One connection's Pairing PSK attempts. After the pairing `server/activate` the player sends
/// `client/pair-init` and `client/pair-finalize` with a new long-term key at once; the record
/// is kept only when `server/pair-finalize` confirms the server kept its own.
#[derive(Debug, Default)]
pub struct PairingFlow {
    /// Pairing activations received since the last Noise handshake.
    index: u32,
    attempt: Attempt,
}

#[derive(Debug, Default)]
enum Attempt {
    #[default]
    Idle,
    /// Sent the key, waiting for the server to confirm it kept it.
    Sent { psk: Psk, deadline: i64 },
    /// The player aborted: pairing messages are dropped until the next activation.
    Aborted,
}

/// What [`PairingFlow::on_finalize`] found.
#[derive(Debug, PartialEq, Eq)]
pub enum Finalize {
    /// Keep this key with the server's id.
    Keep(Psk),
    /// Dropped: the player had aborted.
    Ignore,
    /// No attempt was waiting for it: a protocol error.
    OutOfSequence,
}

impl PairingFlow {
    /// A Noise handshake, first or repeated, restarts the count of pairing activations.
    pub fn on_handshake(&mut self) {
        self.index = 0;
    }

    /// Counts an activation that declares `pairing`, admissible or not.
    pub fn count_pairing_activation(&mut self) {
        self.index = self.index.saturating_add(1);
    }

    /// Every activation ends the attempt in progress without keeping anything, and ends the
    /// silence after an abort.
    pub fn on_activate(&mut self) {
        self.attempt = Attempt::Idle;
    }

    /// Starts an attempt at `now`: the `pairing_index` for `client/pair-init` and the new
    /// long-term key for `client/pair-finalize`. The key is never the Sentinel key or the
    /// pairing key, which the server would take as a protocol error.
    pub fn begin(&mut self, now: i64, pairing_psk: &Psk) -> Result<(u32, Psk), RandomError> {
        let psk = loop {
            let psk = Psk::random()?;
            if psk != SENTINEL_PSK && &psk != pairing_psk {
                break psk;
            }
        };
        self.attempt = Attempt::Sent {
            psk: psk.clone(),
            deadline: now.saturating_add(ATTEMPT_TIMEOUT_US),
        };
        Ok((self.index, psk))
    }

    pub fn on_finalize(&mut self) -> Finalize {
        match std::mem::take(&mut self.attempt) {
            Attempt::Sent { psk, .. } => Finalize::Keep(psk),
            Attempt::Aborted => {
                self.attempt = Attempt::Aborted;
                Finalize::Ignore
            }
            Attempt::Idle => Finalize::OutOfSequence,
        }
    }

    /// The server aborted. Whatever was sent is not kept.
    pub fn on_server_abort(&mut self) {
        if matches!(self.attempt, Attempt::Sent { .. }) {
            self.attempt = Attempt::Idle;
        }
    }

    /// Whether the player's other pairing messages from the server are to be dropped.
    pub fn aborted(&self) -> bool {
        matches!(self.attempt, Attempt::Aborted)
    }

    /// The player aborts the attempt in progress.
    pub fn abort(&mut self) {
        self.attempt = Attempt::Aborted;
    }

    /// Whether an attempt waits for the server.
    pub fn in_progress(&self) -> bool {
        matches!(self.attempt, Attempt::Sent { .. })
    }

    /// When the attempt in progress times out.
    pub fn deadline(&self) -> Option<i64> {
        match self.attempt {
            Attempt::Sent { deadline, .. } => Some(deadline),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(server: &str, byte: u8) -> PairingRecord {
        PairingRecord {
            server_id: server.into(),
            long_term_psk: Psk::new([byte; 32]),
        }
    }

    #[test]
    fn base32_matches_rfc_4648() {
        // RFC 4648 section 10, without padding.
        for (input, output) in [
            ("", ""),
            ("f", "MY"),
            ("fo", "MZXQ"),
            ("foo", "MZXW6"),
            ("foob", "MZXW6YQ"),
            ("fooba", "MZXW6YTB"),
            ("foobar", "MZXW6YTBOI"),
        ] {
            assert_eq!(base32(input.as_bytes()), output);
        }
    }

    #[test]
    fn token_matches_the_reference_vector() {
        let public: [u8; 32] = std::array::from_fn(|i| i as u8);
        let pairing: [u8; 32] = std::array::from_fn(|i| 0xe0 + i as u8);
        let store = PairingStore::with_pairing_psk(Psk::new(pairing));
        assert_eq!(
            store.token(&Identity::from_keys([0; 32], public)),
            "SP:0AAAQEAYEAUDAOCAJBIFQYDIOB4IBCEQTCQKRMFYYDENBWHA5DYP6BYPC4PSOLZXH5DU6V97M5XXO74HR6LZ7J5PW674PT6X37T6757Y"
        );
    }

    #[test]
    fn keeps_eight_and_evicts_the_oldest() {
        let mut store = PairingStore::with_pairing_psk(Psk::new([1; 32]));
        for i in 0..MAX_PAIRINGS {
            assert_eq!(store.insert(record(&format!("s{i}"), i as u8), &[]), []);
        }
        assert_eq!(store.pairings().len(), MAX_PAIRINGS);
        // Pairing again with a known server replaces its record and moves it to the end.
        assert_eq!(store.insert(record("s0", 50), &["s0"]), []);
        assert_eq!(store.pairings().last(), Some(&record("s0", 50)));
        assert_eq!(store.pairings().len(), MAX_PAIRINGS);
        // A ninth server evicts the oldest, now s1.
        assert_eq!(store.insert(record("s9", 9), &[]), [record("s1", 1)]);
        assert_eq!(store.pairings().len(), MAX_PAIRINGS);
        assert!(store.remove("s9"));
        assert!(!store.remove("s9"));
        assert_eq!(store.pairings().len(), MAX_PAIRINGS - 1);
    }

    #[test]
    fn never_evicts_the_record_of_an_open_connection() {
        let mut store = PairingStore::with_pairing_psk(Psk::new([1; 32]));
        for i in 0..MAX_PAIRINGS {
            store.insert(record(&format!("s{i}"), i as u8), &[]);
        }
        // A provisional connection keyed with the oldest record (s0) is open while the admitted
        // connection, from a ninth server, completes a pairing: s1 goes instead.
        assert_eq!(
            store.insert(record("s9", 9), &["s0", "s9"]),
            [record("s1", 1)]
        );
        assert!(store.pairings().contains(&record("s0", 0)));
        // With every record in use, the store goes over capacity instead.
        let all: Vec<String> = store
            .pairings()
            .iter()
            .map(|r| r.server_id.clone())
            .collect();
        let all: Vec<&str> = all.iter().map(String::as_str).collect();
        assert_eq!(store.insert(record("s10", 10), &all), []);
        assert_eq!(store.pairings().len(), MAX_PAIRINGS + 1);
        // Once they close, the next pairing brings it back to capacity.
        assert_eq!(
            store.insert(record("s11", 11), &[]),
            [record("s0", 0), record("s2", 2)]
        );
        assert_eq!(store.pairings().len(), MAX_PAIRINGS);
    }

    #[test]
    fn candidates_always_hold_the_pairing_and_sentinel_keys() {
        let mut store = PairingStore::with_pairing_psk(Psk::new([1; 32]));
        store.insert(record("a", 2), &[]);
        let c = store.candidates();
        assert_eq!(c.len(), 3);
        assert_eq!(c[0].category, PskCategory::LongTerm);
        assert_eq!(c[0].server_id, Some("a"));
        assert_eq!(c[1].category, PskCategory::Pairing);
        assert_eq!(c[2].psk, &SENTINEL_PSK);
    }

    #[test]
    fn store_round_trips_through_toml_without_showing_keys_in_debug() {
        let mut store = PairingStore::generate().unwrap();
        store.insert(record("a", 2), &[]);
        store.insert(record("b", 3), &[]);
        assert!(store.set_last_playback_server("b"));
        assert!(!store.set_last_playback_server("b"));
        let text = toml::to_string(&store).unwrap();
        assert_eq!(toml::from_str::<PairingStore>(&text).unwrap(), store);
        let debug = format!("{store:?}");
        assert!(
            !debug.contains(&store.pairing_psk().to_base64url()),
            "{debug}"
        );
        assert!(
            !debug.contains(&Psk::new([2; 32]).to_base64url()),
            "{debug}"
        );
        // A store from an older file without pairings still reads.
        let bare = format!("pairing_psk = \"{}\"\n", store.pairing_psk().to_base64url());
        let read: PairingStore = toml::from_str(&bare).unwrap();
        assert!(read.pairings().is_empty());
        assert_eq!(read.last_playback_server(), None);
    }

    #[test]
    fn flow() {
        let pairing = Psk::new([1; 32]);
        let mut flow = PairingFlow::default();
        assert_eq!(flow.on_finalize(), Finalize::OutOfSequence);

        flow.count_pairing_activation();
        flow.on_activate();
        let (index, psk) = flow.begin(1000, &pairing).unwrap();
        assert_eq!(index, 1);
        assert_ne!(psk, pairing);
        assert!(flow.in_progress());
        assert_eq!(flow.deadline(), Some(1000 + ATTEMPT_TIMEOUT_US));
        assert_eq!(flow.on_finalize(), Finalize::Keep(psk));
        assert!(!flow.in_progress());
        assert_eq!(flow.on_finalize(), Finalize::OutOfSequence);

        // An activation in place of server/pair-finalize keeps nothing.
        flow.count_pairing_activation();
        flow.on_activate();
        assert_eq!(flow.begin(0, &pairing).unwrap().0, 2);
        flow.on_activate();
        assert_eq!(flow.on_finalize(), Finalize::OutOfSequence);

        // After the player aborts, a late server/pair-finalize is dropped until the next
        // activation.
        flow.begin(0, &pairing).unwrap();
        flow.abort();
        assert_eq!(flow.on_finalize(), Finalize::Ignore);
        assert_eq!(flow.on_finalize(), Finalize::Ignore);
        flow.on_activate();
        assert_eq!(flow.on_finalize(), Finalize::OutOfSequence);

        // The server's abort ends the attempt.
        flow.begin(0, &pairing).unwrap();
        flow.on_server_abort();
        assert_eq!(flow.on_finalize(), Finalize::OutOfSequence);

        // The count restarts with each handshake.
        flow.on_handshake();
        flow.count_pairing_activation();
        assert_eq!(flow.begin(0, &pairing).unwrap().0, 1);
    }
}
