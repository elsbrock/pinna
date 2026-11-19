//! Calls to another member's `PeerService` (ADR 0026), on its dashboard's
//! listener, by IP address.
//!
//! A member is called at the addresses the search heard (ADR 0021): the
//! first IPv4 address, then the first IPv6 address, each with its own
//! connect limit. The call-back that merges an unknown caller's list goes to
//! the address that call came from instead. Every answer but `Join`'s is
//! signed, and one whose tag doesn't verify counts as no answer: it never
//! makes a satellite forget the group.

use std::future::Future;
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use buffa::Message;
use connectrpc::client::{CallOptions, ClientConfig, HttpClient};
use connectrpc::{ConnectError, ErrorCode};
use pinna_api::{PeerEnvelope, PeerJoinRequest, PeerJoinResponse, PeerServiceClient};
use tokio::time::Instant;

use super::envelope::{self, Rpc};
use super::model::same_mac;
use super::persisted::Joined;
use crate::satellites::Found;

/// Where a member is called.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct To {
    /// Tried in order, each until the connect limit.
    addresses: Vec<SocketAddr>,
}

impl To {
    /// At the addresses the search heard: its first IPv4 address, then its
    /// first IPv6 address, on its dashboard's port.
    pub fn heard(found: &Found) -> Self {
        let v4 = found.ipv4.first().map(|ip| IpAddr::V4(*ip));
        let v6 = found.ipv6.first().map(|ip| IpAddr::V6(*ip));
        Self {
            addresses: v4
                .into_iter()
                .chain(v6)
                .map(|ip| SocketAddr::new(ip, found.web_port))
                .collect(),
        }
    }

    /// At one address: where a call came from, on the port it named.
    pub fn at(address: SocketAddr) -> Self {
        Self {
            addresses: vec![address],
        }
    }
}

/// Why a call brought no usable answer.
#[derive(Debug)]
pub enum Failed {
    /// Nothing answered in time, or nothing could be reached.
    Unreachable(ConnectError),
    /// An answer came, but its tag, group or sender is wrong. It counts as
    /// no answer.
    BadAnswer,
    /// The member refused, with an unsigned Connect error.
    Refused(ConnectError),
}

impl Failed {
    /// The refusal a member that is in no group, or in another one, sends.
    pub fn is_not_in_this_group(&self) -> bool {
        matches!(self, Self::Refused(e)
            if e.code == ErrorCode::FailedPrecondition
                && e.message.as_deref() == Some(super::peer::NOT_IN_THIS_GROUP))
    }
}

impl std::fmt::Display for Failed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unreachable(e) => write!(f, "no answer: {e}"),
            Self::BadAnswer => f.write_str("its answer isn't signed with the group's key"),
            Self::Refused(e) => write!(f, "refused: {e}"),
        }
    }
}

/// The client every call to another member goes through. Cheap to share:
/// its connections are pooled.
#[derive(Clone)]
pub struct Client {
    http: HttpClient,
}

impl Client {
    /// A client that gives each address `connect` to accept a connection.
    pub fn new(connect: Duration) -> Self {
        Self {
            http: HttpClient::builder()
                .tcp_connect_timeout(connect)
                .establishment_timeout(connect)
                .plaintext(),
        }
    }

    /// Add the satellite at `to` to a group. The only unsigned call.
    pub async fn join(
        &self,
        to: &To,
        request: PeerJoinRequest,
        deadline: Duration,
    ) -> Result<PeerJoinResponse, Failed> {
        self.reach(to, deadline, |client, options| {
            let request = request.clone();
            async move {
                let answer = client.join_with_options(request, options).await?;
                Ok(answer.into_view().to_owned_message())
            }
        })
        .await
    }

    /// Call `rpc` on the member `peer_mac` at `to` with `body`, signed with
    /// `group`'s key as this satellite (`own_mac`), and open its signed
    /// answer.
    #[expect(clippy::too_many_arguments, reason = "each is one part of the call")]
    pub async fn signed<Req: Message, Resp: Message>(
        &self,
        to: &To,
        rpc: Rpc,
        group: &Joined,
        own_mac: &str,
        peer_mac: &str,
        body: &Req,
        deadline: Duration,
    ) -> Result<Resp, Failed> {
        let call = envelope::seal(group, own_mac, rpc.call(), body);
        let answer = self
            .reach(to, deadline, |client, options| {
                let call = call.clone();
                async move {
                    let answer = match rpc {
                        Rpc::GetGroup => client.get_group_with_options(call, options).await,
                        Rpc::PutGroup => client.put_group_with_options(call, options).await,
                    }?;
                    Ok(answer.into_view().to_owned_message())
                }
            })
            .await?;
        open_answer(group, rpc, peer_mac, &answer)
    }

    /// Run `call` against each of `to`'s addresses in turn, until one is
    /// reached, all within `deadline`. Only a failure to connect moves on to
    /// the next address: an answer, even a refusal, is the member's. With no
    /// address to try, nothing answered.
    async fn reach<T, F, Fut>(&self, to: &To, deadline: Duration, call: F) -> Result<T, Failed>
    where
        F: Fn(PeerServiceClient<HttpClient>, CallOptions) -> Fut,
        Fut: Future<Output = Result<T, ConnectError>>,
    {
        let started = Instant::now();
        let mut last = None;
        for address in &to.addresses {
            let left = deadline.saturating_sub(started.elapsed());
            if left.is_zero() {
                break;
            }
            let uri = format!("http://{address}").parse().map_err(|e| {
                Failed::Unreachable(ConnectError::internal(format!("calling {address}: {e}")))
            })?;
            let client = PeerServiceClient::new(self.http.clone(), ClientConfig::new(uri));
            let options = CallOptions::default().with_timeout(left);
            match tokio::time::timeout(left, call(client, options)).await {
                Err(_) => {
                    return Err(Failed::Unreachable(ConnectError::deadline_exceeded(
                        format!("{address} didn't answer within {deadline:?}"),
                    )));
                }
                Ok(Err(e)) if !connected(&e) => last = Some(e),
                Ok(done) => return done.map_err(classify),
            }
        }
        Err(Failed::Unreachable(last.unwrap_or_else(|| {
            ConnectError::unavailable("The search heard no address for the satellite.")
        })))
    }
}

