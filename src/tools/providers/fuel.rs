use std::collections::HashSet;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::domain::facts::{SourceKind, ToolOutput};
use crate::domain::geo::haversine_km;
use crate::tools::providers::{fallback_fuel, load_mock, osmmap, overpass};
use crate::tools::registry::{Tool, ToolCtx, ToolError};

pub struct FuelTool;

#[async_trait]
impl Tool for FuelTool {
    fn name(&self) -> &'static str {
        "find_fuel"
    }

    fn description(&self) -> &'static str {
        "АЗС вдоль маршрута. Координаты станций — OpenStreetMap; наличие топлива и цены — демо-оценки (не live-данные)."
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

        // Coordinates come from OSM, prices from the demo catalogue. The map
        // tiles around the route stops are the source that actually answers: a
        // station pinned 500 metres from where it stands is worse than none,
        // and that is what the catalogue's approximate coordinates did.
        let deadline = Instant::now() + FUEL_BUDGET;
        let wanted = vec!["fuel".to_string()];
        let mut stations: Vec<Value> = Vec::new();
        for (lat, lon) in sample_route(&route, MAX_TILES) {
            if Instant::now() >= deadline {
                break;
            }
            let tile = osmmap::tile(
                ctx,
                lat,
                lon,
                &wanted,
                deadline
                    .saturating_duration_since(Instant::now())
                    .min(TILE_TIMEOUT),
            )
            .await;
            for poi in tile {
                stations.push(poi_to_station(&poi));
            }
        }

        // One Overpass request covers the whole corridor, including the
        // stations between two stops. It is the slowest source, so it is asked
        // only while the tiles have not already found the corridor.
        if stations.len() < MIN_LIVE_STATIONS && Instant::now() < deadline {
            if let Some(value) = overpass::query(
                ctx.http.clone(),
                &corridor_query(&route, radius_km),
                deadline,
                OVERPASS_BUDGET,
            )
            .await
            {
                stations.extend(parse_stations(&value));
            }
        }

        let mut stations = near_route(dedupe_stations(stations), &route, radius_km);
        // The catalogue covers what tiles cannot: a tile sees one neighbourhood,
        // and a route leaves it in a minute. It is topped up only while the live
        // walk came back thin, and every row of it stands on the OSM station it
        // names (`osm_node`), so the pins land on real stations either way.
        if stations.len() < MIN_STATIONS {
            let mock = load_mock(ctx.data_dir.as_path(), "fuel.json", fallback_fuel());
            stations.extend(
                mock.get("stations")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default(),
            );
            stations = near_route(dedupe_stations(stations), &route, radius_km);
        }
        let stations = attach_demo_prices(stations, &demo_prices(ctx));
        let live = stations
            .iter()
            .any(|station| station.get("source").and_then(Value::as_str) == Some("live"));
        let summary = if stations.is_empty() {
            "АЗС: рядом с маршрутом нет станций".to_string()
        } else if live {
            format!(
                "АЗС из OpenStreetMap: {} рядом с маршрутом (координаты — OSM, наличие и цены — демо-оценки)",
                stations.len()
            )
        } else {
            format!(
                "АЗС вдоль маршрута: {} — координаты станций из OSM, цены демо-оценки",
                stations.len()
            )
        };
        Ok(ToolOutput::ok(
            json!({"stations": stations}),
            if live {
                SourceKind::Live
            } else {
                SourceKind::Mock
            },
            summary,
        ))
    }
}

/// Time the live fuel sources may spend; the tool itself is cut off at
/// TOOL_TIMEOUT_SECS (15 s).
const FUEL_BUDGET: Duration = Duration::from_secs(10);
const OVERPASS_BUDGET: Duration = Duration::from_secs(7);
const TILE_TIMEOUT: Duration = Duration::from_secs(4);

/// Route stops whose tiles are fetched. More than a handful of tiles costs
/// more than it finds: stations repeat along a city route.
const MAX_TILES: usize = 6;

