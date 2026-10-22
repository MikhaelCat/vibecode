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
        "Варианты ночёвки в любом населённом пункте или рядом с ориентиром Приморья (маяк, пляж, озеро), отсортированные по расстоянию: сначала сам посёлок, затем ближайший город в радиусе 35 км (например, Находка для Южно-Морского). Владивостокские отели возвращаются только для владивостокских запросов. Наличие и цены нужно проверять перед выездом."
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
        // No silent default: a Vladivostok hotel booked for a Khasan or Nakhodka
        // trip is exactly the reported bug, so an unnamed place asks instead.
        let requested = args
            .get("settlement")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        if requested.is_empty() {
            return Ok(ToolOutput::ok(
                json!({"options": []}),
                SourceKind::Stale,
                "Жильё: укажите населённый пункт, бухту или район".to_string(),
            ));
        }
        let limit = limit_from(&args, 3, 10);

        let Some(area) = geo::resolve_area(ctx, &requested).await else {
            return Ok(ToolOutput::ok(
                json!({"options": [], "area": requested}),
                SourceKind::Stale,
                format!("Жильё: не удалось определить «{requested}»"),
            ));
        };

        // Level 1: the hub turns a small place, bay or cape into the hotels of
        // the place itself plus the nearest town ("Находка" for "Южно-Морской").
        let candidates = crate::domain::hubs::lodging_candidates(&requested);

        // Level 2: one live query around the reference point covers the hub, so
        // a 22 km neighbour town is found without a request per settlement.
        let mut options = overpass_lodging(ctx, &area).await;
        options.extend(local_lodging(ctx, &requested, &candidates));
        let options = rank_lodging(options, &area, &requested, limit);

        let live = options
            .iter()
            .any(|option| option.get("source").and_then(Value::as_str) == Some("live"));
        let summary = if options.is_empty() {
            let hint = nearest_town(&area);
            format!("Жильё в {requested} не найдено{hint}")
        } else {
            format!(
                "Жильё в {} и рядом: {} вариантов",
                area.label,
                options.len()
            )
        };
        Ok(ToolOutput::ok(
            json!({"options": options, "area": area.label}),
            if live {
                SourceKind::Live
            } else {
                SourceKind::Mock
            },
            summary,
        ))
    }
}

/// Keeps the options that belong to the place: closest 15 km first, then the
/// nearest town within 35 km, always sorted by distance and labelled with the
/// distance from the requested place ("Находка (22 км от Южно-Морского)").
fn rank_lodging(
    mut options: Vec<Value>,
    area: &geo::Area,
    requested: &str,
    limit: usize,
) -> Vec<Value> {
    if options.is_empty() {
        return options;
    }
    let mut nearby: Vec<(f64, Value)> = Vec::new();
    for mut option in options.drain(..) {
        let (Some(lat), Some(lon)) = (
            option.get("lat").and_then(Value::as_f64),
            option.get("lon").and_then(Value::as_f64),
        ) else {
            continue;
        };
        let distance_km = haversine_km((area.lat, area.lon), (lat, lon));
        if distance_km > 35.0 {
            continue;
        }
        let settlement = settlement_label(lat, lon, requested, distance_km);
        if let Some(object) = option.as_object_mut() {
            object.insert("settlement".to_string(), json!(settlement));
            object.insert(
                "distance_meters".to_string(),
                json!((distance_km * 1000.0).round()),
            );
            object.insert(
                "distance_km".to_string(),
                json!((distance_km * 10.0).round() / 10.0),
            );
        }
        nearby.push((distance_km, option));
    }
    // Within 15 km the place is served by its own guest houses; only when the
    // village has almost nothing does the search reach the nearest town.
    let within_15 = nearby
        .iter()
        .filter(|(distance, _)| *distance <= 15.0)
        .count();
    if within_15 < 2 {
        nearby.retain(|(distance, _)| *distance <= 35.0);
    } else {
        nearby.retain(|(distance, _)| *distance <= 15.0);
    }
    nearby.sort_by(|left, right| {
        left.0
            .partial_cmp(&right.0)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    nearby.truncate(limit);
    nearby.into_iter().map(|(_, option)| option).collect()
}

/// A hotel in another settlement is shown with its own name and its distance.
fn settlement_label(lat: f64, lon: f64, requested: &str, distance_km: f64) -> String {
    let nearest = crate::domain::settlements::nearest(lat, lon)
        .map(|settlement| settlement.name.clone())
        .filter(|name| !name.eq_ignore_ascii_case(requested));
    match nearest {
        Some(name) if distance_km > 10.0 => {
            format!("{name} ({:.0} км от {requested})", distance_km)
        }
        Some(name) => name,
        None if distance_km > 10.0 => format!("{requested} ({:.0} км)", distance_km),
        None => requested.to_string(),
    }
}

/// "…в ближайшем городе: Находка" — what to do when the village has no hotels.
fn nearest_town(area: &geo::Area) -> String {
    let hub = crate::domain::hubs::nearest(area.lat, area.lon)
        .and_then(crate::domain::hubs::primary_city)
        .map(str::to_string);
    match hub {
        Some(city) => format!(" — ищите гостевые дома или базы отдыха в ближайшем городе: {city}"),
        None => " — попробуйте назвать ближайший город".to_string(),
    }
}

/// Offline catalogue entries for the place and its hub, matched by settlement.
fn local_lodging(ctx: &ToolCtx, requested: &str, candidates: &[String]) -> Vec<Value> {
    let mock = load_mock(ctx.data_dir.as_path(), "lodging.json", fallback_lodging());
    let known = mock
        .get("options")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut matched: Vec<Value> = Vec::new();
    for candidate in candidates {
        let needle = candidate.to_lowercase();
        for option in &known {
            let settlement = option
                .get("settlement")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_lowercase();
            if settlement.contains(&needle)
                && !matched.iter().any(|kept| {
                    kept.get("id").and_then(Value::as_str)
                        == option.get("id").and_then(Value::as_str)
                })
            {
                matched.push(option.clone());
            }
        }
    }
    if matched.is_empty() {
        let needle = requested.to_lowercase();
        matched.extend(known.into_iter().filter(|option| {
            option
                .get("settlement")
                .and_then(Value::as_str)
                .map(|value| value.to_lowercase().contains(&needle))
                .unwrap_or(false)
        }));
    }
    matched
}

async fn overpass_lodging(ctx: &ToolCtx, area: &geo::Area) -> Vec<Value> {
    // One query covers the place itself and the nearest town: the reported
    // example is 22 km from Южно-Морской to Находка, so a 35 km box is used
    // and the ranking step decides which distances are acceptable.
    let lat_delta = 35.0 / 111.0;
    let lon_delta = 35.0 / (111.0 * area.lat.to_radians().cos().abs().max(0.2));
    let (south, north) = (area.lat - lat_delta, area.lat + lat_delta);
    let (west, east) = (area.lon - lon_delta, area.lon + lon_delta);
    let query = format!(
        "[out:json][timeout:8];\
         (nwr[\"tourism\"~\"hotel|guest_house|hostel|apartment|camp_site|chalet\"]({south:.4},{west:.4},{north:.4},{east:.4}););\
         out center 60;"
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
            "settlement": "",
            "lat": lat,
            "lon": lon,
            "price_per_night": 0,
            "kind": kind,
            "source": "live",
            "distance_meters": (haversine_km((area.lat, area.lon), (lat, lon)) * 1000.0).round()
        }));
    }
    options
}
