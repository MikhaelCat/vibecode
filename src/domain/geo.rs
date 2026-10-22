use std::sync::OnceLock;

use serde_json::Value;

use crate::domain::intent::TransportMode;

/// Primorsky Krai bounding box: south, west, north, east.
pub const PRIMORYE_BBOX: (f64, f64, f64, f64) = (42.2, 130.0, 48.5, 139.1);

/// A point within this distance of the coastline counts as land. Beaches and
/// piers sit exactly on the line, and a generalized coastline may put them a
/// few hundred metres into the water.
const SHORE_TOLERANCE_DEG: f64 = 0.002;

type Ring = Vec<(f64, f64)>; // (lon, lat)

struct AreaPoly {
    bbox: (f64, f64, f64, f64), // min_lon, min_lat, max_lon, max_lat
    outer: Ring,
    holes: Vec<Ring>,
}

static REGION: OnceLock<Vec<AreaPoly>> = OnceLock::new();
static LAND: OnceLock<Vec<AreaPoly>> = OnceLock::new();

fn region() -> &'static Vec<AreaPoly> {
    REGION.get_or_init(|| parse_geojson(include_str!("../../data/geo/primorye_krai.geojson")))
}

fn land() -> &'static Vec<AreaPoly> {
    LAND.get_or_init(|| parse_geojson(include_str!("../../data/geo/primorye_land.geojson")))
}

fn parse_ring(value: &Value) -> Ring {
    value
        .as_array()
        .map(|points| {
            points
                .iter()
                .filter_map(|point| Some((point.get(0)?.as_f64()?, point.get(1)?.as_f64()?)))
                .collect()
        })
        .unwrap_or_default()
}

fn parse_polygon(value: &Value) -> Option<AreaPoly> {
    let rings = value.as_array()?;
    let outer = parse_ring(rings.first()?);
    if outer.len() < 4 {
        return None;
    }
    let holes = rings
        .iter()
        .skip(1)
        .map(parse_ring)
        .filter(|ring| ring.len() >= 4)
        .collect();
    let (mut min_lon, mut min_lat, mut max_lon, mut max_lat) = (
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    );
    for &(lon, lat) in &outer {
        min_lon = min_lon.min(lon);
        max_lon = max_lon.max(lon);
        min_lat = min_lat.min(lat);
        max_lat = max_lat.max(lat);
    }
    Some(AreaPoly {
        bbox: (min_lon, min_lat, max_lon, max_lat),
        outer,
        holes,
    })
}

fn parse_geojson(text: &str) -> Vec<AreaPoly> {
    let Ok(root) = serde_json::from_str::<Value>(text) else {
        return Vec::new();
    };
    let mut polygons = Vec::new();
    for feature in root
        .get("features")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(geometry) = feature.get("geometry") else {
            continue;
        };
        let coordinates = geometry.get("coordinates");
        match geometry.get("type").and_then(Value::as_str) {
            Some("Polygon") => polygons.extend(coordinates.and_then(parse_polygon)),
            Some("MultiPolygon") => {
                for polygon in coordinates.and_then(Value::as_array).into_iter().flatten() {
                    polygons.extend(parse_polygon(polygon));
                }
            }
            _ => {}
        }
    }
    polygons
}

fn ring_contains(ring: &[(f64, f64)], lat: f64, lon: f64) -> bool {
    let mut inside = false;
    let mut j = ring.len() - 1;
    for i in 0..ring.len() {
        let (xi, yi) = ring[i];
        let (xj, yj) = ring[j];
        if (yi > lat) != (yj > lat) && lon < (xj - xi) * (lat - yi) / (yj - yi) + xi {
            inside = !inside;
        }
        j = i;
    }
    inside
}

