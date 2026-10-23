use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::domain::facts::{SourceKind, ToolOutput};
use crate::domain::geo::{haversine_km, on_land};
use crate::tools::providers::{
    fallback_lodging, geo, is_place_name, limit_from, load_mock, normalize_name, osmmap, overpass,
    DEFAULT_AREA_CITY,
};
use crate::tools::registry::{Tool, ToolCtx, ToolError};

pub struct LodgingTool;

#[async_trait]
impl Tool for LodgingTool {
    fn name(&self) -> &'static str {
        "find_lodging"
    }

    fn description(&self) -> &'static str {
        "Варианты ночёвки в любом населённом пункте или рядом с ориентиром Приморья (маяк, пляж, озеро), отсортированные по расстоянию: сначала сам посёлок, затем ближайший город в радиусе 35 км (например, Находка для Южно-Морского). Координаты и типы жилья — OpenStreetMap; цены — демо-оценки по населённому пункту и типу жилья, их нужно проверять перед выездом. Владивостокские отели возвращаются только для владивостокских запросов."
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
        let limit = limit_from(&args, 5, 10);

        let Some(area) = geo::resolve_area(ctx, &requested).await else {
            return Ok(ToolOutput::ok(
                json!({"options": [], "area": requested}),
                SourceKind::Stale,
                format!("Жильё: не удалось определить «{requested}»"),
            ));
        };
        // "Пожить по Приморью" is not a place: the krai centroid is nobody's
        // city and its hotels would sit 400 km from the plan. A region-wide
        // request gets the default starting city, exactly as `search_places`.
        let area = if area.radius_km >= 100.0 {
            match geo::resolve_area(ctx, DEFAULT_AREA_CITY).await {
                Some(city) => city,
                None => area,
            }
        } else {
            area
        };

        // Level 1: the hub turns a small place, bay or cape into the hotels of
        // the place itself plus the nearest town ("Находка" for "Южно-Морской").
        let candidates = crate::domain::hubs::lodging_candidates(&requested);

        // Level 2: live sources around the reference point, all at once. The
        // map tiles hold the nearest hotels, guest houses and camp sites of the
        // settlement itself; Nominatim answers with the lodging of the whole
        // 35 km window, which is what makes the neighbouring town visible.
        let deadline = Instant::now() + LIVE_BUDGET;
        let wanted = lodging_tags();
        let window = window_around(area.lat, area.lon, NEARBY_KM);
        let tiles = osmmap::search(ctx, area.lat, area.lon, &wanted, limit.max(3), deadline);
        let phrases = async {
            let mut rows: Vec<Value> = Vec::new();
            for phrase in lodging_phrases().iter().take(MAX_PHRASES) {
                if Instant::now() >= deadline {
                    break;
                }
                rows.extend(geo::nominatim_pois(ctx, phrase, window, 50).await);
            }
            rows
        };
        let mut overpass_task = {
            let http = ctx.http.clone();
            let body = overpass_body(area.lat, area.lon, NEARBY_KM);
            Some(tokio::spawn(async move {
                overpass::query(http, &body, deadline, OVERPASS_BUDGET).await
            }))
        };

        let (tiles, phrase_rows) = tokio::join!(tiles, phrases);
        let mut options: Vec<Value> = tiles.iter().map(tile_option).collect();
        options.extend(phrase_rows.into_iter().map(|row| {
            let kind = geo_tag_option_kind(&row);
            let mut row = row;
            if let Some(object) = row.as_object_mut() {
                object.insert("settlement".to_string(), json!(""));
                object.insert("price_per_night".to_string(), json!(0));
                object.insert("kind".to_string(), json!(kind));
            }
            row
        }));
        let live_found = options.len();
        // Overpass covers the corners the tiles and Nominatim miss (a holiday
        // camp on the coast, a hostel in the next settlement); it is only
        // waited for while the answer is thin.
        if live_found < limit {
            if let Some(task) = overpass_task.as_mut() {
                if let Ok(Ok(Some(value))) = tokio::time::timeout_at(deadline.into(), task).await {
                    options.extend(parse_lodging_elements(&value, &area));
                }
            }
        }
        if let Some(task) = overpass_task {
            task.abort();
        }
        options.extend(local_lodging(ctx, &requested, &candidates));
        let options = rank_lodging(dedupe_lodging(options), &area, &requested, limit);
        // Prices exist only in the local catalogue: no open source publishes
        // them. A live hotel therefore gets the estimate of its settlement and
        // kind, so every card carries a labelled demo price instead of a row of
        // "цена за ночь не указана".
        let options = attach_demo_prices(options, &DemoPrices::load(ctx));

        let live = options
            .iter()
            .any(|option| option.get("source").and_then(Value::as_str) == Some("live"));
        let summary = if options.is_empty() {
            let hint = nearest_town(&area);
            format!("Жильё в {requested} не найдено{hint}")
        } else if live {
            format!(
                "Жильё в {} и рядом: {} вариантов (живые данные OSM, цены — демо-оценки)",
                area.label,
                options.len()
            )
        } else {
            format!(
                "Жильё в {} и рядом: {} вариантов (цены — демо-оценки)",
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

/// Time the live lodging sources may spend. The tool is cut off at
/// TOOL_TIMEOUT_SECS (15 s); what is missing then comes from the catalogue.
const LIVE_BUDGET: Duration = Duration::from_secs(12);

/// Overpass may hold a thin answer for this long.
const OVERPASS_BUDGET: Duration = Duration::from_secs(10);

/// Nominatim phrase requests per lodging search.
const MAX_PHRASES: usize = 3;

/// Reach of one lodging search: far enough that the nearest town serves a
/// village, exactly as `rank_lodging` documents.
const NEARBY_KM: f64 = 35.0;

/// Where people sleep, as OSM tag values.
fn lodging_tags() -> Vec<String> {
    [
        "hotel",
        "guest_house",
        "hostel",
        "motel",
        "apartment",
        "chalet",
        "camp_site",
        "resort",
    ]
    .iter()
    .map(|value| value.to_string())
    .collect()
}

/// Nominatim phrases for lodging, in the order they are asked. "Гостиница"
/// covers the towns; "база отдыха" is what Primorye calls a holiday camp on the
/// coast; the rest fill in the villages.
fn lodging_phrases() -> Vec<geo::Phrase> {
    vec![
        geo::Phrase {
            text: "гостиница",
            accepted: &[
                ("tourism", "hotel"),
                ("tourism", "motel"),
                ("tourism", "apartment"),
            ],
        },
        geo::Phrase {
            text: "база отдыха",
            accepted: &[
                ("leisure", "resort"),
                ("tourism", "hotel"),
                ("tourism", "guest_house"),
                ("tourism", "chalet"),
                ("tourism", "camp_site"),
                ("tourism", "apartment"),
            ],
        },
        geo::Phrase {
            text: "гостевой дом",
            accepted: &[("tourism", "guest_house"), ("tourism", "chalet")],
        },
        geo::Phrase {
            text: "хостел",
            accepted: &[("tourism", "hostel")],
        },
    ]
}

/// A square search window around a point.
fn window_around(lat: f64, lon: f64, radius_km: f64) -> (f64, f64, f64, f64) {
    let delta_lat = radius_km / 111.0;
    let delta_lon = radius_km / (111.0 * lat.to_radians().cos().abs().max(0.2));
    (
        lat - delta_lat,
        lon - delta_lon,
        lat + delta_lat,
        lon + delta_lon,
    )
}

/// One map-API object as a lodging option.
fn tile_option(poi: &osmmap::Poi) -> Value {
    json!({
        "id": poi.id,
        "name": poi.name,
        "settlement": "",
        "lat": poi.lat,
        "lon": poi.lon,
        "price_per_night": 0,
        "kind": lodging_kind(poi.tourism.as_deref()),
        "source": "live",
        "distance_meters": poi.distance_meters,
    })
}

/// The lodging vocabulary of the UI: a hotel, a guest house, a hostel or an
/// apartment. A holiday camp and a camp site are guest houses for a traveller.
fn lodging_kind(tourism: Option<&str>) -> &'static str {
    match tourism {
        Some("hostel") => "hostel",
        Some("apartment") => "apartment",
        Some("hotel") | Some("motel") => "hotel",
        _ => "guesthouse",
    }
}

/// The kind of a Nominatim lodging row from its raw OSM tag (`osm_tag`).
fn geo_tag_option_kind(row: &Value) -> &'static str {
    let tag = row.get("osm_tag").and_then(Value::as_str).unwrap_or("");
    let (_, value) = tag.split_once('=').unwrap_or(("", tag));
    lodging_kind(Some(value.trim()))
}

