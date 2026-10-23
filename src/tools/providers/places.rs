//! `search_places`: places of a requested kind around a Primorye settlement,
//! lake or landmark.
//!
//! Four live sources answer in parallel, because each one alone leaves a hole:
//!
//! * OSM map-API tiles hold the nearest objects with exact coordinates and
//!   opening hours, but only ever cover a neighbourhood;
//! * Nominatim's phrase search returns up to fifty objects of one OSM type
//!   across the whole search window in about a second (`гостиница`, `кафе`,
//!   `достопримечательность`), which is how a city outside the centre becomes
//!   visible at all;
//! * Wikipedia geosearch covers the sights an OSM tag misses;
//! * Overpass is the only source that can filter a whole district by tag, and
//!   the slowest: it starts next to the others and is waited for only while the
//!   answer is still thin.
//!
//! The local catalogue stays the last resort, and every summary says which
//! source answered.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::domain::facts::{SourceKind, ToolOutput};
use crate::domain::geo::{haversine_km, on_land, waterbody_title};
use crate::domain::spatial::KdTree;
use crate::tools::providers::{
    fallback_places, geo, is_place_name, limit_from, load_mock, normalize_name, osmmap, overpass,
    yandex, DEFAULT_AREA_CITY,
};
use crate::tools::registry::{Tool, ToolCtx, ToolError};

pub struct PlacesTool;

#[async_trait]
impl Tool for PlacesTool {
    fn name(&self) -> &'static str {
        "search_places"
    }

    fn description(&self) -> &'static str {
        "Найти места в любом районе Приморья: достопримечательности, музеи, природу, пляжи, озёра, реки, рестораны/кафе, бары и клубы (только по явному запросу), отели. area — город, село, озеро или река; categories — nature, museum, viewpoint, park, historic, beach, lake, river, restaurant, cafe, bar, club, hotel, attraction."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "area": {"type": "string", "description": "Город, село, озеро, река или район Приморья"},
                "query": {"type": "string", "description": "Название заведения или кухня, например Dockers Pub или пицца"},
                "categories": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "nature, museum, viewpoint, park, historic, beach, lake, river, restaurant, cafe, bar, club, hotel, attraction"
                },
                "limit": {"type": "integer", "minimum": 1, "maximum": 120},
                "nearest": {
                    "type": "boolean",
                    "description": "Искать ближайшие к центру места, когда пользователь просит «ближайшее», «поблизости», «рядом»"
                },
                "date": {"type": "string"}
            },
            "required": ["area"]
        })
    }

    async fn call(&self, args: Value, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let limit = limit_from(&args, 10, 120);
        let area_text = args
            .get("area")
            .and_then(Value::as_str)
            .unwrap_or("Приморский край");
        // "Ближайшее кафе к маяку" must return the closest ones, not the ones
        // that merely fall inside the search bbox.
        let nearest_only = args
            .get("nearest")
            .and_then(Value::as_bool)
            .unwrap_or(false)
            || nearest_requested(&args);
        let query = args
            .get("query")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        let categories: Vec<String> = args
            .get("categories")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(|value| value.to_lowercase())
                    .collect()
            })
            .unwrap_or_default();

        let Some(area) = geo::resolve_area(ctx, area_text).await else {
            return Ok(ToolOutput::ok(
                json!({"places": []}),
                SourceKind::Stale,
                format!("Не удалось определить район «{area_text}» — уточните населённый пункт"),
            ));
        };
        // "По Приморью" is the whole krai: a stop list spread over 1000 km can
        // never become a two-day route (the daily-mileage rule rejects it), so
        // a region-wide request starts from the default city. The model can ask
        // for other cities explicitly when the trip really spans the region,
        // and every summary names the default so it can do exactly that.
        let area = if area.radius_km >= 100.0 {
            match geo::resolve_area(ctx, DEFAULT_AREA_CITY).await {
                Some(mut city) => {
                    city.label = format!(
                        "{} (старт по умолчанию — назовите другой город, если он нужен)",
                        city.label
                    );
                    city
                }
                None => area,
            }
        } else {
            area
        };

        // A lake or a river is a search area, not a marker in the water. Around
        // such an area any precise land object (shore village, beach, cape,
        // museum) is more useful than the water centroid.
        let area_is_water = waterbody_title(area_text) || waterbody_title(&area.label);

        // The official Yandex Maps Search API understands venue names and
        // cuisines better than OSM tags. Use it first only for food/nightlife
        // requests (or an explicit venue query) when a key is configured.
        if ctx.yandex_maps_api_key.is_some() && wants_yandex(&query, &categories) {
            if let Some(places) = yandex::search(ctx, &area, &query, &categories, limit).await {
                let places = scope_to_area(places, &area, nearest_only, limit);
                if !places.is_empty() {
                    return Ok(ToolOutput::ok(
                        json!({"places": places}),
                        SourceKind::Live,
                        format!("Яндекс Карты: {} мест", places.len()),
                    ));
                }
            }
        }

        let deadline = Instant::now() + SEARCH_BUDGET;
        let wanted = osm_values(&categories);
        // Enough objects for the answer to count as full: half of what was
        // asked for, and never less than the catalogue top-up threshold. A
        // dense city reaches it with one tile, so the search rarely waits for
        // the slow source at all.
        let target = limit.div_ceil(2).max(MIN_LIVE_PLACES);

        let tiles = osmmap::search(ctx, area.lat, area.lon, &wanted, target, deadline);
        let phrases = phrases_for(&categories);
        let phrase_rows = async {
            let mut rows: Vec<Value> = Vec::new();
            for phrase in &phrases {
                if Instant::now() >= deadline {
                    break;
                }
                rows.extend(geo::nominatim_pois(ctx, phrase, area.bbox, limit).await);
            }
            rows
        };
        let wikipedia_rows = async {
            if !wants_wikipedia(&categories) {
                return Vec::new();
            }
            wikipedia_matching_places(ctx, area.lat, area.lon, limit, &categories, area_is_water)
                .await
                .unwrap_or_default()
        };
        // Overpass is optional by design: it gets its own task so a queued
        // mirror cannot hold the fast answer, and it is only awaited when the
        // tiles and Nominatim came back thin.
        let mut overpass_task = (!wanted.is_empty()).then(|| {
            let http = ctx.http.clone();
            let body = overpass_query(&categories, &area);
            tokio::spawn(
                async move { overpass::query(http, &body, deadline, OVERPASS_BUDGET).await },
            )
        });

        let (tiles, phrase_rows, wikipedia_rows) = tokio::join!(tiles, phrase_rows, wikipedia_rows);

        let mut sources: Vec<&str> = Vec::new();
        let mut rows: Vec<Value> = Vec::new();
        if !tiles.is_empty() {
            sources.push("OSM (тайлы)");
            rows.extend(tiles.iter().map(osmmap::Poi::to_place));
        }
        if !phrase_rows.is_empty() {
            sources.push("Nominatim (категории)");
            rows.extend(phrase_rows);
        }
        if !wikipedia_rows.is_empty() {
            sources.push("Википедия");
            rows.extend(wikipedia_rows);
        }

        let mut places = dedupe_places(rows);
        let mut overpass_answer = None;
        if places.len() < target {
            if let Some(task) = overpass_task.as_mut() {
                if let Ok(Ok(Some(value))) = tokio::time::timeout_at(deadline.into(), task).await {
                    overpass_answer = Some(value);
                }
            }
        }
        if let Some(task) = overpass_task {
            task.abort();
        }
        if let Some(value) = overpass_answer {
            let extra = parse_elements(&value, &area, limit);
            if !extra.is_empty() {
                sources.push("Overpass");
                places.extend(extra);
                places = dedupe_places(places);
            }
        }

        let places = scope_to_area(places, &area, nearest_only, limit);
        if !places.is_empty() {
            let (places, supplemented) =
                with_local_supplement(ctx, &area, &categories, places, nearest_only, limit);
            return Ok(ToolOutput::ok(
                json!({"places": places}),
                SourceKind::Live,
                format!(
                    "{}: {} мест рядом с {}{}",
                    sources.join(" + "),
                    places.len(),
                    area.label,
                    supplement_note(supplemented)
                ),
            ));
        }

        let local = scope_to_area(
            local_places(ctx, &area, &categories, limit),
            &area,
            nearest_only,
            limit,
        );
        let summary = if local.is_empty() {
            format!("В районе {} не найдено объектов по запросу", area.label)
        } else {
            format!(
                "Локальный справочник: {} объектов рядом с {}",
                local.len(),
                area.label
            )
        };
        Ok(ToolOutput::ok(
            json!({"places": local}),
            SourceKind::Mock,
            summary,
        ))
    }
}