/// Fewer stations than this and the route is worth topping up from the local
/// catalogue, which knows the region's stations.
const MIN_STATIONS: usize = 4;

/// Fewer live stations than this and the whole corridor is worth one Overpass
/// request.
const MIN_LIVE_STATIONS: usize = 3;

/// The stops of the route, evenly thinned to `max` when the model sent more.
fn sample_route(route: &[(f64, f64)], max: usize) -> Vec<(f64, f64)> {
    if route.len() <= max {
        return route.to_vec();
    }
    let step = route.len() as f64 / max as f64;
    (0..max)
        .filter_map(|index| route.get((index as f64 * step) as usize).copied())
        .collect()
}

/// The Overpass query for the whole route corridor.
fn corridor_query(route: &[(f64, f64)], radius_km: f64) -> String {
    // Query only the route corridor. Asking for the whole krai and taking the
    // first 30 nodes spends the Overpass quota on stations that are then
    // discarded by `near_route`.
    let pad = radius_km / 111.0;
    let south = route.iter().map(|point| point.0).fold(f64::MAX, f64::min) - pad;
    let north = route.iter().map(|point| point.0).fold(f64::MIN, f64::max) + pad;
    let west = route.iter().map(|point| point.1).fold(f64::MAX, f64::min) - pad * 1.4;
    let east = route.iter().map(|point| point.1).fold(f64::MIN, f64::max) + pad * 1.4;
    format!(
        "[out:json][timeout:10];node[\"amenity\"=\"fuel\"]({south:.4},{west:.4},{north:.4},{east:.4});out 100;"
    )
}

/// One map-API station as a fuel row.
fn poi_to_station(poi: &osmmap::Poi) -> Value {
    json!({
        "id": poi.id,
        "name": poi.name,
        "brand": poi.brand,
        "lat": poi.lat,
        "lon": poi.lon,
        "source": "live",
        // OSM publishes neither availability nor prices; the demo estimate is
        // attached by brand below and labelled as one.
        "fuel": [],
        "price_ai92": null,
        "price_ai95": null,
        "price_dt": null,
        "price_note": "демо-оценка"
    })
}

/// One row per station: tiles and Overpass return the same node, and a
/// catalogue row repeats the station it was snapped to, so both its own id and
/// its `osm_node` count as known.
fn dedupe_stations(stations: Vec<Value>) -> Vec<Value> {
    let mut kept: Vec<Value> = Vec::new();
    let mut ids: HashSet<String> = HashSet::new();
    let mut names: Vec<(String, f64, f64)> = Vec::new();
    for station in stations {
        let (Some(lat), Some(lon)) = (
            station.get("lat").and_then(Value::as_f64),
            station.get("lon").and_then(Value::as_f64),
        ) else {
            continue;
        };
        let mut known = false;
        if let Some(id) = station.get("id").and_then(Value::as_str) {
            if !ids.insert(id.to_string()) {
                known = true;
            }
        }
        if let Some(node) = station.get("osm_node").and_then(Value::as_str) {
            // The catalogue stores "node/123"; live rows use "osm:node/123".
            if !ids.insert(format!("osm:{node}")) {
                known = true;
            }
        }
        if known {
            continue;
        }
        let name = station
            .get("name")
            .and_then(Value::as_str)
            .map(crate::tools::providers::normalize_name)
            .unwrap_or_default();
        if !name.is_empty()
            && names.iter().any(|(known, known_lat, known_lon)| {
                *known == name && haversine_km((lat, lon), (*known_lat, *known_lon)) < 0.3
            })
        {
            continue;
        }
        names.push((name, lat, lon));
        kept.push(station);
    }
    kept
}

