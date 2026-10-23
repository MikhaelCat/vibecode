//! Live check of the place and lodging providers.
//!
//! These tests hit the real OSM map API, Nominatim, Wikipedia and Overpass, so
//! they are opt-in and never run in CI or `scripts/qa.sh`:
//!
//! ```text
//! cargo test --test live_poi_search -- --ignored --nocapture
//! ```
//!
//! They exist because the providers can look healthy while answering with two
//! demo rows: a timed-out tile, an unknown tag family or a dead Overpass mirror
//! each turned a "where to eat" request into a mock-catalogue answer.

use travel_ai::config::Config;
use travel_ai::domain::intent::{Intent, Pace, TransportMode};
use travel_ai::tools::Registry;

fn intent() -> Intent {
    let start = chrono::NaiveDate::from_ymd_opt(2026, 10, 24).expect("date");
    Intent {
        region: "Владивосток".to_string(),
        start_date: start,
        end_date: start + chrono::Duration::days(1),
        max_days: 2,
        transport: TransportMode::Car,
        budget_rub: None,
        party_size: 1,
        with_children: false,
        interests: Vec::new(),
        must_see: Vec::new(),
        avoid: Vec::new(),
        pace: Pace::Normal,
        assumptions: Vec::new(),
    }
}

fn names(output: &travel_ai::domain::facts::ToolOutput, key: &str) -> Vec<String> {
    output
        .data
        .get(key)
        .and_then(serde_json::Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    Some(format!(
                        "{} [{}]",
                        item.get("name")?.as_str()?,
                        item.get("kind")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("-")
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Name and the price the card would show.
fn priced(output: &travel_ai::domain::facts::ToolOutput, key: &str) -> Vec<(String, u64)> {
    output
        .data
        .get(key)
        .and_then(serde_json::Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    Some((
                        item.get("name")?.as_str()?.to_string(),
                        item.get("price_per_night")
                            .and_then(serde_json::Value::as_u64)
                            .unwrap_or(0),
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
}

#[tokio::test]
#[ignore = "hits live OSM/Nominatim/Wikipedia; run with -- --ignored"]
async fn eateries_and_sights_of_a_city_are_live() {
    let config = Config::from_env().expect("config");
    let registry = Registry::new(&config).expect("registry");
    let intent = intent();

    let food = registry
        .call(
            "search_places",
            serde_json::json!({"area": "Владивосток", "categories": ["food"], "limit": 60}),
            &intent,
        )
        .await
        .expect("food search");
    let food_names = names(&food, "places");
    println!("food: {} — {}", food.summary, food_names.join(", "));
    assert!(
        food_names.len() >= 20,
        "a city must answer with more than a handful of eateries: {:?}",
        food_names
    );
    assert!(
        food_names.iter().any(|name| name.contains("[food]")),
        "eateries must keep their kind: {food_names:?}"
    );

    let sights = registry
        .call(
            "search_places",
            serde_json::json!({
                "area": "Владивосток",
                "categories": ["attraction", "museum", "historic", "viewpoint"],
                "limit": 60
            }),
            &intent,
        )
        .await
        .expect("sights search");
    let sight_names = names(&sights, "places");
    println!("sights: {} — {}", sights.summary, sight_names.join(", "));
    assert!(
        sight_names.len() >= 20,
        "a city must answer with more than a handful of sights: {:?}",
        sight_names
    );
}

#[tokio::test]
#[ignore = "hits live OSM/Nominatim/Overpass; run with -- --ignored"]
async fn lodging_of_a_city_is_live() {
    let config = Config::from_env().expect("config");
    let registry = Registry::new(&config).expect("registry");
    let intent = intent();

    let lodging = registry
        .call(
            "find_lodging",
            serde_json::json!({"settlement": "Владивосток", "limit": 8}),
            &intent,
        )
        .await
        .expect("lodging");
    let options = names(&lodging, "options");
    println!("lodging: {} — {}", lodging.summary, options.join(", "));
    assert!(
        options.len() >= 5,
        "a city must offer more than the demo catalogue: {:?}",
        options
    );
    // Prices are demo estimates, but every card must carry one: a row of
    // "цена за ночь не указана" on live hotels is what the user reported.
    let with_prices = priced(&lodging, "options");
    println!(
        "lodging prices: {}",
        with_prices
            .iter()
            .map(|(name, price)| format!("{name} {price} ₽"))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let priced_out: Vec<String> = with_prices
        .iter()
        .filter(|(_, price)| *price == 0)
        .map(|(name, _)| name.clone())
        .collect();
    assert!(
        priced_out.is_empty(),
        "every lodging option must show a demo price, these have none: {priced_out:?}"
    );

    // A village on the coast: the nearest town has to serve it.
    let village = registry
        .call(
            "find_lodging",
            serde_json::json!({"settlement": "Южно-Морской", "limit": 6}),
            &intent,
        )
        .await
        .expect("village lodging");
    let village_options = names(&village, "options");
    println!(
        "village lodging: {} — {}",
        village.summary,
        village_options.join(", ")
    );
    assert!(
        !village_options.is_empty(),
        "Южно-Морской must be served by Находка: {:?}",
        village_options
    );
}

#[tokio::test]
#[ignore = "hits live OSM/Overpass; run with -- --ignored"]
async fn fuel_stations_along_a_route_are_live() {
    let config = Config::from_env().expect("config");
    let registry = Registry::new(&config).expect("registry");
    let intent = intent();

    // Владивосток → Артём: a city corridor with stations on both ends.
    let fuel = registry
        .call(
            "find_fuel",
            serde_json::json!({
                "route_points": [
                    {"lat": 43.1155, "lon": 131.8855},
                    {"lat": 43.3500, "lon": 132.1000}
                ],
                "radius_km": 20
            }),
            &intent,
        )
        .await
        .expect("fuel");
    let stations = fuel
        .data
        .get("stations")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    println!("fuel: {} — {}", fuel.summary, stations.len());
    assert!(
        stations.len() >= 3,
        "a city corridor must have more than a couple of stations: {:?}",
        fuel.data
    );
    // The point of the check: whatever the source, every pin must sit on a fuel
    // station that OSM actually knows, not somewhere near one. The map API
    // throttles bursts, so a tile that did not load leaves its station
    // unverified instead of failing the check.
    let mut off = Vec::new();
    let mut unverified = Vec::new();
    for (index, station) in stations.iter().enumerate() {
        let (Some(lat), Some(lon)) = (
            station.get("lat").and_then(serde_json::Value::as_f64),
            station.get("lon").and_then(serde_json::Value::as_f64),
        ) else {
            continue;
        };
        if index > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
        }
        let name = station
            .get("name")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("?");
        match nearest_fuel_station_distance(lat, lon).await {
            Tile::Unreadable => unverified.push(name.to_string()),
            Tile::Station(distance) if distance > 0.15 => {
                off.push(format!("{name} ({lat:.5},{lon:.5}) — {distance:.2} км"))
            }
            Tile::Station(_) => {}
            Tile::NoStation => off.push(format!("{name} ({lat:.5},{lon:.5}) — нет АЗС рядом")),
        }
    }
    println!("fuel pins unverified (плитка не загрузилась): {unverified:?}");
    assert!(
        off.is_empty(),
        "every fuel pin must stand on an OSM fuel station, these do not: {off:?}"
    );
}

/// What a tile around a fuel pin says about it.
enum Tile {
    /// The map API did not answer (throttled or busy): the pin cannot be judged.
    Unreadable,
    /// No fuel station anywhere in the tile.
    NoStation,
    /// Kilometres to the nearest fuel station of the tile.
    Station(f64),
}

/// The nearest OSM fuel station to a point, read from a small map tile.
async fn nearest_fuel_station_distance(lat: f64, lon: f64) -> Tile {
    let span = 0.006;
    let bbox = format!(
        "{:.5},{:.5},{:.5},{:.5}",
        lon - span * 1.2,
        lat - span,
        lon + span * 1.2,
        lat + span
    );
    let response = reqwest::Client::new()
        .get(format!(
            "https://api.openstreetmap.org/api/0.6/map?bbox={bbox}"
        ))
        .header(
            "User-Agent",
            "vibecode-travel-planner/1.0 (+https://github.com/MikhaelCat/vibecode)",
        )
        .send()
        .await;
    let Ok(response) = response else {
        return Tile::Unreadable;
    };
    if !response.status().is_success() {
        return Tile::Unreadable;
    }
    let Ok(xml) = response.text().await else {
        return Tile::Unreadable;
    };
    if !xml.contains("<node ") {
        return Tile::Unreadable;
    }
    let mut best: Option<f64> = None;
    for chunk in xml.split("<node ").skip(1) {
        let Some(header_end) = chunk.find('>') else {
            continue;
        };
        let header = &chunk[..header_end];
        let body = chunk[header_end + 1..]
            .split("</node>")
            .next()
            .unwrap_or("");
        if !body.contains("k=\"amenity\" v=\"fuel\"") {
            continue;
        }
        let (Some(node_lat), Some(node_lon)) = (
            header
                .split("lat=\"")
                .nth(1)
                .and_then(|value| value.split('"').next())
                .and_then(|value| value.parse::<f64>().ok()),
            header
                .split("lon=\"")
                .nth(1)
                .and_then(|value| value.split('"').next())
                .and_then(|value| value.parse::<f64>().ok()),
        ) else {
            continue;
        };
        let distance = travel_ai::domain::geo::haversine_km((lat, lon), (node_lat, node_lon));
        best = Some(best.map_or(distance, |known: f64| known.min(distance)));
    }
    match best {
        Some(distance) => Tile::Station(distance),
        None => Tile::NoStation,
    }
}
