use std::collections::HashSet;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::domain::facts::{SourceKind, ToolOutput};
use crate::domain::geo::{haversine_km, in_primorye};
use crate::tools::providers::{fallback_places, geo, limit_from, load_mock};
use crate::tools::registry::{Tool, ToolCtx, ToolError};

pub struct PlacesTool;

#[async_trait]
impl Tool for PlacesTool {
    fn name(&self) -> &'static str {
        "search_places"
    }

    fn description(&self) -> &'static str {
        "Найти места в любом районе Приморья: достопримечательности, музеи, природу, пляжи, озёра, реки, рестораны/кафе, отели. area — город, село, озеро или река; categories — nature, museum, viewpoint, park, historic, beach, lake, river, restaurant, cafe, hotel, attraction."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "area": {"type": "string", "description": "Город, село, озеро, река или район Приморья"},
                "categories": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "nature, museum, viewpoint, park, historic, beach, lake, river, restaurant, cafe, hotel, attraction"
                },
                "limit": {"type": "integer", "minimum": 1, "maximum": 40},
                "date": {"type": "string"}
            },
            "required": ["area"]
        })
    }

    async fn call(&self, args: Value, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let limit = limit_from(&args, 12, 40);
        let area_text = args
            .get("area")
            .and_then(Value::as_str)
            .unwrap_or("Приморский край");
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

        if let Some(places) = overpass_places(ctx, &area, &categories, limit).await {
            if !places.is_empty() {
                return Ok(ToolOutput::ok(
                    json!({"places": places}),
                    SourceKind::Live,
                    format!("OSM: {} объектов в районе {}", places.len(), area.label),
                ));
            }
        }

        if wants_wikipedia(&categories) {
            if let Some(places) = wikipedia_places(ctx, area.lat, area.lon, limit).await {
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
        "[out:json][timeout:8];({});out center 120;",
        clauses.join("")
    );
    let _permit = crate::tools::providers::OVERPASS_SLOTS
        .acquire()
        .await
        .ok()?;
    let response = tokio::time::timeout(
        Duration::from_secs(9),
        ctx.http
            .post("https://overpass-api.de/api/interpreter")
            .body(query)
            .send(),
    )
    .await
    .ok()?
    .ok()?;
    if !response.status().is_success() {
        return None;
    }
    let value = response.json::<Value>().await.ok()?;
    Some(parse_elements(&value, area, limit))
}

fn overpass_filters(categories: &[String]) -> Vec<&'static str> {
    let has = |needle: &str| categories.iter().any(|category| category.contains(needle));
    let default = categories.is_empty()
        || has("all")
        || has("attraction")
        || has("sight")
        || has("достоприм");
    let mut filters = Vec::new();

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
    if has("lake") || has("озер") {
        add_filter(
            &mut filters,
            "nwr[\"natural\"=\"water\"][\"water\"~\"lake|reservoir|pond\"]",
        );
        add_filter(&mut filters, "nwr[\"natural\"=\"water\"]");
    }
    if has("river") || has("рек") {
        add_filter(&mut filters, "nwr[\"waterway\"~\"river|stream\"]");
        add_filter(
            &mut filters,
            "nwr[\"natural\"=\"water\"][\"water\"=\"river\"]",
        );
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
            "nwr[\"amenity\"~\"restaurant|cafe|fast_food|bar|pub\"]",
        );
    }
    if has("hotel") || has("отель") || has("гостин") || has("lodging") {
        add_filter(
            &mut filters,
            "nwr[\"tourism\"~\"hotel|guest_house|hostel|apartment|camp_site|chalet\"]",
        );
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

fn add_filter(filters: &mut Vec<&'static str>, filter: &'static str) {
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
        if !in_primorye(lat, lon) {
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

fn kind_from_tags(tags: &Value) -> &'static str {
    let value = |key: &str| tags.get(key).and_then(Value::as_str).unwrap_or("");
    let amenity = value("amenity");
    if matches!(amenity, "restaurant" | "cafe" | "fast_food" | "bar" | "pub") {
        return "food";
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
            haversine_km((area.lat, area.lon), (lat, lon)) <= 60.0
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
        value if value.contains("food") || value.contains("ресторан") || value.contains("кафе") => {
            kind == "food"
        }
        value if value.contains("beach") || value.contains("пляж") => kind == "nature",
        value if value.contains("lake") || value.contains("озер") => kind == "nature",
        value if value.contains("river") || value.contains("рек") => kind == "nature",
        value if value.contains("nature") || value.contains("природ") => kind == "nature",
        value if value.contains("museum") || value.contains("музе") => kind == "museum",
        value if value.contains("viewpoint") || value.contains("смотров") => {
            kind == "viewpoint"
        }
        value if value.contains("park") || value.contains("парк") => kind == "park",
        value if value.contains("historic") || value.contains("истор") => kind == "historic",
        value if value.contains("hotel") || value.contains("отель") => false,
        _ => true,
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
        if generic_title(title) {
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
        if !in_primorye(lat, lon) {
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

fn generic_title(title: &str) -> bool {
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
    if title.contains("музей") || title.contains("галерея") {
        "museum"
    } else if title.contains("маяк") {
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
    }
}
