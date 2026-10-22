//! Yandex Maps Search API provider for food, nightlife and other businesses.
//!
//! The official Search API is optional and is used only when
//! `YANDEX_MAPS_API_KEY` is set. Requests are restricted to the resolved area
//! (`ll`/`spn` + `rspn=1`) and results are filtered with `on_land`, so pins in
//! bays, lakes or outside Primorye never reach the plan.

use std::collections::HashSet;

use serde_json::{json, Value};

use crate::domain::geo::{haversine_km, on_land};
use crate::tools::providers::geo::Area;
use crate::tools::registry::ToolCtx;

const SEARCH_ENDPOINT: &str = "https://search-maps.yandex.ru/v1/";

/// Searches the official Yandex Maps Search API. Returns `None` when no key
/// is configured, there is nothing to search for, or the upstream request or
/// response is unusable, so callers can fall back to OSM/Wikipedia.
pub async fn search(
    ctx: &ToolCtx,
    area: &Area,
    query: &str,
    categories: &[String],
    limit: usize,
) -> Option<Vec<Value>> {
    let key = ctx.yandex_maps_api_key.as_deref()?;
    let query = query.trim();
    if query.is_empty() && categories.is_empty() {
        return None;
    }

    let text = if query.is_empty() {
        text_from_categories(categories)
    } else {
        query.to_string()
    };
    let (south, west, north, east) = area.bbox;
    let dx = (east - west).clamp(0.05, 2.0);
    let dy = (north - south).clamp(0.05, 2.0);
    let ll = format!("{:.6},{:.6}", area.lon, area.lat);
    let spn = format!("{dx:.6},{dy:.6}");

    let response = ctx
        .http
        .get(SEARCH_ENDPOINT)
        .query(&[
            ("apikey", key),
            ("text", text.as_str()),
            ("type", "biz"),
            ("lang", "ru_RU"),
            ("results", "50"),
            ("ll", ll.as_str()),
            ("spn", spn.as_str()),
            ("rspn", "1"),
        ])
        .send()
        .await
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    let value = response.json::<Value>().await.ok()?;
    Some(parse_features(&value, area, limit))
}

/// Builds the `text` query from categories when the caller passed no explicit
/// venue/cuisine query. Unrecognized categories fall back to a broad food query.
fn text_from_categories(categories: &[String]) -> String {
    let mut words: Vec<&'static str> = Vec::new();
    for category in categories {
        let Some(text) = category_words(category) else {
            continue;
        };
        for word in text.split_whitespace() {
            if !words.contains(&word) {
                words.push(word);
            }
        }
    }
    if words.is_empty() {
        categories.join(" ")
    } else {
        words.join(" ")
    }
}

fn category_words(category: &str) -> Option<&'static str> {
    let category = category.to_lowercase();
    if category.contains("food")
        || category.contains("restaurant")
        || category.contains("cafe")
        || category.contains("bakery")
        || category.contains("coffee")
        || category.contains("еда")
        || category.contains("кафе")
        || category.contains("ресторан")
    {
        Some("ресторан кафе")
    } else if category.contains("bar")
        || category.contains("pub")
        || category.contains("club")
        || category.contains("night")
        || category.contains("бар")
        || category.contains("паб")
        || category.contains("клуб")
        || category.contains("ночн")
    {
        Some("бар паб клуб")
    } else if category.contains("cinema") || category.contains("кино") {
        Some("кинотеатр")
    } else if category.contains("theat") || category.contains("театр") {
        Some("театр")
    } else if category.contains("market") || category.contains("рын") {
        Some("рынок")
    } else if category.contains("clothes")
        || category.contains("одежд")
        || category.contains("shop")
        || category.contains("магазин")
        || category.contains("mall")
        || category.contains("молл")
        || category.contains("торгов")
    {
        Some("магазин")
    } else if category.contains("pharmacy") || category.contains("аптек") {
        Some("аптека")
    } else if category.contains("museum") || category.contains("музе") {
        Some("музей")
    } else if category.contains("hotel")
        || category.contains("hostel")
        || category.contains("guest")
        || category.contains("отель")
        || category.contains("гостин")
    {
        Some("отель")
    } else if category.contains("park") || category.contains("парк") {
        Some("парк")
    } else if category.contains("attraction")
        || category.contains("nature")
        || category.contains("viewpoint")
        || category.contains("sight")
        || category.contains("достоприм")
        || category.contains("природ")
        || category.contains("смотров")
    {
        Some("достопримечательность")
    } else {
        None
    }
}

