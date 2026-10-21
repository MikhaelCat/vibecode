use async_trait::async_trait;
use serde_json::{json, Value};

use crate::domain::facts::{SourceKind, ToolOutput};
use crate::tools::providers::{fallback_places, load_mock};
use crate::tools::registry::{Tool, ToolCtx, ToolError};

/// Real attractions from Russian Wikipedia geosearch (the source Matvey prototyped
/// in the `matvey/tools.py` branch).
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
            "https://ru.wikipedia.org/w/api.php?action=query&list=geosearch\
             &gscoord={lat}%7C{lon}&gsradius={radius}&gslimit=8&format=json"
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

        let mock = load_mock(ctx.data_dir.as_path(), "places.json", fallback_places());
        let places = mock
            .get("places")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .take(4)
            .collect::<Vec<_>>();
        Ok(ToolOutput::ok(
            json!({"places": places}),
            SourceKind::Mock,
            "Википедия недоступна: демонстрационные места",
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
        let Some(lat) = item.get("lat").and_then(Value::as_f64) else {
            continue;
        };
        let Some(lon) = item.get("lon").and_then(Value::as_f64) else {
            continue;
        };
        let distance = item.get("dist").and_then(Value::as_f64).unwrap_or(0.0);
        places.push(json!({
            "id": format!("wikipedia:{page_id}"),
            "name": title,
            "lat": lat,
            "lon": lon,
            "kind": "sight",
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
}