/// Time the live providers of one search may spend before the tool gives the
/// remaining seconds to the local catalogue instead of being killed: the tool
/// itself is cut off at TOOL_TIMEOUT_SECS (15 s).
const SEARCH_BUDGET: Duration = Duration::from_secs(12);

/// Overpass may hold a thin answer for this long. The public instances queue
/// heavy queries, so this is a ceiling, not a promise.
const OVERPASS_BUDGET: Duration = Duration::from_secs(10);

/// Nominatim phrase requests per search. Each one costs a second of the shared
/// rate limit, and three families cover what people ask for.
const MAX_PHRASES: usize = 3;

/// A live answer with fewer places than this is topped up from the offline
/// catalogue: a slow or blocked source left a two-day plan with two stops, and
/// the catalogue is the documented reserve. Every entry keeps its own `source`,
/// so the model and the UI can tell live rows from demo ones.
const MIN_LIVE_PLACES: usize = 8;

/// One row per real object. The sources overlap heavily — the same hotel is a
/// map tile node, a Nominatim record and an Overpass element — so an id or a
/// name match within 300 metres keeps the first copy. The tile copy comes
/// first: it carries the exact coordinates and the opening hours.
fn dedupe_places(rows: Vec<Value>) -> Vec<Value> {
    let mut kept: Vec<Value> = Vec::new();
    let mut ids: HashSet<String> = HashSet::new();
    let mut names: Vec<(String, String, f64, f64)> = Vec::new();
    for row in rows {
        let (Some(lat), Some(lon)) = (
            row.get("lat").and_then(Value::as_f64),
            row.get("lon").and_then(Value::as_f64),
        ) else {
            // A row without coordinates cannot be placed and cannot be
            // compared: it is kept as it came, and the caller keeps it only
            // when the search found nothing else.
            kept.push(row);
            continue;
        };
        if let Some(id) = row.get("id").and_then(Value::as_str) {
            if !ids.insert(id.to_string()) {
                continue;
            }
        }
        let original = row
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let name = normalize_name(&original);
        if name.is_empty()
            || names
                .iter()
                .any(|(known, known_original, known_lat, known_lon)| {
                    haversine_km((lat, lon), (*known_lat, *known_lon)) < 0.3
                        && (*known == name || loose_same_name(known_original, &original))
                })
        {
            continue;
        }
        names.push((name, original, lat, lon));
        kept.push(row);
    }
    kept
}

/// The same place under a different name. OSM and Wikipedia describe one museum
/// as "Музей истории Дальнего Востока имени В. К. Арсеньева" and "…им. В.К.
/// Арсеньева": the words match, the abbreviations do not, so a row is a
/// duplicate when the significant words of two names are the same and the two
/// points are 300 metres apart at most. `"имени"`/`"им."` is the abbreviation
/// Russian place names actually use.
fn loose_same_name(left: &str, right: &str) -> bool {
    let words = |name: &str| -> Vec<String> {
        normalize_name(name)
            .split_whitespace()
            .map(|word| match word {
                "им" | "имени" | "назван" | "названа" => "имени".to_string(),
                _ => word.to_string(),
            })
            // "в", "к", "ул" and "г" carry no meaning on their own.
            .filter(|word| word.chars().count() > 2 || word == "имени")
            .collect()
    };
    let mut left = words(left);
    let mut right = words(right);
    if left.is_empty() || right.is_empty() {
        return false;
    }
    left.sort();
    right.sort();
    left == right
}

