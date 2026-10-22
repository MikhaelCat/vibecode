use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::domain::facts::{SourceKind, ToolOutput};
use crate::domain::geo::{haversine_km, travel_minutes};
use crate::domain::intent::TransportMode;
use crate::tools::registry::{Tool, ToolCtx, ToolError};

/// OSRM is often unreachable from dev networks, so keep this request short;
/// the mock-geometry fallback must still fit inside the tool timeout.
const OSRM_TIMEOUT: Duration = Duration::from_secs(4);

pub struct RouteTool;

#[async_trait]
impl Tool for RouteTool {
    fn name(&self) -> &'static str {
        "get_route"
    }

    fn description(&self) -> &'static str {
        "Расстояние, время в пути и геометрия маршрута (линия для карты: OSRM для car, иначе мок-оценка) между точками. points — массив объектов {lat, lon}. mode: car, foot, bike, transit."
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

        let from = latlon(coords[0]);
        let to = latlon(coords[coords.len() - 1]);

        if mode == TransportMode::Car {
            if let Some(route) = osrm_route(&coords, ctx).await {
                let mut data = json!({
                    "from": from,
                    "to": to,
                    "distance_km": route.distance_km,
                    "duration_min": route.duration_min,
                    "geometry": route.geometry,
                    "source": "live"
                });
                // Per-leg numbers are optional extra detail; existing consumers
                // only read the top-level fields and ignore unknown keys.
                if coords.len() > 2 && route.legs.len() == coords.len() - 1 {
                    data["legs"] = Value::Array(
                        coords
                            .windows(2)
                            .zip(route.legs.iter())
                            .map(|(pair, leg)| {
                                json!({
                                    "from": latlon(pair[0]),
                                    "to": latlon(pair[1]),
                                    "distance_km": leg.distance_km,
                                    "duration_min": leg.duration_min
                                })
                            })
                            .collect(),
                    );
                }
                return Ok(ToolOutput::ok(data, SourceKind::Live, "Маршрут OSRM"));
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
                "geometry": mock_geometry(&coords),
                "source": "estimated"
            }),
            SourceKind::Estimated,
            format!("Маршрут оценён приблизительно ({mode}): геометрия — мок"),
        ))
    }
}

/// A route parsed from an OSRM response; geometry is a compact GeoJSON string.
struct OsrmRoute {
    distance_km: f64,
    duration_min: u32,
    geometry: String,
    legs: Vec<OsrmLeg>,
}

struct OsrmLeg {
    distance_km: f64,
    duration_min: u32,
}

async fn osrm_route(coords: &[(f64, f64)], ctx: &ToolCtx) -> Option<OsrmRoute> {
    let path = coords
        .iter()
        .map(|(lat, lon)| format!("{lon},{lat}"))
        .collect::<Vec<_>>()
        .join(";");
    let url = format!(
        "https://router.project-osrm.org/route/v1/driving/{path}?overview=full&geometries=geojson"
    );
    let response = ctx.http.get(url).timeout(OSRM_TIMEOUT).send().await.ok()?;
    if !response.status().is_success() {
        return None;
    }
    let value: Value = response.json().await.ok()?;
    parse_osrm(&value)
}

