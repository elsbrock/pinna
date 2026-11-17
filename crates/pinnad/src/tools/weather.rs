//! The `weather` tool and the place search, over Open-Meteo (ADR 0017,
//! architecture §8).
//!
//! Both APIs are keyless. The forecast is for the owner's place, in the
//! place's own time zone; units follow the language: Fahrenheit and miles per
//! hour for US English, Celsius and kilometres per hour otherwise. The text is
//! for the brain, which says it in the owner's language.

use jiff::civil::Date;
use pinna_assist::config::Place;
use pinna_http::{ClientKind, NetError, classify, reqwest};
use pinna_mcp::ToolOutput;
use serde::Deserialize;

pub const NAME: &str = "weather";

const FORECAST_URL: &str = "https://api.open-meteo.com/v1/forecast";
const GEOCODING_URL: &str = "https://geocoding-api.open-meteo.com/v1/search";
/// Both answers are a few KiB.
const BODY_CAP: usize = 256 * 1024;
/// Today and the next two days.
const DAYS: usize = 3;
/// A place name to search for is at most this many characters.
const PLACE_NAME_MAX: usize = 100;

const NO_PLACE: &str = "No place is set for the weather. The owner sets one on the Assistant page, under Conversation.";

/// Why the place search found nothing to store.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlaceError {
    /// The search answered, without a match.
    NotFound,
    /// The search could not answer; one owner sentence says why.
    Unavailable(String),
}

/// The Open-Meteo forecast and geocoding APIs.
#[derive(Clone)]
pub struct OpenMeteo {
    http: reqwest::Client,
    forecast: String,
    geocoding: String,
}

impl Default for OpenMeteo {
    fn default() -> Self {
        Self::at(
            pinna_http::client(ClientKind::Tool),
            FORECAST_URL,
            GEOCODING_URL,
        )
    }
}

impl OpenMeteo {
    /// The APIs at other addresses, for tests.
    pub fn at(
        http: reqwest::Client,
        forecast: impl Into<String>,
        geocoding: impl Into<String>,
    ) -> Self {
        Self {
            http,
            forecast: forecast.into(),
            geocoding: geocoding.into(),
        }
    }

    /// The `weather` tool.
    pub async fn forecast(&self, place: Option<&Place>, language: Option<&str>) -> ToolOutput {
        let Some(place) = place else {
            return ToolOutput::error(NO_PLACE);
        };
        if !(-90.0..=90.0).contains(&place.lat) || !(-180.0..=180.0).contains(&place.lon) {
            return ToolOutput::error(NO_PLACE);
        }
        let units = Units::for_language(language);
        let url = format!(
            "{}?latitude={}&longitude={}\
             &current=temperature_2m,apparent_temperature,weather_code,wind_speed_10m\
             &daily=weather_code,temperature_2m_max,temperature_2m_min,precipitation_probability_max\
             &timezone=auto&forecast_days={DAYS}{}",
            self.forecast,
            place.lat,
            place.lon,
            units.query()
        );
        let forecast = match self.get::<Forecast>(&url).await {
            Ok(forecast) => forecast,
            Err(failure) => return ToolOutput::error(failure.sentence("The weather service")),
        };
        ToolOutput::ok(forecast.describe(&place.name, units))
    }

    /// Search a place by name, in the owner's language.
    pub async fn geocode(&self, name: &str, language: Option<&str>) -> Result<Place, PlaceError> {
        let name: String = name.trim().chars().take(PLACE_NAME_MAX).collect();
        let url = format!(
            "{}?name={}&count=1&language={}&format=json",
            self.geocoding,
            encode(&name),
            encode(&primary_language(language)),
        );
        let search = self
            .get::<Search>(&url)
            .await
            .map_err(|failure| PlaceError::Unavailable(failure.sentence("The place search")))?;
        search
            .results
            .into_iter()
            .next()
            .map(Found::into_place)
            .ok_or(PlaceError::NotFound)
    }