/// One Overpass lodging query over the whole 35 km window.
fn overpass_body(lat: f64, lon: f64, radius_km: f64) -> String {
    let (south, west, north, east) = window_around(lat, lon, radius_km);
    format!(
        "[out:json][timeout:12];\
         (nwr[\"tourism\"~\"hotel|guest_house|hostel|motel|apartment|chalet|camp_site|resort\"]({south:.4},{west:.4},{north:.4},{east:.4});\
         nwr[\"leisure\"=\"resort\"]({south:.4},{west:.4},{north:.4},{east:.4}););\
         out center 120;"
    )
}

fn parse_lodging_elements(value: &Value, area: &geo::Area) -> Vec<Value> {
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
            .filter(|name| is_place_name(name))
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
        let tourism = tags.get("tourism").and_then(Value::as_str);
        let kind = if tourism.is_some() {
            lodging_kind(tourism)
        } else {
            "guesthouse"
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
        // A night is spent on land: a boat or a pier tagged as a hotel would
        // otherwise put a stop in the bay with a lodging card to match.
        if !on_land(lat, lon) {
            continue;
        }
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
            format!("{name} ({:.0} км от {})", distance_km, genitive(requested))
        }
        Some(name) => name,
        None if distance_km > 10.0 => format!("{requested} ({:.0} км)", distance_km),
        None => requested.to_string(),
    }
}

