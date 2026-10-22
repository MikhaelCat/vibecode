//! Nominatim/Photon geocoding and Primorye area resolution.
//!
//! Parsers normalise external place JSON into the tool schema and discard
//! results outside the Primorye bounding box. `resolve_area` turns settlements,
//! lakes and rivers into a center point plus an Overpass bbox: built-in
//! gazetteer first, then Nominatim, then Photon, and never a silent default to
//! Vladivostok. Nominatim is limited to 1 req/s and successful responses are
//! memoized for 30 minutes. Results carry `live` provenance.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::domain::geo::{haversine_km, in_primorye, PRIMORYE_BBOX};
use crate::tools::registry::ToolCtx;

static NOMINATIM_LIMIT: tokio::sync::Mutex<Option<Instant>> = tokio::sync::Mutex::const_new(None);
static AREA_CACHE: tokio::sync::Mutex<Vec<(String, Instant, Area)>> =
    tokio::sync::Mutex::const_new(Vec::new());
/// Successful Nominatim responses are reused for 30 minutes. This skips both
/// the network and the 1 req/s limiter for repeated geocode/resolve_area calls.
static NOMINATIM_CACHE: tokio::sync::Mutex<Vec<(String, Instant, Vec<Value>)>> =
    tokio::sync::Mutex::const_new(Vec::new());
const NOMINATIM_CACHE_TTL: Duration = Duration::from_secs(1800);
const NOMINATIM_CACHE_MAX_ENTRIES: usize = 64;

