//! The other satellites on the network (ADR 0021): a one-shot mDNS query for
//! `_esphomelib._tcp`, sent from an ordinary UDP port. A responder answers such
//! a legacy unicast query by unicast to that port (RFC 6762, section 6.7), so
//! pinnad never binds 5353: systemd-resolved stays the only mDNS socket.

use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

use anyhow::{Context, Result};
use pinna_api::Interface;
use simple_dns::rdata::RData;
use simple_dns::{CLASS, Name, Packet, PacketFlag, QCLASS, QTYPE, Question, TYPE};
use tokio::net::UdpSocket;
use tokio::time::Instant;

const SERVICE: &str = "_esphomelib._tcp.local";
const PROJECT: &str = "pinna.satellite";
const MDNS: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(224, 0, 0, 251)), 5353);
/// How long answers to the first query are collected.
const FIRST_WINDOW: Duration = Duration::from_millis(1000);
/// How long answers to each follow-up for missing records are collected.
const SECOND_WINDOW: Duration = Duration::from_millis(500);
/// Follow-ups after the first query. A responder may answer only the question
/// asked: PTR, then SRV and TXT, then the addresses of the host the SRV names.
const FOLLOW_UPS: usize = 3;
/// The satellite's own USB link, which every satellite has.
const USB_NET: (Ipv4Addr, u8) = (Ipv4Addr::new(10, 42, 0, 0), 24);

/// Another satellite, as its mDNS records describe it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    /// The instance label: the node name, which is also the hostname.
    pub name: String,
    pub friendly_name: String,
    pub board: String,
    pub mac: String,
    pub version: String,
    pub web_port: u16,
    pub url: String,
    pub ipv4: Vec<Ipv4Addr>,
    pub ipv6: Vec<Ipv6Addr>,
}

/// The records heard so far, merged across packets. Names are lowercase, in
/// presentation form.
#[derive(Debug, Default)]
pub struct Collected {
    /// Instance names (lowercase) with their first label as sent.
    instances: Vec<(String, String)>,
    srv: HashMap<String, (String, u16)>,
    txt: HashMap<String, HashMap<String, String>>,
    ipv4: HashMap<String, Vec<Ipv4Addr>>,
    ipv6: HashMap<String, Vec<Ipv6Addr>>,
}

fn key(name: &Name<'_>) -> String {
    name.to_string().to_ascii_lowercase()
}

fn push_new<T: PartialEq>(list: &mut Vec<T>, v: T) {
    if !list.contains(&v) {
        list.push(v);
    }
}

impl Collected {
    /// Merge one packet: answers and additional records alike. Anything that
    /// isn't a response, or doesn't parse, is ignored.
    pub fn ingest(&mut self, data: &[u8]) {
        let Ok(packet) = Packet::parse(data) else {
            return;
        };
        if !packet.has_flags(PacketFlag::RESPONSE) {
            return;
        }
        let service = key(&Name::new_unchecked(SERVICE));
        for rr in packet.answers.iter().chain(&packet.additional_records) {
            let owner = key(&rr.name);
            match &rr.rdata {
                RData::PTR(ptr) if owner == service => {
                    let instance = key(&ptr.0);
                    let label = ptr
                        .0
                        .get_labels()
                        .first()
                        .map(|l| String::from_utf8_lossy(l.as_ref()).into_owned())
                        .unwrap_or_default();
                    let suffix = format!(".{service}");
                    if instance.ends_with(&suffix)
                        && !label.is_empty()
                        && !self.instances.iter().any(|(k, _)| *k == instance)
                    {
                        self.instances.push((instance, label));
                    }
                }
                RData::SRV(srv) => {
                    self.srv.insert(owner, (key(&srv.target), srv.port));
                }
                RData::TXT(txt) => {
                    let mut map = HashMap::new();
                    for (k, v) in txt.iter_raw() {
                        let k = String::from_utf8_lossy(k).to_ascii_lowercase();
                        let v = String::from_utf8_lossy(v.unwrap_or_default()).into_owned();
                        map.entry(k).or_insert(v);
                    }
                    self.txt.insert(owner, map);
                }
                RData::A(a) => push_new(
                    self.ipv4.entry(owner).or_default(),
                    Ipv4Addr::from(a.address),
                ),
                RData::AAAA(a) => push_new(
                    self.ipv6.entry(owner).or_default(),
                    Ipv6Addr::from(a.address),
                ),
                _ => {}
            }
        }
    }

