//! The radio tools: the owner's stations and the radio-browser.info
//! directory (ADR 0017 §"The pipeline").
//!
//! `play_station` finds one of the owner's stations by name and plays it
//! through the same checked path as `play_url`. `search_radio` lists
//! stations the media lane can decode; the caller plays one with `play_url`.
//! A station address may be an .m3u or .pls playlist: the media lane
//! resolves those.

use std::collections::HashSet;

use pinna_assist::config::Station;
use pinna_http::reqwest::header::USER_AGENT;
use pinna_http::{ClientKind, NetError, classify, reqwest};
use serde::Deserialize;
use serde_json::{Value, json};

use super::DeviceTools;
use crate::checks;
use crate::voice::ApiCommand;

const SEARCH_URL: &str = "https://all.api.radio-browser.info/json/stations/search";
/// radio-browser.info asks its clients to name themselves.
const AGENT: &str = concat!("pinna/", env!("CARGO_PKG_VERSION"));
/// A search answer is a few KiB per station.
const BODY_CAP: usize = 256 * 1024;
/// Stations a search returns at most.
const RESULTS: usize = 8;
/// Stations a search asks for: some are left out for their codec.
const FETCH: usize = 32;
/// A search name is cut to this many characters.
const QUERY_MAX: usize = 100;
/// A station name from the directory is cut to this many characters.
const NAME_MAX: usize = 100;

/// The codecs radio-browser.info reports that pinna-audio's symphonia build
/// decodes (its `mp3`, `ogg`/`vorbis`, `flac` and `wav` features). AAC joins
/// once symphonia's `aac` feature is enabled.
const PLAYABLE: &[&str] = &["MP3", "OGG", "FLAC", "WAV"];

const NO_STATIONS: &str = "The owner has added no radio stations. They are added on the \
                           Assistant page, under Tools. search_radio finds stations by name.";

/// The owner's station `query` names, played on the media lane.
pub(super) async fn play_station(tools: &DeviceTools, query: &str) -> Result<String, String> {
    let stations = tools.live.stations.as_slice();
    let station = match find(stations, query) {
        Match::One(station) => station,
        Match::Several(candidates) => {
            return Err(format!(
                "Several stations match \"{}\": {}. Ask which one is meant.",
                query.trim(),
                names(candidates.into_iter())
            ));
        }
        Match::Nothing if stations.is_empty() => return Err(NO_STATIONS.into()),
        Match::Nothing => {
            return Err(format!(
                "No station matches \"{}\". The stations are: {}.",
                query.trim(),
                names(stations.iter())
            ));
        }
    };
    let url = checks::media_url(&station.url)?;
    tools
        .ask(|reply| ApiCommand::PlayMediaChecked(url, reply))
        .await
        .map_err(|why| format!("{} didn't play: {why}", station.name))?;
    Ok(format!("Playing {}.", station.name))
}

/// The owner's station names. Their addresses can carry tokens, so they are
/// never shown.
pub(super) fn list_stations(stations: &[Station]) -> String {
    let names: Vec<&str> = stations.iter().map(|s| s.name.as_str()).collect();
    json!({ "stations": names }).to_string()
}

fn names<'a>(stations: impl Iterator<Item = &'a Station>) -> String {
    stations
        .map(|s| s.name.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// What a name finds among the owner's stations.
#[derive(Debug, PartialEq)]
pub(super) enum Match<'a> {
    One(&'a Station),
    Several(Vec<&'a Station>),
    Nothing,
}

/// The station `query` names. The first rule that finds any station
/// decides: the same name, then a name that starts with the query, then one
/// that contains it, then the most words in common. Case, accents and
/// punctuation don't count.
pub(super) fn find<'a>(stations: &'a [Station], query: &str) -> Match<'a> {
    let query = key(query);
    if query.is_empty() {
        return Match::Nothing;
    }
    let keyed: Vec<(String, &Station)> = stations.iter().map(|s| (key(&s.name), s)).collect();
    let rules: [&dyn Fn(&str) -> bool; 3] = [
        &|name| name == query,
        &|name| name.starts_with(&query),
        &|name| name.contains(&query),
    ];
    for rule in rules {
        let found: Vec<&Station> = keyed
            .iter()
            .filter(|(name, _)| rule(name))
            .map(|(_, s)| *s)
            .collect();
        if let Some(found) = decided(found) {
            return found;
        }
    }

    let words: HashSet<&str> = query.split(' ').collect();
    let shared: Vec<(usize, &Station)> = keyed
        .iter()
        .map(|(name, s)| {
            let common: HashSet<&str> = name.split(' ').filter(|w| words.contains(w)).collect();
            (common.len(), *s)
        })
        .collect();
    let most = shared.iter().map(|(n, _)| *n).max().unwrap_or(0);
    if most == 0 {
        return Match::Nothing;
    }
    let found = shared
        .into_iter()
        .filter(|(n, _)| *n == most)
        .map(|(_, s)| s)
        .collect();
    decided(found).unwrap_or(Match::Nothing)
}

fn decided(mut found: Vec<&Station>) -> Option<Match<'_>> {
    match found.len() {
        0 => None,
        1 => Some(Match::One(found.remove(0))),
        _ => Some(Match::Several(found)),
    }
}