fn distance_to_segment(lat: f64, lon: f64, a: (f64, f64), b: (f64, f64)) -> f64 {
    let (ax, ay) = (a.0, a.1);
    let (bx, by) = (b.0, b.1);
    let dx = bx - ax;
    let dy = by - ay;
    let length_squared = dx * dx + dy * dy;
    let t = if length_squared == 0.0 {
        0.0
    } else {
        (((lon - ax) * dx + (lat - ay) * dy) / length_squared).clamp(0.0, 1.0)
    };
    let px = ax + t * dx;
    let py = ay + t * dy;
    ((lon - px).powi(2) + (lat - py).powi(2)).sqrt()
}

fn ring_near(ring: &[(f64, f64)], lat: f64, lon: f64, tolerance: f64) -> bool {
    if ring.len() < 2 {
        return false;
    }
    let mut previous = ring[ring.len() - 1];
    for &point in ring {
        if distance_to_segment(lat, lon, previous, point) <= tolerance {
            return true;
        }
        previous = point;
    }
    false
}

fn contains(poly: &AreaPoly, lat: f64, lon: f64) -> bool {
    let (min_lon, min_lat, max_lon, max_lat) = poly.bbox;
    lon >= min_lon
        && lon <= max_lon
        && lat >= min_lat
        && lat <= max_lat
        && ring_contains(&poly.outer, lat, lon)
        && !poly.holes.iter().any(|hole| ring_contains(hole, lat, lon))
}

fn near_shore(poly: &AreaPoly, lat: f64, lon: f64, tolerance: f64) -> bool {
    let (min_lon, min_lat, max_lon, max_lat) = poly.bbox;
    lon >= min_lon - tolerance
        && lon <= max_lon + tolerance
        && lat >= min_lat - tolerance
        && lat <= max_lat + tolerance
        && ring_near(&poly.outer, lat, lon, tolerance)
}

/// True when the point is inside the Primorye bounding box. This is the cheap
/// area check used when resolving lakes and rivers: their centre may be in
/// water, so it must not depend on the land mask. Use [`on_land`] for stops.
pub fn in_primorye(lat: f64, lon: f64) -> bool {
    let (south, west, north, east) = PRIMORYE_BBOX;
    lat >= south && lat <= north && lon >= west && lon <= east
}

/// True when the point is inside the administrative border of Primorsky Krai.
/// The bounding box is only a pre-check; the polygon rejects Chinese and
/// Korean border settlements.
fn in_krai(lat: f64, lon: f64) -> bool {
    if !in_primorye(lat, lon) {
        return false;
    }
    let polygons = region();
    if polygons.is_empty() {
        return true;
    }
    polygons.iter().any(|poly| contains(poly, lat, lon))
}

/// True when the point is inside Primorye AND on land. Bays, lakes and the sea
/// are rejected. If the land file is missing or empty, falls back to the region
/// check. Points within a few hundred metres of the coastline count as land so
/// beaches, capes and piers are not filtered out.
pub fn on_land(lat: f64, lon: f64) -> bool {
    if !in_krai(lat, lon) {
        return false;
    }
    let polygons = land();
    if polygons.is_empty() {
        return true;
    }
    let inside = polygons.iter().any(|poly| contains(poly, lat, lon));
    inside
        || polygons
            .iter()
            .any(|poly| near_shore(poly, lat, lon, SHORE_TOLERANCE_DEG))
}

/// True when a title names a water area rather than a point on land.
/// Such objects are useful as search areas, but a centroid marker in the sea
/// or in a lake looks wrong, so providers never return them as itinerary
/// stops. Shore objects (beaches, capes, viewpoints, settlements) stay.
pub fn waterbody_title(title: &str) -> bool {
    let lower = title.to_lowercase();
    // Wikipedia disambiguates water articles in parentheses:
    // "Находка (бухта, Японское море)", "Андреевка (река, ...)".
    if let Some(open) = lower.find('(') {
        let descriptor = lower[open..]
            .split(|c: char| !c.is_alphabetic())
            .filter(|word| !word.is_empty());
        if descriptor.into_iter().any(is_water_word) {
            return true;
        }
    }
    let words: Vec<&str> = lower
        .split(|c: char| !c.is_alphabetic())
        .filter(|word| !word.is_empty())
        .collect();
    // The head of a water name is either first ("Бухта Новик", "Озеро Ханка")
    // or last ("Японское море"). A land object that merely mentions water
    // ("Смотровая площадка «Бухта Находка»", "Морской вокзал") stays.
    words.first().is_some_and(|word| is_water_word(word))
        || words.last().is_some_and(|word| is_water_word(word))
}