    async fn get<T: for<'de> Deserialize<'de>>(&self, url: &str) -> Result<T, Failure> {
        let mut response = self.http.get(url).send().await.map_err(Failure::net)?;
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Units {
    Metric,
    UnitedStates,
}

impl Units {
    fn for_language(language: Option<&str>) -> Self {
        let region = language
            .into_iter()
            .flat_map(|tag| tag.split(['-', '_']).skip(1))
            .find(|part| part.len() == 2);
        match region {
            Some(region) if region.eq_ignore_ascii_case("us") => Self::UnitedStates,
            _ => Self::Metric,
        }
    }

    fn query(self) -> &'static str {
        match self {
            Self::Metric => "&temperature_unit=celsius&wind_speed_unit=kmh",
            Self::UnitedStates => "&temperature_unit=fahrenheit&wind_speed_unit=mph",
        }
    }

    fn degrees(self) -> &'static str {
        match self {
            Self::Metric => "°C",
            Self::UnitedStates => "°F",
        }
    }

    fn speed(self) -> &'static str {
        match self {
            Self::Metric => "km/h",
            Self::UnitedStates => "mph",
        }
    }
}

/// The language subtag Open-Meteo knows, such as `de` for `de-AT`.
fn primary_language(language: Option<&str>) -> String {
    language
        .and_then(|tag| tag.split(['-', '_']).next())
        .filter(|tag| !tag.is_empty())
        .unwrap_or("en")
        .to_ascii_lowercase()
}

fn encode(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
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

    fn sentence(&self, service: &str) -> String {
        match self {
            Self::Timeout => format!("{service} didn't answer in time. Try again later."),
            Self::Unreachable => format!("{service} can't be reached. Try again later."),
            Self::Http(status) => format!("{service} answered {status}. Try again later."),
            Self::Unreadable => format!("{service} sent an answer that can't be read."),
        }
    }
}

#[derive(Deserialize)]
struct Search {
    #[serde(default)]
    results: Vec<Found>,
}

#[derive(Deserialize)]
struct Found {
    name: String,
    latitude: f64,
    longitude: f64,
    admin1: Option<String>,
    country: Option<String>,
}

impl Found {
    /// "Munich, Bavaria, Germany". A region that repeats the name, such as
    /// "Land Berlin", is left out.
    fn into_place(self) -> Place {
        let mut name = self.name.clone();
        let region = self
            .admin1
            .filter(|region| !region.is_empty() && !region.contains(&self.name));
        for part in [region, self.country.filter(|country| !country.is_empty())]
            .into_iter()
            .flatten()
        {
            name.push_str(", ");
            name.push_str(&part);
        }
        Place {
            name,
            lat: self.latitude,
            lon: self.longitude,
        }
    }
}

#[derive(Deserialize)]
struct Forecast {
    current: Current,
    daily: Daily,
}

#[derive(Deserialize)]
struct Current {
    temperature_2m: f64,
    apparent_temperature: Option<f64>,
    weather_code: Option<u8>,
    wind_speed_10m: Option<f64>,
}

/// Open-Meteo sends `null` for a value it doesn't have.
#[derive(Deserialize)]
struct Daily {
    time: Vec<String>,
    weather_code: Vec<Option<u8>>,
    temperature_2m_max: Vec<Option<f64>>,
    temperature_2m_min: Vec<Option<f64>>,
    precipitation_probability_max: Vec<Option<f64>>,
}

impl Forecast {
    fn describe(&self, place: &str, units: Units) -> String {
        let (deg, speed) = (units.degrees(), units.speed());
        let c = &self.current;
        let mut now = Vec::new();
        now.extend(c.weather_code.and_then(sky).map(str::to_owned));
        now.push(format!("{} {deg}", whole(c.temperature_2m)));
        now.extend(
            c.apparent_temperature
                .map(|t| format!("feels like {} {deg}", whole(t))),
        );
        now.extend(
            c.wind_speed_10m
                .map(|w| format!("wind {} {speed}", whole(w))),
        );
        let mut text = format!(
            "Weather at {place}. Temperatures in {deg}, wind in {speed}.\nNow: {}.",
            now.join(", ")
        );

        let d = &self.daily;
        for (i, date) in d.time.iter().enumerate().take(DAYS) {
            let mut day = Vec::new();
            day.extend(
                d.weather_code
                    .get(i)
                    .copied()
                    .flatten()
                    .and_then(sky)
                    .map(str::to_owned),
            );
            let low = d.temperature_2m_min.get(i).copied().flatten();
            let high = d.temperature_2m_max.get(i).copied().flatten();
            if let (Some(low), Some(high)) = (low, high) {
                day.push(format!("{} to {} {deg}", whole(low), whole(high)));
            }
            if let Some(p) = d.precipitation_probability_max.get(i).copied().flatten() {
                day.push(format!("{}% chance of precipitation", whole(p)));
            }
            if !day.is_empty() {
                text.push_str(&format!("\n{}: {}.", day_name(i, date), day.join(", ")));
            }
        }
        text
    }
}