/// Whether the call got as far as the member: anything but a transport
/// failure (refused, no route, the connect limit), which the client reports
/// as `unavailable` with its cause attached. A member's own `unavailable`
/// arrives without one.
fn connected(e: &ConnectError) -> bool {
    !(e.code == ErrorCode::Unavailable && e.source_arc().is_some())
}

/// A failed call as the member's refusal, or as no answer at all.
fn classify(e: ConnectError) -> Failed {
    if connected(&e) && e.code != ErrorCode::DeadlineExceeded {
        Failed::Refused(e)
    } else {
        Failed::Unreachable(e)
    }
}

/// The member's signed answer, opened. It must be from the member called,
/// in this group, for this call.
fn open_answer<Resp: Message>(
    group: &Joined,
    rpc: Rpc,
    peer_mac: &str,
    answer: &PeerEnvelope,
) -> Result<Resp, Failed> {
    if !same_mac(&answer.sender_mac, peer_mac) {
        return Err(Failed::BadAnswer);
    }
    envelope::open(group, rpc.answer(), answer.into()).map_err(|_| Failed::BadAnswer)
}

#[cfg(test)]
mod tests {
    use pinna_api::PeerHello;

    use super::*;
    use crate::group::seal::Key;

    const PEER: &str = "02:00:00:00:00:02";

    fn group(key: u8) -> Joined {
        Joined {
            id: "00112233445566778899aabbccddeeff".into(),
            key: Key::new([key; 32]),
            members: Vec::new(),
        }
    }

    fn opened(answer: &PeerEnvelope) -> Result<PeerHello, Failed> {
        open_answer(&group(1), Rpc::PutGroup, PEER, answer)
    }

    #[test]
    fn only_the_members_signed_answer_to_this_call_is_taken() {
        let body = PeerHello::default();
        let answer = envelope::seal(&group(1), PEER, Rpc::PutGroup.answer(), &body);
        assert_eq!(opened(&answer).unwrap(), body);
        // Its MAC in another form is still its MAC.
        let mut upper = answer.clone();
        upper.sender_mac = PEER.to_uppercase();
        let upper = envelope::seal(&group(1), &upper.sender_mac, Rpc::PutGroup.answer(), &body);
        assert!(opened(&upper).is_ok());

        // A wrong tag, an answer to another call, a call instead of an
        // answer, or another satellite's answer: none is an answer.
        for wrong in [
            envelope::seal(&group(2), PEER, Rpc::PutGroup.answer(), &body),
            envelope::seal(&group(1), PEER, Rpc::GetGroup.answer(), &body),
            envelope::seal(&group(1), PEER, Rpc::PutGroup.call(), &body),
            envelope::seal(
                &group(1),
                "02:00:00:00:00:03",
                Rpc::PutGroup.answer(),
                &body,
            ),
        ] {
            assert!(
                matches!(opened(&wrong), Err(Failed::BadAnswer)),
                "{wrong:?}"
            );
        }
    }

    #[test]
    fn a_member_is_called_at_its_first_ipv4_then_its_first_ipv6_address() {
        let found = Found {
            name: "kitchen".into(),
            host: "kitchen".into(),
            friendly_name: "Kitchen".into(),
            board: String::new(),
            mac: PEER.into(),
            version: String::new(),
            web_port: 8080,
            url: String::new(),
            ipv4: vec![
                "192.168.1.20".parse().unwrap(),
                "192.168.1.21".parse().unwrap(),
            ],
            ipv6: vec!["fd00::20".parse().unwrap(), "fd00::21".parse().unwrap()],
            group: None,
            missed: 0,
        };
        assert_eq!(
            To::heard(&found).addresses,
            [
                "192.168.1.20:8080".parse::<SocketAddr>().unwrap(),
                "[fd00::20]:8080".parse().unwrap()
            ]
        );
    }

    #[tokio::test]
    async fn an_address_that_refuses_the_connection_falls_back_to_the_next() {
        // Nothing listens on the first port; the second answers with a
        // refusal, which is the member's own and ends the call.
        let closed = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let closed_at = closed.local_addr().unwrap();
        drop(closed);
        let server = connectrpc::Server::bind("127.0.0.1:0").await.unwrap();
        let open_at = server.local_addr().unwrap();
        tokio::spawn(server.serve(connectrpc::Router::new()));
        let client = Client::new(Duration::from_millis(300));
        let to = To {
            addresses: vec![closed_at, open_at],
        };
        let failed = client
            .join(&to, PeerJoinRequest::default(), Duration::from_secs(3))
            .await
            .unwrap_err();
        assert!(
            matches!(&failed, Failed::Refused(e) if e.code == ErrorCode::Unimplemented),
            "{failed}"
        );
        // Only the closed one, or no address at all: nothing answers.
        for addresses in [vec![closed_at], Vec::new()] {
            let to = To { addresses };
            let failed = client
                .join(&to, PeerJoinRequest::default(), Duration::from_secs(3))
                .await
                .unwrap_err();
            assert!(matches!(failed, Failed::Unreachable(_)), "{failed}");
        }
    }
}
