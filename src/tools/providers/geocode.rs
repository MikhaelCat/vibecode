use async_trait::async_trait;
use serde_json::{json, Value};

use crate::domain::facts::{SourceKind, ToolOutput};
use crate::tools::providers::{fallback_places, limit_from, load_mock};
use crate::tools::registry::{Tool, ToolCtx, ToolError};

pub struct GeocodeTool;

#[async_trait]
impl Tool for GeocodeTool {
    fn name(&self) -> &'static str {
        "geocode"
    }

    fn description(&self) -> &'static str {
        "Превратить название места в координаты. Только Приморский край."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {"type": "string"},
                "limit": {"type": "integer"}
            },
            "required": ["query"]
        })
    }

    async fn call(&self, args: Value, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let query = args
            .get("query")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::BadArgs("query is required".to_string()))?;
        let limit = limit_from(&args, 3, 10);

        let url = format!(
            "https://nominatim.openstreetmap.org/search?format=jsonv2&q={}&limit={limit}\
             &countrycodes=ru&viewbox=130.0,48.5,139.1,42.2&bounded=1&accept-language=ru",
            urlencoding(query)
        );

        if let Ok(response) = ctx
            .http
            .get(url)
            .header("User-Agent", &ctx.nominatim_user_agent)
            .send()
            .await
        {
            if response.status().is_success() {
                if let Ok(items) = response.json::<Vec<Value>>().await {
                    let places: Vec<Value> = items
                        .into_iter()
                        .filter_map(|item| {
                            let lat = item.get("lat")?.as_str()?.parse::<f64>().ok()?;
                            let lon = item.get("lon")?.as_str()?.parse::<f64>().ok()?;
                            let name = item.get("display_name")?.as_str()?;
                            let id = format!(
                                "nominatim:{}/{}",
                                item.get("osm_type")
                                    .and_then(Value::as_str)
                                    .unwrap_or("node"),
                                item.get("osm_id").and_then(Value::as_u64).unwrap_or(0)
                            );
                            Some(json!({
                                "id": id,
                                "name": name,
                                "lat": lat,
                                "lon": lon,
                                "kind": "place",
                                "opening_hours": null,
                                "source": "live"
                            }))
                        })
                        .take(limit)
                        .collect();
                    if !places.is_empty() {
                        return Ok(ToolOutput::ok(
                            json!({"places": places}),
                            SourceKind::Live,
                            "Nominatim",
                        ));
                    }
                }
            }
        }

        let mock = load_mock(ctx.data_dir.as_path(), "places.json", fallback_places());
        let needle = query.to_lowercase();
        let mut places: Vec<Value> = mock
            .get("places")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|place| {
                place
                    .get("name")
                    .and_then(Value::as_str)
                    .map(|name| name.to_lowercase().contains(&needle))
                    .unwrap_or(false)
            })
            .collect();
        if places.is_empty() {
            places = mock
                .get("places")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
        }
        places.truncate(limit);

        Ok(ToolOutput::ok(
            json!({"places": places}),
            SourceKind::Mock,
            "Nominatim недоступен: локальный справочник",
        ))
    }
}

fn urlencoding(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char)
            }
            b' ' => encoded.push_str("%20"),
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}
