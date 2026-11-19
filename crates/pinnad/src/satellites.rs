//! The other satellites on the network (ADR 0021), looked for in the
//! background once a minute: a one-shot mDNS query for `_esphomelib._tcp`,
//! sent from an ordinary UDP port. A responder answers such a legacy unicast
//! query by unicast to that port (RFC 6762, section 6.7), so pinnad never binds
//! 5353: systemd-resolved stays the only mDNS socket. A satellite is listed
//! while its dashboard accepts a connection at its host name's addresses.

use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV6};
use std::time::Duration;

use anyhow::{Context, Result};
use pinna_api::Interface;
use simple_dns::rdata::RData;
use simple_dns::{CLASS, Name, Packet, PacketFlag, QCLASS, QTYPE, Question, TYPE};
use tokio::net::{TcpStream, UdpSocket};
use tokio::sync::watch;
use tokio::time::Instant;

use crate::network;

const SERVICE: &str = "_esphomelib._tcp.local";
const PROJECT: &str = "pinna.satellite";
const MDNS: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(224, 0, 0, 251)), 5353);
const MDNS6: Ipv6Addr = Ipv6Addr::new(0xff02, 0, 0, 0, 0, 0, 0, 0xfb);
/// How long answers to the first query are collected.
const FIRST_WINDOW: Duration = Duration::from_millis(1000);
/// How long answers to each follow-up for missing records are collected.
const SECOND_WINDOW: Duration = Duration::from_millis(500);
/// Follow-ups after the first query. A responder may answer only the question
/// asked: PTR, then SRV and TXT, then the addresses of the host the SRV names.
const FOLLOW_UPS: usize = 3;
/// How often the background search runs.
const EVERY: Duration = Duration::from_secs(60);
/// How often it checks for a network while the satellite has none, which
/// sends nothing: pinnad starts before Wi-Fi is up, and the list shouldn't say
/// "not on a network" for a minute after boot.
const OFF_NETWORK_EVERY: Duration = Duration::from_secs(10);
/// How long a satellite's dashboard has to accept a connection.
const CONNECT: Duration = Duration::from_secs(3);
/// Searches a satellite may miss in a row before it leaves the list: one lost
/// packet shouldn't drop it.
const MISSES: u8 = 2;
/// The satellite's own USB link, which every satellite has.
const USB_NET: (Ipv4Addr, u8) = (Ipv4Addr::new(10, 42, 0, 0), 24);

/// Another satellite, as its mDNS records describe it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    /// The instance label: the node name, unless systemd-resolved renamed the
    /// instance after a conflict (ADR 0024).
    pub name: String,
    /// The host its SRV record names, without `.local`. Usually `name`, but
    /// systemd-resolved may rename either after a conflict (ADR 0024), and
    /// only this one resolves.
    pub host: String,
    pub friendly_name: String,
    pub board: String,
    pub mac: String,
    pub version: String,
    pub web_port: u16,
    pub url: String,
    pub ipv4: Vec<Ipv4Addr>,
    pub ipv6: Vec<Ipv6Addr>,
    /// The satellite group its record names (ADR 0026): `None` without the
    /// key (its *Satellite group* switch is off, or its Pinna is older),
    /// `Some("")` in no group, otherwise the group's id.
    pub group: Option<String>,
    /// Searches it has missed in a row since it was last heard and its
    /// dashboard answered: 0 when the latest search heard it. Until it has
    /// missed `MISSES`, it stays listed as the last search that heard it.
    pub missed: u8,
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
            let host_label = host
                .and_then(|(h, _)| h.strip_suffix(".local"))
                .unwrap_or_default()
                .to_owned();
            let web_port = txt
                .get("web_port")
                .and_then(|p| p.parse::<u16>().ok())
                .filter(|&p| p != 0)
                .unwrap_or(80);
            out.push(Found {
                name: label.clone(),
                host: host_label,
                friendly_name: get("friendly_name"),
                board: get("board"),
                mac,
                version: get("project_version"),
                web_port,
                url: get("url"),
                // Not through `get`: an absent key and an empty value differ.
                // An empty value may come as the bare key, which `ingest`
                // stores as "".
                group: txt.get("group").map(|g| g.to_ascii_lowercase()),
                missed: 0,
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
        let mut out = one_per_mac(out);
        by_name(&mut out);
        out
    }
}

