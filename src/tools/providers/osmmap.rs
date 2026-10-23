//! OpenStreetMap map-API tiles: the fast, dependable source of business POIs
//! around a point.
//!
//! The map API takes no category filter and caps one request at 50k nodes, so a
//! tile is a snapshot of one neighbourhood and never the whole city. The search
//! therefore walks tiles outwards, nearest first, and stops as soon as enough
//! objects are in hand: a dense city-centre tile answers a "where to eat"
//! request on its own, while a village needs the whole 3x3 grid because its
//! tiles are small and hold one or two guest houses each.
//!
//! Tile size is a latency budget, not a preference: the centre tile of a
//! 0.008-degree box around Vladivostok is 14 MB and arrives at 2.8-3.0 s, which
//! is why a 3-second timeout silently returned nothing and the search fell back
//! to the demo catalogue. A 0.006-degree box is about 5 MB and arrives in under
//! two seconds, so the timeout below has room to spare.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::domain::geo::{haversine_km, on_land_strict};
use crate::tools::providers::is_place_name;
use crate::tools::registry::ToolCtx;

/// Half-height of one tile in degrees (~670 m). Kept small enough that a city
/// centre tile fits comfortably under the node cap and the timeout.
const TILE_SPAN: f64 = 0.006;
/// Distance between neighbouring tile centres: one tile apart, so the grid
/// covers the area without gaps and without overlapping requests.
const STEP_LAT: f64 = 0.012;
const STEP_LON: f64 = 0.0144;

/// The centre tile carries the answer in almost every case, so it gets the
/// generous timeout; the ring and the corners are already secondary.
const CENTRE_TIMEOUT: Duration = Duration::from_secs(6);
const RING_TIMEOUT: Duration = Duration::from_secs(3);
const CORNER_TIMEOUT: Duration = Duration::from_secs(2);

/// Ceiling for the whole tile walk of one search. A city centre tile alone
/// answers a "where to eat" request; the ring is only there for villages and
/// sparse tags, where tiles arrive in a fraction of a second anyway.
const TILE_BUDGET: Duration = Duration::from_secs(7);

/// One OSM object found in a tile, in the shape the providers need.
#[derive(Debug, Clone)]
pub struct Poi {
    pub id: String,
    pub name: String,
    pub lat: f64,
    pub lon: f64,
    /// Plan vocabulary kind (`food`, `rest`, `museum`, …).
    pub kind: &'static str,
    /// Raw `tourism` value, for providers with their own vocabulary (lodging).
    pub tourism: Option<String>,
    /// Raw `brand`/`operator` value: a fuel station is placed by its network.
    pub brand: Option<String>,
    pub opening_hours: Option<String>,
    pub distance_meters: f64,
}

impl Poi {
    /// The `search_places` row shape.
    pub fn to_place(&self) -> Value {
        json!({
            "id": self.id,
            "name": self.name,
            "lat": self.lat,
            "lon": self.lon,
            "kind": self.kind,
            "opening_hours": self.opening_hours,
            "source": "live",
            "distance_meters": self.distance_meters,
        })
    }
}

/// One tile of the wanted tags around a point. Callers with their own walk
/// (the fuel corridor visits the stops of a route) use this directly.
pub async fn tile(
    ctx: &ToolCtx,
    lat: f64,
    lon: f64,
    wanted: &[String],
    timeout: Duration,
) -> Vec<Poi> {
    if wanted.is_empty() {
        return Vec::new();
    }
    match fetch(ctx, lat, lon, TILE_SPAN, timeout).await {
        Some(xml) => parse(&xml, lat, lon, wanted),
        None => Vec::new(),
    }
}

