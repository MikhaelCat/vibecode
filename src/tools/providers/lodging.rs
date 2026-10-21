use std::collections::HashSet;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::domain::facts::{SourceKind, ToolOutput};
use crate::domain::geo::haversine_km;
use crate::tools::providers::geo;
use crate::tools::providers::{fallback_lodging, limit_from, load_mock};
use crate::tools::registry::{Tool, ToolCtx, ToolError};

pub struct LodgingTool;

#[async_trait]
impl Tool for LodgingTool {
    fn name(&self) -> &'static str {
        "find_lodging"
    }

    fn description(&self) -> &'static str {
        "Варианты ночёвки в любом населённом пункте или рядом с ориентиром Приморья (маяк, пляж, озеро): локальные цены или отели/гостевые дома из OSM. Наличие и цены нужно проверять перед выездом."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "settlement": {"type": "string"},
                "check_in": {"type": "string"},
                "check_out": {"type": "string"},
                "party": {"type": "integer"},
                "limit": {"type": "integer"}
            },
            "required": ["settlement"]
        })
    }

    async fn call(&self, args: Value, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let settlement = args
            .get("settlement")
            .and_then(Value::as_str)
            .unwrap_or("Владивосток")
            .trim()
            .to_string();
        let normalized = settlement.to_lowercase();
        let limit = limit_from(&args, 3, 10);

        let mock = load_mock(ctx.data_dir.as_path(), "lodging.json", fallback_lodging());
        let mut local: Vec<Value> = mock
            .get("options")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|option| {
                option
                    .get("settlement")
                    .and_then(Value::as_str)
                    .map(|value| value.to_lowercase().contains(&normalized))
                    .unwrap_or(false)
            })
            .collect();
        local.truncate(limit);
        if !local.is_empty() {
            return Ok(ToolOutput::ok(
                json!({"options": local, "area": settlement}),
                SourceKind::Mock,
                format!("Жильё: {} вариантов (локальные цены)", local.len()),
            ));
        }

        let Some(area) = geo::resolve_area(ctx, &settlement).await else {
            return Ok(ToolOutput::ok(
                json!({"options": [], "area": settlement}),
                SourceKind::Stale,
                format!("Жильё: не удалось определить «{settlement}»"),
            ));
        };

        let options = overpass_lodging(ctx, &area, &settlement, limit).await;
        let summary = if options.is_empty() {
            format!("Жильё в {} не найдено в OSM", area.label)
        } else {
            format!("Жильё в {}: {} вариантов из OSM", area.label, options.len())
        };
        Ok(ToolOutput::ok(
            json!({"options": options, "area": area.label}),
            SourceKind::Live,
            summary,
        ))
    }
}

async fn overpass_lodging(
    ctx: &ToolCtx,
    area: &geo::Area,
    settlement: &str,
    limit: usize,
) -> Vec<Value> {
    let (south, west, north, east) = area.bbox;
    let query = format!(
        "[out:json][timeout:8];\
         (nwr[\"tourism\"~\"hotel|guest_house|hostel|apartment|camp_site|chalet\"]({south:.4},{west:.4},{north:.4},{east:.4}););\
         out center 40;"
    );
    let _permit = match crate::tools::providers::OVERPASS_SLOTS.acquire().await {
        Ok(permit) => permit,
        Err(_) => return Vec::new(),
    };
    let response = match tokio::time::timeout(
        Duration::from_secs(9),
        ctx.http
            .post("https://overpass-api.de/api/interpreter")
            .body(query)
            .send(),
    )
    .await
    {
        Ok(Ok(response)) if response.status().is_success() => response,
        _ => return Vec::new(),
    };
    let Ok(value) = response.json::<Value>().await else {
        return Vec::new();
    };
    let Some(elements) = value.get("elements").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut options = Vec::new();
    let mut seen = HashSet::new();
    for element in elements {
        let Some(tags) = element.get("tags") else {
            continue;
        };
        let Some(name) = tags
            .get("name:ru")
            .or_else(|| tags.get("name"))
            .and_then(Value::as_str)
        else {
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
        if !seen.insert(id.clone()) {
            continue;
        }
        let kind = match tags.get("tourism").and_then(Value::as_str) {
            Some("hotel") => "hotel",
            Some("hostel") => "hostel",
            Some("apartment") => "apartment",
            _ => "guesthouse",
        };
        options.push(json!({
            "id": id,
            "name": name,
            "settlement": settlement,
            "lat": lat,
            "lon": lon,
            "price_per_night": 0,
            "kind": kind,
            "source": "live",
            "distance_meters": (haversine_km((area.lat, area.lon), (lat, lon)) * 1000.0).round()
        }));
    }
    options.sort_by(|a, b| {
        let left = a
            .get("distance_meters")
            .and_then(Value::as_f64)
            .unwrap_or(f64::MAX);
        let right = b
            .get("distance_meters")
            .and_then(Value::as_f64)
            .unwrap_or(f64::MAX);
        left.partial_cmp(&right)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    options.truncate(limit);
    options
}