/// One entry per satellite: a reload of systemd-resolved 257 can leave an old
/// instance of the same satellite published (ADR 0024). Of those, the one
/// whose host has addresses, then the one named like its host.
fn one_per_mac(found: Vec<Found>) -> Vec<Found> {
    let rank = |f: &Found| {
        (
            f.ipv4.is_empty() && f.ipv6.is_empty(),
            !f.name.eq_ignore_ascii_case(&f.host),
        )
    };
    let mut out: Vec<Found> = Vec::new();
    for f in found {
        let mac = mac_digits(&f.mac);
        match out
            .iter_mut()
            .find(|o| !mac.is_empty() && mac_digits(&o.mac) == mac)
        {
            Some(o) if rank(&f) < rank(o) => *o = f,
            Some(_) => {}
            None => out.push(f),
        }
    }
    out
}

fn by_name(list: &mut [Found]) {
    list.sort_by(|a, b| {
        (a.friendly_name.to_lowercase(), &a.name).cmp(&(b.friendly_name.to_lowercase(), &b.name))
    });
}

/// What the background search knows.
#[derive(Debug, Clone, Default)]
pub struct Seen {
    /// A search has finished since pinnad started.
    pub searched: bool,
    /// This satellite has an address beyond the USB link.
    pub on_network: bool,
    /// The other satellites whose dashboard answered, by friendly name.
    pub satellites: Vec<Found>,
    /// Why the last search failed.
    pub error: Option<String>,
    /// How many searches have run, failed ones included. None runs while the
    /// satellite is off the network. A reader of the watch counts searches
    /// in a row with it (ADR 0026), even when it missed a value in between.
    pub search: u64,
}

impl Seen {
    /// The satellite with this MAC, in any separator or case. An empty MAC
    /// matches none.
    pub fn by_mac(&self, mac: &str) -> Option<&Found> {
        let mac = mac_digits(mac);
        if mac.is_empty() {
            return None;
        }
        self.satellites.iter().find(|f| mac_digits(&f.mac) == mac)
    }
}

/// Look for the other satellites now and then every minute, and publish what
/// was found. Runs for as long as pinnad does.
pub async fn run(own_mac: String, tx: watch::Sender<Seen>) {
    let mut kept = Vec::new();
    loop {
        let last = tx.borrow().search;
        let seen = after(&mut kept, last, look(&own_mac).await);
        // Once per new failure, not once a minute.
        if let Some(why) = &seen.error
            && tx.borrow().error.as_ref() != Some(why)
        {
            tracing::warn!("looking for other satellites: {why}");
        }
        let wait = if seen.on_network {
            EVERY
        } else {
            OFF_NETWORK_EVERY
        };
        tx.send_replace(seen);
        tokio::time::sleep(wait).await;
    }
}

/// What is known after one more search, of which `last` came before.
fn after(kept: &mut Vec<(Found, u8)>, last: u64, looked: Result<Option<Vec<Found>>>) -> Seen {
    match looked {
        // Nothing was asked, so it isn't a search.
        Ok(None) => {
            kept.clear();
            Seen {
                searched: true,
                search: last,
                ..Seen::default()
            }
        }
        Ok(Some(found)) => Seen {
            searched: true,
            on_network: true,
            satellites: keep(kept, found),
            error: None,
            search: last + 1,
        },
        // A failed search is a miss for every satellite: the list doesn't
        // freeze while searches fail, and the error shows once it is empty.
        Err(e) => Seen {
            searched: true,
            on_network: true,
            satellites: keep(kept, Vec::new()),
            error: Some(format!("{e:#}")),
            search: last + 1,
        },
    }
}

/// One search, or `None` when this satellite isn't on a network, so nothing
/// was asked. Satellites whose dashboard doesn't answer are left out.
async fn look(own_mac: &str) -> Result<Option<Vec<Found>>> {
    let interfaces = network::interfaces()
        .await
        .context("reading network interfaces")?;
    if !on_network(&interfaces) {
        return Ok(None);
    }
    let found = discover(own_mac, &v6_links(&interfaces)).await?;
    let up = futures::future::join_all(found.iter().map(answers)).await;
    Ok(Some(
        found
            .into_iter()
            .zip(up)
            .filter_map(|(f, up)| up.then_some(f))
            .collect(),
    ))
}