/// Nominatim phrase requests for the requested categories, deduplicated and
/// capped: the tiles already cover the centre, and every phrase costs a second
/// of the shared rate limit.
fn phrases_for(categories: &[String]) -> Vec<geo::Phrase> {
    let mut phrases: Vec<geo::Phrase> = Vec::new();
    for category in categories {
        for phrase in geo::phrases_for(category) {
            if phrases.len() >= MAX_PHRASES {
                return phrases;
            }
            if phrases.iter().any(|kept| kept.text == phrase.text) {
                continue;
            }
            phrases.push(phrase);
        }
    }
    phrases
}

/// The Overpass query of one search: every requested tag family over the whole
/// search window, one request.
fn overpass_query(categories: &[String], area: &geo::Area) -> String {
    let filters = overpass_filters(categories);
    let (south, west, north, east) = area.bbox;
    let bbox = format!("{south:.4},{west:.4},{north:.4},{east:.4}");
    let clauses: Vec<String> = filters
        .iter()
        .map(|filter| format!("{filter}({bbox});"))
        .collect();
    format!(
        "[out:json][timeout:12];({});out center 120;",
        clauses.join("")
    )
}

/// A user who asks for the nearest café must get the nearest one, not merely
/// the ones inside the search box. The model often keeps that wording in
/// `query`, so both the flag and the text are checked.
fn nearest_requested(args: &Value) -> bool {
    let text = args
        .get("query")
        .and_then(Value::as_str)
        .map(str::to_lowercase)
        .unwrap_or_default();
    ["ближайш", "поблизост", "неподалёк", "недалеко", "рядом"]
        .iter()
        .any(|needle| text.contains(needle))
}

/// Says out loud when a live answer was topped up from the demo catalogue.
fn supplement_note(supplemented: bool) -> &'static str {
    if supplemented {
        " + локальный справочник (демо-данные)"
    } else {
        ""
    }
}

fn with_local_supplement(
    ctx: &ToolCtx,
    area: &geo::Area,
    categories: &[String],
    places: Vec<Value>,
    nearest_only: bool,
    limit: usize,
) -> (Vec<Value>, bool) {
    if places.len() >= MIN_LIVE_PLACES {
        return (places, false);
    }
    let mut merged = places;
    for place in scope_to_area(
        local_places(ctx, area, categories, limit),
        area,
        nearest_only,
        limit,
    ) {
        merged.push(place);
    }
    // The catalogue repeats places the live sources already found (the same
    // oceanarium, the same fortress museum), and `dedupe_places` keeps the
    // first row: the live one, with its exact coordinates.
    let merged = scope_to_area(dedupe_places(merged), area, nearest_only, limit);
    (merged, true)
}

/// Keeps the results that belong to the requested place, closest first.
/// Providers search by bbox, and a bbox around a small village would otherwise
/// pull in the neighbouring town's cafés and museums; the radius of the
/// resolved area is the authority on what counts as "in this place". The k-d
/// tree runs the radius filter and the nearest-first ordering in one pass,
/// which is what "ближайшее" and "поблизости" requests rely on.
fn scope_to_area(
    places: Vec<Value>,
    area: &geo::Area,
    nearest_only: bool,
    limit: usize,
) -> Vec<Value> {
    let mut with_coords: Vec<Value> = Vec::new();
    let mut without_coords: Vec<Value> = Vec::new();
    let mut points: Vec<(f64, f64)> = Vec::new();
    for place in places {
        // A bay, lake or river is an area, never a stop: the plan layer rejects
        // such points anyway, and returning them costs the model its context
        // and hides the useful places behind them.
        if place
            .get("name")
            .and_then(Value::as_str)
            .is_some_and(waterbody_title)
        {
            continue;
        }
        match (
            place.get("lat").and_then(Value::as_f64),
            place.get("lon").and_then(Value::as_f64),
        ) {
            (Some(lat), Some(lon)) => {
                // A stop stands on land. Providers filter most of these, but a
                // search that answers from several sources must not let a pin
                // in a bay through just because one source was less careful.
                if !on_land(lat, lon) {
                    continue;
                }
                points.push((lat, lon));
                with_coords.push(place);
            }
            _ => without_coords.push(place),
        }
    }
    let tree = KdTree::new(&points);
    let picked = if nearest_only {
        // "Ближайшее кафе" — the closest ones, whatever the bbox says.
        tree.nearest(area.lat, area.lon, limit.max(1))
    } else {
        tree.within(area.lat, area.lon, area.radius_km)
    };
    let mut kept: Vec<Value> = picked
        .into_iter()
        .filter_map(|(index, distance_km)| {
            let mut place = with_coords.get(index)?.clone();
            if let Some(object) = place.as_object_mut() {
                object.insert(
                    "distance_meters".to_string(),
                    json!((distance_km * 1000.0).round()),
                );
            }
            Some(place)
        })
        .collect();
    kept.truncate(limit.max(1));
    // A result without coordinates cannot be placed; keep it as a last resort
    // only when the place search found nothing at all.
    if kept.is_empty() {
        kept.extend(without_coords.into_iter().take(limit.max(1)));
    }
    kept
}