/// Demo prices by network, read from the local catalogue. The catalogue is the
/// only place fuel prices exist at all: OSM does not publish them, and a price
/// the app shows is an estimate for that brand, never a price tag of that
/// station.
fn demo_prices(ctx: &ToolCtx) -> Vec<(String, Value)> {
    let mock = load_mock(ctx.data_dir.as_path(), "fuel.json", fallback_fuel());
    let mut prices: Vec<(String, Value)> = Vec::new();
    for station in mock
        .get("stations")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(brand) = station
            .get("name")
            .and_then(Value::as_str)
            .and_then(brand_in_name)
        else {
            continue;
        };
        if prices.iter().any(|(known, _)| *known == brand) {
            continue;
        }
        prices.push((
            brand,
            json!({
                "fuel": station.get("fuel").cloned().unwrap_or(json!([])),
                "price_ai92": station.get("price_ai92").cloned().unwrap_or(Value::Null),
                "price_ai95": station.get("price_ai95").cloned().unwrap_or(Value::Null),
                "price_dt": station.get("price_dt").cloned().unwrap_or(Value::Null),
                "price_note": station.get("price_note").cloned().unwrap_or(json!("демо-оценка")),
            }),
        ));
    }
    prices
}

/// Attaches the brand's demo estimate to a station that was found live.
fn attach_demo_prices(mut stations: Vec<Value>, prices: &[(String, Value)]) -> Vec<Value> {
    for station in stations.iter_mut() {
        if station.get("source").and_then(Value::as_str) != Some("live") {
            continue;
        }
        let Some(brand) = station
            .get("brand")
            .and_then(Value::as_str)
            .or_else(|| station.get("name").and_then(Value::as_str))
        else {
            continue;
        };
        let Some((_, estimate)) = prices
            .iter()
            .find(|(known, _)| brand.to_lowercase().contains(&known.to_lowercase()))
        else {
            continue;
        };
        if let (Some(object), Some(estimate)) = (station.as_object_mut(), estimate.as_object()) {
            for (key, value) in estimate {
                object.insert(key.clone(), value.clone());
            }
        }
    }
    stations
}

