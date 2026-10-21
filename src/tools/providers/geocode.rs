use async_trait::async_trait;
use serde_json::{json, Value};

use crate::domain::facts::{SourceKind, ToolOutput};
use crate::tools::providers::geo;
use crate::tools::providers::{fallback_places, limit_from, load_mock};
use crate::tools::registry::{Tool, ToolCtx, ToolError};

pub struct GeocodeTool;

#[async_trait]
impl Tool for GeocodeTool {
    fn name(&self) -> &'static str {
        "geocode"
    }

    fn description(&self) -> &'static str {
        "Превратить название места или района в координаты и bbox. Только Приморский край."
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

        let places = geo::nominatim(ctx, query, limit).await;
        if !places.is_empty() {
            return Ok(ToolOutput::ok(
                json!({"places": places}),
                SourceKind::Live,
                "Nominatim",
            ));
        }

        let places = geo::photon(ctx, query, limit).await;
        if !places.is_empty() {
            return Ok(ToolOutput::ok(
                json!({"places": places}),
                SourceKind::Live,
                "Photon/OSM",
            ));
        }

        // Never return unrelated coordinates for a geocoding query. The local
        // catalogue is used only when a place name actually matches.
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
        places.truncate(limit);

        let summary = if places.is_empty() {
            "Nominatim/Photon недоступны: в локальном справочнике нет совпадений"
        } else {
            "Nominatim/Photon недоступны: совпадения из локального справочника"
        };
        Ok(ToolOutput::ok(
            json!({"places": places}),
            SourceKind::Mock,
            summary,
        ))
    }
}