/// Parses a GeoJSON `FeatureCollection` into tool places.
fn parse_features(value: &Value, area: &Area, limit: usize) -> Vec<Value> {
    let Some(features) = value.get("features").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut places = Vec::new();
    let mut seen_ids = HashSet::new();
    let mut seen_names: Vec<(String, f64, f64)> = Vec::new();

    for feature in features {
        let Some(properties) = feature.get("properties") else {
            continue;
        };
        let metadata = properties.get("CompanyMetaData");
        let Some(name) = properties
            .get("name")
            .and_then(Value::as_str)
            .or_else(|| {
                metadata
                    .and_then(|meta| meta.get("name"))
                    .and_then(Value::as_str)
            })
            .map(str::trim)
            .filter(|name| !name.is_empty())
        else {
            continue;
        };
        let Some(coordinates) = feature
            .get("geometry")
            .and_then(|geometry| geometry.get("coordinates"))
            .and_then(Value::as_array)
        else {
            continue;
        };
        let (Some(lon), Some(lat)) = (
            coordinates.first().and_then(Value::as_f64),
            coordinates.get(1).and_then(Value::as_f64),
        ) else {
            continue;
        };
        if !on_land(lat, lon) {
            continue;
        }

        let metadata_id = metadata
            .and_then(|meta| meta.get("id"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|id| !id.is_empty());
        let id = match metadata_id {
            Some(metadata_id) => format!("yandex:{metadata_id}"),
            None => format!("yandex:{}:{lat:.5}:{lon:.5}", normalize_name(name)),
        };
        if !seen_ids.insert(id.clone()) {
            continue;
        }
        let normalized = normalize_name(name);
        if seen_names.iter().any(|(seen_name, seen_lat, seen_lon)| {
            *seen_name == normalized && haversine_km((lat, lon), (*seen_lat, *seen_lon)) < 0.1
        }) {
            continue;
        }
        seen_names.push((normalized, lat, lon));

        let opening_hours = metadata
            .and_then(|meta| meta.get("Hours"))
            .and_then(|hours| hours.get("text"))
            .and_then(Value::as_str);
        let classes: Vec<&str> = metadata
            .and_then(|meta| meta.get("Categories"))
            .and_then(Value::as_array)
            .map(|categories| {
                categories
                    .iter()
                    .filter_map(|category| category.get("class").and_then(Value::as_str))
                    .collect()
            })
            .unwrap_or_default();

        places.push(json!({
            "id": id,
            "name": name,
            "lat": lat,
            "lon": lon,
            "kind": kind_from_categories(&classes),
            "opening_hours": opening_hours,
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
    places.truncate(limit.clamp(1, 50));
    places
}

/// Picks the most specific kind from all category classes. `sight` is the
/// default when nothing matches.
fn kind_from_categories(classes: &[&str]) -> &'static str {
    let kinds: Vec<&'static str> = classes.iter().map(|class| kind_from_class(class)).collect();
    for candidate in [
        "club", "bar", "food", "market", "shop", "theater", "cinema", "museum", "rest", "park",
    ] {
        if kinds.contains(&candidate) {
            return candidate;
        }
    }
    "sight"
}

fn kind_from_class(class: &str) -> &'static str {
    match class.to_lowercase().as_str() {
        "nightclub" | "club" => "club",
        "bar" | "pub" => "bar",
        "restaurant" | "cafe" | "fast_food" | "food" | "bakery" | "coffee" => "food",
        "museum" => "museum",
        "hotel" | "hostel" | "guest_house" => "rest",
        "park" => "park",
        "marketplace" | "market" => "market",
        "theatre" | "theater" => "theater",
        "cinema" => "cinema",
        "shop" | "mall" | "clothes" | "shoes" | "department_store" | "supermarket"
        | "convenience" => "shop",
        "attraction" => "sight",
        _ => "sight",
    }
}

fn normalize_name(name: &str) -> String {
    name.to_lowercase()
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

    fn sample_area() -> Area {
        Area {
            lat: 43.1155,
            lon: 131.8855,
            bbox: (43.0, 131.7, 43.3, 132.05),
            label: "Владивосток".to_string(),
        }
    }

    #[test]
    fn maps_yandex_category_classes() {
        assert_eq!(kind_from_class("nightclub"), "club");
        assert_eq!(kind_from_class("club"), "club");
        assert_eq!(kind_from_class("bar"), "bar");
        assert_eq!(kind_from_class("pub"), "bar");
        assert_eq!(kind_from_class("restaurant"), "food");
        assert_eq!(kind_from_class("cafe"), "food");
        assert_eq!(kind_from_class("fast_food"), "food");
        assert_eq!(kind_from_class("bakery"), "food");
        assert_eq!(kind_from_class("coffee"), "food");
        assert_eq!(kind_from_class("museum"), "museum");
        assert_eq!(kind_from_class("hotel"), "rest");
        assert_eq!(kind_from_class("hostel"), "rest");
        assert_eq!(kind_from_class("guest_house"), "rest");
        assert_eq!(kind_from_class("park"), "park");
        assert_eq!(kind_from_class("theatre"), "theater");
        assert_eq!(kind_from_class("cinema"), "cinema");
        assert_eq!(kind_from_class("marketplace"), "market");
        assert_eq!(kind_from_class("clothes"), "shop");
        assert_eq!(kind_from_class("attraction"), "sight");
        assert_eq!(kind_from_class("unknown"), "sight");
    }

    #[test]
    fn parses_sample_feature_collection() {
        let payload = json!({
            "type": "FeatureCollection",
            "features": [
                {
                    "type": "Feature",
                    "geometry": {"type": "Point", "coordinates": [131.8855, 43.1155]},
                    "properties": {
                        "name": "Dockers Pub",
                        "CompanyMetaData": {
                            "id": "101",
                            "name": "Dockers Pub",
                            "address": "ул. Светланская, 1",
                            "Hours": {"text": "12:00–02:00"},
                            "Categories": [
                                {"class": "bar", "name": "Бар"},
                                {"class": "restaurant", "name": "Ресторан"}
                            ]
                        }
                    }
                },
                {
                    "type": "Feature",
                    "geometry": {"type": "Point", "coordinates": [131.8431, 43.0731]},
                    "properties": {
                        "CompanyMetaData": {
                            "id": "202",
                            "name": "Музей Арсеньева",
                            "Categories": [{"class": "museum", "name": "Музей"}]
                        }
                    }
                },
                {
                    "type": "Feature",
                    "geometry": {"type": "Point", "coordinates": [131.8855, 43.1155]},
                    "properties": {
                        "name": "Dockers Pub",
                        "CompanyMetaData": {
                            "id": "101",
                            "name": "Dockers Pub",
                            "Categories": [{"class": "bar", "name": "Бар"}]
                        }
                    }
                },
                {
                    "type": "Feature",
                    "geometry": {"type": "Point", "coordinates": [131.82, 43.15]},
                    "properties": {
                        "name": "Точка в воде",
                        "CompanyMetaData": {"id": "303"}
                    }
                }
            ]
        });

        let places = parse_features(&payload, &sample_area(), 10);
        assert_eq!(places.len(), 2);
        assert_eq!(places[0]["id"], "yandex:101");
        assert_eq!(places[0]["name"], "Dockers Pub");
        assert_eq!(places[0]["kind"], "bar");
        assert_eq!(places[0]["opening_hours"], "12:00–02:00");
        assert_eq!(places[0]["source"], "live");
        assert_eq!(places[0]["distance_meters"], 0.0);
        assert_eq!(places[1]["id"], "yandex:202");
        assert_eq!(places[1]["name"], "Музей Арсеньева");
        assert_eq!(places[1]["kind"], "museum");
    }

    #[test]
    fn builds_text_from_categories() {
        assert_eq!(
            text_from_categories(&["restaurant".to_string(), "cafe".to_string()]),
            "ресторан кафе"
        );
        assert_eq!(
            text_from_categories(&["bar".to_string(), "club".to_string()]),
            "бар паб клуб"
        );
        assert_eq!(text_from_categories(&["museum".to_string()]), "музей");
        assert_eq!(text_from_categories(&["cinema".to_string()]), "кинотеатр");
        assert_eq!(
            text_from_categories(&["market".to_string(), "clothes".to_string()]),
            "рынок магазин"
        );
        assert_eq!(text_from_categories(&["unknown".to_string()]), "unknown");
    }
}
