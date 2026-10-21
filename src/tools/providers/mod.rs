pub mod events;
pub mod fuel;
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
    json!({
        "places": [
            {
                "id": "mock:place:oceanarium",
                "name": "Приморский океанариум",
                "lat": 43.1406,
                "lon": 131.9276,
                "kind": "museum",
                "opening_hours": "Tu-Su 10:00-18:00",
                "source": "mock"
            },
            {
                "id": "mock:place:fortress",
                "name": "Владивостокская крепость",
                "lat": 43.1155,
                "lon": 131.8855,
                "kind": "historic",
                "opening_hours": "Tu-Su 10:00-18:00",
                "source": "mock"
            },
            {
                "id": "mock:place:tokarevskaya",
                "name": "Токаревская кошка",
                "lat": 43.0739,
                "lon": 131.8431,
                "kind": "viewpoint",
                "opening_hours": null,
                "source": "mock"
            },
            {
                "id": "mock:place:novik",
                "name": "Бухта Новик",
                "lat": 43.0050,
                "lon": 131.7500,
                "kind": "nature",
                "opening_hours": null,
                "source": "mock"
            }
        ]
    })
}

pub fn fallback_lodging() -> Value {
    json!({
        "options": [
            {
                "id": "mock:lodge:vladivostok",
                "name": "Гостевой дом у Золотого Рога",
                "settlement": "Владивосток",
                "lat": 43.1160,
                "lon": 131.8850,
                "price_per_night": 4500,
                "kind": "guesthouse",
                "source": "mock"
            },
            {
                "id": "mock:lodge:andreevka",
                "name": "База отдыха «Бухта»",
                "settlement": "Андреевка",
                "lat": 42.6500,
                "lon": 130.9000,
                "price_per_night": 3800,
                "kind": "guesthouse",
                "source": "mock"
            }
        ]
    })
}

pub fn fallback_transport() -> Value {
    json!({
        "options": [
            {
                "id": "mock:trip:vlad-uss",
                "from": "Владивосток",
                "to": "Уссурийск",
                "mode": "bus",
                "depart": "08:00",
                "arrive": "10:30",
                "price_rub": 600,
                "source": "mock"
            }
        ]
    })
}

pub fn fallback_fuel() -> Value {
    json!({
        "stations": [
            {"id": "mock:fuel:1", "name": "АЗС на трассе", "lat": 43.3, "lon": 131.95, "source": "mock"},
            {"id": "mock:fuel:2", "name": "АЗС у Уссурийска", "lat": 43.8, "lon": 131.95, "source": "mock"},
            {"id": "mock:fuel:3", "name": "АЗС у Находки", "lat": 42.82, "lon": 132.87, "source": "mock"}
        ]
    })
}

pub fn fallback_events() -> Value {
    json!({
        "events": [
            {
                "id": "mock:event:1",
                "title": "Экскурсия по Владивостокской крепости",
                "date": "2026-10-24",
                "place": "Владивосток",
                "source": "mock"
            }
        ]
    })
}