/// "22 км от Южно-Морского", not "от Южно-Морской": a compact genitive for the
/// place names users type. Only the endings settlements actually have.
fn genitive(name: &str) -> String {
    let lower = name.to_lowercase();
    // Cyrillic letters are two bytes, so endings are trimmed by characters.
    let trim = |count: usize| -> String {
        let length = name.chars().count();
        name.chars()
            .take(length.saturating_sub(count))
            .collect::<String>()
    };
    if lower.ends_with("ий") || lower.ends_with("ый") || lower.ends_with("ой") {
        return format!("{}ого", trim(2));
    }
    if lower.ends_with("ия") || lower.ends_with("ея") {
        return format!("{}и", trim(1));
    }
    if lower.ends_with("ие") || lower.ends_with("ье") {
        return format!("{}я", trim(1));
    }
    if lower.ends_with("ка") || lower.ends_with("га") || lower.ends_with("ха") {
        return format!("{}и", trim(1));
    }
    if lower.ends_with('а') || lower.ends_with('я') {
        return format!("{}ы", trim(1));
    }
    if lower.ends_with('о') || lower.ends_with('е') {
        return format!("{}а", trim(1));
    }
    if lower.ends_with('ь') || lower.ends_with('й') {
        return format!("{}я", trim(1));
    }
    format!("{name}а")
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

/// Demo prices by settlement and kind, read from the local catalogue.
///
/// No open source publishes lodging prices, so a price the app shows is an
/// estimate: first the settlement's own catalogue price for that kind of stay,
/// then any of the settlement's prices, then the region's median for the kind.
/// A live hotel never gets a price invented for it alone — the estimate belongs
/// to the place and the kind, and every row is labelled `демо-оценка`.
#[derive(Default)]
struct DemoPrices {
    by_settlement_kind: HashMap<(String, String), Vec<u32>>,
    by_settlement: HashMap<String, Vec<u32>>,
    by_kind: HashMap<String, Vec<u32>>,
}

impl DemoPrices {
    fn load(ctx: &ToolCtx) -> Self {
        let mock = load_mock(ctx.data_dir.as_path(), "lodging.json", fallback_lodging());
        Self::from_catalogue(&mock)
    }

    fn from_catalogue(catalogue: &Value) -> Self {
        let mut prices = DemoPrices::default();
        for option in catalogue
            .get("options")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let (Some(settlement), Some(kind)) = (
                option.get("settlement").and_then(Value::as_str),
                option.get("kind").and_then(Value::as_str),
            ) else {
                continue;
            };
            let Some(price) = option
                .get("price_per_night")
                .and_then(Value::as_u64)
                .filter(|price| *price > 0)
                .map(|price| price as u32)
            else {
                continue;
            };
            let settlement = normalize_name(settlement);
            prices
                .by_settlement_kind
                .entry((settlement.clone(), kind.to_string()))
                .or_default()
                .push(price);
            prices
                .by_settlement
                .entry(settlement)
                .or_default()
                .push(price);
            prices
                .by_kind
                .entry(kind.to_string())
                .or_default()
                .push(price);
        }
        prices
    }

    /// The demo price of a stay in `settlement` of `kind`, if the catalogue can
    /// say anything about it at all.
    fn estimate(&self, settlement: &str, kind: &str) -> Option<u32> {
        let settlement = normalize_name(settlement);
        if let Some(prices) = self
            .by_settlement_kind
            .get(&(settlement.clone(), kind.to_string()))
        {
            return Some(median(prices));
        }
        if let Some(prices) = self.by_settlement.get(&settlement) {
            return Some(median(prices));
        }
        self.by_kind.get(kind).map(|prices| median(prices))
    }
}