/// Whether a satellite's dashboard accepts a connection at its host name's
/// addresses (the first IPv4 and IPv6 address heard): its web listener is on
/// and pinnad runs there. Its dashboard address isn't tried: it may lead
/// through a proxy, a login or a VPN that this satellite can't pass.
async fn answers(f: &Found) -> bool {
    let ips = (f.ipv4.first().copied().map(IpAddr::V4))
        .into_iter()
        .chain(f.ipv6.first().copied().map(IpAddr::V6));
    for ip in ips {
        let connect = TcpStream::connect((ip, f.web_port));
        if let Ok(Ok(_)) = tokio::time::timeout(CONNECT, connect).await {
            return true;
        }
    }
    false
}

/// Merge one search into what the earlier ones found: a satellite found now is
/// listed as it is now; one not found stays until it has missed `MISSES`
/// searches in a row.
fn keep(kept: &mut Vec<(Found, u8)>, found: Vec<Found>) -> Vec<Found> {
    for (_, missed) in kept.iter_mut() {
        *missed += 1;
    }
    for f in found {
        match kept.iter_mut().find(|(k, _)| same(k, &f)) {
            Some(entry) => *entry = (f, 0),
            None => kept.push((f, 0)),
        }
    }
    kept.retain(|(_, missed)| *missed < MISSES);
    listed(kept)
}

/// The same satellite: by MAC, or by node name when a record has no MAC.
fn same(a: &Found, b: &Found) -> bool {
    let (ma, mb) = (mac_digits(&a.mac), mac_digits(&b.mac));
    if ma.is_empty() || mb.is_empty() {
        a.name.eq_ignore_ascii_case(&b.name)
    } else {
        ma == mb
    }
}

fn listed(kept: &[(Found, u8)]) -> Vec<Found> {
    let mut out: Vec<Found> = kept
        .iter()
        .map(|(f, missed)| Found {
            missed: *missed,
            ..f.clone()
        })
        .collect();
    by_name(&mut out);
    out
}

/// The addresses heard for an instance's host.
fn addrs<T: Clone>(host: Option<&(String, u16)>, heard: &HashMap<String, Vec<T>>) -> Vec<T> {
    host.and_then(|(h, _)| heard.get(h))
        .cloned()
        .unwrap_or_default()
}

/// A MAC's hex digits, lowercase, without separators.
pub(crate) fn mac_digits(mac: &str) -> String {
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

/// The interfaces to ask over IPv6, by index: those besides the USB link
/// that are up and have an address. ff02::fb is link-local, so each gets its
/// own query, and a link-local IPv6 address (which the interface list leaves
/// out) is enough; on a link without IPv6 that send just fails.
fn v6_links(interfaces: &[Interface]) -> Vec<u32> {
    interfaces
        .iter()
        .filter(|i| i.name != "usb0" && i.name != "lo" && i.up && !i.addresses.is_empty())
        .filter_map(|i| {
            std::fs::read_to_string(format!("/sys/class/net/{}/ifindex", i.name))
                .ok()?
                .trim()
                .parse()
                .ok()
        })
        .collect()
}

/// Ask the network for satellites over IPv4 and, on each of `v6_links`, over
/// IPv6: one query, then follow-ups for the records the answers so far lack.
/// What either family brings is merged: a responder may answer over one only
/// (ADR 0021).
async fn discover(own_mac: &str, v6_links: &[u32]) -> Result<Vec<Found>> {
    let mut targets = vec![MDNS];
    targets.extend(
        v6_links
            .iter()
            .map(|&i| SocketAddr::V6(SocketAddrV6::new(MDNS6, 5353, 0, i))),
    );
    discover_at(&targets, own_mac, FIRST_WINDOW, SECOND_WINDOW).await
}

async fn discover_at(
    targets: &[SocketAddr],
    own_mac: &str,
    first: Duration,
    second: Duration,
) -> Result<Vec<Found>> {
    // Either family alone is enough: `send` fails when nothing went out.
    let v4 = if targets.iter().any(SocketAddr::is_ipv4) {
        match UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).await {
            Ok(s) => {
                s.set_multicast_ttl_v4(255)
                    .context("setting the multicast TTL")?;
                Some(s)
            }
            Err(e) => {
                tracing::debug!("satellites: no IPv4 socket: {e}");
                None
            }
        }
    } else {
        None
    };
    let v6 = if targets.iter().any(SocketAddr::is_ipv6) {
        UdpSocket::bind((Ipv6Addr::UNSPECIFIED, 0))
            .await
            .inspect_err(|e| tracing::debug!("satellites: no IPv6 socket: {e}"))
            .ok()
    } else {
        None
    };
    let sockets = (v4.as_ref(), v6.as_ref());
    let mut collected = Collected::default();
    let ask = vec![(SERVICE.to_owned(), TYPE::PTR)];
    send(sockets, targets, &query(fastrand::u16(1..), &ask)?).await?;
    listen(sockets, &mut collected, first).await;
    for _ in 0..FOLLOW_UPS {
        let missing = collected.missing();
        if missing.is_empty() {
            break;
        }
        send(sockets, targets, &query(fastrand::u16(1..), &missing)?).await?;
        listen(sockets, &mut collected, second).await;
    }
    Ok(collected.finish(own_mac))
}