/// POIs carrying one of the wanted tag values around a point, nearest first.
///
/// `target` is the number of objects that makes the answer good enough to stop
/// paying for more tiles. The reference point for `distance_meters` is the
/// requested centre, not the tile centre, so the caller can order and scope the
/// rows without recomputing anything.
pub async fn search(
    ctx: &ToolCtx,
    lat: f64,
    lon: f64,
    wanted: &[String],
    target: usize,
    deadline: Instant,
) -> Vec<Poi> {
    if wanted.is_empty() {
        return Vec::new();
    }
    let target = target.max(1);
    // The tiles are the fast source, but a dense city tile costs seconds and a
    // whole ring of them cost the entire search budget: the walk gets its own
    // ceiling, and whatever the tiles miss is answered by the category search
    // running next to them.
    let tile_deadline = deadline.min(Instant::now() + TILE_BUDGET);
    let mut found: Vec<Poi> = Vec::new();
    let mut seen_ids: HashSet<String> = HashSet::new();
    let mut seen_names: Vec<(String, f64, f64)> = Vec::new();

    let mut take = |tile: Option<String>, found: &mut Vec<Poi>| {
        let Some(xml) = tile else { return };
        for poi in parse(&xml, lat, lon, wanted) {
            if seen_ids.insert(poi.id.clone())
                && !seen_names.iter().any(|(name, known_lat, known_lon)| {
                    *name == poi.name.to_lowercase()
                        && haversine_km((poi.lat, poi.lon), (*known_lat, *known_lon)) < 0.1
                })
            {
                seen_names.push((poi.name.to_lowercase(), poi.lat, poi.lon));
                found.push(poi);
            }
        }
    };

    take(
        fetch(
            ctx,
            lat,
            lon,
            TILE_SPAN,
            remaining(tile_deadline, CENTRE_TIMEOUT),
        )
        .await,
        &mut found,
    );

    // The ring and the corners: fetched one after another, because the API
    // throttles parallel map calls hard (six tiles at once arrived in 7-12 s
    // each instead of 2 s), and only while the answer is still thin.
    for (delta_lat, delta_lon, timeout) in ring(STEPS) {
        if found.len() >= target || Instant::now() >= tile_deadline {
            break;
        }
        take(
            fetch(
                ctx,
                lat + delta_lat,
                lon + delta_lon,
                TILE_SPAN,
                remaining(tile_deadline, timeout),
            )
            .await,
            &mut found,
        );
    }

    found.sort_by(|left, right| {
        left.distance_meters
            .partial_cmp(&right.distance_meters)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    found
}

/// Orthogonal neighbours first, then the diagonals: the diagonal tiles are the
/// last thing fetched, and in a city they are usually never needed.
const STEPS: [(f64, f64); 8] = [
    (STEP_LAT, 0.0),
    (-STEP_LAT, 0.0),
    (0.0, STEP_LON),
    (0.0, -STEP_LON),
    (STEP_LAT, STEP_LON),
    (STEP_LAT, -STEP_LON),
    (-STEP_LAT, STEP_LON),
    (-STEP_LAT, -STEP_LON),
];

fn ring(steps: [(f64, f64); 8]) -> impl Iterator<Item = (f64, f64, Duration)> {
    steps.into_iter().map(|(delta_lat, delta_lon)| {
        let timeout = if delta_lat != 0.0 && delta_lon != 0.0 {
            CORNER_TIMEOUT
        } else {
            RING_TIMEOUT
        };
        (delta_lat, delta_lon, timeout)
    })
}

/// Never wait past the caller's deadline for one tile.
fn remaining(deadline: Instant, wanted: Duration) -> Duration {
    deadline
        .saturating_duration_since(Instant::now())
        .min(wanted)
}

/// One OSM map-API tile. The timeout covers the whole exchange: a dense city
/// tile is several megabytes, and reading such a body without a limit is what
/// pushed a search past the 15-second tool timeout.
async fn fetch(ctx: &ToolCtx, lat: f64, lon: f64, span: f64, timeout: Duration) -> Option<String> {
    if timeout.is_zero() {
        return None;
    }
    let bbox = format!(
        "{:.5},{:.5},{:.5},{:.5}",
        lon - span * 1.2,
        lat - span,
        lon + span * 1.2,
        lat + span
    );
    let url = format!("https://api.openstreetmap.org/api/0.6/map?bbox={bbox}");
    let user_agent = ctx.nominatim_user_agent.clone();
    let request = async move {
        let response = ctx
            .http
            .get(&url)
            .header("User-Agent", user_agent)
            .send()
            .await
            .ok()?;
        if !response.status().is_success() {
            return None;
        }
        response.text().await.ok()
    };
    tokio::time::timeout(timeout, request).await.ok()?
}

/// Tag values of one tile that carry a name, are on land and match `wanted`.
/// Cinema, mall and theatre polygons are mapped as ways, so their centre is
/// built from the referenced nodes.
fn parse(xml: &str, origin_lat: f64, origin_lon: f64, wanted: &[String]) -> Vec<Poi> {
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

    let mut pois = Vec::new();
    let mut seen_ids: HashSet<String> = HashSet::new();
    let mut seen_names: Vec<(String, f64, f64)> = Vec::new();
    let mut push = |id: String, tags: &HashMap<String, String>, lat: f64, lon: f64| {
        let Some(name) = tags
            .get("name:ru")
            .or_else(|| tags.get("name"))
            .map(|name| name.trim().to_string())
            .filter(|name| !name.is_empty() && is_place_name(name))
        else {
            return;
        };
        if !seen_ids.insert(id.clone()) {
            return;
        }
        let key = name.to_lowercase();
        if seen_names.iter().any(|(known, known_lat, known_lon)| {
            *known == key && haversine_km((lat, lon), (*known_lat, *known_lon)) < 0.1
        }) {
            return;
        }
        seen_names.push((key, lat, lon));
        pois.push(Poi {
            id,
            name,
            lat,
            lon,
            kind: osm_kind(tags),
            tourism: tags.get("tourism").cloned(),
            brand: tags
                .get("brand")
                .or_else(|| tags.get("operator"))
                .map(|brand| brand.trim().to_string())
                .filter(|brand| !brand.is_empty()),
            opening_hours: tags.get("opening_hours").cloned(),
            distance_meters: (haversine_km((origin_lat, origin_lon), (lat, lon)) * 1000.0).round(),
        });
    };

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
        let tags = parse_tags(body);
        if osm_map_matches(&tags, wanted) {
            push(format!("osm:node/{id}"), &tags, lat, lon);
        }
    }

    for chunk in xml.split("<way ").skip(1) {
        let Some(header_end) = chunk.find('>') else {
            continue;
        };
        let header = &chunk[..header_end];
        let Some(id) = xml_attr(header, "id") else {
            continue;
        };
        let body = chunk[header_end + 1..].split("</way>").next().unwrap_or("");
        let tags = parse_tags(body);
        if !osm_map_matches(&tags, wanted) {
            continue;
        }
        let mut sum_lat = 0.0;
        let mut sum_lon = 0.0;
        let mut count = 0usize;
        for nd in body.split("<nd ").skip(1) {
            let end = nd.find("/>").or_else(|| nd.find('>')).unwrap_or(nd.len());
            let Some(reference) = xml_attr(&nd[..end], "ref") else {
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
        push(format!("osm:way/{id}"), &tags, lat, lon);
    }

    pois
}

fn parse_tags(body: &str) -> HashMap<String, String> {
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
    tags
}

/// Plan vocabulary of one OSM object. The order is deliberate: an object tagged
/// `amenity=cafe` inside a mall is still a café, not a shop.
pub fn osm_kind(tags: &HashMap<String, String>) -> &'static str {
    [
        "amenity", "natural", "man_made", "leisure", "shop", "tourism", "historic",
    ]
    .iter()
    .find_map(|key| tags.get(*key).and_then(|value| kind_from_tag(key, value)))
    .unwrap_or("sight")
}

/// Plan vocabulary of one OSM tag pair, or `None` when the pair says nothing
/// about the kind of place. Providers that receive tag pairs instead of a tag
/// map (the Nominatim phrase search) use this directly.
pub fn kind_from_tag(category: &str, value: &str) -> Option<&'static str> {
    Some(match (category, value) {
        ("amenity", "nightclub") => "club",
        ("amenity", "bar" | "pub") => "bar",
        ("amenity", "restaurant" | "cafe" | "fast_food" | "food_court" | "ice_cream")
        | ("amenity", "bakery") => "food",
        ("amenity", "marketplace") => "market",
        ("amenity", "theatre") => "theater",
        ("amenity", "cinema") => "cinema",
        ("amenity", "hotel" | "hostel" | "guest_house" | "motel" | "apartment") => "rest",
        ("amenity", "sauna" | "public_bath") => "rest",
        ("amenity", "museum") => "museum",
        ("amenity", "place_of_worship" | "monastery") => "historic",
        // A bakery is where people eat, whichever key the mapper used.
        ("shop", "bakery" | "pastry" | "confectionery") => "food",
        ("shop", _) => "shop",
        ("natural", "waterfall" | "cave_entrance" | "peak" | "spring") => "nature",
        ("natural", "beach") => "beach",
        ("natural", "water") => "nature",
        ("waterway", "waterfall") => "nature",
        ("waterway", "river" | "stream") => "river",
        ("man_made", "lighthouse") => "viewpoint",
        ("leisure", "park" | "nature_reserve" | "garden") => "park",
        ("leisure", "resort" | "water_park" | "swimming_pool" | "bowling_alley") => "park",
        ("leisure", "sports_centre" | "horse_riding" | "sauna") => "park",
        ("tourism", "museum") => "museum",
        ("tourism", "viewpoint") => "viewpoint",
        ("tourism", "zoo" | "aquarium" | "theme_park" | "attraction" | "artwork") => "sight",
        ("tourism", "hotel" | "guest_house" | "hostel" | "motel" | "apartment" | "chalet")
        | ("tourism", "camp_site")
        | ("tourism", "resort") => "rest",
        ("historic", _) => "historic",
        _ => return None,
    })
}

/// True when any of the eight business keys carries one of the wanted values.
/// A bare key name (`tourism`, `historic`) matches every value of that key, so
/// a caller can ask for "all hotels" without listing the values.
fn osm_map_matches(tags: &HashMap<String, String>, wanted: &[String]) -> bool {
    let keys = [
        "amenity", "shop", "tourism", "leisure", "natural", "man_made", "sport", "historic",
    ];
    keys.iter().any(|key| {
        tags.get(*key)
            .is_some_and(|value| wanted.iter().any(|wanted| wanted == value))
            || (tags.get(*key).is_some() && wanted.iter().any(|wanted| wanted == *key))
    })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nodes_and_ways_with_the_wanted_tags() {
        let xml = r#"<osm>
        <node id="1" lat="43.1155" lon="131.8855"><tag k="amenity" v="bar"/><tag k="name" v="Soda Bar"/></node>
        <node id="2" lat="43.116" lon="131.886"><tag k="amenity" v="cafe"/><tag k="name" v="Cafe"/></node>
        <node id="3" lat="43.117" lon="131.887"><tag k="amenity" v="parking"/><tag k="name" v="Parking"/></node>
        <node id="4" lat="43.114" lon="131.884"/>
        <node id="5" lat="43.113" lon="131.883"/>
        <way id="10"><nd ref="4"/><nd ref="5"/><tag k="amenity" v="cinema"/><tag k="name" v="Кино"/></way>
        </osm>"#;
        let pois = parse(
            xml,
            43.1155,
            131.8855,
            &["bar".to_string(), "cafe".to_string()],
        );
        assert_eq!(pois.len(), 2);
        assert_eq!(pois[0].name, "Soda Bar");
        assert_eq!(pois[0].kind, "bar");
        assert_eq!(pois[1].kind, "food");

        let cinemas = parse(xml, 43.1155, 131.8855, &["cinema".to_string()]);
        assert_eq!(cinemas.len(), 1, "way centre must be parsed");
        assert_eq!(cinemas[0].kind, "cinema");
        assert!(cinemas[0].distance_meters > 0.0);
    }

    #[test]
    fn lodging_family_shares_one_kind_and_keeps_tourism() {
        let xml = r#"<osm>
        <node id="1" lat="43.1155" lon="131.8855"><tag k="tourism" v="hotel"/><tag k="name" v="Отель"/></node>
        <node id="2" lat="43.116" lon="131.886"><tag k="tourism" v="guest_house"/><tag k="name" v="Гостевой дом"/></node>
        <node id="3" lat="43.117" lon="131.887"><tag k="tourism" v="hostel"/><tag k="name" v="Хостел"/></node>
        </osm>"#;
        let wanted: Vec<String> = ["hotel", "guest_house", "hostel"]
            .iter()
            .map(|value| value.to_string())
            .collect();
        let pois = parse(xml, 43.1155, 131.8855, &wanted);
        assert_eq!(pois.len(), 3);
        assert!(pois.iter().all(|poi| poi.kind == "rest"));
        assert_eq!(pois[0].tourism.as_deref(), Some("hotel"));
        assert_eq!(pois[1].tourism.as_deref(), Some("guest_house"));
    }

    #[test]
    fn a_bare_key_matches_every_value_of_that_key() {
        let tags: HashMap<String, String> = [("historic".to_string(), "monument".to_string())]
            .into_iter()
            .collect();
        assert!(osm_map_matches(&tags, &["historic".to_string()]));
        assert!(!osm_map_matches(&tags, &["tourism".to_string()]));
    }
}