/// Maps free-form model categories to the OSM tag values that answer them.
///
/// The model can send `clothes`, `shop=clothes`, `marketplace`, `театр` or any
/// other tag value, and a category always expands to its family: asking for
/// hotels must not hide guest houses and hostels, and asking for food must not
/// hide the bistros and bakeries that share the street with restaurants.
fn osm_values(categories: &[String]) -> Vec<String> {
    let mut values = Vec::new();
    for category in categories {
        let category = category.to_lowercase();
        let raw = category
            .split_once('=')
            .map(|(_, value)| value.trim())
            .unwrap_or(category.as_str());
        let mapped: &[&str] = match raw {
            // Where people eat: the whole family, because "поесть" covers
            // restaurants, cafés, fast food and bakeries alike.
            "food" | "еда" | "поесть" | "перекус" | "перекусить" => &[
                "restaurant",
                "cafe",
                "fast_food",
                "ice_cream",
                "bakery",
                "food_court",
            ],
            "restaurant" | "ресторан" => &["restaurant"],
            "cafe" | "кафе" | "кофейня" => &["cafe"],
            "fast_food" | "фастфуд" | "быстрое питание" | "пиццерия" => {
                &["fast_food"]
            }
            "bakery" | "пекарня" | "хлеб" => &["bakery"],
            // Where people stay: one family for hotels, guest houses, hostels,
            // apartments, camp sites and holiday camps.
            "hotel" | "отель" | "гостиница" | "lodging" | "жилье" | "жильё" | "ночлег" => {
                &[
                    "hotel",
                    "guest_house",
                    "hostel",
                    "motel",
                    "apartment",
                    "chalet",
                    "camp_site",
                    "resort",
                ]
            }
            "hostel" | "хостел" => &["hostel"],
            "guest_house" | "гостевой дом" => &["guest_house"],
            "camp_site" | "кемпинг" => &["camp_site"],
            "resort" | "база отдыха" | "дом отдыха" => &["resort"],
            "market" | "marketplace" | "рынок" | "рынки" => &["marketplace"],
            "theater" | "theatre" | "театр" | "театры" => &["theatre"],
            "cinema" | "кино" | "кинотеатр" => &["cinema"],
            "clothes" | "clothing" | "одежда" | "одежды" | "магазин одежды" => {
                &["clothes"]
            }
            "shoes" | "обувь" => &["shoes"],
            "shopping" | "mall" | "молл" | "тц" | "торговый центр" => {
                &["mall", "department_store", "clothes", "shoes"]
            }
            "pharmacy" | "аптека" => &["pharmacy"],
            "bank" | "банк" => &["bank"],
            "supermarket" | "супермаркет" => &["supermarket"],
            "bar" | "бар" => &["bar"],
            "pub" | "паб" => &["pub"],
            "club" | "nightclub" | "клуб" => &["nightclub"],
            "museum" | "музей" => &["museum"],
            "park" | "парк" | "сад" => &["park"],
            "beach" | "пляж" => &["beach"],
            "viewpoint" | "смотровая" | "смотровая площадка" => {
                &["viewpoint", "peak"]
            }
            "attraction" | "достопримечательность" | "достопримечательности" => {
                &["attraction", "artwork"]
            }
            "fuel" | "азс" => &["fuel"],
            "hospital" | "больница" => &["hospital"],
            "historic" | "история" | "памятник" => &["historic"],
            "waterfall" | "водопад" => &["waterfall"],
            "cave_entrance" | "cave" | "пещера" | "пещеры" => &["cave_entrance"],
            "lighthouse" | "маяк" => &["lighthouse"],
            "playground" | "детская площадка" => &["playground"],
            "swimming_pool" | "бассейн" => &["swimming_pool"],
            "water_park" | "аквапарк" => &["water_park"],
            "zoo" | "зоопарк" => &["zoo"],
            "aquarium" | "океанариум" | "дельфинарий" => &["aquarium"],
            "bowling" | "bowling_alley" | "боулинг" => &["bowling_alley"],
            "karting" | "картинг" => &["karting"],
            "horse_riding" | "конный" | "лошади" => &["horse_riding"],
            "fitness_centre" | "фитнес" => &["fitness_centre"],
            "sauna" | "баня" | "сауна" => &["sauna"],
            "scuba_diving" | "дайвинг" => &["scuba_diving"],
            "surfing" | "серфинг" => &["surfing"],
            "rental" | "прокат" => &["rental"],
            "trail" | "хайкинг" | "треккинг" => {
                &["park", "nature_reserve", "viewpoint"]
            }
            "place_of_worship" | "церковь" | "храм" => &["place_of_worship"],
            "picnic_table" | "пикник" => &["picnic_table", "picnic_site"],
            _ => &[],
        };
        if mapped.is_empty() {
            values.push(raw.to_string());
        } else {
            values.extend(mapped.iter().map(|value| (*value).to_string()));
        }
    }
    values.sort();
    values.dedup();
    values
}