/// A station name as it is compared: lower case, without accents, and its
/// words of letters and digits separated by single spaces.
pub fn key(name: &str) -> String {
    let mut plain = String::with_capacity(name.len());
    for c in name.chars().flat_map(char::to_lowercase) {
        if let Some(letters) = unaccented(c) {
            plain.push_str(letters);
        } else if c.is_alphanumeric() {
            plain.push(c);
        } else if !('\u{300}'..='\u{36f}').contains(&c) {
            // Anything but a combining accent separates words.
            plain.push(' ');
        }
    }
    plain.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The base letters of an accented lower-case Latin letter.
fn unaccented(c: char) -> Option<&'static str> {
    Some(match c {
        'à'..='å' | 'ā' | 'ă' | 'ą' => "a",
        'æ' => "ae",
        'ç' | 'ć' | 'ĉ' | 'ċ' | 'č' => "c",
        'ð' | 'ď' | 'đ' => "d",
        'è'..='ë' | 'ē' | 'ĕ' | 'ė' | 'ę' | 'ě' => "e",
        'ĝ' | 'ğ' | 'ġ' | 'ģ' => "g",
        'ĥ' | 'ħ' => "h",
        'ì'..='ï' | 'ĩ' | 'ī' | 'ĭ' | 'į' | 'ı' => "i",
        'ĳ' => "ij",
        'ĵ' => "j",
        'ķ' => "k",
        'ĺ' | 'ļ' | 'ľ' | 'ŀ' | 'ł' => "l",
        'ñ' | 'ń' | 'ņ' | 'ň' => "n",
        'ò'..='ö' | 'ø' | 'ō' | 'ŏ' | 'ő' => "o",
        'œ' => "oe",
        'ŕ' | 'ŗ' | 'ř' => "r",
        'ś' | 'ŝ' | 'ş' | 'š' | 'ș' => "s",
        'ß' => "ss",
        'ţ' | 'ť' | 'ŧ' | 'ț' => "t",
        'þ' => "th",
        'ù'..='ü' | 'ũ' | 'ū' | 'ŭ' | 'ů' | 'ű' | 'ų' => "u",
        'ŵ' => "w",
        'ý' | 'ÿ' | 'ŷ' => "y",
        'ź' | 'ż' | 'ž' => "z",
        _ => return None,
    })
}

/// The radio-browser.info station search.
#[derive(Clone)]
pub struct RadioBrowser {
    http: reqwest::Client,
    url: String,
}

impl Default for RadioBrowser {
    fn default() -> Self {
        Self::at(pinna_http::client(ClientKind::Tool), SEARCH_URL)
    }
}

impl RadioBrowser {
    /// The search at another address, for tests.
    pub fn at(http: reqwest::Client, url: impl Into<String>) -> Self {
        Self {
            http,
            url: url.into(),
        }
    }