    /// What the follow-up asks for: SRV and TXT of an instance that lacks
    /// one, and the addresses of a host that has none.
    fn missing(&self) -> Vec<(String, TYPE)> {
        let mut asks: Vec<(String, TYPE)> = Vec::new();
        for (instance, _) in &self.instances {
            if !self.srv.contains_key(instance) {
                asks.push((instance.clone(), TYPE::SRV));
            }
            if !self.txt.contains_key(instance) {
                asks.push((instance.clone(), TYPE::TXT));
            }
            if let Some((host, _)) = self.srv.get(instance)
                && !self.ipv4.contains_key(host)
                && !self.ipv6.contains_key(host)
            {
                asks.push((host.clone(), TYPE::A));
                asks.push((host.clone(), TYPE::AAAA));
            }
        }
        asks.sort_by(|a, b| (&a.0, u16::from(a.1)).cmp(&(&b.0, u16::from(b.1))));
        asks.dedup();
        asks
    }

    /// The Pinna satellites heard, other than `own_mac`, by friendly name.
    pub fn finish(&self, own_mac: &str) -> Vec<Found> {
        let own = mac_digits(own_mac);
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        for (instance, label) in &self.instances {
            let Some(txt) = self.txt.get(instance) else {
                continue;
            };
            let get = |k: &str| txt.get(k).cloned().unwrap_or_default();
            if get("project_name") != PROJECT {
                continue;
            }
            let mac = get("mac");
            if !own.is_empty() && mac_digits(&mac) == own {
                continue;
            }
            if !seen.insert(instance.clone()) {
                continue;
            }
            let host = self.srv.get(instance);
            let web_port = txt
                .get("web_port")
                .and_then(|p| p.parse::<u16>().ok())
                .filter(|&p| p != 0)
                .unwrap_or(80);
            out.push(Found {
                name: label.clone(),
                friendly_name: get("friendly_name"),
                board: get("board"),
                mac,
                version: get("project_version"),
                web_port,
                url: get("url"),
                ipv4: addrs(host, &self.ipv4)
                    .into_iter()
                    .filter(|a| !in_usb_net(*a))
                    .collect(),
                ipv6: addrs(host, &self.ipv6)
                    .into_iter()
                    .filter(|a| !is_link_local_v6(a))
                    .collect(),
            });
        }
        out.sort_by(|a, b| {
            (a.friendly_name.to_lowercase(), &a.name)
                .cmp(&(b.friendly_name.to_lowercase(), &b.name))
        });
        out
    }
}

/// The addresses heard for an instance's host.
fn addrs<T: Clone>(host: Option<&(String, u16)>, heard: &HashMap<String, Vec<T>>) -> Vec<T> {
    host.and_then(|(h, _)| heard.get(h))
        .cloned()
        .unwrap_or_default()
}

/// A MAC's hex digits, lowercase, without separators.
fn mac_digits(mac: &str) -> String {
    mac.chars()
        .filter(char::is_ascii_hexdigit)
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

fn in_usb_net(a: Ipv4Addr) -> bool {
    let (net, bits) = USB_NET;
    let mask = u32::MAX << (32 - u32::from(bits));
    u32::from(a) & mask == u32::from(net) & mask
}

fn is_link_local_v6(a: &Ipv6Addr) -> bool {
    a.segments()[0] & 0xffc0 == 0xfe80
}

/// A standard query for these questions (class IN, no unicast-response bit:
/// the source port isn't 5353, so responders answer by unicast anyway).
fn query(id: u16, questions: &[(String, TYPE)]) -> Result<Vec<u8>> {
    let mut packet = Packet::new_query(id);
    for (name, ty) in questions {
        packet.questions.push(Question::new(
            Name::new_unchecked(name).into_owned(),
            QTYPE::TYPE(*ty),
            QCLASS::CLASS(CLASS::IN),
            false,
        ));
    }
    packet.build_bytes_vec().context("building the mDNS query")
}

/// Whether some interface besides the USB link has an address that isn't
/// link-local: the satellite is on a network others can be on.
pub fn on_network(interfaces: &[Interface]) -> bool {
    interfaces.iter().any(|i| {
        i.name != "usb0"
            && i.name != "lo"
            && i.addresses.iter().any(|a| {
                match a.split('/').next().and_then(|ip| ip.parse::<IpAddr>().ok()) {
                    Some(IpAddr::V4(v4)) => !v4.is_link_local(),
                    Some(IpAddr::V6(v6)) => !is_link_local_v6(&v6),
                    None => false,
                }
            })
    })
}

/// Ask the network for satellites: one query, then follow-ups for the records
/// the answers so far lack.
pub async fn discover(own_mac: &str) -> Result<Vec<Found>> {
    discover_at(MDNS, own_mac, FIRST_WINDOW, SECOND_WINDOW).await
}

async fn discover_at(
    target: SocketAddr,
    own_mac: &str,
    first: Duration,
    second: Duration,
) -> Result<Vec<Found>> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))
        .await
        .context("opening a UDP socket")?;
    socket
        .set_multicast_ttl_v4(255)
        .context("setting the multicast TTL")?;
    let mut collected = Collected::default();
    let ask = vec![(SERVICE.to_owned(), TYPE::PTR)];
    socket
        .send_to(&query(fastrand::u16(1..), &ask)?, target)
        .await
        .context("sending the mDNS query")?;
    listen(&socket, &mut collected, first).await;
    for _ in 0..FOLLOW_UPS {
        let missing = collected.missing();
        if missing.is_empty() {
            break;
        }
        socket
            .send_to(&query(fastrand::u16(1..), &missing)?, target)
            .await
            .context("sending the mDNS query")?;
        listen(&socket, &mut collected, second).await;
    }
    Ok(collected.finish(own_mac))
}

