use std::collections::{BTreeMap, HashSet};

use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use serde::{de, Deserialize, Deserializer, Serialize};
use uuid::Uuid;

use crate::domain::dates::{add_days, days_inclusive};
use crate::domain::facts::{Facts, Lodging, SourceKind, SourceRef};
use crate::domain::geo::{haversine_km, travel_minutes};
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
    Food,
    Transfer,
    Rest,
}

impl ItemKind {
    pub fn is_outdoor(self) -> bool {
        matches!(
            self,
            Self::Nature | Self::Viewpoint | Self::Park | Self::Historic
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
pub struct DayPlan {
    pub date: NaiveDate,
    pub title: String,
    pub items: Vec<PlanItem>,
    pub legs: Vec<Leg>,
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
    let mut date = intent.start_date;

    while date <= intent.end_date {
        let mut items = Vec::new();
        for item_draft in items_by_date.remove(&date).unwrap_or_default() {
            let fact = facts.place(&item_draft.place_id);

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
                kind: item_draft.kind,
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
        days.push(DayPlan {
            date,
            title: match items.first() {
                Some(item) => format!("День {} — {}", index + 1, item.name),
                None => "Свободный день".to_string(),
            },
            items,
            legs,
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

    let mut lodging = Vec::new();
    for lodging_id in &draft.lodging_ids {
        if let Some(option) = facts.lodging.get(lodging_id) {
            lodging.push(option.clone());
        }
    }
    if lodging.is_empty() && days.len() > 1 {
        if let Some(option) = facts.lodging.values().next() {
            lodging.push(option.clone());
            warnings.push(Warning {
                code: "lodging_auto".to_string(),
                message: "Ночёвка выбрана автоматически из доступных вариантов".to_string(),
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
    let cost = if lodging.is_empty() {
        None
    } else {
        Some(
            lodging
                .iter()
                .take(nights.max(1))
                .map(|option| option.price_per_night)
                .sum(),
        )
    };

    if facts
        .places
        .values()
        .any(|place| place.source != SourceKind::Live)
        || facts.weather.values().next().is_some()
            && facts
                .sources
                .iter()
                .any(|source| source.kind != SourceKind::Live)
    {
        warnings.push(Warning {
            code: "demo_data".to_string(),
            message: "Часть данных демонстрационная — проверьте перед выездом".to_string(),
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
}