fn overpass_filters(categories: &[String]) -> Vec<String> {
    let has = |needle: &str| categories.iter().any(|category| category.contains(needle));
    let default = categories.is_empty()
        || has("all")
        || has("attraction")
        || has("sight")
        || has("достоприм");
    let mut filters: Vec<String> = Vec::new();

    if default || has("nature") || has("природ") {
        add_filter(
            &mut filters,
            "nwr[\"natural\"~\"beach|peak|cave_entrance|spring|water\"]",
        );
        add_filter(&mut filters, "nwr[\"waterway\"~\"river|stream\"]");
        add_filter(&mut filters, "nwr[\"leisure\"~\"park|nature_reserve\"]");
    }
    if default || has("museum") || has("музе") {
        add_filter(&mut filters, "nwr[\"tourism\"=\"museum\"]");
    }
    if default || has("viewpoint") || has("смотров") {
        add_filter(&mut filters, "nwr[\"tourism\"=\"viewpoint\"]");
        add_filter(&mut filters, "nwr[\"natural\"=\"peak\"]");
    }
    if default || has("park") || has("парк") {
        add_filter(&mut filters, "nwr[\"leisure\"~\"park|nature_reserve\"]");
    }
    if default || has("historic") || has("истор") || has("monument") || has("памят") {
        add_filter(&mut filters, "nwr[\"historic\"]");
    }
    if default {
        add_filter(
            &mut filters,
            "nwr[\"tourism\"~\"attraction|zoo|aquarium|theme_park\"]",
        );
    }
    if has("beach") || has("пляж") {
        add_filter(&mut filters, "nwr[\"natural\"=\"beach\"]");
        add_filter(&mut filters, "nwr[\"leisure\"=\"beach_resort\"]");
    }
    if has("lake") || has("озер") || has("river") || has("рек") {
        // Water polygons put a pin in the middle of a lake. Search the shore
        // instead: beaches, viewpoints, parks and historic objects are precise
        // land stops for a lake/river trip.
        add_filter(&mut filters, "nwr[\"natural\"=\"beach\"]");
        add_filter(&mut filters, "nwr[\"tourism\"=\"viewpoint\"]");
        add_filter(&mut filters, "nwr[\"leisure\"~\"park|nature_reserve\"]");
        add_filter(&mut filters, "nwr[\"historic\"]");
    }
    if has("restaurant")
        || has("food")
        || has("cafe")
        || has("еда")
        || has("поесть")
        || has("ресторан")
        || has("кафе")
    {
        add_filter(
            &mut filters,
            "nwr[\"amenity\"~\"restaurant|cafe|fast_food|ice_cream|food_court\"]",
        );
    }
    // Bars and clubs are returned only when the user explicitly asks for
    // nightlife categories, not as filler for a generic city walk.
    if has("bar") || has("бар") || has("pub") || has("паб") {
        add_filter(&mut filters, "nwr[\"amenity\"~\"bar|pub\"]");
    }
    if has("club") || has("клуб") || has("night") || has("ночн") {
        add_filter(&mut filters, "nwr[\"amenity\"~\"nightclub|bar|pub\"]");
    }
    if has("hotel")
        || has("отель")
        || has("гостин")
        || has("lodging")
        || has("хостел")
        || has("guest_house")
        || has("кемпинг")
        || has("жиль")
        || has("база отдыха")
        || has("resort")
    {
        add_filter(
            &mut filters,
            "nwr[\"tourism\"~\"hotel|guest_house|hostel|motel|apartment|chalet|camp_site|resort\"]",
        );
    }
    // Tags that live outside shop/amenity/tourism/leisure: waterfalls and caves
    // are `natural`, lighthouses are `man_made`, karting and diving are `sport`.
    if has("waterfall") || has("водопад") {
        add_filter(&mut filters, "nwr[\"natural\"=\"waterfall\"]");
        add_filter(&mut filters, "nwr[\"waterway\"=\"waterfall\"]");
    }
    if has("cave") || has("cave_entrance") || has("пещер") {
        add_filter(&mut filters, "nwr[\"natural\"=\"cave_entrance\"]");
    }
    if has("lighthouse") || has("маяк") {
        add_filter(&mut filters, "nwr[\"man_made\"=\"lighthouse\"]");
    }
    for (category, sport) in [
        ("karting", "karting"),
        ("картинг", "karting"),
        ("scuba_diving", "scuba_diving"),
        ("дайвинг", "scuba_diving"),
        ("surfing", "surfing"),
        ("серфинг", "surfing"),
    ] {
        if has(category) {
            add_filter(&mut filters, format!("nwr[\"sport\"=\"{sport}\"]"));
        }
    }
    if has("пикник") || has("picnic") {
        add_filter(
            &mut filters,
            "nwr[\"leisure\"~\"picnic_table|picnic_site\"]",
        );
    }
    // Any explicit tag value works: theatre, cinema, marketplace, clothes,
    // butcher, pharmacy, etc. This is what lets the model search what the user
    // asked for instead of a fixed list of categories.
    for value in osm_values(categories) {
        for key in ["shop", "amenity", "tourism", "leisure"] {
            add_filter(&mut filters, format!("nwr[\"{key}\"=\"{value}\"]"));
        }
    }
    if filters.is_empty() {
        add_filter(
            &mut filters,
            "nwr[\"tourism\"~\"attraction|museum|viewpoint|zoo|aquarium|theme_park\"]",
        );
        add_filter(&mut filters, "nwr[\"historic\"]");
    }
    filters
}

fn add_filter(filters: &mut Vec<String>, filter: impl Into<String>) {
    let filter = filter.into();
    if !filters.contains(&filter) {
        filters.push(filter);
    }
}

fn parse_elements(value: &Value, area: &geo::Area, limit: usize) -> Vec<Value> {
    let Some(elements) = value.get("elements").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut places = Vec::new();
    let mut seen_ids = HashSet::new();
    let mut seen_places: Vec<(String, f64, f64)> = Vec::new();
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
        if waterbody_title(name) || is_water_feature(tags) {
            continue;
        }
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
        if !on_land(lat, lon) {
            continue;
        }
        let element_type = element
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("node");
        let id = element
            .get("id")
            .and_then(Value::as_u64)
            .map(|id| format!("osm:{element_type}/{id}"))
            .unwrap_or_else(|| format!("osm:{element_type}/{name}"));
        if !seen_ids.insert(id.clone()) {
            continue;
        }
        let normalized = normalize_name(name);
        if seen_places.iter().any(|(seen_name, seen_lat, seen_lon)| {
            *seen_name == normalized && haversine_km((lat, lon), (*seen_lat, *seen_lon)) < 0.3
        }) {
            continue;
        }
        seen_places.push((normalized, lat, lon));

        places.push(json!({
            "id": id,
            "name": name,
            "lat": lat,
            "lon": lon,
            "kind": osm_kind_of(tags),
            "opening_hours": tags.get("opening_hours").and_then(Value::as_str),
            "source": "live",
            "distance_meters": (haversine_km((area.lat, area.lon), (lat, lon)) * 1000.0).round()
        }));
    }
    places.sort_by(|a, b| {
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
    places.truncate(limit);
    places
}

/// The plan vocabulary of an Overpass element: the tag map the shared kind
/// mapper expects, built from the JSON tags.
fn osm_kind_of(tags: &Value) -> &'static str {
    let as_map: HashMap<String, String> = tags
        .as_object()
        .map(|object| {
            object
                .iter()
                .filter_map(|(key, value)| Some((key.clone(), value.as_str()?.to_string())))
                .collect()
        })
        .unwrap_or_default();
    osmmap::osm_kind(&as_map)
}