type NatureArea = (&'static str, f64, f64, (f64, f64, f64, f64));

const KNOWN_NATURE: &[NatureArea] = &[
    // Озеро Ханка: south, west, north, east.
    ("ханка", 44.9, 132.45, (44.5, 132.0, 45.3, 132.9)),
    // Река Раздольная.
    ("раздольная", 44.05, 131.8, (43.3, 131.2, 44.8, 132.4)),
];

/// Areas people name that are not settlements in the gazetteer: districts of
/// Vladivostok and Nakhodka, and the island city district. Keeping them here
/// avoids sending the request to a geocoder that could answer with a namesake
/// from another region.
const KNOWN_AREAS: &[(&str, f64, f64, f64)] = &[
    ("ливадия", 42.8667, 132.6833, 8.0),
    ("врангель", 42.7333, 133.0833, 8.0),
    ("русский", 43.0, 131.9, 12.0),
    ("пидан", 43.4, 132.83, 12.0),
];

/// An area resolved to a center point and an Overpass-friendly bbox.
#[derive(Debug, Clone)]
pub struct Area {
    /// Center point used for nearby searches.
    pub lat: f64,
    pub lon: f64,
    /// south, west, north, east
    pub bbox: (f64, f64, f64, f64),
    /// Display label for the resolved area.
    pub label: String,
    /// Radius of the place in kilometres. Every provider drops results farther
    /// than this from the center, so a village search cannot return the next
    /// town's venues and a city search keeps its whole agglomeration.
    pub radius_km: f64,
}

impl Area {
    /// True when the point belongs to this place, not to a neighbouring one.
    pub fn contains_point(&self, lat: f64, lon: f64) -> bool {
        haversine_km((self.lat, self.lon), (lat, lon)) <= self.radius_km
    }
}

/// Nominatim is rate-limited to one request per second. Results are memoized
/// for 30 minutes so repeated geocode/resolve_area calls stay off the limiter.
/// Only Primorye results are returned; upstream errors yield an empty list.
pub async fn nominatim(ctx: &ToolCtx, query: &str, limit: usize) -> Vec<Value> {
    let key = nominatim_cache_key(query, limit);
    {
        let cache = NOMINATIM_CACHE.lock().await;
        if let Some((_, _, items)) = cache
            .iter()
            .find(|(cached_key, expires, _)| *cached_key == key && *expires > Instant::now())
        {
            return items.clone();
        }
    }

    {
        let mut last = NOMINATIM_LIMIT.lock().await;
        if let Some(last_request) = *last {
            let elapsed = last_request.elapsed();
            if elapsed < Duration::from_secs(1) {
                tokio::time::sleep(Duration::from_secs(1) - elapsed).await;
            }
        }
        *last = Some(Instant::now());
    }

    let limit_text = limit.to_string();
    let viewbox = format!(
        "{:.4},{:.4},{:.4},{:.4}",
        PRIMORYE_BBOX.1, PRIMORYE_BBOX.2, PRIMORYE_BBOX.3, PRIMORYE_BBOX.0
    );
    let response = match ctx
        .http
        .get("https://nominatim.openstreetmap.org/search")
        .query(&[
            ("format", "jsonv2"),
            ("q", query),
            ("limit", limit_text.as_str()),
            ("countrycodes", "ru"),
            ("viewbox", viewbox.as_str()),
            ("bounded", "1"),
            ("accept-language", "ru"),
        ])
        .header("User-Agent", &ctx.nominatim_user_agent)
        .send()
        .await
    {
        Ok(response) if response.status().is_success() => response,
        _ => return Vec::new(),
    };
    let Ok(raw_items) = response.json::<Vec<Value>>().await else {
        return Vec::new();
    };
    let items: Vec<Value> = raw_items
        .into_iter()
        .filter_map(|item| {
            let lat = item.get("lat")?.as_str()?.parse::<f64>().ok()?;
            let lon = item.get("lon")?.as_str()?.parse::<f64>().ok()?;
            if !in_primorye(lat, lon) {
                return None;
            }
            let name = item
                .get("display_name")
                .and_then(Value::as_str)
                .map(short_name)
                .unwrap_or_else(|| query.to_string());
            let class = item.get("class").and_then(Value::as_str).unwrap_or("");
            let type_name = item.get("type").and_then(Value::as_str).unwrap_or("");
            let kind = geo_kind(class, type_name, &name);
            let bbox = parse_bbox(item.get("boundingbox"));
            let osm_type = item
                .get("osm_type")
                .and_then(Value::as_str)
                .unwrap_or("node");
            let osm_id = item.get("osm_id").and_then(Value::as_u64).unwrap_or(0);
            Some(json!({
                "id": format!("nominatim:{osm_type}/{osm_id}"),
                "name": name,
                "lat": lat,
                "lon": lon,
                "bbox": bbox,
                "kind": kind,
                "opening_hours": null,
                "source": "live"
            }))
        })
        .take(limit)
        .collect();

    let now = Instant::now();
    let mut cache = NOMINATIM_CACHE.lock().await;
    cache.retain(|(_, expires, _)| *expires > now);
    if cache.len() >= NOMINATIM_CACHE_MAX_ENTRIES {
        cache.remove(0);
    }
    cache.push((key, now + NOMINATIM_CACHE_TTL, items.clone()));
    items
}

fn nominatim_cache_key(query: &str, limit: usize) -> String {
    let normalized = query
        .to_lowercase()
        .replace('ё', "е")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    format!("{normalized}|{limit}")
}

/// Photon geocoder used as the last area fallback. It mirrors `nominatim`'s
/// output shape, filters to Primorye, and returns an empty list on errors.
pub async fn photon(ctx: &ToolCtx, query: &str, limit: usize) -> Vec<Value> {
    let limit_text = limit.to_string();
    let response = match ctx
        .http
        .get("https://photon.komoot.io/api/")
        .query(&[("q", query), ("limit", limit_text.as_str())])
        .send()
        .await
    {
        Ok(response) if response.status().is_success() => response,
        _ => return Vec::new(),
    };
    let Ok(value) = response.json::<Value>().await else {
        return Vec::new();
    };
    let Some(features) = value.get("features").and_then(Value::as_array) else {
        return Vec::new();
    };
    features
        .iter()
        .filter_map(|feature| {
            let props = feature.get("properties")?;
            let name = clean_name(
                props
                    .get("name")
                    .and_then(Value::as_str)
                    .or_else(|| props.get("city").and_then(Value::as_str))
                    .or_else(|| props.get("street").and_then(Value::as_str))?,
            );
            let osm_key = props.get("osm_key").and_then(Value::as_str).unwrap_or("");
            let osm_value = props.get("osm_value").and_then(Value::as_str).unwrap_or("");
            let kind = geo_kind(osm_key, osm_value, &name);
            let coords = feature
                .get("geometry")
                .and_then(|geometry| geometry.get("coordinates"))
                .and_then(Value::as_array)?;
            let lon = coords.first()?.as_f64()?;
            let lat = coords.get(1)?.as_f64()?;
            if !in_primorye(lat, lon) {
                return None;
            }
            let osm_type = props.get("osm_type").and_then(Value::as_str).unwrap_or("N");
            let osm_id = props.get("osm_id").and_then(Value::as_u64).unwrap_or(0);
            Some(json!({
                "id": format!("photon:{osm_type}{osm_id}"),
                "name": name,
                "lat": lat,
                "lon": lon,
                "bbox": null,
                "kind": kind,
                "opening_hours": null,
                "source": "live"
            }))
        })
        .take(limit)
        .collect()
}

/// Resolves a free-form area ("Уссурийск", "село Верхняя Фудинка", "озеро Ханка")
/// to coordinates and a bbox. Never silently falls back to Vladivostok.
pub async fn resolve_area(ctx: &ToolCtx, area: &str) -> Option<Area> {
    let normalized = area.to_lowercase().replace('ё', "е");
    {
        let cache = AREA_CACHE.lock().await;
        if let Some((_, _, resolved)) = cache
            .iter()
            .find(|(key, expires, _)| *key == normalized && *expires > Instant::now())
        {
            return Some((*resolved).clone());
        }
    }

    let resolved = resolve_area_uncached(ctx, area, &normalized).await;
    if let Some(resolved) = &resolved {
        let now = Instant::now();
        let mut cache = AREA_CACHE.lock().await;
        cache.retain(|(_, expires, _)| *expires > now);
        if cache.len() >= 64 {
            cache.remove(0);
        }
        cache.push((
            normalized,
            now + Duration::from_secs(1800),
            resolved.clone(),
        ));
    }
    resolved
}

/// Gazetter first, then online geocoders; `None` when nothing inside
/// Primorye resolves.
///
/// The curated gazetteer is authoritative for settlements: it is the only
/// source that knows the difference between the village Приморский and the
/// region, and it carries the search radius that keeps results inside the
/// place the user asked for. Geocoders handle lakes, rivers and landmarks.
async fn resolve_area_uncached(ctx: &ToolCtx, area: &str, normalized: &str) -> Option<Area> {
    let normalized = crate::domain::settlements::normalize(normalized);
    if crate::domain::settlements::mentions_region(&normalized) {
        return Some(Area {
            lat: 44.0,
            lon: 133.0,
            bbox: PRIMORYE_BBOX,
            label: "Приморский край".to_string(),
            radius_km: 400.0,
        });
    }

    if let Some((_name, lat, lon, bbox)) = KNOWN_NATURE
        .iter()
        .find(|(name, _, _, _)| normalized.contains(*name))
    {
        return Some(Area {
            lat: *lat,
            lon: *lon,
            bbox: *bbox,
            label: area.trim().to_string(),
            radius_km: 30.0,
        });
    }

    // A named settlement wins over any geocoder answer: "Кировский" must be
    // the settlement of that name, never the district or a namesake elsewhere.
    if let Some(settlement) = crate::domain::settlements::resolve(area) {
        return Some(area_around(
            settlement.lat,
            settlement.lon,
            settlement.scope_km(),
            area,
        ));
    }
    // A multi-city area ("Владивосток и Артём") still resolves to its first
    // named settlement, so the search starts where the user asked.
    if let Some(settlement) = crate::domain::settlements::find_all_in_text(area).first() {
        return Some(area_around(
            settlement.lat,
            settlement.lon,
            settlement.scope_km(),
            area,
        ));
    }

    if let Some((_name, lat, lon, radius_km)) = KNOWN_AREAS
        .iter()
        .find(|(name, _, _, _)| normalized.split_whitespace().any(|word| word == *name))
    {
        return Some(area_around(*lat, *lon, *radius_km, area));
    }

    if let Some(item) = nominatim(ctx, area, 1).await.first() {
        if let Some(mut resolved) = area_from_value(item) {
            resolved.label = area.trim().to_string();
            return Some(resolved);
        }
    }
    if let Some(item) = photon(ctx, area, 1).await.first() {
        if let Some(mut resolved) = area_from_value(item) {
            resolved.label = area.trim().to_string();
            return Some(resolved);
        }
    }
    None
}

/// Square search window around a place with a known radius in kilometres.
fn area_around(lat: f64, lon: f64, radius_km: f64, label: &str) -> Area {
    let delta_lat = radius_km / 111.0;
    let delta_lon = radius_km / (111.0 * lat.to_radians().cos().abs().max(0.2));
    Area {
        lat,
        lon,
        bbox: (
            lat - delta_lat,
            lon - delta_lon,
            lat + delta_lat,
            lon + delta_lon,
        ),
        label: label.trim().to_string(),
        radius_km,
    }
}

/// Builds an `Area` from a geocoder item, expanding point-like or oversized
/// bboxes into a usable search window.
fn area_from_value(item: &Value) -> Option<Area> {
    let lat = item.get("lat")?.as_f64()?;
    let lon = item.get("lon")?.as_f64()?;
    if !in_primorye(lat, lon) {
        return None;
    }
    let label = item
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("Приморье")
        .to_string();
    let item_type = item.get("type").and_then(Value::as_str).unwrap_or("");
    let bbox = item
        .get("bbox")
        .and_then(Value::as_array)
        .and_then(|array| {
            Some((
                array.first()?.as_f64()?,
                array.get(1)?.as_f64()?,
                array.get(2)?.as_f64()?,
                array.get(3)?.as_f64()?,
            ))
        })
        .unwrap_or((lat - 0.2, lon - 0.3, lat + 0.2, lon + 0.3));
    let (south, west, north, east) = bbox;
    let (south, west, north, east) = if north - south < 0.03 || east - west < 0.03 {
        // A point-like result (lighthouse, airport, small landmark): search
        // a walkable/drivable radius around it instead of a tiny bbox.
        (lat - 0.08, lon - 0.12, lat + 0.08, lon + 0.12)
    } else if north - south > 1.2 || east - west > 1.8 {
        (lat - 0.35, lon - 0.5, lat + 0.35, lon + 0.5)
    } else {
        (south, west, north, east)
    };
    let radius_km = radius_for_bbox((south, west, north, east), lat, item_type);
    Some(Area {
        lat,
        lon,
        bbox: (south, west, north, east),
        label,
        radius_km,
    })
}

/// Search radius for a geocoded area: the bbox diagonal, clamped so a district
/// polygon cannot turn into a region-wide search and a point result still has
/// a usable neighbourhood.
fn radius_for_bbox(bbox: (f64, f64, f64, f64), lat: f64, item_type: &str) -> f64 {
    let (south, west, north, east) = bbox;
    let height_km = (north - south) * 111.0;
    let width_km = (east - west) * 111.0 * lat.to_radians().cos().abs().max(0.2);
    let diagonal = (height_km * height_km + width_km * width_km).sqrt() / 2.0;
    let settlement = matches!(item_type, "city" | "town" | "village" | "hamlet");
    let max = if settlement { 25.0 } else { 35.0 };
    diagonal.clamp(5.0, max)
}

fn short_name(value: &str) -> String {
    clean_name(value.split(',').next().unwrap_or(value).trim())
}

/// Remove CJK characters that some OSM labels append to Russian names.
fn clean_name(value: &str) -> String {
    value
        .chars()
        .filter(|c| !matches!(*c as u32, 0x3000..=0x9FFF | 0xAC00..=0xD7AF | 0xF900..=0xFAFF | 0xFF00..=0xFFEF))
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Maps OSM tags and Russian name hints to the plan's item-kind vocabulary.
fn geo_kind(class: &str, kind: &str, name: &str) -> &'static str {
    let lower = name.to_lowercase();
    if lower.contains("озер")
        || (class == "natural" && kind == "water")
        || (class == "water" && kind == "lake")
    {
        "lake"
    } else if lower.contains("рек") || class == "waterway" || kind == "river" {
        "river"
    } else if lower.contains("пляж") || (class == "natural" && kind == "beach") {
        "beach"
    } else if matches!(
        kind,
        "hotel" | "guest_house" | "hostel" | "apartment" | "camp_site" | "chalet"
    ) || lower.contains("отель")
        || lower.contains("гостин")
    {
        "rest"
    } else if (class == "tourism" && kind == "museum") || lower.contains("музей") {
        "museum"
    } else if (class == "tourism" && kind == "viewpoint")
        || lower.contains("маяк")
        || lower.contains("смотров")
    {
        "viewpoint"
    } else if (class == "leisure" && matches!(kind, "park" | "nature_reserve"))
        || lower.contains("парк")
    {
        "park"
    } else if class == "historic" || lower.contains("памятник") || lower.contains("мемориал")
    {
        "historic"
    } else if lower.contains("бухт")
        || lower.contains("мыс")
        || lower.contains("остров")
        || lower.contains("сопк")
        || lower.contains("гор")
    {
        "nature"
    } else {
        "sight"
    }
}

/// Parses Nominatim's string-valued `boundingbox` into south/west/north/east.
fn parse_bbox(value: Option<&Value>) -> Option<(f64, f64, f64, f64)> {
    let array = value?.as_array()?;
    Some((
        array.first()?.as_str()?.parse().ok()?,
        array.get(1)?.as_str()?.parse().ok()?,
        array.get(2)?.as_str()?.parse().ok()?,
        array.get(3)?.as_str()?.parse().ok()?,
    ))
}

/// Nearest city with a VL.ru weather page; used only as a weather fallback.
pub fn nearest_city(lat: f64, lon: f64) -> Option<(&'static str, &'static str)> {
    const CITIES: &[(&str, f64, f64, &str)] = &[
        ("Владивосток", 43.1155, 131.8855, "vladivostok"),
        ("Уссурийск", 43.7973, 131.9518, "ussuriysk"),
        ("Находка", 42.8167, 132.8731, "nakhodka"),
        ("Арсеньев", 44.1620, 133.2820, "arsenyev"),
        ("Дальнегорск", 44.5540, 135.5700, "dalnegorsk"),
        ("Спасск-Дальний", 44.6000, 132.8200, "spassk-dalniy"),
        ("Славянка", 42.8640, 131.3910, "slavyanka"),
        ("Андреевка", 42.6159, 131.1177, "andreevka"),
    ];
    CITIES
        .iter()
        .min_by(|left, right| {
            let left_distance = haversine_km((lat, lon), (left.1, left.2));
            let right_distance = haversine_km((lat, lon), (right.1, right.2));
            left_distance
                .partial_cmp(&right_distance)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|(name, _, _, slug)| (*name, *slug))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gazetteer_resolves_settlements_before_geocoders() {
        let settlement = crate::domain::settlements::resolve("Уссурийск").expect("gazetteer hit");
        assert_eq!(settlement.name, "Уссурийск");
        assert!(settlement.scope_km() >= 9.0);
    }

    #[test]
    fn area_around_keeps_the_window_near_the_place() {
        let area = area_around(45.0898, 133.5062, 15.0, "Кировский");
        assert_eq!(area.radius_km, 15.0);
        let (south, west, north, east) = area.bbox;
        assert!(area.contains_point(45.0898, 133.5062));
        assert!(!area.contains_point(45.6, 133.5));
        assert!(south < area.lat && north > area.lat);
        assert!(west < area.lon && east > area.lon);
    }

    #[test]
    fn region_text_is_the_krai_not_a_village() {
        assert!(crate::domain::settlements::mentions_region(
            &crate::domain::settlements::normalize("по Приморью на выходные")
        ));
        assert!(!crate::domain::settlements::mentions_region(
            &crate::domain::settlements::normalize("поехали в Приморский")
        ));
    }

    #[test]
    fn nearest_city_for_ussuriysk() {
        let city = nearest_city(43.7973, 131.9518).unwrap();
        assert_eq!(city.0, "Уссурийск");
    }
}
