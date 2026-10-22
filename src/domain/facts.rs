//! Verified tool output, keyed for planning.
//!
//! `Facts` is the only data source a plan may reference: the orchestrator
//! ingests each successful `ToolOutput` here, and the validator rejects any
//! `place_id` that is not present (V10). Every fact carries `SourceKind`
//! provenance (live, cache, mock, stale, estimated). If all providers fail,
//! `ensure_minimal` loads the committed `data/mock/*.json` fixtures so a
//! degraded plan can still be produced.

use std::collections::HashMap;

use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::domain::geo::haversine_km;

/// Provenance of a fact or tool result.
///
/// `Mock` is the serde default, so an unlabelled value is treated as fixture
/// data rather than live data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum SourceKind {
    Live,
    Cache,
    #[default]
    Mock,
    Stale,
    Estimated,
}

impl SourceKind {
    /// Stable lower-case label used in JSON and traces.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Live => "live",
            Self::Cache => "cache",
            Self::Mock => "mock",
            Self::Stale => "stale",
            Self::Estimated => "estimated",
        }
    }
}

impl std::fmt::Display for SourceKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Per-call provenance: origin, fetch time, cache and staleness flags.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolMeta {
    pub source: SourceKind,
    pub fetched_at: DateTime<Utc>,
    pub cached: bool,
    pub stale: bool,
}

impl ToolMeta {
    /// Creates metadata for a fresh result from `source`.
    pub fn new(source: SourceKind) -> Self {
        Self {
            source,
            fetched_at: Utc::now(),
            cached: false,
            stale: false,
        }
    }
}

/// Uniform tool response: success flag, JSON payload, provenance and summary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolOutput {
    pub ok: bool,
    pub data: Value,
    pub meta: ToolMeta,
    pub summary: String,
}

impl ToolOutput {
    /// Builds a successful output with fresh provenance.
    pub fn ok(data: Value, source: SourceKind, summary: impl Into<String>) -> Self {
        Self {
            ok: true,
            data,
            meta: ToolMeta::new(source),
            summary: summary.into(),
        }
    }
}

/// Source attribution carried into the final plan.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceRef {
    pub provider: String,
    pub url: Option<String>,
    pub fetched_at: DateTime<Utc>,
    pub kind: SourceKind,
}

/// A place verified by a tool, keyed by provider id (e.g. `osm:node/1`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaceFact {
    pub id: String,
    pub name: String,
    pub lat: f64,
    pub lon: f64,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub opening_hours: Option<String>,
    #[serde(default)]
    pub source: SourceKind,
}

/// Daily forecast used by the weather checks; one entry per date.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WeatherDay {
    pub date: NaiveDate,
    pub precip_prob: f64,
    pub wind_max: f64,
    pub temp_min: f64,
    pub temp_max: f64,
    #[serde(default)]
    pub sunrise: Option<NaiveTime>,
    #[serde(default)]
    pub sunset: Option<NaiveTime>,
}

/// A lodging option returned by `find_lodging`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Lodging {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub settlement: String,
    pub lat: f64,
    pub lon: f64,
    #[serde(default)]
    pub price_per_night: u32,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub source: SourceKind,
}

/// A route between two provider coordinates, keyed by `from->to`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteFact {
    #[serde(default)]
    pub from: String,
    #[serde(default)]
    pub to: String,
    pub distance_km: f64,
    pub duration_min: u32,
    #[serde(default)]
    pub geometry: Option<String>,
    #[serde(default)]
    pub source: SourceKind,
}

/// A scheduled transport option returned by `find_transport`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransportOption {
    pub id: String,
    pub from: String,
    pub to: String,
    #[serde(default)]
    pub mode: String,
    #[serde(default)]
    pub depart: String,
    #[serde(default)]
    pub arrive: String,
    #[serde(default)]
    pub price_rub: u32,
    #[serde(default)]
    pub source: SourceKind,
}

/// A fuel station returned by `find_fuel`.
///
/// Coordinates come from OSM. `fuel` and the prices are only populated for the
/// demo fixture (`data/mock/fuel.json`); live OSM stations have no availability
/// or price data, so those fields stay empty.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FuelStation {
    pub id: String,
    #[serde(default)]
    pub name: String,
    pub lat: f64,
    pub lon: f64,
    #[serde(default)]
    pub source: SourceKind,
    /// Fuel grades the station sells (demo data for mock stations).
    #[serde(default)]
    pub fuel: Vec<String>,
    /// Demo price for АИ-92, roubles per litre.
    #[serde(default)]
    pub price_ai92: Option<u32>,
    /// Demo price for АИ-95, roubles per litre.
    #[serde(default)]
    pub price_ai95: Option<u32>,
    /// Demo price for diesel (ДТ), roubles per litre.
    #[serde(default)]
    pub price_dt: Option<u32>,
}