fn is_water_feature(tags: &Value) -> bool {
    let value = |key: &str| tags.get(key).and_then(Value::as_str).unwrap_or("");
    value("natural") == "water"
        || matches!(
            value("waterway"),
            "river" | "stream" | "canal" | "drain" | "ditch"
        )
}

/// Yandex is preferred for named venues, food and nightlife. Attraction-like
/// categories stay on Wikipedia/OSM, where they are precise.
fn wants_yandex(query: &str, categories: &[String]) -> bool {
    if !query.trim().is_empty() {
        return true;
    }
    categories.iter().any(|category| {
        let category = category.to_lowercase();
        [
            "food",
            "bar",
            "club",
            "pub",
            "nightlife",
            "hotel",
            "restaurant",
            "cafe",
            "cinema",
            "theater",
            "theatre",
            "market",
            "shop",
            "clothes",
            "mall",
            "pharmacy",
            "еда",
            "кафе",
            "ресторан",
            "бар",
            "паб",
            "клуб",
            "ночн",
            "отель",
            "гостин",
            "кино",
            "театр",
            "рын",
            "магазин",
            "одежд",
            "торгов",
            "молл",
            "аптек",
        ]
        .iter()
        .any(|needle| category.contains(needle))
    })
}

fn wants_wikipedia(categories: &[String]) -> bool {
    categories.is_empty()
        || categories.iter().any(|category| {
            [
                "nature",
                "природ",
                "museum",
                "музе",
                "viewpoint",
                "смотров",
                "park",
                "парк",
                "historic",
                "истор",
                "attraction",
                "sight",
                "достоприм",
                "beach",
                "пляж",
                "lake",
                "озер",
                "river",
                "рек",
            ]
            .iter()
            .any(|needle| category.contains(needle))
        })
}

async fn wikipedia_matching_places(
    ctx: &ToolCtx,
    lat: f64,
    lon: f64,
    limit: usize,
    categories: &[String],
    area_is_water: bool,
) -> Option<Vec<Value>> {
    let places = wikipedia_places(ctx, lat, lon, limit).await?;
    let filtered: Vec<Value> = places
        .into_iter()
        .filter(|place| wikipedia_matches_categories(place, categories, area_is_water))
        .collect();
    if filtered.is_empty() {
        None
    } else {
        Some(filtered)
    }
}

/// Keep Wikipedia results category-accurate: a beach query should not be
/// satisfied by a random museum article. Near a lake/river the water article is
/// filtered later, so shore objects of any non-food kind are valid.
fn wikipedia_matches_categories(place: &Value, categories: &[String], area_is_water: bool) -> bool {
    if categories.is_empty() {
        return true;
    }
    let kind = place.get("kind").and_then(Value::as_str).unwrap_or("sight");
    if area_is_water
        && categories.iter().any(|category| {
            category.contains("lake")
                || category.contains("озер")
                || category.contains("river")
                || category.contains("рек")
        })
    {
        return !matches!(kind, "food" | "bar" | "club" | "rest");
    }
    categories.iter().any(|category| {
        let category = category.as_str();
        if category.contains("all")
            || category.contains("attraction")
            || category.contains("sight")
            || category.contains("достоприм")
        {
            !matches!(kind, "food" | "bar" | "club" | "rest")
        } else if category.contains("nature") || category.contains("природ") {
            matches!(
                kind,
                "nature" | "beach" | "lake" | "river" | "park" | "viewpoint"
            )
        } else if category.contains("museum") || category.contains("музе") {
            kind == "museum"
        } else if category.contains("viewpoint") || category.contains("смотров") {
            matches!(kind, "viewpoint" | "nature")
        } else if category.contains("park") || category.contains("парк") {
            matches!(kind, "park" | "nature")
        } else if category.contains("historic") || category.contains("истор") {
            kind == "historic"
        } else if category.contains("beach") || category.contains("пляж") {
            kind == "beach"
        } else if category.contains("lake") || category.contains("озер") {
            kind == "lake"
        } else if category.contains("river") || category.contains("рек") {
            kind == "river"
        } else if category.contains("bar") || category.contains("бар") || category.contains("pub")
        {
            kind == "bar"
        } else if category.contains("club")
            || category.contains("клуб")
            || category.contains("night")
            || category.contains("ночн")
        {
            matches!(kind, "club" | "bar")
        } else {
            false
        }
    })
}

fn local_places(
    ctx: &ToolCtx,
    area: &geo::Area,
    categories: &[String],
    limit: usize,
) -> Vec<Value> {
    let mock = load_mock(ctx.data_dir.as_path(), "places.json", fallback_places());
    // The offline catalogue uses the same nearest-first selection as the live
    // providers, so a village request cannot fall back to a city 60 km away.
    let candidates: Vec<Value> = mock
        .get("places")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|place| {
            let (Some(lat), Some(lon)) = (
                place.get("lat").and_then(Value::as_f64),
                place.get("lon").and_then(Value::as_f64),
            ) else {
                return false;
            };
            let Some(name) = place.get("name").and_then(Value::as_str) else {
                return false;
            };
            !waterbody_title(name)
                && on_land(lat, lon)
                && matches_categories(place.get("kind").and_then(Value::as_str), categories)
        })
        .collect();
    let points: Vec<(f64, f64)> = candidates
        .iter()
        .filter_map(|place| Some((place.get("lat")?.as_f64()?, place.get("lon")?.as_f64()?)))
        .collect();
    let tree = KdTree::new(&points);
    tree.within(area.lat, area.lon, area.radius_km)
        .into_iter()
        .filter_map(|(index, distance_km)| {
            let mut place = candidates.get(index)?.clone();
            if let Some(object) = place.as_object_mut() {
                object.insert(
                    "distance_meters".to_string(),
                    json!((distance_km * 1000.0).round()),
                );
            }
            Some(place)
        })
        .take(limit)
        .collect()
}

