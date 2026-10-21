//! Parser layer: one module per external source.
//!
//! Live sources are OSM Overpass (places, food, hotels, fuel), Wikipedia
//! geosearch, Nominatim/Photon (areas), Open-Meteo (weather, VL.ru fallback),
//! OSRM (routes), public transport and VL.ru JSON-LD (events). Local JSON in
//! `data/mock` is used only when a live source is unavailable. `geo` resolves
//! any Primorye settlement/lake/river into a bbox; `load_mock` is the single
//! read path for fallback data.
pub mod events;
pub mod fuel;
pub mod geo;
pub mod geocode;
pub mod lodging;
pub mod places;
pub mod route;
pub mod transport;
pub mod weather;
pub mod wikipedia;

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

pub const PRIMORYE_BBOX: &str = "42.2,130.0,48.5,139.1";

/// Overpass fair-use policy: at most two concurrent queries.
pub static OVERPASS_SLOTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);

pub fn load_mock(data_dir: &Path, file: &str, fallback: Value) -> Value {
    let path = data_dir.join("mock").join(file);
    std::fs::read_to_string(path)
        .ok()
        .and_then(|content| serde_json::from_str::<Value>(&content).ok())
        .unwrap_or(fallback)
}

pub fn limit_from(args: &Value, default: usize, max: usize) -> usize {
    args.get("limit")
        .and_then(Value::as_u64)
        .map(|value| value as usize)
        .unwrap_or(default)
        .clamp(1, max)
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