async fn listen(socket: &UdpSocket, collected: &mut Collected, window: Duration) {
    let end = Instant::now() + window;
    let mut buf = vec![0u8; 9000];
    while let Ok(Ok((n, _))) = tokio::time::timeout_at(end, socket.recv_from(&mut buf)).await {
        collected.ingest(&buf[..n]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use simple_dns::ResourceRecord;
    use simple_dns::rdata::{A, AAAA, PTR, SRV, TXT};

    const HOST: &str = "kitchen.local";
    const INSTANCE: &str = "kitchen._esphomelib._tcp.local";

    fn name(s: &str) -> Name<'_> {
        Name::new_unchecked(s)
    }

    fn rr<'a>(owner: &'a str, rdata: RData<'a>) -> ResourceRecord<'a> {
        ResourceRecord::new(name(owner), CLASS::IN, 120, rdata)
    }

    fn ptr(instance: &str) -> RData<'_> {
        RData::PTR(PTR(name(instance)))
    }

    fn srv(host: &str, port: u16) -> RData<'_> {
        RData::SRV(SRV {
            priority: 0,
            weight: 0,
            port,
            target: name(host),
        })
    }

    fn txt<'a>(pairs: &[(&str, &str)]) -> RData<'a> {
        let mut t = TXT::new();
        for (k, v) in pairs {
            t.add_attribute(k, Some(v.as_bytes())).unwrap();
        }
        RData::TXT(t)
    }

    fn pinna_txt() -> Vec<(&'static str, &'static str)> {
        vec![
            ("mac", "02:00:00:aa:bb:cc"),
            ("board", "Echo Dot (2nd Gen)"),
            ("friendly_name", "Kitchen"),
            ("project_name", "pinna.satellite"),
            ("project_version", "0.9.0"),
            ("web_port", "8080"),
            ("url", "https://kitchen.example.com/"),
        ]
    }

    fn a(ip: [u8; 4]) -> RData<'static> {
        RData::A(A {
            address: u32::from(Ipv4Addr::from(ip)),
        })
    }

    fn aaaa(ip: &str) -> RData<'static> {
        RData::AAAA(AAAA {
            address: ip.parse::<Ipv6Addr>().unwrap().into(),
        })
    }

    fn response(
        answers: Vec<ResourceRecord<'_>>,
        additional: Vec<ResourceRecord<'_>>,
        compressed: bool,
    ) -> Vec<u8> {
        let mut p = Packet::new_reply(7);
        p.answers = answers;
        p.additional_records = additional;
        if compressed {
            p.build_bytes_vec_compressed().unwrap()
        } else {
            p.build_bytes_vec().unwrap()
        }
    }

    #[test]
    fn merges_a_full_answer_with_compressed_names() {
        let bytes = response(
            vec![rr(SERVICE, ptr(INSTANCE))],
            vec![
                rr(INSTANCE, srv(HOST, 6053)),
                rr(INSTANCE, txt(&pinna_txt())),
                rr(HOST, a([192, 168, 1, 20])),
                rr(HOST, a([10, 42, 0, 1])),
                rr(HOST, aaaa("fe80::1")),
                rr(HOST, aaaa("2001:db8::20")),
                rr(HOST, aaaa("2001:db8::20")),
            ],
            true,
        );
        let mut c = Collected::default();
        c.ingest(&bytes);
        assert!(c.missing().is_empty());
        let found = c.finish("02:00:00:00:00:01");
        assert_eq!(
            found,
            [Found {
                name: "kitchen".into(),
                friendly_name: "Kitchen".into(),
                board: "Echo Dot (2nd Gen)".into(),
                mac: "02:00:00:aa:bb:cc".into(),
                version: "0.9.0".into(),
                web_port: 8080,
                url: "https://kitchen.example.com/".into(),
                ipv4: vec![Ipv4Addr::new(192, 168, 1, 20)],
                ipv6: vec!["2001:db8::20".parse().unwrap()],
            }]
        );
    }

    #[test]
    fn merges_records_split_across_packets() {
        let mut c = Collected::default();
        c.ingest(&response(vec![rr(SERVICE, ptr(INSTANCE))], vec![], false));
        let missing = c.missing();
        assert_eq!(
            missing,
            [
                (INSTANCE.to_owned(), TYPE::TXT),
                (INSTANCE.to_owned(), TYPE::SRV)
            ]
        );
        c.ingest(&response(
            vec![
                rr(INSTANCE, srv(HOST, 6053)),
                rr(INSTANCE, txt(&pinna_txt())),
            ],
            vec![],
            true,
        ));
        assert_eq!(
            c.missing(),
            [(HOST.to_owned(), TYPE::A), (HOST.to_owned(), TYPE::AAAA)]
        );
        c.ingest(&response(
            vec![rr(HOST, a([192, 168, 1, 20]))],
            vec![],
            false,
        ));
        assert!(c.missing().is_empty());
        let found = c.finish("");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].ipv4, [Ipv4Addr::new(192, 168, 1, 20)]);
    }

    #[test]
    fn keeps_only_other_pinna_satellites_sorted() {
        let mut c = Collected::default();
        let mut records = Vec::new();
        let mut other = |label: &str, friendly: &str, project: &str, mac: &str| {
            let inst = format!("{label}._esphomelib._tcp.local");
            let host = format!("{label}.local");
            let t = vec![
                ("mac", mac.to_owned()),
                ("friendly_name", friendly.to_owned()),
                ("project_name", project.to_owned()),
            ];
            records.push((inst, host, t));
        };
        other("zed", "Alpha", "pinna.satellite", "02:00:00:00:00:02");
        other("bee", "Alpha", "pinna.satellite", "02:00:00:00:00:03");
        other("self", "Me", "pinna.satellite", "02-00-00-AA-BB-CC");
        other("ha", "Other", "esphome.voice", "02:00:00:00:00:05");
        other("omega", "Beta", "pinna.satellite", "02:00:00:00:00:06");
        for (inst, host, t) in &records {
            let pairs: Vec<(&str, &str)> = t.iter().map(|(k, v)| (*k, v.as_str())).collect();
            let bytes = response(
                vec![rr(SERVICE, ptr(inst)), rr(inst, srv(host, 6053))],
                vec![rr(inst, txt(&pairs)), rr(host, a([192, 168, 1, 9]))],
                true,
            );
            c.ingest(&bytes);
        }
        let found = c.finish("02:00:00:aa:bb:cc");
        let order: Vec<_> = found.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(order, ["bee", "zed", "omega"]);
        // No web_port in the record: older firmware, port 80.
        assert!(found.iter().all(|f| f.web_port == 80 && f.url.is_empty()));
    }

    #[test]
    fn ignores_queries_and_garbage() {
        let mut c = Collected::default();
        c.ingest(&[1, 2, 3]);
        let mut q = Packet::new_query(1);
        q.answers.push(rr(SERVICE, ptr(INSTANCE)));
        c.ingest(&q.build_bytes_vec().unwrap());
        assert!(c.instances.is_empty());
    }

    #[test]
    fn usb_link_and_link_local_are_dropped() {
        assert!(in_usb_net(Ipv4Addr::new(10, 42, 0, 1)));
        assert!(in_usb_net(Ipv4Addr::new(10, 42, 0, 200)));
        assert!(!in_usb_net(Ipv4Addr::new(10, 42, 1, 1)));
        assert!(is_link_local_v6(&"febf::1".parse().unwrap()));
        assert!(!is_link_local_v6(&"fc00::1".parse().unwrap()));
    }

    #[test]
    fn network_means_an_address_beyond_usb() {
        let iface = |name: &str, addrs: &[&str]| Interface {
            name: name.into(),
            addresses: addrs.iter().map(|a| (*a).to_owned()).collect(),
            ..Default::default()
        };
        assert!(!on_network(&[]));
        assert!(!on_network(&[iface("usb0", &["10.42.0.1/24"])]));
        assert!(!on_network(&[
            iface("usb0", &["10.42.0.1/24"]),
            iface("wlan0", &["fe80::1/64"]),
            iface("eth0", &[]),
        ]));
        assert!(on_network(&[
            iface("usb0", &["10.42.0.1/24"]),
            iface("wlan0", &["192.168.1.20/24"]),
        ]));
    }

    #[tokio::test]
    async fn asks_again_for_what_the_first_answers_lack() {
        let server = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let target = server.local_addr().unwrap();
        let responder = tokio::spawn(async move {
            let mut buf = vec![0u8; 2000];
            // First query: the PTR record alone.
            let (n, from) = server.recv_from(&mut buf).await.unwrap();
            let q = Packet::parse(&buf[..n]).unwrap();
            assert_eq!(q.questions.len(), 1);
            let mut p = Packet::new_reply(q.id());
            p.answers.push(rr(SERVICE, ptr(INSTANCE)));
            server
                .send_to(&p.build_bytes_vec_compressed().unwrap(), from)
                .await
                .unwrap();
            // Follow-up: SRV and TXT, then (after the next one) the address.
            let (n, from) = server.recv_from(&mut buf).await.unwrap();
            let q = Packet::parse(&buf[..n]).unwrap();
            assert_eq!(q.questions.len(), 2);
            let mut p = Packet::new_reply(q.id());
            p.answers.push(rr(INSTANCE, srv(HOST, 6053)));
            p.answers.push(rr(INSTANCE, txt(&pinna_txt())));
            p.additional_records.push(rr(HOST, a([192, 168, 1, 20])));
            server
                .send_to(&p.build_bytes_vec().unwrap(), from)
                .await
                .unwrap();
        });
        let found = discover_at(
            target,
            "02:00:00:00:00:01",
            Duration::from_millis(150),
            Duration::from_millis(150),
        )
        .await
        .unwrap();
        responder.await.unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].friendly_name, "Kitchen");
        assert_eq!(found[0].ipv4, [Ipv4Addr::new(192, 168, 1, 20)]);
    }

    /// A responder that answers only the questions asked, with no additional
    /// records: the addresses come only after the SRV record names the host.
    #[tokio::test]
    async fn asks_until_the_addresses_come() {
        let server = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let target = server.local_addr().unwrap();
        let responder = tokio::spawn(async move {
            let mut buf = vec![0u8; 2000];
            let mut asked = Vec::new();
            // Answer until no more questions come.
            let wait = Duration::from_millis(500);
            while let Ok(Ok((n, from))) =
                tokio::time::timeout(wait, server.recv_from(&mut buf)).await
            {
                let q = Packet::parse(&buf[..n]).unwrap();
                let mut p = Packet::new_reply(q.id());
                for question in &q.questions {
                    let ty = match question.qtype {
                        QTYPE::TYPE(ty) => ty,
                        _ => continue,
                    };
                    asked.push(ty);
                    p.answers.push(match ty {
                        TYPE::PTR => rr(SERVICE, ptr(INSTANCE)),
                        TYPE::SRV => rr(INSTANCE, srv(HOST, 6053)),
                        TYPE::TXT => rr(INSTANCE, txt(&pinna_txt())),
                        TYPE::A => rr(HOST, a([192, 168, 1, 20])),
                        TYPE::AAAA => rr(HOST, aaaa("2001:db8::20")),
                        _ => continue,
                    });
                }
                server
                    .send_to(&p.build_bytes_vec_compressed().unwrap(), from)
                    .await
                    .unwrap();
            }
            asked
        });
        let found = discover_at(
            target,
            "",
            Duration::from_millis(150),
            Duration::from_millis(150),
        )
        .await
        .unwrap();
        let asked = responder.await.unwrap();
        assert_eq!(asked.len(), 5, "{asked:?}");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].url, "https://kitchen.example.com/");
        assert_eq!(found[0].ipv4, [Ipv4Addr::new(192, 168, 1, 20)]);
        assert_eq!(found[0].ipv6, ["2001:db8::20".parse::<Ipv6Addr>().unwrap()]);
    }

    #[tokio::test]
    async fn nobody_answering_is_an_empty_list() {
        let server = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let found = discover_at(
            server.local_addr().unwrap(),
            "",
            Duration::from_millis(50),
            Duration::from_millis(50),
        )
        .await
        .unwrap();
        assert!(found.is_empty());
    }
}