fn matches_categories(kind: Option<&str>, categories: &[String]) -> bool {
    let Some(kind) = kind else {
        return false;
    };
    if categories.is_empty() || categories.iter().any(|category| category.contains("all")) {
        return true;
    }
    categories.iter().any(|category| match category.as_str() {
        value
            if value.contains("food")
                || value.contains("cafe")
                || value.contains("кафе")
                || value.contains("ресторан")
                || value.contains("еда")
                || value.contains("поесть") =>
        {
            kind == "food"
        }
        value if value.contains("beach") || value.contains("пляж") => kind == "nature",
        value if value.contains("lake") || value.contains("озер") => {
            matches!(
                kind,
                "nature" | "sight" | "beach" | "park" | "viewpoint" | "museum" | "historic"
            )
        }
        value if value.contains("river") || value.contains("рек") => {
            matches!(
                kind,
                "nature" | "sight" | "beach" | "park" | "viewpoint" | "museum" | "historic"
            )
        }
        value if value.contains("nature") || value.contains("природ") => kind == "nature",
        value if value.contains("museum") || value.contains("музе") => kind == "museum",
        value if value.contains("viewpoint") || value.contains("смотров") => {
            kind == "viewpoint"
        }
        value if value.contains("park") || value.contains("парк") => kind == "park",
        value if value.contains("historic") || value.contains("истор") => kind == "historic",
        value if value.contains("bar") || value.contains("бар") || value.contains("pub") => {
            kind == "bar"
        }
        value
            if value.contains("club")
                || value.contains("клуб")
                || value.contains("night")
                || value.contains("ночн") =>
        {
            matches!(kind, "club" | "bar")
        }
        value
            if value.contains("attraction")
                || value.contains("sight")
                || value.contains("достоприм") =>
        {
            !matches!(kind, "food" | "bar" | "club" | "rest")
        }
        value if value.contains("cinema") || value.contains("кино") => kind == "cinema",
        value
            if value.contains("theater")
                || value.contains("theatre")
                || value.contains("театр") =>
        {
            kind == "theater"
        }
        value if value.contains("market") || value.contains("рын") => kind == "market",
        value
            if value.contains("shop")
                || value.contains("clothes")
                || value.contains("mall")
                || value.contains("магазин")
                || value.contains("одежд")
                || value.contains("торгов")
                || value.contains("молл") =>
        {
            matches!(kind, "shop" | "market")
        }
        value if value.contains("hotel") || value.contains("отель") => false,
        _ => false,
    })
}

