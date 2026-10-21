use async_trait::async_trait;
use serde_json::{json, Value};

use crate::domain::facts::{SourceKind, ToolOutput};
use crate::tools::providers::{fallback_places, limit_from, load_mock, PRIMORYE_BBOX};
use crate::tools::registry::{Tool, ToolCtx, ToolError};

pub struct PlacesTool;

#[async_trait]
impl Tool for PlacesTool {
    fn name(&self) -> &'static str {
        "search_places"
    }

    fn description(&self) -> &'static str {
        "Найти достопримечательности, музеи и природные точки в Приморском крае. Возвращает place_id, координаты, часы работы."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "area": {"type": "string"},
                "categories": {"type": "array", "items": {"type": "string"}},
                "limit": {"type": "integer", "minimum": 1, "maximum": 40},
                "date": {"type": "string"}
            },
            "required": ["area"]
        })
    }

    async fn call(&self, args: Value, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let limit = limit_from(&args, 12, 40);
        let query = format!(
            "[out:json][timeout:10];\
             (\
               node[\"tourism\"]({bbox});\
               node[\"historic\"]({bbox});\
               node[\"leisure\"~\"park|nature_reserve\"]({bbox});\
               node[\"natural\"]({bbox});\
             );\
             out center 50;",
            bbox = PRIMORYE_BBOX
        );

        if let Ok(response) = ctx
            .http
            .post("https://overpass-api.de/api/interpreter")
            .body(query)
            .send()
            .await
        {
            if response.status().is_success() {
                if let Ok(value) = response.json::<Value>().await {
                    let mut places = parse_overpass(&value);
                    if !places.is_empty() {
                        places.truncate(limit);
                        return Ok(ToolOutput::ok(
                            json!({"places": places}),
                            SourceKind::Live,
                            format!("OSM Overpass: {} мест", places.len()),
                        ));
                    }
                }
            }
        }

        let mock = load_mock(ctx.data_dir.as_path(), "places.json", fallback_places());
        let mut places = mock
            .get("places")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        places.truncate(limit);
        Ok(ToolOutput::ok(
            json!({"places": places}),
            SourceKind::Mock,
            "OSM недоступен: демонстрационные места",
        ))
    }
}

fn parse_overpass(value: &Value) -> Vec<Value> {
    let Some(elements) = value.get("elements").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut places = Vec::new();
    for element in elements {
        let Some(tags) = element.get("tags") else {
            continue;
        };
        let name = tags
            .get("name:ru")
            .or_else(|| tags.get("name"))
            .and_then(Value::as_str);
        let Some(name) = name else {
            continue;
        };
        let lat = element.get("lat").and_then(Value::as_f64).or_else(|| {
            element
                .get("center")
                .and_then(|center| center.get("lat"))
                .and_then(Value::as_f64)
        });
        let lon = element.get("lon").and_then(Value::as_f64).or_else(|| {
            element
                .get("center")
                .and_then(|center| center.get("lon"))
                .and_then(Value::as_f64)
        });
        let (Some(lat), Some(lon)) = (lat, lon) else {
            continue;
        };
        let element_type = element
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("node");
        let id = element
            .get("id")
            .and_then(Value::as_u64)
            .map(|id| format!("osm:{element_type}/{id}"))
            .unwrap_or_else(|| format!("osm:{element_type}/{name}"));
        let kind = tags
            .get("tourism")
            .or_else(|| tags.get("historic"))
            .or_else(|| tags.get("leisure"))
            .or_else(|| tags.get("natural"))
            .and_then(Value::as_str)
            .unwrap_or("sight");
        let opening_hours = tags.get("opening_hours").and_then(Value::as_str);

        places.push(json!({
            "id": id,
            "name": name,
            "lat": lat,
            "lon": lon,
            "kind": kind,
            "opening_hours": opening_hours,
            "source": "live"
        }));
    }
    places
}
