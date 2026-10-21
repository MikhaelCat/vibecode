use std::collections::HashMap;

use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::domain::dates::add_days;

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolMeta {
    pub source: SourceKind,
    pub fetched_at: DateTime<Utc>,
    pub cached: bool,
    pub stale: bool,
}

impl ToolMeta {
    pub fn new(source: SourceKind) -> Self {
        Self {
            source,
            fetched_at: Utc::now(),
            cached: false,
            stale: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolOutput {
    pub ok: bool,
    pub data: Value,
    pub meta: ToolMeta,
    pub summary: String,
}

impl ToolOutput {
    pub fn ok(data: Value, source: SourceKind, summary: impl Into<String>) -> Self {
        Self {
            ok: true,
            data,
            meta: ToolMeta::new(source),
            summary: summary.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceRef {
    pub provider: String,
    pub url: Option<String>,
    pub fetched_at: DateTime<Utc>,
    pub kind: SourceKind,
}

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FuelStation {
    pub id: String,
    #[serde(default)]
    pub name: String,
    pub lat: f64,
    pub lon: f64,
    #[serde(default)]
    pub source: SourceKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventFact {
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub date: Option<NaiveDate>,
    #[serde(default)]
    pub place: String,
    #[serde(default)]
    pub source: SourceKind,
}

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
    pub fn ingest(&mut self, tool: &str, out: &ToolOutput) {
        self.push_source(tool, out);
        if !out.ok {
            return;
        }

        match tool {
            "search_places" | "geocode" => {
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
                        self.transport.push(option);
                    }
                }
            }
            "find_fuel" => {
                for station in array_field(&out.data, "stations") {
                    if let Ok(station) = serde_json::from_value::<FuelStation>(station.clone()) {
                        self.fuel.push(station);
                    }
                }
            }
            "find_events" => {
                for event in array_field(&out.data, "events") {
                    if let Ok(event) = serde_json::from_value::<EventFact>(event.clone()) {
                        self.events.push(event);
                    }
                }
            }
            _ => {}
        }
    }

    fn push_source(&mut self, tool: &str, out: &ToolOutput) {
        let already = self.sources.iter().any(|s| {
            s.provider == tool && s.kind == out.meta.source && s.fetched_at == out.meta.fetched_at
        });
        if !already {
            self.sources.push(SourceRef {
                provider: tool.to_string(),
                url: None,
                fetched_at: out.meta.fetched_at,
                kind: out.meta.source,
            });
        }
    }

    pub fn contains_place(&self, place_id: &str) -> bool {
        self.places.contains_key(place_id)
    }

    pub fn place(&self, place_id: &str) -> Option<&PlaceFact> {
        self.places.get(place_id)
    }

    pub fn opening_hours(&self, place_id: &str) -> Option<&str> {
        self.place(place_id)
            .and_then(|place| place.opening_hours.as_deref())
    }

    pub fn weather_for(&self, date: NaiveDate) -> Option<&WeatherDay> {
        self.weather.get(&date)
    }

    pub fn route_between(&self, from: &str, to: &str) -> Option<&RouteFact> {
        self.routes.get(&route_key(from, to))
    }

    pub fn enough_for_plan(&self) -> bool {
        self.places.len() >= 2
    }

    /// Guarantees that a plan can be assembled even if every external provider failed.
    pub fn ensure_minimal(&mut self, start: NaiveDate, end: NaiveDate) {
        if self.places.is_empty() {
            for place in sample_places() {
                self.places.insert(place.id.clone(), place);
            }
        }
        if self.lodging.is_empty() {
            for option in sample_lodging() {
                self.lodging.insert(option.id.clone(), option);
            }
        }
        if self.transport.is_empty() {
            self.transport = sample_transport();
        }
        if self.fuel.is_empty() {
            self.fuel = sample_fuel();
        }
        let mut date = start;
        while date <= end {
            self.weather
                .entry(date)
                .or_insert_with(|| sample_weather(date));
            date = add_days(date, 1);
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

fn sample_weather(date: NaiveDate) -> WeatherDay {
    WeatherDay {
        date,
        precip_prob: 10.0,
        wind_max: 5.0,
        temp_min: 8.0,
        temp_max: 16.0,
        sunrise: NaiveTime::from_hms_opt(7, 30, 0),
        sunset: NaiveTime::from_hms_opt(18, 30, 0),
    }
}

fn sample_places() -> Vec<PlaceFact> {
    vec![
        PlaceFact {
            id: "mock:place:oceanarium".to_string(),
            name: "Приморский океанариум".to_string(),
            lat: 43.1406,
            lon: 131.9276,
            kind: "museum".to_string(),
            opening_hours: Some("Tu-Su 10:00-18:00".to_string()),
            source: SourceKind::Mock,
        },
        PlaceFact {
            id: "mock:place:fortress".to_string(),
            name: "Владивостокская крепость".to_string(),
            lat: 43.1155,
            lon: 131.8855,
            kind: "historic".to_string(),
            opening_hours: Some("Tu-Su 10:00-18:00".to_string()),
            source: SourceKind::Mock,
        },
        PlaceFact {
            id: "mock:place:tokarevskaya".to_string(),
            name: "Токаревская кошка".to_string(),
            lat: 43.0739,
            lon: 131.8431,
            kind: "viewpoint".to_string(),
            opening_hours: None,
            source: SourceKind::Mock,
        },
        PlaceFact {
            id: "mock:place:novik".to_string(),
            name: "Бухта Новик".to_string(),
            lat: 43.0050,
            lon: 131.7500,
            kind: "nature".to_string(),
            opening_hours: None,
            source: SourceKind::Mock,
        },
    ]
}

fn sample_lodging() -> Vec<Lodging> {
    vec![
        Lodging {
            id: "mock:lodge:vladivostok".to_string(),
            name: "Гостевой дом у Золотого Рога".to_string(),
            settlement: "Владивосток".to_string(),
            lat: 43.1160,
            lon: 131.8850,
            price_per_night: 4500,
            kind: "guesthouse".to_string(),
            source: SourceKind::Mock,
        },
        Lodging {
            id: "mock:lodge:andreevka".to_string(),
            name: "База отдыха «Бухта»".to_string(),
            settlement: "Андреевка".to_string(),
            lat: 42.6500,
            lon: 130.9000,
            price_per_night: 3800,
            kind: "guesthouse".to_string(),
            source: SourceKind::Mock,
        },
    ]
}

fn sample_transport() -> Vec<TransportOption> {
    vec![TransportOption {
        id: "mock:trip:vlad-uss".to_string(),
        from: "Владивосток".to_string(),
        to: "Уссурийск".to_string(),
        mode: "bus".to_string(),
        depart: "08:00".to_string(),
        arrive: "10:30".to_string(),
        price_rub: 600,
        source: SourceKind::Mock,
    }]
}

fn sample_fuel() -> Vec<FuelStation> {
    vec![FuelStation {
        id: "mock:fuel:1".to_string(),
        name: "АЗС на трассе".to_string(),
        lat: 43.3000,
        lon: 131.9500,
        source: SourceKind::Mock,
    }]
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
