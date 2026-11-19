//! The signed envelope that carries every call and answer between members
//! except `Join` (ADR 0026). A Connect handler never sees the bytes that
//! were sent, so the inner message travels as bytes, and the tag covers
//! those bytes, what they are for, the group id and the sender's MAC.

use buffa::Message;
use pinna_api::PeerEnvelope;

use super::persisted::Joined;
use super::seal;

/// The calls that travel in an envelope. Each one's purpose goes into the
/// tag, so an answer can't be sent back as a call, nor one call's body as
/// another's. `Deliver` and `Call` join them with messages and control.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rpc {
    GetGroup,
    PutGroup,
}

impl Rpc {
    /// The purpose of a call to it.
    pub fn call(self) -> &'static str {
        match self {
            Self::GetGroup => "GetGroup/call",
            Self::PutGroup => "PutGroup/call",
        }
    }

    /// The purpose of its answer.
    pub fn answer(self) -> &'static str {
        match self {
            Self::GetGroup => "GetGroup/answer",
            Self::PutGroup => "PutGroup/answer",
        }
    }
}

/// Why an envelope wasn't opened.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unopened {
    /// It names another group.
    Group,
    /// Its tag isn't the one the group's key makes.
    Tag,
    /// The tag is right, but the body isn't the message it should be.
    Body,
}

/// `body`, signed by this satellite (`own_mac`) with `group`'s key for
/// `purpose`.
pub fn seal<M: Message>(group: &Joined, own_mac: &str, purpose: &str, body: &M) -> PeerEnvelope {
    let body = body.encode_to_vec();
    let tag = seal::sign(group.key.bytes(), purpose, &group.id, own_mac, &body);
    PeerEnvelope {
        group_id: group.id.clone(),
        sender_mac: own_mac.to_owned(),
        body,
        tag: tag.to_vec(),
        ..Default::default()
    }
}

/// The parts of an envelope as it arrived.
#[derive(Clone, Copy, Debug)]
pub struct Parts<'a> {
    pub group_id: &'a str,
    pub sender_mac: &'a str,
    pub body: &'a [u8],
    pub tag: &'a [u8],
}

impl<'a> From<&'a PeerEnvelope> for Parts<'a> {
    fn from(e: &'a PeerEnvelope) -> Self {
        Self {
            group_id: &e.group_id,
            sender_mac: &e.sender_mac,
            body: &e.body,
            tag: &e.tag,
        }
    }
}

/// The message in an envelope of `group` for `purpose`. The tag is checked on
/// the bytes as they arrived, before anything is decoded.
pub fn open<M: Message>(group: &Joined, purpose: &str, parts: Parts<'_>) -> Result<M, Unopened> {
    if parts.group_id != group.id {
        return Err(Unopened::Group);
    }
    if !seal::verify(
        group.key.bytes(),
        purpose,
        parts.group_id,
        parts.sender_mac,
        parts.body,
        parts.tag,
    ) {
        return Err(Unopened::Tag);
    }
    M::decode_from_slice(parts.body).map_err(|_| Unopened::Body)
}

#[cfg(test)]
mod tests {
    use pinna_api::{PeerHello, PeerMemberList, PeerSender};

    use super::*;
    use crate::group::seal::Key;

    const OWN: &str = "02:00:00:00:00:01";

    fn group(key: u8) -> Joined {
        Joined {
            id: "00112233445566778899aabbccddeeff".into(),
            key: Key::new([key; 32]),
            members: Vec::new(),
        }
    }

    fn hello() -> PeerHello {
        PeerHello {
            sender: buffa::MessageField::some(PeerSender {
                web_port: 8080,
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn a_sealed_envelope_opens_for_its_purpose_and_group_only() {
        let sealed = seal(&group(1), OWN, Rpc::GetGroup.call(), &hello());
        assert_eq!(sealed.sender_mac, OWN);
        let opened: PeerHello = open(&group(1), Rpc::GetGroup.call(), (&sealed).into()).unwrap();
        assert_eq!(opened, hello());

        // Another key, another purpose, another group.
        assert_eq!(
            open::<PeerHello>(&group(2), Rpc::GetGroup.call(), (&sealed).into()),
            Err(Unopened::Tag)
        );
        assert_eq!(
            open::<PeerHello>(&group(1), Rpc::GetGroup.answer(), (&sealed).into()),
            Err(Unopened::Tag)
        );
        assert_eq!(
            open::<PeerHello>(&group(1), Rpc::PutGroup.call(), (&sealed).into()),
            Err(Unopened::Tag)
        );
        let other = Joined {
            id: "ffeeddccbbaa99887766554433221100".into(),
            ..group(1)
        };
        assert_eq!(
            open::<PeerHello>(&other, Rpc::GetGroup.call(), (&sealed).into()),
            Err(Unopened::Group)
        );
        // Signed by one MAC, claimed by another.
        let mut claimed = sealed.clone();
        claimed.sender_mac = "02:00:00:00:00:02".into();
        assert_eq!(
            open::<PeerHello>(&group(1), Rpc::GetGroup.call(), (&claimed).into()),
            Err(Unopened::Tag)
        );
    }

    #[test]
    fn a_signed_body_that_is_not_the_message_is_refused() {
        let mut sealed = seal(
            &group(1),
            OWN,
            Rpc::PutGroup.call(),
            &PeerMemberList::default(),
        );
        sealed.body = vec![0xff, 0xff, 0xff];
        let tag = seal::sign(
            group(1).key.bytes(),
            Rpc::PutGroup.call(),
            &sealed.group_id,
            OWN,
            &sealed.body,
        );
        sealed.tag = tag.to_vec();
        assert_eq!(
            open::<PeerMemberList>(&group(1), Rpc::PutGroup.call(), (&sealed).into()),
            Err(Unopened::Body)
        );
    }

    #[test]
    fn every_purpose_is_its_own() {
        let all = [Rpc::GetGroup, Rpc::PutGroup];
        let mut purposes: Vec<&str> = all.iter().flat_map(|r| [r.call(), r.answer()]).collect();
        purposes.sort_unstable();
        purposes.dedup();
        assert_eq!(purposes.len(), 2 * all.len());
    }
}