/// The middle price of a list; an even list averages its two middle values.
fn median(prices: &[u32]) -> u32 {
    let mut sorted = prices.to_vec();
    sorted.sort_unstable();
    let middle = sorted.len() / 2;
    if sorted.len() % 2 == 1 {
        sorted[middle]
    } else {
        (sorted[middle - 1] + sorted[middle]) / 2
    }
}

/// Fills the demo price of every option that has none: the catalogue rows keep
/// their own numbers, live rows get their settlement's estimate.
fn attach_demo_prices(options: Vec<Value>, prices: &DemoPrices) -> Vec<Value> {
    let mut priced = Vec::with_capacity(options.len());
    for mut option in options {
        let known = option
            .get("price_per_night")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let (Some(lat), Some(lon)) = (
            option.get("lat").and_then(Value::as_f64),
            option.get("lon").and_then(Value::as_f64),
        ) else {
            priced.push(option);
            continue;
        };
        if known > 0 {
            priced.push(option);
            continue;
        }
        // The estimate follows the settlement the option actually stands in,
        // not the place the user named: "Находка (22 км от Южно-Морского)"
        // is priced with Находка's demo price.
        let settlement =
            crate::domain::settlements::nearest(lat, lon).map(|settlement| settlement.name.clone());
        let kind = option.get("kind").and_then(Value::as_str).unwrap_or("");
        let estimate = settlement
            .as_deref()
            .and_then(|settlement| prices.estimate(settlement, kind));
        if let (Some(object), Some(estimate)) = (option.as_object_mut(), estimate) {
            object.insert("price_per_night".to_string(), json!(estimate));
            object.insert("price_note".to_string(), json!("демо-оценка"));
        }
        priced.push(option);
    }
    priced
}

