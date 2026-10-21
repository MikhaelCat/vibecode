use async_trait::async_trait;
use serde_json::{json, Value};

use crate::domain::facts::{SourceKind, ToolOutput};
use crate::domain::geo::{haversine_km, travel_minutes};
use crate::domain::intent::TransportMode;
use crate::tools::registry::{Tool, ToolCtx, ToolError};

pub struct RouteTool;

#[async_trait]
impl Tool for RouteTool {
    fn name(&self) -> &'static str {
        "get_route"
    }

    fn description(&self) -> &'static str {
        "Расстояние, время в пути и геометрия между точками. points — массив объектов {lat, lon}. mode: car, foot, bike, transit."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "points": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "lat": {"type": "number"},
                            "lon": {"type": "number"}
                        },
                        "required": ["lat", "lon"]
                    }
                },
                "mode": {"type": "string", "enum": ["car", "foot", "bike", "transit"]}
            },
            "required": ["points"]
        })
    }

    async fn call(&self, args: Value, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let Some(points) = args.get("points").and_then(Value::as_array) else {
            return Err(ToolError::BadArgs("points is required".to_string()));
        };
        let coords: Vec<(f64, f64)> = points
            .iter()
            .filter_map(|point| Some((point.get("lat")?.as_f64()?, point.get("lon")?.as_f64()?)))
            .collect();
        if coords.len() < 2 {
            return Err(ToolError::BadArgs(
                "at least two points are required".to_string(),
            ));
        }

        let mode = args
            .get("mode")
            .and_then(Value::as_str)
            .map(mode_from_str)
            .unwrap_or(ctx.intent.transport);

        let from = format!("{:.4},{:.4}", coords[0].0, coords[0].1);
        let to = format!(
            "{:.4},{:.4}",
            coords[coords.len() - 1].0,
            coords[coords.len() - 1].1
        );

        if mode == TransportMode::Car {
            if let Some((distance_km, duration_min)) = osrm_route(&coords, ctx).await {
                return Ok(ToolOutput::ok(
                    json!({
                        "from": from,
                        "to": to,
                        "distance_km": distance_km,
                        "duration_min": duration_min,
                        "geometry": null,
                        "source": "live"
                    }),
                    SourceKind::Live,
                    "Маршрут OSRM",
                ));
            }
        }

        let distance: f64 = coords
            .windows(2)
            .map(|pair| haversine_km(pair[0], pair[1]) * 1.2)
            .sum();
        let distance_km = (distance * 10.0).round() / 10.0;
        let duration_min = travel_minutes(distance_km, mode);
        Ok(ToolOutput::ok(
            json!({
                "from": from,
                "to": to,
                "distance_km": distance_km,
                "duration_min": duration_min,
                "geometry": null,
                "source": "estimated"
            }),
            SourceKind::Estimated,
            format!("Маршрут оценён приблизительно ({mode})"),
        ))
    }
}

async fn osrm_route(coords: &[(f64, f64)], ctx: &ToolCtx) -> Option<(f64, u32)> {
    let path = coords
        .iter()
        .map(|(lat, lon)| format!("{lon},{lat}"))
        .collect::<Vec<_>>()
        .join(";");
    let url = format!("https://router.project-osrm.org/route/v1/driving/{path}?overview=false");
    let response = ctx.http.get(url).send().await.ok()?;
    if !response.status().is_success() {
        return None;
    }
    let value: Value = response.json().await.ok()?;
    let route = value.get("routes")?.as_array()?.first()?;
    let distance = route.get("distance")?.as_f64()? / 1000.0;
    let duration = route.get("duration")?.as_f64()? / 60.0;
    Some(((distance * 10.0).round() / 10.0, duration.ceil() as u32))
}

fn mode_from_str(value: &str) -> TransportMode {
    match value {
        "foot" => TransportMode::Foot,
        "bike" => TransportMode::Bike,
        "transit" => TransportMode::Transit,
        _ => TransportMode::Car,
    }
}