type Sockets<'a> = (Option<&'a UdpSocket>, Option<&'a UdpSocket>);

/// Send to every target from the socket of its family. Fails only when no
/// send went out: a link without IPv6 shouldn't stop the IPv4 query.
async fn send((v4, v6): Sockets<'_>, targets: &[SocketAddr], packet: &[u8]) -> Result<()> {
    let mut last = None;
    let mut sent = false;
    for &t in targets {
        let Some(s) = (if t.is_ipv4() { v4 } else { v6 }) else {
            continue;
        };
        match s.send_to(packet, t).await {
            Ok(_) => sent = true,
            Err(e) => last = Some(e),
        }
    }
    match (sent, last) {
        (false, Some(e)) => Err(e).context("sending the mDNS query"),
        (false, None) => anyhow::bail!("sending the mDNS query: no socket"),
        _ => Ok(()),
    }
}

/// Collect answers on both sockets until `window` ends.
async fn listen((v4, v6): Sockets<'_>, collected: &mut Collected, window: Duration) {
    async fn recv(s: Option<&UdpSocket>, buf: &mut [u8]) -> std::io::Result<usize> {
        match s {
            Some(s) => s.recv_from(buf).await.map(|(n, _)| n),
            None => std::future::pending().await,
        }
    }
    let end = Instant::now() + window;
    let (mut b4, mut b6) = (vec![0u8; 9000], vec![0u8; 9000]);
    loop {
        let got = tokio::time::timeout_at(end, async {
            tokio::select! {
                r = recv(v4, &mut b4) => r.map(|n| (n, true)),
                r = recv(v6, &mut b6) => r.map(|n| (n, false)),
            }
        })
        .await;
        match got {
            Ok(Ok((n, true))) => collected.ingest(&b4[..n]),
            Ok(Ok((n, false))) => collected.ingest(&b6[..n]),
            // An ICMP error from an earlier send; the window still bounds it.
            Ok(Err(_)) => {}
            Err(_) => break,
        }
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
                host: "kitchen".into(),
                friendly_name: "Kitchen".into(),
                board: "Echo Dot (2nd Gen)".into(),
                mac: "02:00:00:aa:bb:cc".into(),
                version: "0.9.0".into(),
                web_port: 8080,
                url: "https://kitchen.example.com/".into(),
                ipv4: vec![Ipv4Addr::new(192, 168, 1, 20)],
                ipv6: vec!["2001:db8::20".parse().unwrap()],
                group: None,
                missed: 0,
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
            &[target],
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
            &[target],
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

    fn found(name: &str, mac: &str) -> Found {
        Found {
            name: name.into(),
            host: name.into(),
            friendly_name: name.into(),
            board: String::new(),
            mac: mac.into(),
            version: String::new(),
            web_port: 80,
            url: String::new(),
            ipv4: vec![],
            ipv6: vec![],
            group: None,
            missed: 0,
        }
    }

    #[test]
    fn a_satellite_leaves_after_two_missed_searches() {
        let mut kept = Vec::new();
        let (a, b) = (found("a", "02:00:00:00:00:0a"), found("b", ""));
        let names = |l: Vec<Found>| l.into_iter().map(|f| f.name).collect::<Vec<_>>();
        assert_eq!(
            names(keep(&mut kept, vec![b.clone(), a.clone()])),
            ["a", "b"]
        );
        assert_eq!(names(keep(&mut kept, vec![a.clone()])), ["a", "b"]);
        assert_eq!(names(keep(&mut kept, vec![a.clone()])), ["a"]);
        assert_eq!(names(keep(&mut kept, vec![b.clone()])), ["a", "b"]);
        // Found again, it starts over: one more miss keeps it.
        assert_eq!(names(keep(&mut kept, vec![b])), ["b"]);
    }

    /// The record's `group` key (ADR 0026): an empty value and no value both
    /// mean the switch is on, in no group; resolved may publish an empty
    /// value as the bare key.
    #[test]
    fn the_record_tells_a_switch_that_is_off_from_no_group() {
        let group = |extra: Option<(&str, Option<&str>)>| {
            let mut t = TXT::new();
            for (k, v) in pinna_txt() {
                t.add_attribute(k, Some(v.as_bytes())).unwrap();
            }
            if let Some((k, v)) = extra {
                t.add_attribute(k, v.map(str::as_bytes)).unwrap();
            }
            let mut c = Collected::default();
            c.ingest(&response(
                vec![rr(SERVICE, ptr(INSTANCE)), rr(INSTANCE, srv(HOST, 6053))],
                vec![rr(INSTANCE, RData::TXT(t)), rr(HOST, a([192, 168, 1, 20]))],
                true,
            ));
            c.finish("").remove(0).group
        };
        assert_eq!(group(None), None, "switch off, or an older Pinna");
        assert_eq!(group(Some(("group", Some("")))), Some(String::new()));
        assert_eq!(group(Some(("group", None))), Some(String::new()));
        assert_eq!(
            group(Some(("group", Some("0123456789ABCDEF0123456789abcdef")))),
            Some("0123456789abcdef0123456789abcdef".into())
        );
    }

    #[test]
    fn a_listed_satellite_says_how_many_searches_it_missed() {
        let mut kept = Vec::new();
        let (a, b) = (found("a", "02:00:00:00:00:0a"), found("b", ""));
        let missed = |l: Vec<Found>| l.into_iter().map(|f| f.missed).collect::<Vec<_>>();
        assert_eq!(missed(keep(&mut kept, vec![a.clone(), b.clone()])), [0, 0]);
        assert_eq!(missed(keep(&mut kept, vec![a.clone()])), [0, 1]);
        assert_eq!(missed(keep(&mut kept, vec![b.clone()])), [1, 0]);
        assert_eq!(missed(keep(&mut kept, vec![b])), [0]);
    }

    #[test]
    fn every_search_counts_but_none_runs_off_the_network() {
        let mut kept = Vec::new();
        let a = found("a", "02:00:00:00:00:0a");
        let seen = after(&mut kept, 0, Ok(Some(vec![a.clone()])));
        assert_eq!((seen.search, seen.satellites[0].missed), (1, 0));
        let seen = after(&mut kept, seen.search, Err(anyhow::anyhow!("no socket")));
        assert_eq!(seen.search, 2);
        assert_eq!(seen.error.as_deref(), Some("no socket"));
        assert_eq!(seen.satellites[0].missed, 1, "a failed search is a miss");
        let seen = after(&mut kept, seen.search, Ok(None));
        assert_eq!(seen.search, 2, "nothing was asked");
        assert!(!seen.on_network && seen.satellites.is_empty());
        let seen = after(&mut kept, seen.search, Ok(Some(vec![a])));
        assert_eq!((seen.search, seen.satellites[0].missed), (3, 0));
    }

    #[test]
    fn a_satellite_is_found_by_its_mac_in_any_form() {
        let seen = Seen {
            satellites: vec![found("a", "02:00:00:00:00:0A"), found("b", "")],
            ..Seen::default()
        };
        let name = |mac| seen.by_mac(mac).map(|f| f.name.as_str());
        assert_eq!(name("02-00-00-00-00-0a"), Some("a"));
        assert_eq!(name("02:00:00:00:00:0b"), None);
        assert_eq!(name(""), None, "an empty MAC matches none");
        assert_eq!(name("::"), None);
    }

    #[test]
    fn a_satellite_found_again_is_listed_as_it_is_now() {
        let mut kept = Vec::new();
        keep(&mut kept, vec![found("kitchen", "02:00:00:00:00:0a")]);
        let mut renamed = found("hall", "02-00-00-00-00-0A");
        renamed.url = "https://hall.example.com/".into();
        let list = keep(&mut kept, vec![renamed.clone()]);
        assert_eq!(list, [renamed]);
    }

    #[tokio::test]
    async fn a_dashboard_answers_when_its_port_accepts() {
        let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let mut sat = found("kitchen", "");
        sat.web_port = listener.local_addr().unwrap().port();
        sat.ipv4 = vec![Ipv4Addr::LOCALHOST];
        assert!(answers(&sat).await);
        drop(listener);
        assert!(!answers(&sat).await, "nothing listens on the port");
        sat.ipv4.clear();
        assert!(!answers(&sat).await, "no address heard");
    }

    /// After a conflict, systemd-resolved 257 may publish the instance and
    /// the host under different names; only the host's `.local` name resolves.
    #[test]
    fn the_local_name_comes_from_the_srv_record() {
        let inst = "echo10._esphomelib._tcp.local";
        let mut c = Collected::default();
        c.ingest(&response(
            vec![rr(SERVICE, ptr(inst)), rr(inst, srv("echo8.local", 6053))],
            vec![
                rr(inst, txt(&pinna_txt())),
                rr("echo8.local", a([192, 168, 1, 20])),
            ],
            true,
        ));
        let found = c.finish("");
        assert_eq!(
            (found[0].name.as_str(), found[0].host.as_str()),
            ("echo10", "echo8")
        );
    }

    /// A reload of systemd-resolved 257 leaves the old instance published
    /// next to the new one, under the same MAC.
    #[test]
    fn a_stale_instance_of_the_same_satellite_is_left_out() {
        let mut c = Collected::default();
        let (stale, now) = (
            "libreecho._esphomelib._tcp.local",
            "echo2._esphomelib._tcp.local",
        );
        c.ingest(&response(
            vec![
                rr(SERVICE, ptr(stale)),
                rr(SERVICE, ptr(now)),
                rr(stale, srv("echo2.local", 6053)),
                rr(now, srv("echo2.local", 6053)),
            ],
            vec![
                rr(stale, txt(&pinna_txt())),
                rr(now, txt(&pinna_txt())),
                rr("echo2.local", a([192, 168, 1, 20])),
            ],
            true,
        ));
        let names: Vec<_> = c.finish("").into_iter().map(|f| f.name).collect();
        assert_eq!(
            names,
            ["echo2"],
            "both have addresses: the one named like its host"
        );

        let mut c = Collected::default();
        c.ingest(&response(
            vec![
                rr(SERVICE, ptr(now)),
                rr(SERVICE, ptr(stale)),
                rr(now, srv("echo8.local", 6053)),
                rr(stale, srv("gone.local", 6053)),
            ],
            vec![
                rr(now, txt(&pinna_txt())),
                rr(stale, txt(&pinna_txt())),
                rr("echo8.local", a([192, 168, 1, 20])),
            ],
            true,
        ));
        let names: Vec<_> = c.finish("").into_iter().map(|f| f.name).collect();
        assert_eq!(names, ["echo2"], "the one whose host has addresses");
    }

    /// A responder that answers every question asked, alone.
    fn answer_what_is_asked(server: UdpSocket) -> tokio::task::JoinHandle<usize> {
        tokio::spawn(async move {
            let mut buf = vec![0u8; 2000];
            let mut queries = 0;
            let wait = Duration::from_millis(500);
            while let Ok(Ok((n, from))) =
                tokio::time::timeout(wait, server.recv_from(&mut buf)).await
            {
                queries += 1;
                let q = Packet::parse(&buf[..n]).unwrap();
                let mut p = Packet::new_reply(q.id());
                for question in &q.questions {
                    let QTYPE::TYPE(ty) = question.qtype else {
                        continue;
                    };
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
            queries
        })
    }

    /// The case seen on an Echo (2nd gen): its resolved answered over IPv6
    /// but not over IPv4.
    #[tokio::test]
    async fn hears_a_satellite_that_answers_only_over_ipv6() {
        let Ok(v6) = UdpSocket::bind((Ipv6Addr::LOCALHOST, 0)).await else {
            eprintln!("no IPv6 loopback here; skipped");
            return;
        };
        let silent = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let targets = [silent.local_addr().unwrap(), v6.local_addr().unwrap()];
        let responder = answer_what_is_asked(v6);
        let found = discover_at(
            &targets,
            "",
            Duration::from_millis(150),
            Duration::from_millis(150),
        )
        .await
        .unwrap();
        assert!(
            responder.await.unwrap() >= 3,
            "PTR, then SRV and TXT, then addresses"
        );
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].ipv4, [Ipv4Addr::new(192, 168, 1, 20)]);
    }

    #[tokio::test]
    async fn nobody_answering_is_an_empty_list() {
        let server = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let found = discover_at(
            &[server.local_addr().unwrap()],
            "",
            Duration::from_millis(50),
            Duration::from_millis(50),
        )
        .await
        .unwrap();
        assert!(found.is_empty());
    }
}