/// One place, one card. The demo catalogue repeats what the live sources have
/// already found ("Пекинский Дворик" is in both), and a duplicate wastes one of
/// the few slots the plan sees. The live row wins: it carries coordinates and
/// the real kind, while the catalogue row stays only for places the live answer
/// does not know at all.
fn dedupe_lodging(options: Vec<Value>) -> Vec<Value> {
    let mut live_names: Vec<(String, Option<(f64, f64)>)> = Vec::new();
    let mut kept: Vec<Value> = Vec::new();
    for option in options {
        let name = option
            .get("name")
            .and_then(Value::as_str)
            .map(normalize_name)
            .unwrap_or_default();
        let coords = match (
            option.get("lat").and_then(Value::as_f64),
            option.get("lon").and_then(Value::as_f64),
        ) {
            (Some(lat), Some(lon)) => Some((lat, lon)),
            _ => None,
        };
        if name.is_empty() {
            continue;
        }
        let live = option.get("source").and_then(Value::as_str) == Some("live");
        let duplicate = live_names.iter().any(|(known, known_coords)| {
            *known == name
                && match (coords, known_coords) {
                    // One place, usually one point: half a kilometre covers a
                    // hotel mapped both as a node and as a building.
                    (Some(point), Some(known)) => haversine_km(point, *known) < 0.5,
                    // The catalogue sometimes has no coordinates: the name is
                    // all it can match on.
                    _ => true,
                }
        });
        if duplicate {
            continue;
        }
        if live {
            live_names.push((name, coords));
        }
        kept.push(option);
    }
    kept
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn genitive_reads_like_a_place_name() {
        assert_eq!(genitive("Анисимовка"), "Анисимовки");
        assert_eq!(genitive("Южно-Морской"), "Южно-Морского");
        assert_eq!(genitive("Находка"), "Находки");
        assert_eq!(genitive("Ливадия"), "Ливадии");
        assert_eq!(genitive("Владивосток"), "Владивостока");
        assert_eq!(genitive("Преображение"), "Преображения");
        assert_eq!(genitive("Терней"), "Тернея");
    }

    #[test]
    fn far_lodging_gets_the_distance_label() {
        // Находка is ~20 km from Ливадия, so the label names both.
        let label = settlement_label(42.8169, 132.8855, "Ливадия", 21.0);
        assert!(label.contains("км от"), "{label}");
        assert!(label.starts_with("Находка"), "{label}");
    }

    #[test]
    fn nearby_lodging_keeps_the_place_name() {
        let label = settlement_label(42.8607, 131.3924, "Славянка", 0.4);
        assert_eq!(label, "Славянка");
    }

    #[test]
    fn the_catalogue_does_not_repeat_a_live_hotel() {
        let options = vec![
            json!({"id": "osm:node/1", "name": "Пекинский Дворик", "lat": 43.1170, "lon": 131.8838, "kind": "hotel", "source": "live"}),
            json!({"id": "mock:lodging/pekinskiy", "name": "Пекинский дворик", "lat": 43.1170, "lon": 131.8838, "kind": "hotel", "source": "mock", "price_per_night": 4200}),
            json!({"id": "mock:lodging/versailles", "name": "Версаль", "lat": 43.1160, "lon": 131.8860, "kind": "hotel", "source": "mock", "price_per_night": 3800}),
        ];
        let kept = dedupe_lodging(options);
        assert_eq!(kept.len(), 2);
        assert_eq!(kept[0]["source"], "live");
        assert_eq!(kept[1]["name"], "Версаль");
    }

    fn catalogue() -> DemoPrices {
        let raw: Value = serde_json::from_str(include_str!("../../../data/mock/lodging.json"))
            .expect("mock lodging.json must parse");
        DemoPrices::from_catalogue(&raw)
    }

    #[test]
    fn the_demo_estimate_belongs_to_the_place_and_the_kind() {
        let prices = catalogue();
        // The settlement knows this kind of stay: its own median.
        assert_eq!(prices.estimate("Владивосток", "hotel"), Some(6500));
        assert_eq!(prices.estimate("Владивосток", "hostel"), Some(1500));
        assert_eq!(prices.estimate("Находка", "hotel"), Some(4950));
        assert_eq!(prices.estimate("Андреевка", "guesthouse"), Some(4350));
        // A settlement the catalogue knows nothing about falls back to the
        // region's median for the kind (nine hotels: 3000 … 9000, middle 4700).
        assert_eq!(prices.estimate("Кировский", "hostel"), Some(1500));
        assert_eq!(prices.estimate("Кировский", "hotel"), Some(4700));
        assert_eq!(prices.estimate("Кировский", "chalet"), None);
    }

    #[test]
    fn a_live_hotel_gets_a_labelled_demo_price() {
        let prices = catalogue();
        let options = vec![
            // A live row in Владивосток takes the city's hotel estimate.
            json!({"id": "osm:node/1", "name": "Holiday hotel", "lat": 43.1155, "lon": 131.8855, "kind": "hotel", "source": "live", "price_per_night": 0}),
            // A village the catalogue does not price takes the region's median
            // for the kind of stay.
            json!({"id": "osm:node/2", "name": "Хостел", "lat": 45.0898, "lon": 133.5062, "kind": "hostel", "source": "live", "price_per_night": 0}),
            // A catalogue row keeps the price it came with.
            json!({"id": "mock:lodging/lotte", "name": "ЛОТТЕ Отель Владивосток", "lat": 43.1155, "lon": 131.8855, "kind": "hotel", "source": "mock", "price_per_night": 9000}),
        ];
        let priced = attach_demo_prices(options, &prices);
        assert_eq!(priced[0]["price_per_night"], 6500);
        assert_eq!(priced[0]["price_note"], "демо-оценка");
        assert_eq!(priced[1]["price_per_night"], 1500);
        assert_eq!(priced[2]["price_per_night"], 9000);
    }
}
