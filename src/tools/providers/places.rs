use std::collections::HashSet;

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::domain::facts::{SourceKind, ToolOutput};
use crate::tools::providers::{fallback_places, limit_from, load_mock, PRIMORYE_BBOX};
use crate::tools::registry::{Tool, ToolCtx, ToolError};

pub struct PlacesTool;

#[async_trait]
impl Tool for PlacesTool {
    fn name(&self) -> &'static str {
        "search_places"
    }

    fn description(&self) -> &'static str {
        "Найти достопримечательности, музеи и природные точки в Приморском крае. Источники: OSM Overpass, затем Википедия. Возвращает place_id, координаты, часы работы."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "area": {"type": "string"},
                "categories": {"type": "array", "items": {"type": "string"}},
                "limit": {"type": "integer", "minimum": 1, "maximum": 40},
                "date": {"type": "string"}
            },
            "required": ["area"]
        })
    }

    async fn call(&self, args: Value, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let limit = limit_from(&args, 12, 40);
        let area = args
            .get("area")
            .and_then(Value::as_str)
            .unwrap_or("Владивосток");
        let (lat, lon) = area_coords(area);
        if let Some(places) = wikipedia_places(ctx, lat, lon, limit).await {
            return Ok(ToolOutput::ok(
                json!({"places": places}),
                SourceKind::Live,
                format!("Википедия: {} реальных объектов", places.len()),
            ));
        }

        let query = format!(
            "[out:json][timeout:10];\
             (\
               node[\"tourism\"]({bbox});\
               node[\"historic\"]({bbox});\
               node[\"leisure\"~\"park|nature_reserve\"]({bbox});\
               node[\"natural\"]({bbox});\
             );\
             out center 50;",
            bbox = PRIMORYE_BBOX
        );

        if let Ok(response) = ctx
            .http
            .post("https://overpass-api.de/api/interpreter")
            .body(query)
            .send()
            .await
        {
            if response.status().is_success() {
                if let Ok(value) = response.json::<Value>().await {
                    let mut places = parse_overpass(&value);
                    if !places.is_empty() {
                        places.truncate(limit);
                        return Ok(ToolOutput::ok(
                            json!({"places": places}),
                            SourceKind::Live,
                            format!("OSM Overpass: {} мест", places.len()),
                        ));
                    }
                }
            }
        }

        let mock = load_mock(ctx.data_dir.as_path(), "places.json", fallback_places());
        let mut places = mock
            .get("places")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        places.truncate(limit);
        Ok(ToolOutput::ok(
            json!({"places": places}),
            SourceKind::Mock,
            "OSM и Википедия недоступны: демонстрационные места",
        ))
    }
}

fn area_coords(area: &str) -> (f64, f64) {
    let area = area.to_lowercase();
    let table = [
        ("владивосток", (43.1155, 131.8855)),
        ("русский", (43.0350, 131.8000)),
        ("уссурийск", (43.7973, 131.9518)),
        ("находка", (42.8167, 132.8731)),
        ("артём", (43.3550, 132.1880)),
        ("артем", (43.3550, 132.1880)),
        ("арсеньев", (44.1620, 133.2820)),
        ("дальнегорск", (44.5540, 135.5700)),
        ("славянка", (42.8640, 131.3910)),
        ("андреевка", (42.6159, 131.1177)),
        ("партизанск", (43.1280, 133.1260)),
        ("большой камень", (43.1136, 132.3481)),
        ("фокино", (42.9710, 132.4110)),
        ("терней", (45.0500, 136.6000)),
    ];
    table
        .iter()
        .find(|(name, _)| area.contains(name))
        .map(|(_, coords)| *coords)
        .unwrap_or((43.1155, 131.8855))
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

fn kind_from_title(title: &str) -> &'static str {
    let title = title.to_lowercase();
    if title.contains("музей") || title.contains("галерея") {
        "museum"
    } else if title.contains("маяк") {
        "viewpoint"
    } else if title.contains("бухта")
        || title.contains("мыс")
        || title.contains("остров")
        || title.contains("сопка")
        || title.contains("гора")
        || title.contains("озеро")
    {
        "nature"
    } else if title.contains("парк") || title.contains("сад") {
        "park"
    } else if title.contains("театр") || title.contains("филармония") {
        "theatre"
    } else if title.contains("собор") || title.contains("церковь") || title.contains("храм")
    {
        "religion"
    } else if title.contains("памятник") || title.contains("мемориал") {
        "monument"
    } else {
        "sight"
    }
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

fn parse_overpass(value: &Value) -> Vec<Value> {
    let Some(elements) = value.get("elements").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut places = Vec::new();
    let mut seen_ids = HashSet::new();
    let mut seen_names = HashSet::new();
    for element in elements {
        let Some(tags) = element.get("tags") else {
            continue;
        };
        let name = tags
            .get("name:ru")
            .or_else(|| tags.get("name"))
            .and_then(Value::as_str);
        let Some(name) = name else {
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
        let normalized = normalize_title(name);
        if !seen_ids.insert(id.clone()) || !seen_names.insert(normalized) {
            continue;
        }
        let kind = tags
            .get("tourism")
            .or_else(|| tags.get("historic"))
            .or_else(|| tags.get("leisure"))
            .or_else(|| tags.get("natural"))
            .and_then(Value::as_str)
            .unwrap_or("sight");
        let opening_hours = tags.get("opening_hours").and_then(Value::as_str);

        places.push(json!({
            "id": id,
            "name": name,
            "lat": lat,
            "lon": lon,
            "kind": kind,
            "opening_hours": opening_hours,
            "source": "live"
        }));
    }
    places
}