fn is_water_word(word: &str) -> bool {
    word.starts_with("бухт")
        || matches!(
            word,
            "залив"
                | "залива"
                | "заливе"
                | "заливом"
                | "заливы"
                | "заливов"
                | "заливам"
                | "заливами"
                | "заливах"
        )
        || word.starts_with("озер")
        || word.starts_with("озёр")
        || matches!(
            word,
            "река"
                | "реки"
                | "реке"
                | "реку"
                | "рекой"
                | "рекою"
                | "рек"
                | "рекам"
                | "реками"
                | "реках"
        )
        || matches!(
            word,
            "море" | "моря" | "морю" | "морем" | "морям" | "морями" | "морях"
        )
        || word.starts_with("водохранилищ")
        || word.starts_with("пруд")
        || word.starts_with("пролив")
        || word.starts_with("ковш")
        || word.starts_with("лиман")
        || word.starts_with("затон")
        || word.starts_with("стариц")
        || word.starts_with("проток")
        || word.starts_with("канал")
}

pub fn haversine_km(a: (f64, f64), b: (f64, f64)) -> f64 {
    const R: f64 = 6371.0;
    let d_lat = (b.0 - a.0).to_radians();
    let d_lon = (b.1 - a.1).to_radians();
    let lat1 = a.0.to_radians();
    let lat2 = b.0.to_radians();
    let h = (d_lat / 2.0).sin().powi(2) + lat1.cos() * lat2.cos() * (d_lon / 2.0).sin().powi(2);
    2.0 * R * h.sqrt().asin()
}

pub fn travel_minutes(distance_km: f64, mode: TransportMode) -> u32 {
    let speed = match mode {
        TransportMode::Car => 55.0,
        TransportMode::Foot => 4.5,
        TransportMode::Bike => 15.0,
        TransportMode::Transit => 35.0,
    };
    ((distance_km / speed) * 60.0).ceil().max(1.0) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vladivostok_is_inside() {
        assert!(in_primorye(43.1155, 131.8855));
        assert!(!in_primorye(43.5855, 39.7231)); // Sochi
    }

    #[test]
    fn bays_are_not_land() {
        assert!(on_land(43.1155, 131.8855)); // Vladivostok
        assert!(!on_land(43.15, 131.82)); // Amur Bay
        assert!(!on_land(42.80, 132.00)); // Peter the Great Gulf
        assert!(!on_land(42.87, 130.36)); // Hunchun, China
    }

    #[test]
    fn coastal_places_are_land() {
        assert!(on_land(42.9450, 131.8708)); // Cape Tobizina
        assert!(on_land(42.6479, 131.1270)); // Andreevka beach
        assert!(on_land(42.8164, 132.8892)); // Nakhodka bay viewpoint
        assert!(on_land(43.0050, 131.9249)); // Primorsky Oceanarium
        assert!(on_land(43.0731, 131.8431)); // Tokarevsky lighthouse
    }

    #[test]
    fn haversine_positive() {
        let km = haversine_km((43.1155, 131.8855), (43.8, 131.95));
        assert!(km > 70.0 && km < 90.0, "km={km}");
    }

    #[test]
    fn waterbody_titles_are_not_stops() {
        assert!(waterbody_title("Находка (бухта, Японское море)"));
        assert!(waterbody_title("Солёное (озеро, Находка)"));
        assert!(waterbody_title("озеро Ханка"));
        assert!(waterbody_title("река Раздольная"));
        assert!(!waterbody_title("Пляж Андреевки"));
        assert!(!waterbody_title("Морской вокзал"));
        assert!(!waterbody_title("Мыс Тобизина"));
        assert!(!waterbody_title("Смотровая площадка «Бухта Находка»"));
    }
}