/// The network a catalogue name belongs to: «Роснефть» in
/// "АЗС «Роснефть», ул. Коммунаров".
fn brand_in_name(name: &str) -> Option<String> {
    let start = name.find('«')? + '«'.len_utf8();
    let end = name[start..].find('»')? + start;
    Some(name[start..end].trim().to_string())
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
            "source": "live",
            // OSM has no fuel availability or prices; leave them empty so no
            // consumer can mistake nulls for real values.
            "fuel": [],
            "price_ai92": null,
            "price_ai95": null,
            "price_dt": null
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

    #[test]
    fn a_long_route_is_thinned_to_the_tile_budget() {
        let route: Vec<(f64, f64)> = (0..20)
            .map(|index| (43.0 + index as f64 / 100.0, 131.9))
            .collect();
        let sampled = sample_route(&route, 6);
        assert_eq!(sampled.len(), 6);
        assert_eq!(sampled[0], route[0]);
        assert!(sampled[5].0 > sampled[0].0 && sampled[5].0 <= route[19].0);
        // A short route is used as it came.
        assert_eq!(sample_route(&route[..3], 6).len(), 3);
    }

    #[test]
    fn a_live_station_keeps_its_coordinates_and_gets_a_labelled_estimate() {
        let prices = vec![(
            "Роснефть".to_string(),
            json!({
                "fuel": ["АИ-92", "АИ-95", "ДТ"],
                "price_ai92": 62,
                "price_ai95": 67,
                "price_dt": 74,
                "price_note": "демо-оценка"
            }),
        )];
        let stations = attach_demo_prices(
            vec![
                json!({"id": "osm:node/1", "name": "АЗС № 432 Роснефть", "brand": "Роснефть", "lat": 43.1, "lon": 131.9, "source": "live", "price_ai95": null}),
                // An unknown network keeps empty prices: no estimate is invented.
                json!({"id": "osm:node/2", "name": "АЗС Незнакомая", "lat": 43.2, "lon": 131.8, "source": "live", "price_ai95": null}),
                // A catalogue row already carries its own demo prices.
                json!({"id": "mock:fuel/1", "name": "АЗС «Роснефть», ул. Тестовая", "lat": 43.3, "lon": 131.7, "source": "mock", "price_ai95": 66}),
            ],
            &prices,
        );
        assert_eq!(stations[0]["price_ai95"], 67);
        assert_eq!(stations[0]["price_note"], "демо-оценка");
        assert_eq!(stations[0]["lat"], 43.1);
        assert!(stations[1]["price_ai95"].is_null());
        assert_eq!(stations[2]["price_ai95"], 66);
    }

    #[test]
    fn network_names_come_from_the_quoted_brand() {
        assert_eq!(
            brand_in_name("АЗС «Роснефть», ул. Коммунаров, Владивосток").as_deref(),
            Some("Роснефть")
        );
        assert_eq!(
            brand_in_name("АЗС № 432 «Роснефть», ул. Перевальная, Находка").as_deref(),
            Some("Роснефть")
        );
        assert_eq!(brand_in_name("АЗС без сети"), None);
    }

    /// The committed mock file is the only source of fuel availability and
    /// prices, so it must stay parseable and clearly labelled as a demo. Its
    /// coordinates are not invented either: every entry names the OSM fuel
    /// station it stands on, and a pin without such a station was removed
    /// (Большой Камень had no station anywhere near its old point).
    #[test]
    fn mock_fuel_carries_labelled_demo_prices_on_real_stations() {
        #[derive(serde::Deserialize)]
        struct MockStation {
            #[serde(default)]
            fuel: Vec<String>,
            #[serde(default)]
            osm_node: String,
            #[serde(default)]
            lat: f64,
            #[serde(default)]
            lon: f64,
            #[serde(default)]
            price_ai92: Option<u32>,
            #[serde(default)]
            price_ai95: Option<u32>,
            #[serde(default)]
            price_dt: Option<u32>,
            #[serde(default)]
            price_note: String,
        }
        #[derive(serde::Deserialize)]
        struct MockFile {
            stations: Vec<MockStation>,
        }

        let raw = include_str!("../../../data/mock/fuel.json");
        let file: MockFile = serde_json::from_str(raw).expect("mock fuel.json must parse");
        assert_eq!(file.stations.len(), 11);

        let plausible = |price: u32| price > 50 && price < 100;
        assert!(
            file.stations.iter().any(|station| {
                !station.fuel.is_empty()
                    && station.price_ai92.is_some_and(plausible)
                    && station.price_ai95.is_some_and(plausible)
            }),
            "at least one station must carry a plausible price and a non-empty fuel list"
        );
        for station in &file.stations {
            assert!(!station.fuel.is_empty(), "fuel list must not be empty");
            assert_eq!(station.price_note, "демо-оценка");
            assert!(
                station.osm_node.starts_with("node/"),
                "every demo station names the OSM station it stands on"
            );
            assert!(
                crate::domain::geo::on_land(station.lat, station.lon),
                "a fuel pin must stand on land, in Primorye"
            );
            assert!(station.price_ai92.is_some_and(plausible));
            assert!(station.price_ai95.is_some_and(plausible));
            assert!(station.price_dt.is_none_or(plausible));
            if !station.fuel.iter().any(|grade| grade == "ДТ") {
                assert_eq!(
                    station.price_dt, None,
                    "a station without ДТ must not carry a diesel price"
                );
            }
        }

        // The same file feeds `mock_fuel()` and must survive the Facts ingest
        // with the prices and the fuel list intact.
        let mut facts = crate::domain::facts::Facts::default();
        facts.ingest(
            "find_fuel",
            &ToolOutput::ok(fallback_fuel(), SourceKind::Mock, "test"),
        );
        assert_eq!(facts.fuel.len(), file.stations.len());
        assert!(facts.fuel.iter().any(|station| {
            !station.fuel.is_empty() && station.price_ai92.is_some() && station.price_ai95.is_some()
        }));
    }
}
