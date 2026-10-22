use std::collections::{BTreeMap, HashSet};

use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use serde::{de, Deserialize, Deserializer, Serialize};
use uuid::Uuid;

use crate::domain::dates::{add_days, days_inclusive};
use crate::domain::facts::{EventFact, Facts, Lodging, PlaceFact, SourceKind, SourceRef};
use crate::domain::geo::{haversine_km, on_land, travel_minutes, waterbody_title};
use crate::domain::intent::{Intent, TransportMode};

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

pub fn plan_events_for_day(date: NaiveDate, items: &[PlanItem], facts: &Facts) -> Vec<PlanEvent> {
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
            place: event.place.clone(),
            url: event.url.clone(),
            score: event.score,
        })
        .collect();
    events.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.title.cmp(&right.title))
    });
    let mut seen = HashSet::new();
    events.retain(|event| seen.insert(event.id.clone()));
    events.truncate(20);
    events
}

/// Nearest poster city for a plan point. Used after the plan is built to load
/// events for every city the plan actually visits.
pub fn city_for_point(lat: f64, lon: f64) -> Option<&'static str> {
    const CITIES: &[(&str, f64, f64)] = &[
        ("Владивосток", 43.1155, 131.8855),
        ("Находка", 42.8169, 132.8892),
        ("Уссурийск", 43.7992, 131.9495),
        ("Большой Камень", 43.1175, 132.354),
        ("Артём", 43.355, 132.19),
        ("Спасск", 44.6, 132.82),
        ("Партизанск", 43.12, 133.12),
        ("Арсеньев", 44.1662, 133.2702),
        ("Дальнегорск", 44.566, 135.618),
        ("Лесозаводск", 45.466, 133.4),
        ("Фокино", 42.97, 132.4),
        ("Кавалерово", 44.27, 135.05),
        ("Черниговка", 44.35, 132.57),
    ];
    CITIES
        .iter()
        .map(|(name, city_lat, city_lon)| (*name, haversine_km((lat, lon), (*city_lat, *city_lon))))
        .filter(|(_, distance)| *distance <= 30.0)
        .min_by(|left, right| {
            left.1
                .partial_cmp(&right.1)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|(name, _)| name)
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
                seen_places.push((key, place.lat, place.lon));
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
        let events = plan_events_for_day(date, &items, facts);
        days.push(DayPlan {
            date,
            title: match items.first() {
                Some(item) => format!("День {} — {}", index + 1, item.name),
                None => "Свободный день".to_string(),
            },
            items,
            legs,
            events,
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

    let mut lodging = Vec::new();
    for lodging_id in &draft.lodging_ids {
        if let Some(option) = facts.lodging.get(lodging_id) {
            lodging.push(option.clone());
        }
    }
    if lodging.is_empty() && days.len() > 1 {
        let anchor = days
            .iter()
            .flat_map(|day| day.items.iter())
            .next()
            .map(|item| (item.lat, item.lon));
        let nearest = facts.lodging.values().min_by(|left, right| {
            let left_distance = anchor
                .map(|(lat, lon)| haversine_km((left.lat, left.lon), (lat, lon)))
                .unwrap_or(0.0);
            let right_distance = anchor
                .map(|(lat, lon)| haversine_km((right.lat, right.lon), (lat, lon)))
                .unwrap_or(0.0);
            left_distance
                .partial_cmp(&right_distance)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        if let Some(option) = nearest {
            lodging.push(option.clone());
            warnings.push(Warning {
                code: "lodging_auto".to_string(),
                message: "Ночёвка выбрана автоматически из ближайших вариантов".to_string(),
            });
        }
    }

    let mut lodging_options: Vec<Lodging> = facts.lodging.values().cloned().collect();
    lodging_options.sort_by_key(|option| option.price_per_night);

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
        geometry: None,
        source: SourceKind::Estimated,
    }
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

        let plan = enrich(draft, &intent, &facts);
        assert_eq!(plan.cost_estimate_rub, Some(price * 2));
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
}
