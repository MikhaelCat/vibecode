use async_trait::async_trait;
use serde_json::{json, Value};

use crate::domain::facts::{SourceKind, ToolOutput};
use crate::domain::geo::{haversine_km, on_land, waterbody_title};
use crate::tools::providers::places::{generic_title, kind_from_title};
use crate::tools::providers::{fallback_places, load_mock};
use crate::tools::registry::{Tool, ToolCtx, ToolError};

/// Real attractions from Russian Wikipedia geosearch.
pub struct WikipediaTool;

#[async_trait]
impl Tool for WikipediaTool {
    fn name(&self) -> &'static str {
        "get_nearby_attractions"
    }

    fn description(&self) -> &'static str {
        "Реальные достопримечательности вокруг координат из русской Википедии (geosearch). Возвращает place_id, название, координаты и расстояние."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "lat": {"type": "number"},
                "lon": {"type": "number"},
                "radius_meters": {"type": "integer", "minimum": 1000, "maximum": 10000}
            },
            "required": ["lat", "lon"]
        })
    }

    async fn call(&self, args: Value, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let lat = args
            .get("lat")
            .and_then(Value::as_f64)
            .ok_or_else(|| ToolError::BadArgs("lat is required".to_string()))?;
        let lon = args
            .get("lon")
            .and_then(Value::as_f64)
            .ok_or_else(|| ToolError::BadArgs("lon is required".to_string()))?;
        let radius = args
            .get("radius_meters")
            .and_then(Value::as_u64)
            .unwrap_or(10_000)
            .clamp(1_000, 10_000);

        let url = format!(
            "https://ru.wikipedia.org/w/api.php?action=query&list=geosearch&gscoord={lat}%7C{lon}&gsradius={radius}&gslimit=8&format=json"
        );

        if let Ok(response) = ctx
            .http
            .get(url)
            .header("User-Agent", &ctx.nominatim_user_agent)
            .send()
            .await
        {
            if response.status().is_success() {
                if let Ok(value) = response.json::<Value>().await {
                    let places = parse_geosearch(&value);
                    if !places.is_empty() {
                        return Ok(ToolOutput::ok(
                            json!({"places": places}),
                            SourceKind::Live,
                            format!("Википедия: {} объектов рядом", places.len()),
                        ));
                    }
                }
            }
        }

        // Only return local catalogue entries that are actually near the point.
        let mock = load_mock(ctx.data_dir.as_path(), "places.json", fallback_places());
        let radius_km = radius as f64 / 1000.0;
        let places: Vec<Value> = mock
            .get("places")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|place| {
                let Some(place_lat) = place.get("lat").and_then(Value::as_f64) else {
                    return false;
                };
                let Some(place_lon) = place.get("lon").and_then(Value::as_f64) else {
                    return false;
                };
                let name = place.get("name").and_then(Value::as_str).unwrap_or("");
                !waterbody_title(name)
                    && on_land(place_lat, place_lon)
                    && haversine_km((lat, lon), (place_lat, place_lon)) <= radius_km
            })
            .take(8)
            .collect();
        let summary = if places.is_empty() {
            "Википедия недоступна: рядом нет локальных данных"
        } else {
            "Википедия недоступна: локальные места рядом"
        };
        Ok(ToolOutput::ok(
            json!({"places": places}),
            SourceKind::Mock,
            summary,
        ))
    }
}

fn parse_geosearch(value: &Value) -> Vec<Value> {
    let Some(items) = value
        .get("query")
        .and_then(|query| query.get("geosearch"))
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };
    let mut places = Vec::new();
    for item in items {
        let Some(page_id) = item.get("pageid").and_then(Value::as_u64) else {
            continue;
        };
        let Some(title) = item.get("title").and_then(Value::as_str) else {
            continue;
        };
        if generic_title(title) || waterbody_title(title) {
            continue;
        }
        let Some(lat) = item.get("lat").and_then(Value::as_f64) else {
            continue;
        };
        let Some(lon) = item.get("lon").and_then(Value::as_f64) else {
            continue;
        };
        if !on_land(lat, lon) {
            continue;
        }
        let distance = item.get("dist").and_then(Value::as_f64).unwrap_or(0.0);
        places.push(json!({
            "id": format!("wikipedia:{page_id}"),
            "name": title,
            "lat": lat,
            "lon": lon,
            "kind": kind_from_title(title),
            "opening_hours": null,
            "source": "live",
            "distance_meters": distance.round()
        }));
    }
    places
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_geosearch_items() {
        let payload = serde_json::json!({
            "query": {
                "geosearch": [{
                    "pageid": 123,
                    "title": "Токаревская кошка",
                    "lat": 43.07,
                    "lon": 131.84,
                    "dist": 1500.4
                }]
            }
        });
        let places = parse_geosearch(&payload);
        assert_eq!(places.len(), 1);
        assert_eq!(places[0]["id"], "wikipedia:123");
        assert_eq!(places[0]["name"], "Токаревская кошка");
        assert_eq!(places[0]["source"], "live");
    }

    #[test]
    fn skips_water_centroids_and_generic_titles() {
        let payload = serde_json::json!({
            "query": {
                "geosearch": [
                    {"pageid": 1, "title": "Находка (бухта, Японское море)", "lat": 42.80, "lon": 132.89, "dist": 10.0},
                    {"pageid": 2, "title": "Улица Ленина", "lat": 42.82, "lon": 132.89, "dist": 20.0},
                    {"pageid": 3, "title": "Смотровая площадка «Бухта Находка»", "lat": 42.816, "lon": 132.889, "dist": 30.0}
                ]
            }
        });
        let places = parse_geosearch(&payload);
        assert_eq!(places.len(), 1);
        assert_eq!(places[0]["name"], "Смотровая площадка «Бухта Находка»");
    }
}
