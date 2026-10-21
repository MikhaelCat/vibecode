use async_trait::async_trait;
use serde_json::{json, Value};

use crate::domain::facts::{SourceKind, ToolOutput};
use crate::tools::providers::{fallback_fuel, load_mock, PRIMORYE_BBOX};
use crate::tools::registry::{Tool, ToolCtx, ToolError};

pub struct FuelTool;

#[async_trait]
impl Tool for FuelTool {
    fn name(&self) -> &'static str {
        "find_fuel"
    }

    fn description(&self) -> &'static str {
        "АЗС вдоль маршрута. Станции из OSM; наличие топлива и цены не проверяются."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "route_points": {"type": "array", "items": {"type": "object"}},
                "radius_km": {"type": "number"}
            },
            "required": ["route_points"]
        })
    }

    async fn call(&self, _args: Value, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let query = format!(
            "[out:json][timeout:10];node[\"amenity\"=\"fuel\"]({PRIMORYE_BBOX});out center 30;"
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
                    let stations = parse_stations(&value);
                    if !stations.is_empty() {
                        return Ok(ToolOutput::ok(
                            json!({"stations": stations}),
                            SourceKind::Live,
                            "АЗС из OpenStreetMap",
                        ));
                    }
                }
            }
        }

        let mock = load_mock(ctx.data_dir.as_path(), "fuel.json", fallback_fuel());
        let stations = mock
            .get("stations")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        Ok(ToolOutput::ok(
            json!({"stations": stations}),
            SourceKind::Mock,
            "АЗС: демо-данные",
        ))
    }
}

fn parse_stations(value: &Value) -> Vec<Value> {
    let Some(elements) = value.get("elements").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut stations = Vec::new();
    for element in elements {
        let Some(lat) = element.get("lat").and_then(Value::as_f64) else {
            continue;
        };
        let Some(lon) = element.get("lon").and_then(Value::as_f64) else {
            continue;
        };
        let id = element
            .get("id")
            .and_then(Value::as_u64)
            .map(|id| format!("osm:node/{id}"))
            .unwrap_or_else(|| "osm:node/fuel".to_string());
        let name = element
            .get("tags")
            .and_then(|tags| tags.get("name"))
            .and_then(Value::as_str)
            .unwrap_or("АЗС");
        stations.push(json!({
            "id": id,
            "name": name,
            "lat": lat,
            "lon": lon,
            "source": "live"
        }));
    }
    stations
}