/// Parses `routes[0]` of an OSRM response. A route without the GeoJSON
/// geometry is rejected so the caller falls back to a mock line instead of
/// returning a leg that cannot be drawn.
fn parse_osrm(value: &Value) -> Option<OsrmRoute> {
    let route = value.get("routes")?.as_array()?.first()?;
    let distance_km = round1(route.get("distance")?.as_f64()? / 1000.0);
    let duration_min = ceil_minutes(route.get("duration")?.as_f64()?);
    let geometry = route.get("geometry")?;
    if !geometry.is_object() {
        return None;
    }
    let legs = route
        .get("legs")
        .and_then(Value::as_array)
        .map(|legs| {
            legs.iter()
                .filter_map(|leg| {
                    Some(OsrmLeg {
                        distance_km: round1(leg.get("distance")?.as_f64()? / 1000.0),
                        duration_min: ceil_minutes(leg.get("duration")?.as_f64()?),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Some(OsrmRoute {
        distance_km,
        duration_min,
        geometry: serde_json::to_string(geometry).ok()?,
        legs,
    })
}

/// Builds a mock GeoJSON LineString through the requested points with two
/// interpolated midpoints per leg, so the plan always carries a drawable line
/// even when OSRM is unreachable. Coordinates are `[lon, lat]` per GeoJSON.
fn mock_geometry(coords: &[(f64, f64)]) -> String {
    let mut coordinates: Vec<Value> = Vec::with_capacity(1 + 3 * coords.len().saturating_sub(1));
    if let Some((lat, lon)) = coords.first() {
        coordinates.push(json!([lon, lat]));
    }
    for pair in coords.windows(2) {
        let (from, to) = (pair[0], pair[1]);
        for fraction in [1.0 / 3.0, 2.0 / 3.0] {
            let lat = round6(from.0 + (to.0 - from.0) * fraction);
            let lon = round6(from.1 + (to.1 - from.1) * fraction);
            coordinates.push(json!([lon, lat]));
        }
        coordinates.push(json!([to.1, to.0]));
    }
    serde_json::to_string(&json!({"type": "LineString", "coordinates": coordinates}))
        .unwrap_or_default()
}

fn latlon(point: (f64, f64)) -> String {
    format!("{:.4},{:.4}", point.0, point.1)
}

fn round1(value: f64) -> f64 {
    (value * 10.0).round() / 10.0
}

fn round6(value: f64) -> f64 {
    (value * 1_000_000.0).round() / 1_000_000.0
}

fn ceil_minutes(seconds: f64) -> u32 {
    (seconds / 60.0).ceil() as u32
}

fn mode_from_str(value: &str) -> TransportMode {
    match value {
        "foot" => TransportMode::Foot,
        "bike" => TransportMode::Bike,
        "transit" => TransportMode::Transit,
        _ => TransportMode::Car,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Cut-down real OSRM payload: two points, one leg, a small LineString.
    const OSRM_FIXTURE: &str = r#"{
        "code": "Ok",
        "routes": [
            {
                "distance": 12345.6,
                "duration": 894.2,
                "geometry": {
                    "type": "LineString",
                    "coordinates": [[131.8855, 43.1155], [131.905, 43.125], [131.92, 43.13]]
                },
                "legs": [{"distance": 12345.6, "duration": 894.2}]
            }
        ]
    }"#;

    #[test]
    fn parses_osrm_payload_into_distance_duration_and_geometry() {
        let value: Value = serde_json::from_str(OSRM_FIXTURE).expect("fixture must be valid JSON");
        let route = parse_osrm(&value).expect("fixture must parse");
        assert_eq!(route.distance_km, 12.3);
        assert_eq!(route.duration_min, 15); // 894.2 s = 14.9 min, rounded up
        assert_eq!(route.legs.len(), 1);
        assert_eq!(route.legs[0].distance_km, 12.3);

        let geometry: Value =
            serde_json::from_str(&route.geometry).expect("geometry must be valid JSON");
        assert_eq!(geometry["type"], "LineString");
        assert_eq!(
            geometry["coordinates"].as_array().map(Vec::len),
            Some(3),
            "the LineString must keep all fixture coordinates"
        );
    }

    #[test]
    fn rejects_osrm_route_without_geometry() {
        let value = json!({"routes": [{"distance": 1000.0, "duration": 60.0}]});
        assert!(parse_osrm(&value).is_none());
    }

    #[test]
    fn mock_geometry_draws_one_line_through_the_inputs() {
        let coords = [(43.1155, 131.8855), (43.2, 132.0), (43.3, 132.2)];
        let line: Value = serde_json::from_str(&mock_geometry(&coords)).expect("valid JSON");
        assert_eq!(line["type"], "LineString");
        let coordinates = line["coordinates"].as_array().expect("coordinates array");

        // First and last coordinates equal the requested points ([lon, lat]).
        assert_eq!(
            coordinates.first().and_then(|point| point[0].as_f64()),
            Some(131.8855)
        );
        assert_eq!(
            coordinates.first().and_then(|point| point[1].as_f64()),
            Some(43.1155)
        );
        assert_eq!(
            coordinates.last().and_then(|point| point[0].as_f64()),
            Some(132.2)
        );
        assert_eq!(
            coordinates.last().and_then(|point| point[1].as_f64()),
            Some(43.3)
        );
        // One shared start plus two midpoints and one endpoint per leg.
        assert_eq!(coordinates.len(), 1 + 3 * (coords.len() - 1));
    }
}