async fn wikipedia_places(ctx: &ToolCtx, lat: f64, lon: f64, limit: usize) -> Option<Vec<Value>> {
    let url = format!(
        "https://ru.wikipedia.org/w/api.php?action=query&list=geosearch&gscoord={lat}%7C{lon}&gsradius=10000&gslimit={limit}&format=json"
    );
    let response = ctx
        .http
        .get(&url)
        .header("User-Agent", &ctx.nominatim_user_agent)
        .send()
        .await
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    let value = response.json::<Value>().await.ok()?;
    let items = value
        .get("query")
        .and_then(|query| query.get("geosearch"))
        .and_then(Value::as_array)?;
    let mut places = Vec::new();
    let mut seen_ids = HashSet::new();
    let mut seen_titles = HashSet::new();
    for item in items {
        let Some(page_id) = item.get("pageid").and_then(Value::as_u64) else {
            continue;
        };
        let Some(title) = item.get("title").and_then(Value::as_str) else {
            continue;
        };
        if generic_title(title) || waterbody_title(title) {
            continue;
        }
        let normalized = normalize_name(title);
        if !seen_ids.insert(page_id) || !seen_titles.insert(normalized) {
            continue;
        }
        let (Some(lat), Some(lon)) = (
            item.get("lat").and_then(Value::as_f64),
            item.get("lon").and_then(Value::as_f64),
        ) else {
            continue;
        };
        if !on_land(lat, lon) {
            continue;
        }
        places.push(json!({
            "id": format!("wikipedia:{page_id}"),
            "name": title,
            "lat": lat,
            "lon": lon,
            "kind": kind_from_title(title),
            "opening_hours": null,
            "source": "live",
            "distance_meters": item.get("dist").and_then(Value::as_f64).unwrap_or(0.0).round()
        }));
    }
    places.sort_by(|a, b| {
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
    if places.is_empty() {
        None
    } else {
        Some(places)
    }
}

pub(crate) fn generic_title(title: &str) -> bool {
    let title = title.to_lowercase();
    title == "владивосток"
        || title == "большой владивосток"
        || title == "приморский край"
        || [
            "проспект",
            "улица",
            "переулок",
            "шоссе",
            "район",
            "микрорайон",
            // Administrative units are not stops: the articles about them
            // would otherwise fill a village plan with "сельское поселение".
            "сельское поселение",
            "городское поселение",
            "сельсовет",
            "муниципальн",
            "округ",
        ]
        .iter()
        .any(|word| title.contains(word))
}

pub(crate) fn kind_from_title(title: &str) -> &'static str {
    let title = title.to_lowercase();
    if title.contains("бар") || title.contains("паб") {
        "bar"
    } else if title.contains("клуб") || title.contains("nightclub") {
        "club"
    } else if title.contains("музей") || title.contains("галерея") {
        "museum"
    } else if title.contains("смотров") || title.contains("маяк") {
        "viewpoint"
    } else if title.contains("озеро") || title.contains("водохран") {
        "lake"
    } else if title.contains("река") {
        "river"
    } else if title.contains("пляж") {
        "beach"
    } else if title.contains("бухта")
        || title.contains("мыс")
        || title.contains("остров")
        || title.contains("сопка")
        || title.contains("гора")
    {
        "nature"
    } else if title.contains("парк") || title.contains("сад") {
        "park"
    } else if title.contains("театр") || title.contains("филармония") {
        "sight"
    } else if title.contains("собор")
        || title.contains("церковь")
        || title.contains("храм")
        || title.contains("памятник")
        || title.contains("мемориал")
    {
        "historic"
    } else {
        "sight"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn food_category_adds_food_filter_and_not_hotels() {
        let filters = overpass_filters(&["restaurant".to_string()]);
        assert!(filters.iter().any(|filter| filter.contains("amenity")));
        assert!(!filters
            .iter()
            .any(|filter| filter.contains("tourism\"~\"hotel")));
    }

    #[test]
    fn lake_and_river_water_bodies_are_not_stops() {
        let lake = json!({"natural": "water", "water": "lake"});
        let river = json!({"waterway": "river"});
        assert!(is_water_feature(&lake));
        assert!(is_water_feature(&river));
        assert!(!is_water_feature(&json!({"natural": "beach"})));
    }

    #[test]
    fn bars_and_clubs_have_their_own_categories() {
        let filters = overpass_filters(&["bar".to_string(), "club".to_string()]);
        assert!(filters.iter().any(|filter| filter.contains("bar|pub")));
        assert!(filters.iter().any(|filter| filter.contains("nightclub")));
        assert_eq!(osm_kind_of(&json!({"amenity": "bar"})), "bar");
        assert_eq!(osm_kind_of(&json!({"amenity": "nightclub"})), "club");
    }

    #[test]
    fn a_lodging_request_sees_the_whole_family() {
        let values = osm_values(&["hotel".to_string()]);
        for expected in [
            "hotel",
            "guest_house",
            "hostel",
            "motel",
            "apartment",
            "camp_site",
            "resort",
        ] {
            assert!(
                values.iter().any(|value| value == expected),
                "hotel request must include {expected}"
            );
        }
        // A named venue stays exact: asking for restaurants is not a request
        // for every eatery in the city.
        assert_eq!(osm_values(&["restaurant".to_string()]), vec!["restaurant"]);
    }

    #[test]
    fn a_generic_food_request_covers_every_eatery() {
        let values = osm_values(&["food".to_string()]);
        for expected in ["restaurant", "cafe", "fast_food", "bakery"] {
            assert!(values.iter().any(|value| value == expected));
        }
        let filters = overpass_filters(&["food".to_string()]);
        assert!(filters
            .iter()
            .any(|filter| filter.contains("fast_food") && filter.contains("cafe")));
        assert!(!filters
            .iter()
            .any(|filter| filter.contains("tourism\"~\"hotel")));
    }

    #[test]
    fn duplicates_of_one_object_are_kept_once() {
        let rows = vec![
            json!({"id": "osm:node/1", "name": "Кафе Полина", "lat": 43.1155, "lon": 131.8855}),
            // The same café from another source: another id, another spelling.
            json!({"id": "nominatim:node/1", "name": "кафе «Полина»", "lat": 43.1156, "lon": 131.8856}),
            json!({"id": "osm:node/2", "name": "Кафе АЛОХА", "lat": 43.1160, "lon": 131.8860}),
        ];
        let kept = dedupe_places(rows);
        assert_eq!(kept.len(), 2);
        assert_eq!(kept[0]["name"], "Кафе Полина");
    }

    #[test]
    fn one_museum_under_two_names_is_one_row() {
        // Wikipedia spells the abbreviation out, OSM shortens it; the museum is
        // the same building, and two rows would let the plan repeat it.
        let rows = vec![
            json!({"id": "wikipedia:1", "name": "Музей истории Дальнего Востока имени В. К. Арсеньева", "lat": 43.1163, "lon": 131.8821}),
            json!({"id": "osm:node/2", "name": "Музей истории Дальнего Востока им. В.К. Арсеньева", "lat": 43.1164, "lon": 131.8822}),
            // A different museum on the same street stays.
            json!({"id": "osm:node/3", "name": "Музей пограничников", "lat": 43.1185, "lon": 131.8848}),
        ];
        let kept = dedupe_places(rows);
        assert_eq!(kept.len(), 2);
        assert_eq!(kept[0]["id"], "wikipedia:1");
    }

    #[test]
    fn a_pin_in_the_bay_is_not_a_stop() {
        // The Golden Horn bay next to the Vladivostok centre: a hostel mapped on
        // a boat there must not reach the plan, while the museum 350 m away does.
        let water =
            json!({"id": "osm:node/1", "name": "Вояж", "lat": 43.1092471, "lon": 131.8857033});
        let land =
            json!({"id": "osm:node/2", "name": "Музей", "lat": 43.1163613, "lon": 131.8821714});
        let area = geo::Area {
            lat: 43.1155,
            lon: 131.8855,
            bbox: (43.09, 131.86, 43.14, 131.91),
            label: "Владивосток".to_string(),
            radius_km: 25.0,
        };
        let kept = scope_to_area(vec![water, land], &area, false, 10);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0]["name"], "Музей");
    }

    #[test]
    fn phrases_follow_the_requested_categories() {
        let phrases = phrases_for(&[
            "restaurant".to_string(),
            "cafe".to_string(),
            "museum".to_string(),
            "park".to_string(),
        ]);
        assert_eq!(phrases.len(), MAX_PHRASES);
        assert_eq!(phrases[0].text, "ресторан");
        assert_eq!(phrases[1].text, "кафе");
        assert_eq!(phrases[2].text, "музей");
        // "Где поесть" is two phrases: cafés and restaurants are separate
        // questions for Nominatim.
        let food = phrases_for(&["food".to_string()]);
        assert_eq!(food.len(), 2);
        assert_eq!(food[0].text, "кафе");
        assert_eq!(food[1].text, "ресторан");
        assert!(phrases_for(&["cave_entrance".to_string()]).is_empty());
    }
}
