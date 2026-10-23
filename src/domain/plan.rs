use std::collections::{BTreeMap, HashSet};

use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use serde::{de, Deserialize, Deserializer, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::domain::dates::{add_days, days_inclusive};
use crate::domain::facts::{
    EventFact, Facts, FuelStation, Lodging, PlaceFact, SourceKind, SourceRef,
};
use crate::domain::geo::{haversine_km, on_land, travel_minutes, waterbody_title};
use crate::domain::intent::{Intent, TransportMode};
use crate::domain::spatial::KdTree;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ItemKind {
    #[default]
    Sight,
    Museum,
    Nature,
    Viewpoint,
    Park,
    Historic,
    Beach,
    Lake,
    River,
    Cinema,
    Theater,
    Market,
    Shop,
    Food,
    Bar,
    Club,
    Transfer,
    Rest,
}

impl ItemKind {
    pub fn is_outdoor(self) -> bool {
        matches!(
            self,
            Self::Nature
                | Self::Viewpoint
                | Self::Park
                | Self::Historic
                | Self::Beach
                | Self::Lake
                | Self::River
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Generation {
    #[default]
    Llm,
    Repaired,
    Fallback,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Warning {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanItem {
    pub place_id: String,
    pub name: String,
    pub kind: ItemKind,
    pub lat: f64,
    pub lon: f64,
    pub start: Option<NaiveTime>,
    pub end: Option<NaiveTime>,
    pub opening_hours: Option<String>,
    pub price_rub: Option<u32>,
    pub source: SourceKind,
    pub note: Option<String>,
}

impl PlanItem {
    pub fn is_outdoor(&self) -> bool {
        self.kind.is_outdoor()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Leg {
    pub from_place_id: String,
    pub to_place_id: String,
    pub mode: TransportMode,
    pub distance_km: f64,
    pub duration_min: u32,
    pub geometry: Option<String>,
    pub source: SourceKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanEvent {
    pub id: String,
    pub title: String,
    pub date: Option<NaiveDate>,
    pub end_date: Option<NaiveDate>,
    /// «ЧЧ:ММ» начала события, если афиша его отдала; None — событие дня.
    #[serde(default)]
    pub start_time: Option<String>,
    pub place: String,
    pub url: Option<String>,
    #[serde(default)]
    pub score: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DayPlan {
    pub date: NaiveDate,
    pub title: String,
    pub items: Vec<PlanItem>,
    pub legs: Vec<Leg>,
    /// Events from `find_events` overlapping this date. They are shown as a
    /// separate stack and never become map pins or plan items.
    #[serde(default)]
    pub events: Vec<PlanEvent>,
    /// Fuel stations along this day's route, for the map layer. Coordinates come
    /// from OSM; grades and prices are demo estimates (`data/mock/fuel.json`),
    /// which the UI labels as such.
    #[serde(default)]
    pub fuel: Vec<FuelStation>,
    pub lodging_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanWeather {
    pub date: NaiveDate,
    pub t_min: f64,
    pub t_max: f64,
    pub precip_prob: f64,
    pub wind_max: f64,
    pub desc: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Plan {
    pub id: Uuid,
    pub title: String,
    pub summary: String,
    pub intent: Intent,
    pub days: Vec<DayPlan>,
    pub lodging: Vec<Lodging>,
    #[serde(default)]
    pub lodging_options: Vec<Lodging>,
    pub transport: TransportMode,
    pub total_km: f64,
    pub cost_estimate_rub: Option<u32>,
    pub warnings: Vec<Warning>,
    pub weather: Vec<PlanWeather>,
    pub sources: Vec<SourceRef>,
    pub generation: Generation,
    pub created_at: DateTime<Utc>,
}

impl Plan {
    pub fn nights(&self) -> usize {
        self.days.len().saturating_sub(1)
    }

    pub fn all_items(&self) -> impl Iterator<Item = &PlanItem> {
        self.days.iter().flat_map(|day| day.items.iter())
    }

    pub fn all_legs(&self) -> impl Iterator<Item = &Leg> {
        self.days.iter().flat_map(|day| day.legs.iter())
    }

    pub fn source_kinds(&self) -> Vec<SourceKind> {
        self.sources.iter().map(|source| source.kind).collect()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PlanDraft {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub days: Vec<DayDraft>,
    #[serde(default)]
    pub lodging_ids: Vec<String>,
    #[serde(default)]
    pub transport: Option<TransportMode>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DayDraft {
    pub date: NaiveDate,
    #[serde(default)]
    pub items: Vec<ItemDraft>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ItemDraft {
    #[serde(default)]
    pub place_id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub kind: ItemKind,
    #[serde(default, deserialize_with = "deserialize_time_opt")]
    pub start: Option<NaiveTime>,
    #[serde(default, deserialize_with = "deserialize_time_opt")]
    pub end: Option<NaiveTime>,
    #[serde(default)]
    pub note: Option<String>,
}

fn deserialize_time_opt<'de, D>(deserializer: D) -> Result<Option<NaiveTime>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<String>::deserialize(deserializer)?;
    match value {
        None => Ok(None),
        Some(text) => parse_time(&text)
            .map(Some)
            .ok_or_else(|| de::Error::custom(format!("invalid time: {text}"))),
    }
}

pub fn parse_time(text: &str) -> Option<NaiveTime> {
    for format in ["%H:%M:%S", "%H:%M"] {
        if let Ok(time) = NaiveTime::parse_from_str(text.trim(), format) {
            return Some(time);
        }
    }
    None
}

/// A sensible slot for a kind: bars and clubs are evening stops, museums and
/// nature are daytime stops. Keeping this in one place stops fallback and mock
/// plans from scheduling a bar at 10:00 and failing the opening-hours check.
pub fn slot_times(kind: ItemKind, slot: usize) -> (Option<NaiveTime>, Option<NaiveTime>) {
    let slot = slot.min(19) as u32;
    let (start_hour, end_hour) = match kind {
        ItemKind::Bar | ItemKind::Club => {
            let offset = slot % 4;
            (20 + offset, (21 + offset) % 24)
        }
        ItemKind::Food => {
            let offset = slot % 16;
            (8 + offset, 9 + offset)
        }
        ItemKind::Cinema | ItemKind::Theater => {
            let offset = slot % 14;
            (10 + offset, (11 + offset) % 24)
        }
        ItemKind::Market | ItemKind::Shop => {
            let offset = slot % 12;
            (10 + offset, 11 + offset)
        }
        _ => {
            let offset = slot % 16;
            (8 + offset, 9 + offset)
        }
    };
    (
        NaiveTime::from_hms_opt(start_hour, 0, 0),
        NaiveTime::from_hms_opt(end_hour, 0, 0),
    )
}

fn event_area_center(area: &str) -> Option<(f64, f64)> {
    let area = area.to_lowercase();
    if area.contains("находк") {
        Some((42.8169, 132.8892))
    } else if area.contains("уссурийск") {
        Some((43.7992, 131.9495))
    } else if area.contains("больш") && area.contains("камен") {
        Some((43.1175, 132.354))
    } else if area.contains("артем") || area.contains("артём") {
        Some((43.355, 132.19))
    } else if area.contains("спасск") {
        Some((44.6, 132.82))
    } else if area.contains("партизанск") {
        Some((43.12, 133.12))
    } else if area.contains("арсеньев") {
        Some((44.1662, 133.2702))
    } else if area.contains("дальнегорск") {
        Some((44.566, 135.618))
    } else if area.contains("лесозаводск") {
        Some((45.466, 133.4))
    } else if area.contains("фокино") {
        Some((42.97, 132.4))
    } else if area.contains("кавалерово") {
        Some((44.27, 135.05))
    } else if area.contains("черниговка") {
        Some((44.35, 132.57))
    } else if area.is_empty()
        || area.contains("владивост")
        || area.contains("русск")
        || area.contains("примор")
    {
        Some((43.1155, 131.8855))
    } else {
        None
    }
}

fn event_matches_day(event: &EventFact, items: &[PlanItem]) -> bool {
    let Some(center) = event_area_center(&event.area) else {
        return false;
    };
    items
        .iter()
        .any(|item| haversine_km(center, (item.lat, item.lon)) <= 30.0)
}

pub fn plan_events_for_day(
    date: NaiveDate,
    items: &[PlanItem],
    facts: &Facts,
    limit: usize,
) -> Vec<PlanEvent> {
    let mut events: Vec<PlanEvent> = facts
        .events
        .iter()
        .filter(|event| {
            event_matches_day(event, items)
                && event.date.is_some_and(|start| {
                    let end = event.end_date.unwrap_or(start);
                    date >= start && date <= end
                })
        })
        .map(|event| PlanEvent {
            id: event.id.clone(),
            title: event.title.clone(),
            date: event.date,
            end_date: event.end_date,
            start_time: event.start_time.clone(),
            place: event.place.clone(),
            url: event.url.clone(),
            score: event.score,
        })
        .collect();
    // Хронология дня: события без времени идут «весь день» — первыми, дальше
    // по времени начала; при равном времени решает score. Раньше сортировка
    // была только по score, и утренние события прятались за вечерними
    // концертами, когда лимит обрезал список.
    events.sort_by(|left, right| {
        match (left.start_time.as_deref(), right.start_time.as_deref()) {
            (None, None) => std::cmp::Ordering::Equal,
            (None, Some(_)) => std::cmp::Ordering::Less,
            (Some(_), None) => std::cmp::Ordering::Greater,
            (Some(left_time), Some(right_time)) => left_time.cmp(right_time),
        }
        .then_with(|| {
            right
                .score
                .partial_cmp(&left.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .then_with(|| left.title.cmp(&right.title))
    });
    let mut seen = HashSet::new();
    events.retain(|event| seen.insert(event.id.clone()));
    events.truncate(limit.clamp(1, 20));
    events
}

/// A night stay farther than this from every point of the trip is not a night
/// stay for this trip. Distances follow the hub algorithm: village first,
/// nearest town second, honest "nothing found" third.
pub(crate) const LODGING_MAX_KM: f64 = 45.0;

/// How many night-stay options the API returns; the UI lists them all.
const LODGING_OPTIONS_MAX: usize = 6;

fn lodging_belongs(
    option: &Lodging,
    anchor_tree: &KdTree,
    anchors: &[(f64, f64)],
    allows_vladivostok: bool,
    max_km: f64,
) -> bool {
    if !allows_vladivostok && is_vladivostok(option) {
        return false;
    }
    if option.lat == 0.0 && option.lon == 0.0 {
        // Without coordinates only the settlement text can be judged.
        return true;
    }
    if anchors.is_empty() {
        return true;
    }
    anchor_tree.any_within(option.lat, option.lon, max_km)
}

/// A Vladivostok hotel offered for a Khasan or Nakhodka trip is the reported
/// bug; the settlement text and the pin both count.
pub(crate) fn is_vladivostok(option: &Lodging) -> bool {
    let settlement = option.settlement.to_lowercase();
    if ["владивосток", "русск", "артём", "артем"]
        .iter()
        .any(|needle| settlement.contains(needle))
    {
        return true;
    }
    // A hotel pinned inside the agglomeration but labelled with another name.
    option.lat != 0.0
        && option.lon != 0.0
        && haversine_km((option.lat, option.lon), (43.1155, 131.8855)) <= 25.0
}

/// Night-stay bands used to rank options: inside the near band distance hardly
/// matters, so price decides; farther out the extra drive has to be worth it.
const LODGING_NEAR_BAND_KM: f64 = 12.0;

/// Value ranking: near band by price, the far band by distance and price, so
/// the recommended option is a sensible one instead of the priciest nearby.
fn sort_lodging_by_value(options: &mut [Lodging], anchors: &[(f64, f64)]) {
    let tree = KdTree::new(anchors);
    options.sort_by(|left, right| {
        let left_distance = lodging_distance(&tree, left);
        let right_distance = lodging_distance(&tree, right);
        let band = |distance: f64| u8::from(distance > LODGING_NEAR_BAND_KM);
        band(left_distance)
            .cmp(&band(right_distance))
            .then_with(|| {
                if left_distance <= LODGING_NEAR_BAND_KM {
                    // "Рекомендация" is the cheapest of the walking-distance
                    // options; a 9000 ₽ hotel must not win over a 2500 ₽ one.
                    left.price_per_night
                        .cmp(&right.price_per_night)
                        .then_with(|| left_distance.total_cmp(&right_distance))
                } else {
                    left_distance
                        .total_cmp(&right_distance)
                        .then_with(|| left.price_per_night.cmp(&right.price_per_night))
                }
            })
    });
}

fn lodging_distance(tree: &KdTree, option: &Lodging) -> f64 {
    if tree.is_empty() || (option.lat == 0.0 && option.lon == 0.0) {
        return f64::MAX;
    }
    tree.nearest(option.lat, option.lon, 1)
        .first()
        .map(|(_, distance)| *distance)
        .unwrap_or(f64::MAX)
}

/// "…ищите гостевые дома или базы отдыха" gets a concrete place to look.
fn lodging_hint(anchors: &[(f64, f64)]) -> String {
    let Some((lat, lon)) = anchors.last().copied() else {
        return String::new();
    };
    let hub = crate::domain::hubs::nearest(lat, lon)
        .and_then(crate::domain::hubs::primary_city)
        .map(str::to_string);
    let settlement = crate::domain::settlements::nearest(lat, lon).map(|item| item.name.clone());
    match (hub, settlement) {
        (Some(hub), Some(settlement)) if hub != settlement => {
            format!(" в ближайшем городе: {hub} или {settlement}")
        }
        (Some(hub), _) => format!(" в ближайшем городе: {hub}"),
        (None, Some(settlement)) => format!(" в ближайшем населённом пункте: {settlement}"),
        (None, None) => String::new(),
    }
}

/// How far a day's stops may spread from the trip's centre, by transport and
/// trip length. A two-day car trip cannot span the whole region: without this
/// the deterministic planner and the offline model pulled stops 500–1700 km
/// apart and the daily-mileage rule rejected the plan.
pub fn trip_radius_km(transport: TransportMode, days: u8) -> f64 {
    let days = f64::from(days.clamp(1, 7));
    match transport {
        TransportMode::Foot => 20.0,
        TransportMode::Bike => 50.0,
        TransportMode::Car => (60.0 * days).clamp(60.0, 300.0),
        TransportMode::Transit => (70.0 * days).clamp(70.0, 400.0),
    }
}

/// Fuel stations of one day: those near the day's route, nearest first, capped
/// so the map does not turn into a field of fuel icons. Prices and grades come
/// from the demo fixture and are labelled as demo by the tools and the UI.
const FUEL_MARKERS_PER_DAY: usize = 6;

pub fn fuel_for_day(
    items: &[PlanItem],
    stations: &[FuelStation],
    limit: usize,
) -> Vec<FuelStation> {
    let points: Vec<(f64, f64)> = items
        .iter()
        .map(|item| (item.lat, item.lon))
        .filter(|(lat, lon)| *lat != 0.0 || *lon != 0.0)
        .collect();
    if points.is_empty() || stations.is_empty() {
        return Vec::new();
    }
    let tree = KdTree::new(&points);
    let mut ranked: Vec<(f64, FuelStation)> = stations
        .iter()
        .filter_map(|station| {
            let (_, distance) = tree
                .nearest(station.lat, station.lon, 1)
                .into_iter()
                .next()?;
            Some((distance, station.clone()))
        })
        .collect();
    ranked.sort_by(|left, right| {
        left.0
            .partial_cmp(&right.0)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    // Stations right by the route first. When the model found fuel along a long
    // drive, those stations can all sit far from the day's stops (Андреевка's
    // stops are 100 km from the Vladivostok stations): the map still needs a few
    // pins, and "по пути" is the honest answer.
    let near: Vec<FuelStation> = ranked
        .iter()
        .filter(|(distance, _)| *distance <= FUEL_NEAR_ROUTE_KM)
        .map(|(_, station)| station.clone())
        .take(limit)
        .collect();
    if !near.is_empty() {
        return near;
    }
    ranked
        .into_iter()
        .filter(|(distance, _)| *distance <= FUEL_ALONG_THE_WAY_KM)
        .take(limit.min(3))
        .map(|(_, station)| station)
        .collect()
}

/// How far a fuel station may sit from the day's stops to be shown on the map.
const FUEL_NEAR_ROUTE_KM: f64 = 25.0;

/// Second pass: stations further along the same drive, when nothing is nearby.
const FUEL_ALONG_THE_WAY_KM: f64 = 150.0;

pub fn city_center(name: &str) -> Option<(f64, f64)> {
    crate::domain::settlements::center(name)
}

/// Nearest settlement for a plan point. Used after the plan is built to load
/// events for every city the plan actually visits.
pub fn city_for_point(lat: f64, lon: f64) -> Option<&'static str> {
    crate::domain::settlements::nearest(lat, lon).map(|settlement| settlement.name.as_str())
}

fn item_kind(fact: Option<&PlaceFact>, fallback: ItemKind) -> ItemKind {
    let Some(fact) = fact else {
        return fallback;
    };
    match fact.kind.as_str() {
        "museum" => ItemKind::Museum,
        "nature" => ItemKind::Nature,
        "viewpoint" => ItemKind::Viewpoint,
        "park" => ItemKind::Park,
        "historic" => ItemKind::Historic,
        "beach" => ItemKind::Beach,
        "lake" => ItemKind::Lake,
        "river" => ItemKind::River,
        "cinema" => ItemKind::Cinema,
        "theater" => ItemKind::Theater,
        "market" => ItemKind::Market,
        "shop" => ItemKind::Shop,
        "food" => ItemKind::Food,
        "bar" => ItemKind::Bar,
        "club" => ItemKind::Club,
        "rest" => ItemKind::Rest,
        _ => fallback,
    }
}

fn normalized_name(name: &str) -> String {
    name.to_lowercase()
        .replace('ё', "е")
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn enrich(draft: PlanDraft, intent: &Intent, facts: &Facts) -> Plan {
    let mut warnings = Vec::new();
    let mut days = Vec::new();

    // The requested period is authoritative. Group model-provided days by date so
    // duplicated dates are merged, out-of-range days are dropped and missing dates
    // are inserted as free days instead of shifting the whole plan.
    let mut items_by_date: BTreeMap<NaiveDate, Vec<ItemDraft>> = BTreeMap::new();
    let mut out_of_range_days = 0usize;
    for day_draft in &draft.days {
        if day_draft.date < intent.start_date || day_draft.date > intent.end_date {
            out_of_range_days += 1;
            continue;
        }
        items_by_date
            .entry(day_draft.date)
            .or_default()
            .extend(day_draft.items.iter().cloned());
    }

    let mut seen_ids: HashSet<String> = HashSet::new();
    let mut seen_places: Vec<(String, f64, f64)> = Vec::new();
    let mut seen_coords: Vec<(f64, f64)> = Vec::new();
    let mut duplicate_items = 0usize;
    let mut waterbody_items = 0usize;
    let mut outside_items = 0usize;
    let mut date = intent.start_date;

    while date <= intent.end_date {
        let mut items = Vec::new();
        for item_draft in items_by_date.remove(&date).unwrap_or_default() {
            let fact = facts.place(&item_draft.place_id);

            // A lake, river or bay is an area, not a stop. Providers already
            // filter these; this final guard also covers facts produced by
            // geocoding and old cached tool results.
            if fact.is_some_and(|place| waterbody_title(&place.name)) {
                waterbody_items += 1;
                continue;
            }

            // A coordinate in a bay or in the sea cannot be a stop, even if
            // the name looks like a place. This covers geocoding mistakes and
            // stale cached tool results that bypassed the providers.
            if fact.is_some_and(|place| !on_land(place.lat, place.lon)) {
                outside_items += 1;
                continue;
            }

            // When the query names a single city, drop everything else so
            // the plan does not pull Nakhodka stops into a Vladivostok tour.
            // The radius follows the place itself: a village tour must not
            // accept a stop that belongs to the next town.
            if let Some(city) = intent.city() {
                if let Some(center) = city_center(city) {
                    let scope = crate::domain::settlements::scope_km(city).max(15.0);
                    if fact
                        .is_some_and(|place| haversine_km((place.lat, place.lon), center) > scope)
                    {
                        outside_items += 1;
                        continue;
                    }
                }
            } else if let Some(hub) = intent
                .hub()
                .and_then(|id| crate::domain::hubs::all().iter().find(|hub| hub.id == id))
            {
                // A bay, cape or district query names no settlement; the hub is
                // then the reference, so "бухта Триозёрье" cannot collect
                // Vladivostok stops 170 km away.
                if fact.is_some_and(|place| {
                    haversine_km((place.lat, place.lon), (hub.lat, hub.lon)) > 40.0
                }) {
                    outside_items += 1;
                    continue;
                }
            }

            if !item_draft.place_id.is_empty() && !seen_ids.insert(item_draft.place_id.clone()) {
                duplicate_items += 1;
                continue;
            }

            // Internet providers may return the same object under different ids.
            // Treat same normalized name within 500 m as the same place.
            if let Some(place) = fact {
                let key = normalized_name(&place.name);
                let repeated = seen_places.iter().any(|(name, lat, lon)| {
                    *name == key && haversine_km((place.lat, place.lon), (*lat, *lon)) < 0.5
                });
                if repeated {
                    duplicate_items += 1;
                    continue;
                }
                // Same venue under different ids/names often differs by a few
                // metres. Treat anything within 50 m as the same stop.
                if seen_coords
                    .iter()
                    .any(|(lat, lon)| haversine_km((place.lat, place.lon), (*lat, *lon)) < 0.05)
                {
                    duplicate_items += 1;
                    continue;
                }
                seen_places.push((key, place.lat, place.lon));
                seen_coords.push((place.lat, place.lon));
            }

            if fact.is_none() {
                warnings.push(Warning {
                    code: "unknown_place".to_string(),
                    message: format!(
                        "Место «{}» не найдено в данных инструментов",
                        if item_draft.name.is_empty() {
                            &item_draft.place_id
                        } else {
                            &item_draft.name
                        }
                    ),
                });
            }
            items.push(PlanItem {
                place_id: item_draft.place_id.clone(),
                name: fact
                    .map(|place| place.name.clone())
                    .unwrap_or_else(|| item_draft.name.clone()),
                kind: item_kind(fact, item_draft.kind),
                lat: fact.map(|place| place.lat).unwrap_or(0.0),
                lon: fact.map(|place| place.lon).unwrap_or(0.0),
                start: item_draft.start,
                end: item_draft.end,
                opening_hours: fact.and_then(|place| place.opening_hours.clone()),
                price_rub: None,
                source: fact
                    .map(|place| place.source)
                    .unwrap_or(SourceKind::Estimated),
                note: item_draft.note.clone(),
            });
        }

        let mut legs = Vec::new();
        for window in items.windows(2) {
            legs.push(build_leg(&window[0], &window[1], intent, facts));
        }

        let index = days.len();
        let lodging_id = draft.lodging_ids.get(index).cloned();
        let event_limit = intent
            .per_day()
            .map(|count| count as usize)
            .unwrap_or(20)
            .min(20);
        let events = plan_events_for_day(date, &items, facts, event_limit);
        // Stations the model asked for when they exist; otherwise the demo
        // catalogue, so a car trip always shows fuel on the map. Prices there
        // are demo estimates and the payload says so.
        // Станции, которые нашла модель, когда они есть; иначе демо-каталог —
        // слой АЗС нужен на карте в любой поездке, а не только на машине.
        let fuel_stations: Vec<FuelStation> = if facts.fuel.is_empty() {
            crate::domain::facts::mock_fuel()
        } else {
            facts.fuel.clone()
        };
        let fuel = fuel_for_day(&items, &fuel_stations, FUEL_MARKERS_PER_DAY);
        days.push(DayPlan {
            date,
            title: match items.first() {
                Some(item) => format!("День {} — {}", index + 1, item.name),
                None => "Свободный день".to_string(),
            },
            items,
            legs,
            events,
            fuel,
            lodging_id,
        });
        date = add_days(date, 1);
    }

    if out_of_range_days > 0 {
        warnings.push(Warning {
            code: "days_normalized".to_string(),
            message: "Модель предложила дни вне запрошенного периода — они убраны".to_string(),
        });
    }
    if duplicate_items > 0 {
        warnings.push(Warning {
            code: "duplicates_removed".to_string(),
            message: format!("Убрано повторяющихся точек: {duplicate_items}"),
        });
    }
    if waterbody_items > 0 {
        warnings.push(Warning {
            code: "waterbody_skipped".to_string(),
            message: "Озёра и реки используются как районы поиска — их точки в воде не показаны"
                .to_string(),
        });
    }
    if outside_items > 0 {
        warnings.push(Warning {
            code: "outside_region_skipped".to_string(),
            message: format!("Убрано точек вне Приморского края или в воде: {outside_items}"),
        });
    }

    // Lodging belongs to the trip, not to a city 200 km away: every night stay
    // and every offered option is checked against the plan's own points. This is
    // what stops "Отдых в бухте Триозёрье" from offering Vladivostok hotels.
    let mut anchor_points: Vec<(f64, f64)> = days
        .iter()
        .flat_map(|day| day.items.iter())
        .map(|item| (item.lat, item.lon))
        .filter(|(lat, lon)| *lat != 0.0 || *lon != 0.0)
        .collect();
    if anchor_points.is_empty() {
        // A plan without stops still has a named place: without this anchor the
        // deterministic fallback offered a Лесозаводск hotel for a Терней trip.
        let place = intent.city().and_then(city_center).or_else(|| {
            intent.hub().and_then(|id| {
                crate::domain::hubs::all()
                    .iter()
                    .find(|hub| hub.id == id)
                    .map(|hub| (hub.lat, hub.lon))
            })
        });
        if let Some(center) = place {
            anchor_points.push(center);
        } else {
            // Nothing named at all: the app's default trip is Vladivostok, so
            // the options must not mix hotels from every Primorye city.
            anchor_points.push((43.1155, 131.8855));
        }
    }
    let anchor_tree = KdTree::new(&anchor_points);
    // Vladivostok lodging is allowed only when the trip itself is there: the
    // query names that hub, no place at all was named (Vladivostok is the
    // default), or the plan's own points are in the agglomeration.
    let allows_vladivostok = intent.hub().is_none()
        || intent.hub() == Some("vladivostok")
        || anchor_tree.any_within(43.1155, 131.8855, 30.0);
    let mut lodging_options: Vec<Lodging> = facts
        .lodging
        .values()
        .filter(|option| {
            lodging_belongs(
                option,
                &anchor_tree,
                &anchor_points,
                allows_vladivostok,
                LODGING_MAX_KM,
            )
        })
        .cloned()
        .collect();
    // Near band first, cheapest first inside it: the UI marks the first option
    // as "рекомендация", and a 9000 ₽ hotel recommended over a 2500 ₽ one 3 km
    // further away reads as a mistake. Distance still wins over a longer drive.
    sort_lodging_by_value(&mut lodging_options, &anchor_points);
    // The list is what the user scrolls: keep it short and local.
    lodging_options.truncate(LODGING_OPTIONS_MAX);

    let mut lodging = Vec::new();
    for lodging_id in &draft.lodging_ids {
        if let Some(option) = facts.lodging.get(lodging_id) {
            if lodging_options.iter().any(|kept| kept.id == option.id) {
                lodging.push(option.clone());
            } else {
                warnings.push(Warning {
                    code: "lodging_too_far".to_string(),
                    message: format!(
                        "Отель «{}» ({}) слишком далеко от точек маршрута — заменён ближайшим вариантом",
                        option.name, option.settlement
                    ),
                });
            }
        }
    }
    if lodging.is_empty() && days.len() > 1 {
        if let Some(option) = lodging_options.first() {
            lodging.push(option.clone());
            warnings.push(Warning {
                code: "lodging_auto".to_string(),
                message: "Ночёвка выбрана автоматически из ближайших вариантов".to_string(),
            });
        }
    }
    if lodging_options.is_empty()
        && !anchor_points.is_empty()
        && days.len() > 1
        && !facts.lodging.is_empty()
    {
        // Honest empty answer instead of a hotel on the other side of the region.
        // Only when the tool did return options: an empty facts set means the
        // model never asked for lodging, which stays a hard validation error.
        let hint = lodging_hint(&anchor_points);
        warnings.push(Warning {
            code: "lodging_none_near".to_string(),
            message: format!(
                "Рядом с точками маршрута нет проверенных отелей в нашей базе — ищите гостевые дома или базы отдыха{hint}."
            ),
        });
    }

    let total_km = days
        .iter()
        .flat_map(|day| day.legs.iter())
        .map(|leg| leg.distance_km)
        .sum::<f64>();
    let nights = days.len().saturating_sub(1);
    let cost = if lodging.is_empty() || nights == 0 {
        None
    } else {
        Some(
            (0..nights)
                .map(|index| {
                    lodging
                        .get(index)
                        .or_else(|| lodging.last())
                        .map(|option| option.price_per_night)
                        .unwrap_or(0)
                })
                .sum(),
        )
    };

    let is_demo = |kind: SourceKind| matches!(kind, SourceKind::Mock | SourceKind::Stale);
    if facts.places.values().any(|place| is_demo(place.source))
        || facts
            .lodging
            .values()
            .any(|lodging| is_demo(lodging.source))
        || facts.sources.iter().any(|source| is_demo(source.kind))
    {
        warnings.push(Warning {
            code: "demo_data".to_string(),
            message: "Часть данных недоступна или демонстрационная — проверьте перед выездом"
                .to_string(),
        });
    }

    let weather = days
        .iter()
        .filter_map(|day| {
            facts.weather_for(day.date).map(|weather| PlanWeather {
                date: day.date,
                t_min: weather.temp_min,
                t_max: weather.temp_max,
                precip_prob: weather.precip_prob,
                wind_max: weather.wind_max,
                desc: format!(
                    "осадки {:.0}%, ветер {:.0} м/с",
                    weather.precip_prob, weather.wind_max
                ),
            })
        })
        .collect();

    Plan {
        id: Uuid::new_v4(),
        title: if draft.title.trim().is_empty() {
            "Поездка по Приморскому краю".to_string()
        } else {
            draft.title
        },
        summary: if draft.summary.trim().is_empty() {
            format!(
                "{} дн., {} — план собран по данным инструментов",
                days.len(),
                intent.transport.label_ru()
            )
        } else {
            draft.summary
        },
        intent: intent.clone(),
        days,
        lodging,
        lodging_options,
        transport: intent.transport,
        total_km: (total_km * 10.0).round() / 10.0,
        cost_estimate_rub: cost,
        warnings,
        weather,
        sources: facts.sources.clone(),
        generation: Generation::Llm,
        created_at: Utc::now(),
    }
}

pub fn build_leg(from: &PlanItem, to: &PlanItem, intent: &Intent, facts: &Facts) -> Leg {
    if let Some(route) = facts
        .route_between(&from.place_id, &to.place_id)
        .or_else(|| facts.route_for_points((from.lat, from.lon), (to.lat, to.lon)))
    {
        return Leg {
            from_place_id: from.place_id.clone(),
            to_place_id: to.place_id.clone(),
            mode: intent.transport,
            distance_km: route.distance_km,
            duration_min: route.duration_min,
            geometry: route.geometry.clone(),
            source: route.source,
        };
    }

    let distance = haversine_km((from.lat, from.lon), (to.lat, to.lon));
    Leg {
        from_place_id: from.place_id.clone(),
        to_place_id: to.place_id.clone(),
        mode: intent.transport,
        distance_km: (distance * 10.0).round() / 10.0,
        duration_min: travel_minutes(distance, intent.transport),
        geometry: mock_leg_geometry(from, to),
        source: SourceKind::Estimated,
    }
}

/// A straight line between two stops, with a midpoint. The map draws whatever
/// geometry a leg carries, and a plan without any line reads as broken; when
/// neither OSRM nor the route tool answered, this mock line is what the user
/// sees, and `source: estimated` says so.
fn mock_leg_geometry(from: &PlanItem, to: &PlanItem) -> Option<String> {
    if (from.lat == 0.0 && from.lon == 0.0) || (to.lat == 0.0 && to.lon == 0.0) {
        return None;
    }
    let middle = ((from.lat + to.lat) / 2.0, (from.lon + to.lon) / 2.0);
    Some(
        json!({
            "type": "LineString",
            "coordinates": [
                [from.lon, from.lat],
                [middle.1, middle.0],
                [to.lon, to.lat]
            ]
        })
        .to_string(),
    )
}

pub fn day_count(plan: &Plan) -> usize {
    plan.days.len()
}

pub fn expected_days(intent: &Intent) -> i64 {
    days_inclusive(intent.start_date, intent.end_date).max(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::intent::Pace;

    fn intent() -> Intent {
        Intent {
            region: "primorye".to_string(),
            start_date: NaiveDate::from_ymd_opt(2026, 10, 24).unwrap(),
            end_date: NaiveDate::from_ymd_opt(2026, 10, 25).unwrap(),
            max_days: 2,
            transport: TransportMode::Car,
            budget_rub: None,
            party_size: 1,
            with_children: false,
            interests: vec![],
            must_see: vec![],
            avoid: vec![],
            pace: Pace::Normal,
            assumptions: vec![],
        }
    }

    #[test]
    fn parses_short_time() {
        assert_eq!(parse_time("10:00"), NaiveTime::from_hms_opt(10, 0, 0));
        assert_eq!(parse_time("10:00:00"), NaiveTime::from_hms_opt(10, 0, 0));
    }

    #[test]
    fn enrich_normalizes_period_and_removes_duplicates() {
        let mut intent = intent();
        intent.end_date = NaiveDate::from_ymd_opt(2026, 10, 26).unwrap();
        intent.max_days = 3;
        let mut facts = Facts::default();
        facts.ensure_minimal(intent.start_date, intent.end_date);
        let ids: Vec<String> = facts.places.keys().cloned().collect();
        let item = |id: &str| ItemDraft {
            place_id: id.to_string(),
            name: String::new(),
            kind: ItemKind::Sight,
            start: None,
            end: None,
            note: None,
        };
        let draft = PlanDraft {
            title: "Тест".to_string(),
            summary: "Тест".to_string(),
            days: vec![
                DayDraft {
                    date: intent.start_date,
                    items: vec![item(&ids[0]), item(&ids[0])],
                },
                DayDraft {
                    date: intent.start_date + chrono::Duration::days(1),
                    items: vec![item(&ids[1])],
                },
                DayDraft {
                    date: intent.start_date + chrono::Duration::days(2),
                    items: vec![item(&ids[2])],
                },
                DayDraft {
                    date: intent.start_date + chrono::Duration::days(3),
                    items: vec![item(&ids[3])],
                },
            ],
            lodging_ids: vec![],
            transport: Some(TransportMode::Car),
        };

        let plan = enrich(draft, &intent, &facts);
        let dates: Vec<NaiveDate> = plan.days.iter().map(|day| day.date).collect();
        assert_eq!(
            dates,
            vec![
                NaiveDate::from_ymd_opt(2026, 10, 24).unwrap(),
                NaiveDate::from_ymd_opt(2026, 10, 25).unwrap(),
                NaiveDate::from_ymd_opt(2026, 10, 26).unwrap(),
            ]
        );
        let planned: Vec<&str> = plan
            .all_items()
            .map(|item| item.place_id.as_str())
            .collect();
        assert_eq!(
            planned,
            vec![ids[0].as_str(), ids[1].as_str(), ids[2].as_str()]
        );
        assert!(plan
            .warnings
            .iter()
            .any(|warning| warning.code == "duplicates_removed"));
    }

    #[test]
    fn lodging_cost_counts_every_night() {
        let mut intent = intent();
        intent.end_date = NaiveDate::from_ymd_opt(2026, 10, 26).unwrap();
        intent.max_days = 3;
        let mut facts = Facts::default();
        facts.ensure_minimal(intent.start_date, intent.end_date);
        let lodging_id = facts.lodging.keys().next().unwrap().clone();
        let price = facts.lodging[&lodging_id].price_per_night;
        let draft = PlanDraft {
            title: "Тест".to_string(),
            summary: "Тест".to_string(),
            days: vec![],
            lodging_ids: vec![lodging_id],
            transport: Some(TransportMode::Car),
        };

        let _ = price;
        let plan = enrich(draft, &intent, &facts);
        // The value ranking may pick a cheaper hotel than the draft asked for;
        // the invariant is that every night of the chosen stay is counted.
        let chosen = plan
            .lodging
            .first()
            .expect("lodging chosen")
            .price_per_night;
        assert_eq!(plan.cost_estimate_rub, Some(chosen * 2));
    }

    #[test]
    fn enrich_builds_legs() {
        let mut facts = Facts::default();
        facts.ensure_minimal(intent().start_date, intent().end_date);
        let place_ids: Vec<String> = facts.places.keys().cloned().collect();
        let draft = PlanDraft {
            title: "Тест".to_string(),
            summary: "Тест".to_string(),
            days: vec![DayDraft {
                date: intent().start_date,
                items: place_ids
                    .iter()
                    .take(2)
                    .map(|id| ItemDraft {
                        place_id: id.clone(),
                        name: String::new(),
                        kind: ItemKind::Sight,
                        start: None,
                        end: None,
                        note: None,
                    })
                    .collect(),
            }],
            lodging_ids: vec![],
            transport: Some(TransportMode::Car),
        };
        let plan = enrich(draft, &intent(), &facts);
        assert_eq!(plan.days.len(), 2);
        assert_eq!(plan.days[0].legs.len(), 1);
        assert!(plan.days[1].items.is_empty());
    }

    #[test]
    fn enrich_drops_waterbody_stops() {
        let mut facts = Facts::default();
        facts.ensure_minimal(intent().start_date, intent().end_date);
        facts.places.insert(
            "test:lake".to_string(),
            PlaceFact {
                id: "test:lake".to_string(),
                name: "озеро Ханка".to_string(),
                lat: 44.93,
                lon: 132.44,
                kind: "lake".to_string(),
                opening_hours: None,
                source: SourceKind::Live,
            },
        );
        let draft = PlanDraft {
            title: "Тест".to_string(),
            summary: "Тест".to_string(),
            days: vec![DayDraft {
                date: intent().start_date,
                items: vec![ItemDraft {
                    place_id: "test:lake".to_string(),
                    name: "озеро Ханка".to_string(),
                    kind: ItemKind::Lake,
                    start: None,
                    end: None,
                    note: None,
                }],
            }],
            lodging_ids: vec![],
            transport: Some(TransportMode::Car),
        };
        let plan = enrich(draft, &intent(), &facts);
        assert!(plan.days[0].items.is_empty());
        assert!(plan
            .warnings
            .iter()
            .any(|warning| warning.code == "waterbody_skipped"));
    }

    /// The reported bug: "Хочу погулять в Южно-Морском" was answered with a
    /// Vladivostok hotel 170 km away.
    fn lodging_facts() -> (Facts, PlaceFact) {
        let place = PlaceFact {
            id: "test:yuzhno-morskoy".to_string(),
            name: "Набережная Южно-Морского".to_string(),
            lat: 42.86,
            lon: 132.68,
            kind: "beach".to_string(),
            opening_hours: None,
            source: SourceKind::Live,
        };
        let mut facts = Facts::default();
        facts.places.insert(place.id.clone(), place.clone());
        for (id, name, settlement, lat, lon) in [
            (
                "test:lodging-vvo",
                "Отель Владивосток",
                "Владивосток",
                43.1155,
                131.8855,
            ),
            (
                "test:lodging-nhk",
                "Отель Находка",
                "Находка",
                42.8169,
                132.8855,
            ),
            (
                "test:lodging-livadiya",
                "База Ливадия",
                "Ливадия",
                42.8667,
                132.6833,
            ),
        ] {
            facts.lodging.insert(
                id.to_string(),
                Lodging {
                    id: id.to_string(),
                    name: name.to_string(),
                    settlement: settlement.to_string(),
                    lat,
                    lon,
                    price_per_night: 4000,
                    kind: "hotel".to_string(),
                    source: SourceKind::Live,
                },
            );
        }
        (facts, place)
    }

    fn draft_with(place_id: &str, date: NaiveDate) -> PlanDraft {
        PlanDraft {
            title: "Тест".to_string(),
            summary: "Тест".to_string(),
            days: vec![DayDraft {
                date,
                items: vec![ItemDraft {
                    place_id: place_id.to_string(),
                    name: String::new(),
                    kind: ItemKind::Sight,
                    start: None,
                    end: None,
                    note: None,
                }],
            }],
            lodging_ids: vec![],
            transport: Some(TransportMode::Car),
        }
    }

    #[test]
    fn lodging_far_from_the_trip_is_dropped() {
        let (facts, place) = lodging_facts();
        let mut intent = intent();
        intent.assumptions = vec!["хаб: nakhodka-coast".to_string()];
        let plan = enrich(draft_with(&place.id, intent.start_date), &intent, &facts);
        let offered: Vec<&str> = plan
            .lodging_options
            .iter()
            .map(|option| option.id.as_str())
            .collect();
        assert!(
            !offered.contains(&"test:lodging-vvo"),
            "Vladivostok hotel must not be offered for a Nakhodka-coast trip: {offered:?}"
        );
        assert!(offered.contains(&"test:lodging-livadiya"));
        // Closest first: Ливадия is ~2 km away, Находка ~20 km.
        assert_eq!(plan.lodging_options[0].id, "test:lodging-livadiya");
    }

    #[test]
    fn vladivostok_trip_keeps_vladivostok_lodging() {
        let (mut facts, _) = lodging_facts();
        let place = PlaceFact {
            id: "test:vvo".to_string(),
            name: "Набережная".to_string(),
            lat: 43.1155,
            lon: 131.8855,
            kind: "sight".to_string(),
            opening_hours: None,
            source: SourceKind::Live,
        };
        facts.places.insert(place.id.clone(), place.clone());
        let mut intent = intent();
        intent.assumptions = vec!["хаб: vladivostok".to_string()];
        let plan = enrich(draft_with(&place.id, intent.start_date), &intent, &facts);
        assert!(
            plan.lodging_options
                .iter()
                .any(|option| option.id == "test:lodging-vvo"),
            "a Vladivostok trip must keep its own hotels"
        );
    }

    #[test]
    fn lodging_none_near_warns_instead_of_offering_a_far_hotel() {
        let (facts, place) = lodging_facts();
        let mut intent = intent();
        intent.assumptions = vec!["хаб: terney-north".to_string()];
        let mut draft = draft_with(&place.id, intent.start_date);
        // Терней is ~600 km from Южно-Морской: nothing nearby may be offered.
        draft.days[0].items[0].place_id = "test:terney".to_string();
        let terney = PlaceFact {
            id: "test:terney".to_string(),
            name: "Терней".to_string(),
            lat: 45.0504,
            lon: 136.6137,
            kind: "sight".to_string(),
            opening_hours: None,
            source: SourceKind::Live,
        };
        let mut facts = facts;
        facts.places.insert(terney.id.clone(), terney);
        let plan = enrich(draft, &intent, &facts);
        assert!(plan.lodging_options.is_empty());
        assert!(plan
            .warnings
            .iter()
            .any(|warning| warning.code == "lodging_none_near"));
        assert!(plan.lodging.is_empty());
    }

    /// Утреннее событие не должно прятаться за вечерним концертом: список
    /// дня идёт по времени, «весь день» — первым, при равном времени — score.
    #[test]
    fn day_events_are_sorted_chronologically() {
        let date = NaiveDate::from_ymd_opt(2026, 10, 25).unwrap();
        let mut facts = Facts::default();
        let event = |title: &str, start_time: Option<&str>, score: f64| EventFact {
            id: format!("test:event:{title}"),
            title: title.to_string(),
            date: Some(date),
            end_date: Some(date),
            start_time: start_time.map(str::to_string),
            place: "Владивосток".to_string(),
            area: "Владивосток".to_string(),
            url: None,
            score,
            source: SourceKind::Live,
        };
        facts.events = vec![
            event("Вечерний концерт", Some("19:00"), 12.0),
            event("Ночное шоу", Some("22:00"), 20.0),
            event("Дневная выставка", Some("10:00"), 1.0),
            event("Дневная прогулка", Some("10:00"), 5.0),
            event("Городской маркет", None, 6.0),
        ];
        let items = vec![PlanItem {
            place_id: "test:center".to_string(),
            name: "Центр".to_string(),
            kind: ItemKind::Sight,
            lat: 43.1155,
            lon: 131.8855,
            start: None,
            end: None,
            opening_hours: None,
            price_rub: None,
            source: SourceKind::Live,
            note: None,
        }];
        let events = plan_events_for_day(date, &items, &facts, 20);
        let titles: Vec<&str> = events.iter().map(|event| event.title.as_str()).collect();
        assert_eq!(
            titles,
            vec![
                "Городской маркет",
                "Дневная прогулка",
                "Дневная выставка",
                "Вечерний концерт",
                "Ночное шоу",
            ]
        );
        assert_eq!(events[0].start_time, None);
        assert_eq!(events[1].start_time.as_deref(), Some("10:00"));
    }

    #[test]
    fn fuel_markers_are_present_for_every_trip() {
        // Слой АЗС нужен на карте в любой поездке: пользователь ищет заправки
        // независимо от способа передвижения.
        let (facts, place) = lodging_facts();
        for transport in [
            TransportMode::Car,
            TransportMode::Foot,
            TransportMode::Bike,
            TransportMode::Transit,
        ] {
            let mut intent = intent();
            intent.transport = transport;
            let plan = enrich(draft_with(&place.id, intent.start_date), &intent, &facts);
            assert!(
                !plan.days[0].fuel.is_empty(),
                "fuel stations must be on the map for {transport}"
            );
            assert!(plan.days[0]
                .fuel
                .iter()
                .all(|station| station.lat != 0.0 && station.lon != 0.0));
        }
    }
}
