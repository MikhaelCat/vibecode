use std::collections::{HashMap, HashSet};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::domain::facts::{SourceKind, ToolOutput};
use crate::domain::geo::{haversine_km, on_land, on_land_strict, waterbody_title};
use crate::tools::providers::{fallback_places, geo, limit_from, load_mock, yandex};
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
                "limit": {"type": "integer", "minimum": 1, "maximum": 40},
                "date": {"type": "string"}
            },
            "required": ["area"]
        })
    }

    async fn call(&self, args: Value, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let limit = limit_from(&args, 10, 40);
        let area_text = args
            .get("area")
            .and_then(Value::as_str)
            .unwrap_or("Приморский край");
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

        // A lake or a river is a search area, not a marker in the water. Around
        // such an area any precise land object (shore village, beach, cape,
        // museum) is more useful than the water centroid.
        let area_is_water = waterbody_title(area_text) || waterbody_title(&area.label);

        // The official Yandex Maps Search API understands venue names and
        // cuisines better than OSM tags. Use it first only for food/nightlife
        // requests (or an explicit venue query) when a key is configured.
        if ctx.yandex_maps_api_key.is_some() && wants_yandex(&query, &categories) {
            if let Some(places) = yandex::search(ctx, &area, &query, &categories, limit).await {
                if !places.is_empty() {
                    return Ok(ToolOutput::ok(
                        json!({"places": places}),
                        SourceKind::Live,
                        format!("Яндекс Карты: {} мест", places.len()),
                    ));
                }
            }
        }

        // Wikipedia geosearch answers in one request and does not compete for
        // the two Overpass slots, so try it first for attraction-like
        // categories. Food and lodging stay on OSM, where they are precise.
        let wikipedia_first = prefers_wikipedia(&categories);
        let mut wikipedia_tried = false;
        if wikipedia_first {
            wikipedia_tried = true;
            if let Some(places) = wikipedia_matching_places(
                ctx,
                area.lat,
                area.lon,
                limit,
                &categories,
                area_is_water,
            )
            .await
            {
                return Ok(ToolOutput::ok(
                    json!({"places": places}),
                    SourceKind::Live,
                    format!(
                        "Википедия: {} объектов рядом с {}",
                        places.len(),
                        area.label
                    ),
                ));
            }
        }

        // The OSM map API is small and fast, and Overpass is often blocked.
        // For food/nightlife search it before Overpass so a blocked endpoint
        // cannot eat the whole tool timeout.
        if wants_osm_business(&categories) {
            if let Some(places) = osm_map_places(ctx, &area, &categories, limit).await {
                if !places.is_empty() {
                    return Ok(ToolOutput::ok(
                        json!({"places": places}),
                        SourceKind::Live,
                        format!(
                            "OpenStreetMap API: {} заведений рядом с {}",
                            places.len(),
                            area.label
                        ),
                    ));
                }
            }
        }

        if let Some(places) = overpass_places(ctx, &area, &categories, limit).await {
            if !places.is_empty() {
                return Ok(ToolOutput::ok(
                    json!({"places": places}),
                    SourceKind::Live,
                    format!("OSM: {} объектов в районе {}", places.len(), area.label),
                ));
            }
        }

        if !wikipedia_tried && wants_wikipedia(&categories) {
            if let Some(places) = wikipedia_matching_places(
                ctx,
                area.lat,
                area.lon,
                limit,
                &categories,
                area_is_water,
            )
            .await
            {
                return Ok(ToolOutput::ok(
                    json!({"places": places}),
                    SourceKind::Live,
                    format!(
                        "Википедия: {} объектов рядом с {}",
                        places.len(),
                        area.label
                    ),
                ));
            }
        }

        let local = local_places(ctx, &area, &categories, limit);
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

async fn overpass_places(
    ctx: &ToolCtx,
    area: &geo::Area,
    categories: &[String],
    limit: usize,
) -> Option<Vec<Value>> {
    let filters = overpass_filters(categories);
    let (south, west, north, east) = area.bbox;
    let bbox = format!("{south:.4},{west:.4},{north:.4},{east:.4}");
    let clauses: Vec<String> = filters
        .iter()
        .map(|filter| format!("{filter}({bbox});"))
        .collect();
    let query = format!(
        "[out:json][timeout:7];({});out center 80;",
        clauses.join("")
    );
    let _permit = crate::tools::providers::OVERPASS_SLOTS
        .acquire()
        .await
        .ok()?;
    // The main Overpass instance is occasionally busy. Try one mirror only
    // when the first request fails; a successful empty answer is valid and
    // must not trigger another network round-trip.
    for endpoint in [
        "https://overpass-api.de/api/interpreter",
        "https://overpass.kumi.systems/api/interpreter",
    ] {
        let response = tokio::time::timeout(
            Duration::from_secs(4),
            ctx.http.post(endpoint).body(query.clone()).send(),
        )
        .await
        .ok()
        .and_then(Result::ok);
        let Some(response) = response else {
            continue;
        };
        if !response.status().is_success() {
            continue;
        }
        let Ok(value) = response.json::<Value>().await else {
            continue;
        };
        let places = parse_elements(&value, area, limit);
        return Some(places);
    }
    None
}

fn wants_osm_business(categories: &[String]) -> bool {
    if categories.is_empty() {
        return false;
    }
    categories.iter().any(|category| {
        let category = category.to_lowercase();
        ![
            "nature",
            "park",
            "beach",
            "viewpoint",
            "lake",
            "river",
            "mountain",
        ]
        .iter()
        .any(|plain| category.contains(plain))
    })
}

/// Maps free-form model categories to OSM tag values. The model can send
/// `clothes`, `shop=clothes`, `marketplace`, `театр` or any other tag value.
fn osm_values(categories: &[String]) -> Vec<String> {
    let mut values = Vec::new();
    for category in categories {
        let category = category.to_lowercase();
        let raw = category
            .split_once('=')
            .map(|(_, value)| value.trim())
            .unwrap_or(category.as_str());
        let mapped: &[&str] = match raw {
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
            "bakery" | "пекарня" => &["bakery"],
            "restaurant" | "ресторан" => &["restaurant"],
            "cafe" | "кафе" => &["cafe"],
            "bar" | "бар" => &["bar"],
            "pub" | "паб" => &["pub"],
            "club" | "nightclub" | "клуб" => &["nightclub"],
            "museum" | "музей" => &["museum"],
            "hotel" | "отель" => &["hotel"],
            "park" | "парк" => &["park"],
            "beach" | "пляж" => &["beach"],
            "viewpoint" | "смотровая" => &["viewpoint"],
            "attraction" | "достопримечательность" => &["attraction"],
            "fuel" | "азс" => &["fuel"],
            "hospital" | "больница" => &["hospital"],
            "historic" | "история" => &["historic"],
            "food" => &["restaurant", "cafe", "fast_food", "bakery"],
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

async fn osm_map_places(
    ctx: &ToolCtx,
    area: &geo::Area,
    categories: &[String],
    limit: usize,
) -> Option<Vec<Value>> {
    let wanted = osm_values(categories);
    if wanted.is_empty() {
        return None;
    }
    for span in [0.004_f64, 0.002] {
        let bbox = format!(
            "{:.5},{:.5},{:.5},{:.5}",
            area.lon - span * 1.2,
            area.lat - span,
            area.lon + span * 1.2,
            area.lat + span
        );
        let url = format!("https://api.openstreetmap.org/api/0.6/map?bbox={bbox}");
        let response = tokio::time::timeout(
            Duration::from_secs(6),
            ctx.http
                .get(&url)
                .header("User-Agent", &ctx.nominatim_user_agent)
                .send(),
        )
        .await
        .ok()?
        .ok()?;
        if response.status().as_u16() == 400 {
            continue;
        }
        if !response.status().is_success() {
            return None;
        }
        let xml = response.text().await.ok()?;
        return Some(parse_osm_map(&xml, area, &wanted, limit));
    }
    None
}

fn parse_osm_map(xml: &str, area: &geo::Area, wanted: &[String], limit: usize) -> Vec<Value> {
    let mut node_coords: HashMap<String, (f64, f64)> = HashMap::new();
    for chunk in xml.split("<node ").skip(1) {
        let Some(header_end) = chunk.find('>') else {
            continue;
        };
        let header = &chunk[..header_end];
        let (Some(id), Some(lat), Some(lon)) = (
            xml_attr(header, "id"),
            xml_attr(header, "lat").and_then(|value| value.parse::<f64>().ok()),
            xml_attr(header, "lon").and_then(|value| value.parse::<f64>().ok()),
        ) else {
            continue;
        };
        node_coords.insert(id.to_string(), (lat, lon));
    }
    let mut places = Vec::new();
    let mut seen_ids = HashSet::new();
    let mut seen_names: Vec<(String, f64, f64)> = Vec::new();
    for chunk in xml.split("<node ").skip(1) {
        let Some(header_end) = chunk.find('>') else {
            continue;
        };
        let header = &chunk[..header_end];
        let Some(id) = xml_attr(header, "id") else {
            continue;
        };
        let (Some(lat), Some(lon)) = (
            xml_attr(header, "lat").and_then(|value| value.parse::<f64>().ok()),
            xml_attr(header, "lon").and_then(|value| value.parse::<f64>().ok()),
        ) else {
            continue;
        };
        if !on_land_strict(lat, lon) {
            continue;
        }
        let body = if header.ends_with('/') {
            ""
        } else {
            chunk[header_end + 1..]
                .split("</node>")
                .next()
                .unwrap_or("")
        };
        let mut tags: HashMap<String, String> = HashMap::new();
        for tag in body.split("<tag ").skip(1) {
            let end = tag
                .find("/>")
                .or_else(|| tag.find('>'))
                .unwrap_or(tag.len());
            let segment = &tag[..end];
            if let (Some(key), Some(value)) = (xml_attr(segment, "k"), xml_attr(segment, "v")) {
                tags.insert(key.to_string(), xml_unescape(value));
            }
        }
        let matched = ["amenity", "shop", "tourism", "leisure"].iter().any(|key| {
            tags.get(*key)
                .is_some_and(|value| wanted.iter().any(|wanted| wanted == value))
        }) || (wanted.iter().any(|wanted| wanted == "historic")
            && tags.contains_key("historic"));
        if !matched {
            continue;
        }
        let Some(name) = tags.get("name:ru").or_else(|| tags.get("name")) else {
            continue;
        };
        let name = name.trim();
        if name.is_empty() {
            continue;
        }
        let id = format!("osm:node/{id}");
        if !seen_ids.insert(id.clone()) {
            continue;
        }
        let key = name.to_lowercase();
        if seen_names.iter().any(|(known, known_lat, known_lon)| {
            *known == key && haversine_km((lat, lon), (*known_lat, *known_lon)) < 0.1
        }) {
            continue;
        }
        seen_names.push((key, lat, lon));
        places.push(json!({
            "id": id,
            "name": name,
            "lat": lat,
            "lon": lon,
            "kind": osm_kind(&tags),
            "opening_hours": tags.get("opening_hours"),
            "source": "live",
            "distance_meters": (haversine_km((area.lat, area.lon), (lat, lon)) * 1000.0).round()
        }));
    }
    // Cinemas, malls, theatres and many shops are mapped as ways, not nodes.
    // Build their centre from the referenced nodes so tag search finds them.
    for chunk in xml.split("<way ").skip(1) {
        let Some(header_end) = chunk.find('>') else {
            continue;
        };
        let header = &chunk[..header_end];
        let Some(id) = xml_attr(header, "id") else {
            continue;
        };
        let body = chunk[header_end + 1..].split("</way>").next().unwrap_or("");
        let mut tags: HashMap<String, String> = HashMap::new();
        for tag in body.split("<tag ").skip(1) {
            let end = tag
                .find("/>")
                .or_else(|| tag.find('>'))
                .unwrap_or(tag.len());
            let segment = &tag[..end];
            if let (Some(key), Some(value)) = (xml_attr(segment, "k"), xml_attr(segment, "v")) {
                tags.insert(key.to_string(), xml_unescape(value));
            }
        }
        let matched = ["amenity", "shop", "tourism", "leisure"].iter().any(|key| {
            tags.get(*key)
                .is_some_and(|value| wanted.iter().any(|wanted| wanted == value))
        }) || (wanted.iter().any(|wanted| wanted == "historic")
            && tags.contains_key("historic"));
        if !matched {
            continue;
        }
        let mut sum_lat = 0.0;
        let mut sum_lon = 0.0;
        let mut count = 0usize;
        for nd in body.split("<nd ").skip(1) {
            let end = nd.find("/>").or_else(|| nd.find('>')).unwrap_or(nd.len());
            let segment = &nd[..end];
            let Some(reference) = xml_attr(segment, "ref") else {
                continue;
            };
            let Some((lat, lon)) = node_coords.get(reference) else {
                continue;
            };
            sum_lat += lat;
            sum_lon += lon;
            count += 1;
        }
        if count == 0 {
            continue;
        }
        let lat = sum_lat / count as f64;
        let lon = sum_lon / count as f64;
        if !on_land_strict(lat, lon) {
            continue;
        }
        let Some(name) = tags.get("name:ru").or_else(|| tags.get("name")) else {
            continue;
        };
        let name = name.trim();
        if name.is_empty() {
            continue;
        }
        let id = format!("osm:way/{id}");
        if !seen_ids.insert(id.clone()) {
            continue;
        }
        let key = name.to_lowercase();
        if seen_names.iter().any(|(known, known_lat, known_lon)| {
            *known == key && haversine_km((lat, lon), (*known_lat, *known_lon)) < 0.1
        }) {
            continue;
        }
        seen_names.push((key, lat, lon));
        places.push(json!({
            "id": id,
            "name": name,
            "lat": lat,
            "lon": lon,
            "kind": osm_kind(&tags),
            "opening_hours": tags.get("opening_hours"),
            "source": "live",
            "distance_meters": (haversine_km((area.lat, area.lon), (lat, lon)) * 1000.0).round()
        }));
    }
    places.sort_by(|left, right| {
        let left = left
            .get("distance_meters")
            .and_then(Value::as_f64)
            .unwrap_or(f64::MAX);
        let right = right
            .get("distance_meters")
            .and_then(Value::as_f64)
            .unwrap_or(f64::MAX);
        left.partial_cmp(&right)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    places.truncate(limit);
    places
}

fn osm_kind(tags: &HashMap<String, String>) -> &'static str {
    if let Some(amenity) = tags.get("amenity").map(String::as_str) {
        match amenity {
            "nightclub" => return "club",
            "bar" | "pub" => return "bar",
            "restaurant" | "cafe" | "fast_food" | "bakery" => return "food",
            "marketplace" => return "market",
            "theatre" => return "theater",
            "cinema" => return "cinema",
            "hotel" | "hostel" | "guest_house" => return "rest",
            "museum" => return "museum",
            _ => {}
        }
    }
    if tags.contains_key("shop") {
        return "shop";
    }
    match tags.get("tourism").map(String::as_str) {
        Some("museum") => return "museum",
        Some("viewpoint") => return "viewpoint",
        Some("hotel" | "guest_house" | "hostel" | "apartment" | "camp_site" | "chalet") => {
            return "rest"
        }
        _ => {}
    }
    if tags.contains_key("historic") {
        return "historic";
    }
    if matches!(
        tags.get("leisure").map(String::as_str),
        Some("park" | "nature_reserve")
    ) {
        return "park";
    }
    "sight"
}

fn xml_attr<'a>(header: &'a str, key: &str) -> Option<&'a str> {
    let pattern = format!("{key}=\"");
    let start = header
        .find(&format!(" {pattern}"))
        .map(|index| index + 1)
        .or_else(|| header.find(&pattern))?;
    let value_start = start + pattern.len();
    let end = header[value_start..].find('"')? + value_start;
    Some(&header[value_start..end])
}

fn xml_unescape(value: &str) -> String {
    value
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
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
        || has("ресторан")
        || has("кафе")
    {
        add_filter(
            &mut filters,
            "nwr[\"amenity\"~\"restaurant|cafe|fast_food\"]",
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
    if has("hotel") || has("отель") || has("гостин") || has("lodging") {
        add_filter(
            &mut filters,
            "nwr[\"tourism\"~\"hotel|guest_house|hostel|apartment|camp_site|chalet\"]",
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
        let normalized = normalize_title(name);
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
            "kind": kind_from_tags(tags),
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

fn is_water_feature(tags: &Value) -> bool {
    let value = |key: &str| tags.get(key).and_then(Value::as_str).unwrap_or("");
    value("natural") == "water"
        || matches!(
            value("waterway"),
            "river" | "stream" | "canal" | "drain" | "ditch"
        )
}

fn kind_from_tags(tags: &Value) -> &'static str {
    let value = |key: &str| tags.get(key).and_then(Value::as_str).unwrap_or("");
    let amenity = value("amenity");
    if amenity == "nightclub" {
        return "club";
    }
    if matches!(amenity, "bar" | "pub") {
        return "bar";
    }
    if matches!(amenity, "restaurant" | "cafe" | "fast_food") {
        return "food";
    }
    if amenity == "marketplace" {
        return "market";
    }
    if amenity == "theatre" {
        return "theater";
    }
    if amenity == "cinema" {
        return "cinema";
    }
    if !value("shop").is_empty() {
        return "shop";
    }
    let natural = value("natural");
    if natural == "beach" || value("leisure") == "beach_resort" {
        return "beach";
    }
    if natural == "water" {
        return match value("water") {
            "lake" | "reservoir" | "pond" => "lake",
            _ => "nature",
        };
    }
    if matches!(value("waterway"), "river" | "stream") {
        return "river";
    }
    if natural == "peak" {
        return "viewpoint";
    }
    match value("tourism") {
        "museum" => return "museum",
        "viewpoint" => return "viewpoint",
        "hotel" | "guest_house" | "hostel" | "apartment" | "camp_site" | "chalet" => return "rest",
        "zoo" | "aquarium" | "theme_park" | "attraction" => return "sight",
        _ => {}
    }
    if !value("historic").is_empty() {
        return "historic";
    }
    if matches!(value("leisure"), "park" | "nature_reserve") {
        return "park";
    }
    "sight"
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
            category.contains("nature")
                || category.contains("природ")
                || category.contains("museum")
                || category.contains("музе")
                || category.contains("viewpoint")
                || category.contains("смотров")
                || category.contains("park")
                || category.contains("парк")
                || category.contains("historic")
                || category.contains("истор")
                || category.contains("attraction")
                || category.contains("sight")
                || category.contains("beach")
                || category.contains("пляж")
                || category.contains("lake")
                || category.contains("озер")
                || category.contains("river")
                || category.contains("рек")
        })
}

/// Attraction-like requests are a good fit for a single fast Wikipedia
/// geosearch. If any category needs OSM semantics (food, hotels, "all"),
/// Overpass must run first.
fn prefers_wikipedia(categories: &[String]) -> bool {
    categories.is_empty()
        || categories
            .iter()
            .all(|category| attraction_category(category))
}

fn attraction_category(category: &str) -> bool {
    [
        "attraction",
        "sight",
        "nature",
        "museum",
        "viewpoint",
        "park",
        "historic",
        "beach",
        "lake",
        "river",
        "достоприм",
        "природ",
        "музе",
        "смотров",
        "парк",
        "истор",
        "пляж",
        "озер",
        "рек",
    ]
    .iter()
    .any(|needle| category.contains(needle))
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
/// satisfied by a random museum article, otherwise Overpass never runs. Near a
/// lake/river the water article is filtered later, so shore objects of any
/// non-food kind are valid.
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
    let mut places: Vec<Value> = mock
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
                && haversine_km((area.lat, area.lon), (lat, lon)) <= 60.0
                && matches_categories(place.get("kind").and_then(Value::as_str), categories)
        })
        .collect();
    places.truncate(limit);
    places
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
                || value.contains("ресторан") =>
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
        let normalized = normalize_title(title);
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

fn normalize_title(title: &str) -> String {
    title
        .to_lowercase()
        .replace('ё', "е")
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
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
    fn beach_category_maps_to_beach_kind() {
        let tags = json!({"natural": "beach", "name": "Пляж"});
        assert_eq!(kind_from_tags(&tags), "beach");
    }

    #[test]
    fn lake_and_river_kinds_are_distinct() {
        let lake = json!({"natural": "water", "water": "lake"});
        let river = json!({"waterway": "river"});
        assert_eq!(kind_from_tags(&lake), "lake");
        assert_eq!(kind_from_tags(&river), "river");
        assert!(is_water_feature(&lake));
        assert!(is_water_feature(&river));
        assert!(!is_water_feature(&json!({"natural": "beach"})));
    }

    #[test]
    fn bars_and_clubs_have_their_own_categories() {
        let filters = overpass_filters(&["bar".to_string(), "club".to_string()]);
        assert!(filters.iter().any(|filter| filter.contains("bar|pub")));
        assert!(filters.iter().any(|filter| filter.contains("nightclub")));
        assert_eq!(kind_from_tags(&json!({"amenity": "bar"})), "bar");
        assert_eq!(kind_from_tags(&json!({"amenity": "nightclub"})), "club");
    }

    #[test]
    fn parses_osm_map_businesses() {
        let area = geo::Area {
            lat: 43.1155,
            lon: 131.8855,
            bbox: (43.10, 131.87, 43.13, 131.90),
            label: "Владивосток".to_string(),
        };
        let xml = r#"<osm>
        <node id="1" lat="43.1155" lon="131.8855"><tag k="amenity" v="bar"/><tag k="name" v="Soda Bar"/></node>
        <node id="2" lat="43.116" lon="131.886"><tag k="amenity" v="cafe"/><tag k="name" v="Cafe"/></node>
        <node id="3" lat="43.117" lon="131.887"><tag k="amenity" v="parking"/><tag k="name" v="Parking"/></node>
        </osm>"#;
        let places = parse_osm_map(xml, &area, &["bar".to_string(), "cafe".to_string()], 10);
        assert_eq!(places.len(), 2);
        assert_eq!(places[0]["kind"], "bar");
        assert_eq!(places[1]["kind"], "food");
    }
}