/// A dated event returned by `find_events`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventFact {
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub date: Option<NaiveDate>,
    #[serde(default)]
    pub end_date: Option<NaiveDate>,
    #[serde(default)]
    pub place: String,
    #[serde(default = "default_event_area")]
    pub area: String,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub score: f64,
    #[serde(default)]
    pub source: SourceKind,
}

fn default_event_area() -> String {
    "Владивосток".to_string()
}

/// Accumulated, verified tool output for one run.
///
/// Collections are keyed by provider ids/dates so repeated calls merge rather
/// than duplicate; transport and events are de-duplicated on insert.
#[derive(Debug, Clone, Default)]
pub struct Facts {
    pub places: HashMap<String, PlaceFact>,
    pub weather: HashMap<NaiveDate, WeatherDay>,
    pub lodging: HashMap<String, Lodging>,
    pub transport: Vec<TransportOption>,
    pub fuel: Vec<FuelStation>,
    pub events: Vec<EventFact>,
    pub routes: HashMap<String, RouteFact>,
    pub sources: Vec<SourceRef>,
}

impl Facts {
    /// Merges a tool result by tool name; failed outputs only record provenance.
    /// Unknown tools are ignored, so adding a provider cannot silently corrupt
    /// existing fact types.
    pub fn ingest(&mut self, tool: &str, out: &ToolOutput) {
        self.push_source(tool, out);
        if !out.ok {
            return;
        }

        match tool {
            "search_places" | "geocode" | "get_nearby_attractions" => {
                for place in array_field(&out.data, "places") {
                    if let Ok(place) = serde_json::from_value::<PlaceFact>(place.clone()) {
                        self.places.insert(place.id.clone(), place);
                    }
                }
            }
            "get_weather" => {
                for day in array_field(&out.data, "days") {
                    if let Ok(day) = serde_json::from_value::<WeatherDay>(day.clone()) {
                        self.weather.insert(day.date, day);
                    }
                }
            }
            "get_route" => {
                if let Ok(route) = serde_json::from_value::<RouteFact>(out.data.clone()) {
                    let key = route_key(&route.from, &route.to);
                    self.routes.insert(key, route);
                }
            }
            "find_lodging" => {
                for option in array_field(&out.data, "options") {
                    if let Ok(option) = serde_json::from_value::<Lodging>(option.clone()) {
                        self.lodging.insert(option.id.clone(), option);
                    }
                }
            }
            "find_transport" => {
                for option in array_field(&out.data, "options") {
                    if let Ok(option) = serde_json::from_value::<TransportOption>(option.clone()) {
                        if !self.transport.iter().any(|known| known.id == option.id) {
                            self.transport.push(option);
                        }
                    }
                }
            }
            "find_fuel" => {
                for station in array_field(&out.data, "stations") {
                    if let Ok(station) = serde_json::from_value::<FuelStation>(station.clone()) {
                        if !self.fuel.iter().any(|known| known.id == station.id) {
                            self.fuel.push(station);
                        }
                    }
                }
            }
            "find_events" => {
                for event in array_field(&out.data, "events") {
                    if let Ok(event) = serde_json::from_value::<EventFact>(event.clone()) {
                        let duplicate = self.events.iter().any(|known| {
                            known.id == event.id
                                || (known.title.eq_ignore_ascii_case(&event.title)
                                    && known.date == event.date
                                    && known.place.eq_ignore_ascii_case(&event.place))
                        });
                        if !duplicate {
                            self.events.push(event);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn push_source(&mut self, tool: &str, out: &ToolOutput) {
        let already = self
            .sources
            .iter()
            .any(|s| s.provider == tool && s.kind == out.meta.source);
        if !already {
            self.sources.push(SourceRef {
                provider: tool.to_string(),
                url: None,
                fetched_at: out.meta.fetched_at,
                kind: out.meta.source,
            });
        }
    }

    /// Provenance check used by validator V10.
    pub fn contains_place(&self, place_id: &str) -> bool {
        self.places.contains_key(place_id)
    }

    /// Looks up a verified place by provider id.
    pub fn place(&self, place_id: &str) -> Option<&PlaceFact> {
        self.places.get(place_id)
    }

    /// Opening hours for a known place, if the provider supplied them.
    pub fn opening_hours(&self, place_id: &str) -> Option<&str> {
        self.place(place_id)
            .and_then(|place| place.opening_hours.as_deref())
    }

    /// Forecast for a date, if `get_weather` covered it.
    pub fn weather_for(&self, date: NaiveDate) -> Option<&WeatherDay> {
        self.weather.get(&date)
    }

    /// Route by exact provider coordinate strings.
    pub fn route_between(&self, from: &str, to: &str) -> Option<&RouteFact> {
        self.routes.get(&route_key(from, to))
    }

    /// Route facts are keyed by the provider's "lat,lon" strings; match them to
    /// plan points within roughly 20 m.
    pub fn route_for_points(&self, from: (f64, f64), to: (f64, f64)) -> Option<&RouteFact> {
        self.routes.values().find(|route| {
            let Some(route_from) = parse_latlon(&route.from) else {
                return false;
            };
            let Some(route_to) = parse_latlon(&route.to) else {
                return false;
            };
            haversine_km(from, route_from) < 0.02 && haversine_km(to, route_to) < 0.02
        })
    }

    /// True once at least two places are known; the loop then nudges the model
    /// to submit instead of searching forever.
    pub fn enough_for_plan(&self) -> bool {
        self.places.len() >= 2
    }

    /// Guarantees that a plan can be assembled even if every external provider
    /// failed. Existing facts are never overwritten; all local fallback data
    /// comes from the same `data/mock/*.json` files.
    pub fn ensure_minimal(&mut self, _start: NaiveDate, _end: NaiveDate) {
        if self.places.is_empty() {
            for place in mock_places() {
                self.places.insert(place.id.clone(), place);
            }
        }
        if self.lodging.is_empty() {
            for option in mock_lodging() {
                self.lodging.insert(option.id.clone(), option);
            }
        }
        if self.transport.is_empty() {
            self.transport = mock_transport();
        }
        if self.fuel.is_empty() {
            self.fuel = mock_fuel();
        }
        if self.sources.is_empty() {
            self.sources.push(SourceRef {
                provider: "fallback".to_string(),
                url: None,
                fetched_at: Utc::now(),
                kind: SourceKind::Mock,
            });
        }
    }
}

fn array_field<'a>(data: &'a Value, key: &str) -> &'a [Value] {
    data.get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

fn route_key(from: &str, to: &str) -> String {
    format!("{from}->{to}")
}

fn parse_latlon(value: &str) -> Option<(f64, f64)> {
    let (lat, lon) = value.split_once(',')?;
    Some((lat.trim().parse().ok()?, lon.trim().parse().ok()?))
}

const MOCK_PLACES: &str = include_str!("../../data/mock/places.json");
const MOCK_LODGING: &str = include_str!("../../data/mock/lodging.json");
const MOCK_TRANSPORT: &str = include_str!("../../data/mock/transport.json");
const MOCK_FUEL: &str = include_str!("../../data/mock/fuel.json");

/// Single source of local mock data: the committed `data/mock/*.json` files.
pub fn mock_places() -> Vec<PlaceFact> {
    parse_mock(MOCK_PLACES, "places")
}

/// Local lodging fixtures from `data/mock/lodging.json`.
pub fn mock_lodging() -> Vec<Lodging> {
    parse_mock(MOCK_LODGING, "options")
}

/// Local transport fixtures from `data/mock/transport.json`.
pub fn mock_transport() -> Vec<TransportOption> {
    parse_mock(MOCK_TRANSPORT, "options")
}

/// Local fuel-station fixtures from `data/mock/fuel.json`.
pub fn mock_fuel() -> Vec<FuelStation> {
    parse_mock(MOCK_FUEL, "stations")
}

/// Tolerantly parses one array field from a mock file; bad entries are skipped.
fn parse_mock<T: DeserializeOwned>(raw: &str, key: &str) -> Vec<T> {
    let value: Value = serde_json::from_str(raw).unwrap_or(Value::Null);
    value
        .get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| serde_json::from_value(item.clone()).ok())
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ingest_places() {
        let out = ToolOutput::ok(
            serde_json::json!({
                "places": [{
                    "id": "osm:node/1",
                    "name": "Тест",
                    "lat": 43.1,
                    "lon": 131.9,
                    "kind": "museum",
                    "opening_hours": "Tu-Su 10:00-18:00",
                    "source": "live"
                }]
            }),
            SourceKind::Live,
            "ok",
        );
        let mut facts = Facts::default();
        facts.ingest("search_places", &out);
        assert!(facts.contains_place("osm:node/1"));
    }
}
