use std::collections::HashSet;

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::domain::facts::{SourceKind, ToolOutput};
use crate::domain::geo::haversine_km;
use crate::tools::providers::{fallback_fuel, load_mock};
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

    async fn call(&self, args: Value, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let route = parse_route_points(&args)?;
        let radius_km = args
            .get("radius_km")
            .and_then(Value::as_f64)
            .unwrap_or(30.0)
            .clamp(1.0, 100.0);

        // Query only the route corridor. Asking for the whole krai and taking
        // the first 30 nodes spends the Overpass quota on stations that are
        // then discarded by `near_route`.
        let pad = radius_km / 111.0;
        let south = route.iter().map(|point| point.0).fold(f64::MAX, f64::min) - pad;
        let north = route.iter().map(|point| point.0).fold(f64::MIN, f64::max) + pad;
        let west = route.iter().map(|point| point.1).fold(f64::MAX, f64::min) - pad * 1.4;
        let east = route.iter().map(|point| point.1).fold(f64::MIN, f64::max) + pad * 1.4;
        let query = format!(
            "[out:json][timeout:10];node[\"amenity\"=\"fuel\"]({south:.4},{west:.4},{north:.4},{east:.4});out 100;"
        );
        let _permit = crate::tools::providers::OVERPASS_SLOTS.acquire().await.ok();
        if let Ok(response) = ctx
            .http
            .post("https://overpass-api.de/api/interpreter")
            .body(query)
            .send()
            .await
        {
            if response.status().is_success() {
                if let Ok(value) = response.json::<Value>().await {
                    let stations = near_route(parse_stations(&value), &route, radius_km);
                    if !stations.is_empty() {
                        return Ok(ToolOutput::ok(
                            json!({"stations": stations}),
                            SourceKind::Live,
                            format!("АЗС из OpenStreetMap: {} рядом с маршрутом", stations.len()),
                        ));
                    }
                }
            }
        }

        let mock = load_mock(ctx.data_dir.as_path(), "fuel.json", fallback_fuel());
        let stations = near_route(
            mock.get("stations")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default(),
            &route,
            radius_km,
        );
        let summary = if stations.is_empty() {
            "АЗС: рядом с маршрутом нет станций в локальном справочнике"
        } else {
            "АЗС: локальный справочник"
        };
        Ok(ToolOutput::ok(
            json!({"stations": stations}),
            SourceKind::Mock,
            summary,
        ))
    }
}

fn parse_route_points(args: &Value) -> Result<Vec<(f64, f64)>, ToolError> {
    let Some(points) = args.get("route_points").and_then(Value::as_array) else {
        return Err(ToolError::BadArgs("route_points is required".to_string()));
    };
    let route: Vec<(f64, f64)> = points
        .iter()
        .filter_map(|point| Some((point.get("lat")?.as_f64()?, point.get("lon")?.as_f64()?)))
        .collect();
    if route.is_empty() {
        return Err(ToolError::BadArgs(
            "route_points must contain at least one {lat, lon}".to_string(),
        ));
    }
    Ok(route)
}

fn near_route(mut stations: Vec<Value>, route: &[(f64, f64)], radius_km: f64) -> Vec<Value> {
    stations.retain(|station| {
        let (Some(lat), Some(lon)) = (
            station.get("lat").and_then(Value::as_f64),
            station.get("lon").and_then(Value::as_f64),
        ) else {
            return false;
        };
        route
            .iter()
            .any(|point| haversine_km((lat, lon), *point) <= radius_km)
    });
    stations.truncate(10);
    stations
}

fn parse_stations(value: &Value) -> Vec<Value> {
    let Some(elements) = value.get("elements").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut stations = Vec::new();
    let mut seen = HashSet::new();
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
        if !seen.insert(id.clone()) {
            continue;
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_only_stations_near_the_route() {
        let stations = vec![
            json!({"id": "near", "lat": 43.12, "lon": 131.89}),
            json!({"id": "far", "lat": 44.55, "lon": 135.57}),
        ];
        let filtered = near_route(stations, &[(43.1155, 131.8855)], 30.0);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0]["id"], "near");
    }
}
