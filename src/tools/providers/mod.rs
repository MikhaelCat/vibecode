//! Parser layer: one module per external source.
//!
//! Live sources are the OSM map API (`osmmap`), Nominatim geocoding and its
//! category search (`geo`), Wikipedia geosearch, Overpass (`overpass`),
//! Open-Meteo and Yandex Pogoda (weather, VL.ru as the last fallback), OSRM
//! (routes), public transport and VL.ru JSON-LD (events). Local JSON in
//! `data/mock` is used only when a live source is unavailable. `geo` resolves
//! any Primorye settlement/lake/river into a bbox; `load_mock` is the single
//! read path for fallback data.
pub mod events;
pub mod fuel;
pub mod geo;
pub mod geocode;
pub mod lodging;
pub mod osmmap;
pub mod overpass;
pub mod places;
pub mod route;
pub mod transport;
pub mod weather;
pub mod wikipedia;
pub mod yandex;
pub mod yandex_weather;

pub use events::EventsTool;
pub use fuel::FuelTool;
pub use geocode::GeocodeTool;
pub use lodging::LodgingTool;
pub use places::PlacesTool;
pub use route::RouteTool;
pub use transport::TransportTool;
pub use weather::WeatherTool;
pub use wikipedia::WikipediaTool;

use std::path::Path;

use serde_json::{json, Value};

/// Overpass fair-use policy: at most two concurrent queries.
pub static OVERPASS_SLOTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);

/// Default starting point when the query names the region, not a place. A stop
/// list spread over 1000 km can never become a two-day route, and the krai
/// centroid is nobody's city: the region-wide request starts here, and every
/// summary names the default so the model can ask for another city.
pub const DEFAULT_AREA_CITY: &str = "Владивосток";

/// Parsed `data/mock` fixtures, read from disk once. The old version called
/// blocking `std::fs::read_to_string` on every lookup inside async tool calls,
/// which stalls a runtime worker for no reason; the fixtures never change while
/// the process lives, so one read per file is enough.
static MOCK_CACHE: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, Value>>> =
    std::sync::OnceLock::new();

pub fn load_mock(data_dir: &Path, file: &str, fallback: Value) -> Value {
    let path = data_dir.join("mock").join(file);
    let key = path.to_string_lossy().to_string();
    let cache = MOCK_CACHE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()));
    if let Ok(guard) = cache.lock() {
        if let Some(value) = guard.get(&key) {
            return value.clone();
        }
    }
    let value = std::fs::read_to_string(&path)
        .ok()
        .and_then(|content| serde_json::from_str::<Value>(&content).ok())
        .unwrap_or(fallback);
    if let Ok(mut guard) = cache.lock() {
        guard.insert(key, value.clone());
    }
    value
}

pub fn limit_from(args: &Value, default: usize, max: usize) -> usize {
    args.get("limit")
        .and_then(Value::as_u64)
        .map(|value| value as usize)
        .unwrap_or(default)
        .clamp(1, max)
}

/// Name used to compare the same place across sources: lower case, "ё" folded
/// to "е" and every non-alphanumeric character turned into a separator, so
/// "кафе «Полина»" from one provider matches "Кафе Полина" from another.
pub fn normalize_name(name: &str) -> String {
    name.to_lowercase()
        .replace('ё', "е")
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// True when a name is a place people can recognise. A mapper sometimes tags a
/// building as a hotel and leaves its house number as the name ("10/9"), and
/// such a row is noise in a plan: it names nothing.
pub fn is_place_name(name: &str) -> bool {
    name.chars().filter(|c| c.is_alphabetic()).count() >= 2
}

pub fn fallback_places() -> Value {
    json!({"places": crate::domain::facts::mock_places()})
}

pub fn fallback_lodging() -> Value {
    json!({"options": crate::domain::facts::mock_lodging()})
}

pub fn fallback_transport() -> Value {
    json!({"options": crate::domain::facts::mock_transport()})
}

pub fn fallback_fuel() -> Value {
    json!({"stations": crate::domain::facts::mock_fuel()})
}