/// A value as it is said: rounded, and never "-0".
fn whole(x: f64) -> i64 {
    x.round() as i64
}

/// "Today", "Tomorrow", then the weekday.
fn day_name(index: usize, date: &str) -> String {
    match index {
        0 => "Today".into(),
        1 => "Tomorrow".into(),
        _ => date
            .parse::<Date>()
            .map(|date| date.strftime("%A").to_string())
            .unwrap_or_else(|_| date.to_owned()),
    }
}

/// The WMO weather interpretation codes Open-Meteo uses.
fn sky(code: u8) -> Option<&'static str> {
    Some(match code {
        0 => "clear sky",
        1 => "mainly clear",
        2 => "partly cloudy",
        3 => "overcast",
        45 | 48 => "fog",
        51 | 53 | 55 => "drizzle",
        56 | 57 => "freezing drizzle",
        61 => "light rain",
        63 => "rain",
        65 => "heavy rain",
        66 | 67 => "freezing rain",
        71 => "light snow",
        73 => "snow",
        75 => "heavy snow",
        77 => "snow grains",
        80 | 81 => "rain showers",
        82 => "heavy rain showers",
        85 | 86 => "snow showers",
        95 => "thunderstorm",
        96 | 99 => "thunderstorm with hail",
        _ => return None,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use pinna_assist::testing::{FakeResponse, FakeServer};
    use pinna_http::reqwest::{Method, StatusCode};

    use super::*;

    pub(crate) fn open_meteo(server: &FakeServer) -> OpenMeteo {
        OpenMeteo::at(
            pinna_http::client(ClientKind::Tool),
            server.url("/v1/forecast"),
            server.url("/v1/search"),
        )
    }

    fn munich() -> Place {
        Place {
            name: "Munich, Bavaria, Germany".into(),
            lat: 48.14,
            lon: 11.58,
        }
    }

    fn forecast_path(units: &str) -> String {
        format!(
            "/v1/forecast?latitude=48.14&longitude=11.58\
             &current=temperature_2m,apparent_temperature,weather_code,wind_speed_10m\
             &daily=weather_code,temperature_2m_max,temperature_2m_min,precipitation_probability_max\
             &timezone=auto&forecast_days=3{units}"
        )
    }

    const FORECAST: &str = r#"{
        "latitude": 48.14, "longitude": 11.58, "timezone": "Europe/Berlin",
        "current": {"time": "2026-11-17T14:00", "temperature_2m": 7.4,
                    "apparent_temperature": 4.2, "weather_code": 3, "wind_speed_10m": 12.3},
        "daily": {"time": ["2026-11-17", "2026-11-18", "2026-11-19"],
                  "weather_code": [3, 61, null],
                  "temperature_2m_max": [9.1, 8.0, 6.5],
                  "temperature_2m_min": [3.2, 4.0, 1.1],
                  "precipitation_probability_max": [10, 80, null]}
    }"#;

    #[tokio::test]
    async fn units_follow_the_language() {
        let server = FakeServer::start().await;
        let weather = open_meteo(&server);
        let metric = forecast_path("&temperature_unit=celsius&wind_speed_unit=kmh");
        for _ in 0..2 {
            server.script(
                Method::GET,
                &metric,
                FakeResponse::new(StatusCode::OK, FORECAST),
            );
        }

        for language in [Some("de"), None] {
            assert_eq!(
                weather.forecast(Some(&munich()), language).await,
                ToolOutput::ok(
                    "Weather at Munich, Bavaria, Germany. Temperatures in °C, wind in km/h.\n\
                     Now: overcast, 7 °C, feels like 4 °C, wind 12 km/h.\n\
                     Today: overcast, 3 to 9 °C, 10% chance of precipitation.\n\
                     Tomorrow: light rain, 4 to 8 °C, 80% chance of precipitation.\n\
                     Thursday: 1 to 7 °C."
                )
            );
        }

        // Open-Meteo converts; the numbers here are only the fixture's.
        server.script(
            Method::GET,
            &forecast_path("&temperature_unit=fahrenheit&wind_speed_unit=mph"),
            FakeResponse::new(StatusCode::OK, FORECAST),
        );
        let us = weather.forecast(Some(&munich()), Some("en-US")).await;
        assert!(!us.is_error, "{us:?}");
        assert!(us.text.starts_with(
            "Weather at Munich, Bavaria, Germany. Temperatures in °F, wind in mph.\n"
        ));
        assert!(us.text.contains("wind 12 mph"), "{us:?}");
        assert_eq!(server.requests().len(), 3);
    }

    #[tokio::test]
    async fn no_place_is_an_error_without_a_request() {
        let server = FakeServer::start().await;
        let output = open_meteo(&server).forecast(None, Some("de")).await;
        assert!(output.is_error);
        assert_eq!(output.text, NO_PLACE);
        assert!(server.requests().is_empty());
    }

    #[tokio::test]
    async fn a_failing_forecast_is_an_error_sentence() {
        let server = FakeServer::start().await;
        let weather = open_meteo(&server);
        let path = forecast_path("&temperature_unit=celsius&wind_speed_unit=kmh");
        server.script_status(Method::GET, &path, StatusCode::BAD_GATEWAY);
        server.script(
            Method::GET,
            &path,
            FakeResponse::new(StatusCode::OK, "<html>"),
        );

        assert_eq!(
            weather.forecast(Some(&munich()), None).await,
            ToolOutput::error("The weather service answered 502. Try again later.")
        );
        assert_eq!(
            weather.forecast(Some(&munich()), None).await,
            ToolOutput::error("The weather service sent an answer that can't be read.")
        );
    }

    #[tokio::test]
    async fn a_place_is_found_in_the_owner_language() {
        let server = FakeServer::start().await;
        server.script(
            Method::GET,
            "/v1/search?name=M%C3%BCnchen&count=1&language=de&format=json",
            FakeResponse::new(
                StatusCode::OK,
                r#"{"results": [{"name": "München", "latitude": 48.13743, "longitude": 11.57549,
                    "admin1": "Bayern", "country": "Deutschland", "timezone": "Europe/Berlin"}]}"#,
            ),
        );
        server.script(
            Method::GET,
            "/v1/search?name=Berlin&count=1&language=en&format=json",
            FakeResponse::new(
                StatusCode::OK,
                r#"{"results": [{"name": "Berlin", "latitude": 52.52437, "longitude": 13.41053,
                    "admin1": "Land Berlin", "country": "Germany"}]}"#,
            ),
        );
        let weather = open_meteo(&server);

        assert_eq!(
            weather.geocode(" München ", Some("de-DE")).await,
            Ok(Place {
                name: "München, Bayern, Deutschland".into(),
                lat: 48.13743,
                lon: 11.57549,
            })
        );
        assert_eq!(
            weather.geocode("Berlin", None).await.unwrap().name,
            "Berlin, Germany"
        );
    }

    #[tokio::test]
    async fn an_unknown_place_is_not_found() {
        let server = FakeServer::start().await;
        server.script(
            Method::GET,
            "/v1/search?name=Nowhere&count=1&language=en&format=json",
            FakeResponse::new(StatusCode::OK, r#"{"generationtime_ms": 0.4}"#),
        );
        let weather = open_meteo(&server);
        assert_eq!(
            weather.geocode("Nowhere", Some("en")).await,
            Err(PlaceError::NotFound)
        );
        // Nothing scripted answers 404.
        assert_eq!(
            weather.geocode("Elsewhere", Some("en")).await,
            Err(PlaceError::Unavailable(
                "The place search answered 404. Try again later.".into()
            ))
        );
    }

    #[test]
    fn only_us_regions_get_fahrenheit() {
        for (language, units) in [
            (Some("en-US"), Units::UnitedStates),
            (Some("es_us"), Units::UnitedStates),
            (Some("en"), Units::Metric),
            (Some("en-GB"), Units::Metric),
            (Some("de-AT"), Units::Metric),
            (None, Units::Metric),
        ] {
            assert_eq!(Units::for_language(language), units, "{language:?}");
        }
    }

    #[test]
    fn unknown_weather_codes_are_left_out() {
        assert_eq!(sky(3), Some("overcast"));
        assert_eq!(sky(4), None);
        assert_eq!(whole(-0.4), 0);
        assert_eq!(day_name(2, "not a date"), "not a date");
    }
}