    /// The `search_radio` tool: stations named like `query` that the media
    /// lane can decode, most voted first.
    pub async fn search(&self, query: &str) -> Result<String, String> {
        let query: String = query.trim().chars().take(QUERY_MAX).collect();
        if query.is_empty() {
            return Err("search_radio needs a station name.".into());
        }
        let url = format!(
            "{}?name={}&limit={FETCH}&hidebroken=true&order=votes&reverse=true",
            self.url,
            url::form_urlencoded::byte_serialize(query.as_bytes()).collect::<String>()
        );
        let found = self.get(&url).await.map_err(|f| f.sentence())?;
        let mut seen = HashSet::new();
        let stations: Vec<Value> = found
            .into_iter()
            // One odd entry costs only itself.
            .filter_map(|entry| serde_json::from_value::<Found>(entry).ok())
            .filter_map(Found::playable)
            .filter(|station| seen.insert(station["url"].to_string()))
            .take(RESULTS)
            .collect();
        Ok(json!({ "stations": stations }).to_string())
    }

    async fn get(&self, url: &str) -> Result<Vec<Value>, Failure> {
        let mut response = self
            .http
            .get(url)
            .header(USER_AGENT, AGENT)
            .send()
            .await
            .map_err(Failure::net)?;
        if !response.status().is_success() {
            return Err(Failure::Http(response.status().as_u16()));
        }
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(Failure::net)? {
            if chunk.len() > BODY_CAP - body.len() {
                return Err(Failure::Unreadable);
            }
            body.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&body).map_err(|_| Failure::Unreadable)
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Failure {
    Timeout,
    Unreachable,
    Http(u16),
    /// Not JSON in the expected shape, or too large to be it.
    Unreadable,
}

impl Failure {
    fn net(e: reqwest::Error) -> Self {
        match classify(&e) {
            NetError::Timeout => Self::Timeout,
            NetError::Unreachable | NetError::Tls => Self::Unreachable,
            NetError::Protocol => Self::Unreadable,
        }
    }

    fn sentence(&self) -> String {
        const SERVICE: &str = "The radio directory";
        match self {
            Self::Timeout => format!("{SERVICE} didn't answer in time. Try again later."),
            Self::Unreachable => format!("{SERVICE} can't be reached. Try again later."),
            Self::Http(status) => format!("{SERVICE} answered {status}. Try again later."),
            Self::Unreadable => format!("{SERVICE} sent an answer that can't be read."),
        }
    }
}

/// A station as radio-browser.info lists it; only what the tool uses.
#[derive(Deserialize)]
struct Found {
    name: Option<String>,
    url: Option<String>,
    url_resolved: Option<String>,
    codec: Option<String>,
    bitrate: Option<u64>,
    /// 1 for an HLS stream, which the media lane refuses.
    hls: Option<u8>,
    country: Option<String>,
}

impl Found {
    /// The station as the tool shows it, if the media lane can play it.
    fn playable(self) -> Option<Value> {
        if self.hls == Some(1) {
            return None;
        }
        let codec = self.codec?.trim().to_ascii_uppercase();
        if !PLAYABLE.contains(&codec.as_str()) {
            return None;
        }
        let url = [self.url_resolved, self.url]
            .into_iter()
            .flatten()
            .find_map(|url| checks::media_url(&url).ok())?;
        let name = one_line(&self.name?)?;
        let mut station = json!({ "name": name, "url": url, "codec": codec });
        if let Some(kbps) = self.bitrate.filter(|kbps| *kbps > 0) {
            station["bitrate_kbps"] = kbps.into();
        }
        if let Some(country) = self.country.as_deref().and_then(one_line) {
            station["country"] = country.into();
        }
        Some(station)
    }
}

/// Text from the directory as one short line; `None` when nothing is left.
fn one_line(text: &str) -> Option<String> {
    let line: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(NAME_MAX)
        .collect();
    let line = line.split_whitespace().collect::<Vec<_>>().join(" ");
    (!line.is_empty()).then_some(line)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use pinna_assist::testing::{FakeResponse, FakeServer};
    use pinna_http::reqwest::{Method, StatusCode};
    use pinna_mcp::{ToolOutput, ToolSet};
    use tokio::sync::{mpsc, watch};

    use super::*;
    use crate::standalone::AssistLive;
    use crate::tools::{Caller, Surface};
    use crate::voice::{Input, RunId, Snapshot};

    fn station(name: &str, url: &str) -> Station {
        Station {
            name: name.into(),
            url: url.into(),
        }
    }

    fn owner_stations() -> Vec<Station> {
        [
            "Radio One",
            "Radio One Classics",
            "Radio Two",
            "BBC Radio 4",
            "Österreich 1",
            "Jazz FM",
            "Deutschlandfunk Kultur",
        ]
        .iter()
        .enumerate()
        .map(|(i, name)| station(name, &format!("https://radio.test/{i}")))
        .collect()
    }

    /// Tools over a bare channel with the owner's stations.
    fn with_stations(stations: Vec<Station>) -> (DeviceTools, mpsc::Receiver<Input>) {
        let (inputs, rx) = mpsc::channel(64);
        let status = watch::channel(Snapshot::default()).1;
        let mut live = AssistLive::empty();
        live.stations = Arc::new(stations);
        let tools = DeviceTools::new(
            &crate::tools::ToolKit::new(crate::metrics::Counters::default()),
            inputs,
            status,
            Arc::new(live),
            Surface::Brain,
            Caller::Run(RunId(3)),
        );
        (tools, rx)
    }

    #[test]
    fn names_are_matched_by_the_first_rule_that_finds_one() {
        let stations = owner_stations();
        let one = |name: &str| Match::One(stations.iter().find(|s| s.name == name).unwrap());
        let several = |names: &[&str]| {
            Match::Several(
                names
                    .iter()
                    .map(|name| stations.iter().find(|s| s.name == *name).unwrap())
                    .collect(),
            )
        };
        let table = [
            // The same name wins over a longer one that starts with it.
            ("radio one", one("Radio One")),
            ("  RADIO   one ", one("Radio One")),
            ("Radio One Classics", one("Radio One Classics")),
            // Accents and punctuation don't count.
            ("osterreich 1", one("Österreich 1")),
            ("Österreich-1", one("Österreich 1")),
            ("jazz f.m.", one("Jazz FM")),
            // A prefix.
            ("deutschlandfunk", one("Deutschlandfunk Kultur")),
            (
                "radio",
                several(&["Radio One", "Radio One Classics", "Radio Two"]),
            ),
            // Contained.
            ("kultur", one("Deutschlandfunk Kultur")),
            ("radio 4", one("BBC Radio 4")),
            // Most words in common.
            ("the jazz station", one("Jazz FM")),
            ("classics on radio one", one("Radio One Classics")),
            ("two radio", one("Radio Two")),
            ("one radio", several(&["Radio One", "Radio One Classics"])),
            ("polka", Match::Nothing),
            ("", Match::Nothing),
            ("?!", Match::Nothing),
        ];
        for (query, expected) in table {
            assert_eq!(find(&stations, query), expected, "{query:?}");
        }
        assert_eq!(find(&[], "radio one"), Match::Nothing);
    }

    #[test]
    fn keys_fold_case_accents_and_separators() {
        assert_eq!(key("  Ö1 — Wien!  "), "o1 wien");
        assert_eq!(key("Ça Plane"), "ca plane");
        assert_eq!(key("ŁÓDŹ Straße"), "lodz strasse");
        // A decomposed accent folds like a composed one.
        assert_eq!(key("Cafe\u{301}"), key("Café"));
        assert_eq!(key("Радио 1"), "радио 1");
    }

    #[tokio::test]
    async fn play_station_plays_the_match_through_the_checked_path() {
        let (tools, mut rx) = with_stations(owner_stations());
        let call =
            tokio::spawn(async move { tools.call("play_station", json!({"name": "jazz"})).await });

        match rx.recv().await.unwrap() {
            Input::RunTool {
                run: RunId(3),
                cmd: ApiCommand::PlayMediaChecked(url, reply),
            } => {
                assert_eq!(url, "https://radio.test/5");
                reply.send(Ok(())).unwrap();
            }
            other => panic!("unexpected input {other:?}"),
        }
        assert_eq!(call.await.unwrap(), ToolOutput::ok("Playing Jazz FM."));
    }

    #[tokio::test]
    async fn a_play_failure_returns_the_reason() {
        let (tools, mut rx) = with_stations(owner_stations());
        let call = tokio::spawn(async move {
            tools
                .call("play_station", json!({"name": "Radio Two"}))
                .await
        });

        match rx.recv().await.unwrap() {
            Input::RunTool {
                cmd: ApiCommand::PlayMediaChecked(_, reply),
                ..
            } => reply.send(Err("the stream answered 404".into())).unwrap(),
            other => panic!("unexpected input {other:?}"),
        }
        assert_eq!(
            call.await.unwrap(),
            ToolOutput::error("Radio Two didn't play: the stream answered 404")
        );
    }

    #[tokio::test]
    async fn no_single_match_is_an_error_without_playing() {
        let (tools, mut rx) = with_stations(owner_stations());

        let ambiguous = tools.call("play_station", json!({"name": "radio"})).await;
        assert_eq!(
            ambiguous,
            ToolOutput::error(
                "Several stations match \"radio\": Radio One, Radio One Classics, Radio Two. \
                 Ask which one is meant."
            )
        );
        let unknown = tools.call("play_station", json!({"name": "polka"})).await;
        assert!(unknown.is_error);
        assert!(
            unknown
                .text
                .starts_with("No station matches \"polka\". The stations are: Radio One, ")
        );
        assert!(!unknown.text.contains("radio.test"));
        assert!(rx.try_recv().is_err());

        let (empty, _rx) = with_stations(Vec::new());
        assert_eq!(
            empty.call("play_station", json!({"name": "jazz"})).await,
            ToolOutput::error(NO_STATIONS)
        );
    }

    #[tokio::test]
    async fn list_stations_names_them_without_their_addresses() {
        let (tools, _rx) = with_stations(vec![
            station("Radio One", "https://radio.test/live?token=secret"),
            station("Jazz FM", "http://jazz.test/stream.pls"),
        ]);

        let out = tools.call("list_stations", json!({})).await;

        assert_eq!(
            out,
            ToolOutput::ok(r#"{"stations":["Radio One","Jazz FM"]}"#)
        );
        let (empty, _rx) = with_stations(Vec::new());
        assert_eq!(
            empty.call("list_stations", json!({})).await,
            ToolOutput::ok(r#"{"stations":[]}"#)
        );
    }

    const PATH: &str = "/json/stations/search";

    fn search_path(name: &str) -> String {
        format!("{PATH}?name={name}&limit=32&hidebroken=true&order=votes&reverse=true")
    }

    #[tokio::test]
    async fn search_keeps_only_stations_the_media_lane_decodes() {
        let server = FakeServer::start().await;
        server.script(
            Method::GET,
            &search_path("Radio+Paradise"),
            FakeResponse::new(
                StatusCode::OK,
                r#"[
                {"name": "Radio Paradise AAC", "url": "https://rp.test/aac", "url_resolved": "https://rp.test/aac",
                 "codec": "AAC", "bitrate": 320, "hls": 0, "country": "United States"},
                {"name": " Radio Paradise\n Main ", "url": "https://rp.test/main.m3u", "url_resolved": "https://rp.test/main.mp3",
                 "codec": "MP3", "bitrate": 128, "hls": 0, "country": "United States", "votes": 9000},
                {"name": "Radio Paradise HLS", "url": "https://rp.test/hls.m3u8", "url_resolved": "",
                 "codec": "MP3", "bitrate": 128, "hls": 1},
                {"name": "Radio Paradise FLAC", "url": "https://rp.test/flac", "url_resolved": "",
                 "codec": "flac", "bitrate": 0, "hls": 0, "country": ""},
                {"name": "Radio Paradise Opus", "url": "https://rp.test/opus", "codec": "OPUS", "hls": 0},
                {"name": "Radio Paradise AAC+", "url": "https://rp.test/aacp", "codec": "AAC+", "hls": 0},
                {"name": "Radio Paradise Mellow", "url": "ftp://rp.test/mellow", "url_resolved": "https://rp.test/mellow.ogg",
                 "codec": "OGG", "bitrate": 96, "hls": 0},
                {"name": "Radio Paradise again", "url": "https://rp.test/main.mp3", "codec": "MP3", "hls": 0},
                {"name": "Unknown codec", "url": "https://rp.test/unknown", "codec": "UNKNOWN", "hls": 0},
                {"name": "Broken entry", "url": 7, "codec": "MP3"},
                {"name": "No address", "codec": "MP3", "hls": 0}
            ]"#,
            ),
        );
        let radio = RadioBrowser::at(pinna_http::client(ClientKind::Tool), server.url(PATH));

        let out = radio.search(" Radio Paradise ").await.unwrap();

        let found: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(
            found,
            json!({"stations": [
                {"name": "Radio Paradise Main", "url": "https://rp.test/main.mp3", "codec": "MP3",
                 "bitrate_kbps": 128, "country": "United States"},
                {"name": "Radio Paradise FLAC", "url": "https://rp.test/flac", "codec": "FLAC"},
                {"name": "Radio Paradise Mellow", "url": "https://rp.test/mellow.ogg", "codec": "OGG",
                 "bitrate_kbps": 96},
            ]})
        );
        let request = &server.requests()[0];
        assert_eq!(request.header("user-agent"), Some(AGENT));
    }

    #[tokio::test]
    async fn search_returns_at_most_eight_stations() {
        let server = FakeServer::start().await;
        let many: Vec<Value> = (0..20)
            .map(|i| json!({"name": format!("Station {i}"), "url": format!("https://s.test/{i}"), "codec": "MP3"}))
            .collect();
        server.script(
            Method::GET,
            &search_path("Station"),
            FakeResponse::new(StatusCode::OK, Value::from(many).to_string()),
        );
        let radio = RadioBrowser::at(pinna_http::client(ClientKind::Tool), server.url(PATH));

        let out: Value = serde_json::from_str(&radio.search("Station").await.unwrap()).unwrap();

        let names: Vec<&str> = out["stations"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["name"].as_str().unwrap())
            .collect();
        assert_eq!(names.len(), RESULTS);
        assert_eq!(names[0], "Station 0");
    }

    #[tokio::test]
    async fn a_failing_search_is_an_error_sentence() {
        let server = FakeServer::start().await;
        let path = search_path("Jazz");
        server.script_status(Method::GET, &path, StatusCode::SERVICE_UNAVAILABLE);
        server.script(
            Method::GET,
            &path,
            FakeResponse::new(StatusCode::OK, "x".repeat(BODY_CAP + 1)),
        );
        server.script(
            Method::GET,
            &path,
            FakeResponse::new(StatusCode::OK, r#"{"error": "nope"}"#),
        );
        let radio = RadioBrowser::at(pinna_http::client(ClientKind::Tool), server.url(PATH));

        assert_eq!(
            radio.search("Jazz").await,
            Err("The radio directory answered 503. Try again later.".into())
        );
        for _ in 0..2 {
            assert_eq!(
                radio.search("Jazz").await,
                Err("The radio directory sent an answer that can't be read.".into())
            );
        }
        assert_eq!(
            radio.search("  ").await,
            Err("search_radio needs a station name.".into())
        );
        assert_eq!(server.requests().len(), 3);
    }

    #[tokio::test]
    async fn search_radio_is_offered_on_both_surfaces() {
        let server = FakeServer::start().await;
        server.script(
            Method::GET,
            &search_path("Jazz"),
            FakeResponse::new(StatusCode::OK, "[]"),
        );
        let (mut tools, _rx) = with_stations(Vec::new());
        tools.kit.radio = RadioBrowser::at(pinna_http::client(ClientKind::Tool), server.url(PATH));

        let out = tools.call("search_radio", json!({"name": "Jazz"})).await;

        assert_eq!(out, ToolOutput::ok(r#"{"stations":[]}"#));
        for surface in [Surface::Brain, Surface::Mcp] {
            tools.surface = surface;
            let names: Vec<String> = tools.specs().await.into_iter().map(|s| s.name).collect();
            for name in ["play_station", "list_stations", "search_radio"] {
                assert!(names.iter().any(|n| n == name), "{surface:?} {name}");
            }
        }
    }

    /// The codec filter follows what pinna-audio decodes.
    #[test]
    fn aac_is_offered_only_when_pinna_audio_decodes_it() {
        let manifest = include_str!("../../../pinna-audio/Cargo.toml");
        let symphonia = manifest
            .lines()
            .find(|line| line.trim_start().starts_with("symphonia"))
            .expect("pinna-audio depends on symphonia");
        assert_eq!(
            symphonia.contains("\"aac\""),
            PLAYABLE.contains(&"AAC"),
            "{symphonia}"
        );
        for (feature, codec) in [
            ("mp3", "MP3"),
            ("vorbis", "OGG"),
            ("flac", "FLAC"),
            ("wav", "WAV"),
        ] {
            assert_eq!(
                symphonia.contains(&format!("\"{feature}\"")),
                PLAYABLE.contains(&codec),
                "{feature}"
            );
        }
    }
}
