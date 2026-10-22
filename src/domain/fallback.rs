use crate::domain::dates::add_days;
use crate::domain::facts::Facts;
use crate::domain::geo::{haversine_km, on_land};
use crate::domain::intent::{Intent, TransportMode};
use crate::domain::plan::{enrich, DayDraft, Generation, ItemDraft, ItemKind, Plan, PlanDraft};

/// Deterministic plan from already collected facts. Never calls the LLM.
pub fn assemble(facts: &mut Facts, intent: &Intent) -> Plan {
    facts.ensure_minimal(intent.start_date, intent.end_date);

    let mut places: Vec<_> = facts.places.values().cloned().collect();
    let bad_weather = facts.weather.values().any(|weather| {
        weather.precip_prob >= 70.0
            || (matches!(intent.transport, TransportMode::Foot | TransportMode::Bike)
                && weather.wind_max >= 12.0)
    });
    if bad_weather {
        let indoor: Vec<_> = places
            .iter()
            .filter(|place| matches!(place.kind.as_str(), "museum" | "food" | "rest"))
            .cloned()
            .collect();
        // If there are no verified indoor places, leave the day empty instead
        // of planning an unsafe outdoor visit.
        places = indoor;
    }
    let base = (43.1155, 131.8855);
    let radius_km = match intent.transport {
        TransportMode::Foot => 20.0,
        TransportMode::Bike => 50.0,
        TransportMode::Car => 300.0,
        TransportMode::Transit => 400.0,
    };
    let nearby: Vec<_> = places
        .iter()
        .filter(|place| haversine_km(base, (place.lat, place.lon)) <= radius_km)
        .cloned()
        .collect();
    if !nearby.is_empty() {
        places = nearby;
    }
    places.sort_by(|a, b| {
        haversine_km(base, (a.lat, a.lon))
            .partial_cmp(&haversine_km(base, (b.lat, b.lon)))
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let mut used = std::collections::HashSet::new();
    let mut days = Vec::new();
    let mut date = intent.start_date;
    let mut remaining_days = (intent.end_date - intent.start_date).num_days() as usize + 1;

    while date <= intent.end_date {
        let mut items = Vec::new();
        let available = places
            .iter()
            .filter(|place| !used.contains(&place.id))
            .count();
        let slots = ((available + remaining_days - 1) / remaining_days.max(1)).clamp(1, 3);
        for slot in 0..slots {
            let Some(place) = places.iter().find(|place| !used.contains(&place.id)) else {
                break;
            };
            used.insert(place.id.clone());
            let start = if slot == 0 {
                chrono::NaiveTime::from_hms_opt(10, 0, 0)
            } else {
                chrono::NaiveTime::from_hms_opt(14, 0, 0)
            };
            let end = if slot == 0 {
                chrono::NaiveTime::from_hms_opt(12, 0, 0)
            } else {
                chrono::NaiveTime::from_hms_opt(16, 0, 0)
            };
            items.push(ItemDraft {
                place_id: place.id.clone(),
                name: place.name.clone(),
                kind: kind_from_label(&place.kind),
                start,
                end,
                note: None,
            });
        }
        days.push(DayDraft { date, items });
        remaining_days = remaining_days.saturating_sub(1);
        date = add_days(date, 1);
    }

    let lodging_ids = if days.len() > 1 {
        let anchor = places.first().map(|place| (place.lat, place.lon));
        facts
            .lodging
            .values()
            // Only land coordinates can host a night: bay and lake centroids
            // in fixtures or cached results must not become the chosen stay.
            .filter(|option| on_land(option.lat, option.lon))
            .min_by(|left, right| {
                let left_distance = anchor
                    .map(|(lat, lon)| haversine_km((left.lat, left.lon), (lat, lon)))
                    .unwrap_or(0.0);
                let right_distance = anchor
                    .map(|(lat, lon)| haversine_km((right.lat, right.lon), (lat, lon)))
                    .unwrap_or(0.0);
                left_distance
                    .partial_cmp(&right_distance)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|option| vec![option.id.clone()])
            .unwrap_or_default()
    } else {
        Vec::new()
    };

    let draft = PlanDraft {
        title: "План по Приморью".to_string(),
        summary: format!(
            "{} дн. на {} — собрано детерминированно, без LLM",
            days.len(),
            intent.transport.label_ru()
        ),
        days,
        lodging_ids,
        transport: Some(intent.transport),
    };

    let mut plan = enrich(draft, intent, facts);
    plan.generation = Generation::Fallback;
    if bad_weather {
        plan.warnings.push(crate::domain::plan::Warning {
            code: "weather_unsafe".to_string(),
            message: "Сильный ветер или дождь — уличные точки убраны из плана".to_string(),
        });
    }
    plan.warnings.push(crate::domain::plan::Warning {
        code: "fallback".to_string(),
        message:
            "LLM недоступен или план не прошёл проверку — использована детерминированная сборка"
                .to_string(),
    });
    plan
}

fn kind_from_label(label: &str) -> ItemKind {
    match label.to_lowercase().as_str() {
        "museum" => ItemKind::Museum,
        "nature" => ItemKind::Nature,
        "viewpoint" => ItemKind::Viewpoint,
        "park" => ItemKind::Park,
        "historic" => ItemKind::Historic,
        "beach" => ItemKind::Beach,
        "lake" => ItemKind::Lake,
        "river" => ItemKind::River,
        "food" => ItemKind::Food,
        "bar" => ItemKind::Bar,
        "club" => ItemKind::Club,
        _ => ItemKind::Sight,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::facts::{PlaceFact, SourceKind, WeatherDay};
    use crate::domain::intent::{Pace, TransportMode};
    use crate::domain::validate::validate;
    use chrono::NaiveDate;

    #[test]
    fn fallback_is_never_empty() {
        let mut facts = Facts::default();
        let intent = Intent {
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
        };
        let plan = assemble(&mut facts, &intent);
        assert_eq!(plan.days.len(), 2);
        assert!(!plan.days[0].items.is_empty());
        assert!(!plan.lodging.is_empty());
        assert_eq!(plan.generation, Generation::Fallback);
    }

    #[test]
    fn dangerous_wind_removes_outdoor_points() {
        let date = NaiveDate::from_ymd_opt(2026, 10, 24).unwrap();
        let end = NaiveDate::from_ymd_opt(2026, 10, 25).unwrap();
        let intent = Intent {
            region: "primorye".to_string(),
            start_date: date,
            end_date: end,
            max_days: 2,
            transport: TransportMode::Foot,
            budget_rub: None,
            party_size: 1,
            with_children: false,
            interests: vec![],
            must_see: vec![],
            avoid: vec![],
            pace: Pace::Normal,
            assumptions: vec![],
        };
        let mut facts = Facts::default();
        facts.places.insert(
            "test:outdoor".to_string(),
            PlaceFact {
                id: "test:outdoor".to_string(),
                name: "Сопка".to_string(),
                lat: 43.12,
                lon: 131.89,
                kind: "nature".to_string(),
                opening_hours: None,
                source: SourceKind::Live,
            },
        );
        facts.places.insert(
            "test:museum".to_string(),
            PlaceFact {
                id: "test:museum".to_string(),
                name: "Музей".to_string(),
                lat: 43.1155,
                lon: 131.8855,
                kind: "museum".to_string(),
                opening_hours: None,
                source: SourceKind::Live,
            },
        );
        facts.weather.insert(
            date,
            WeatherDay {
                date,
                precip_prob: 0.0,
                wind_max: 20.0,
                temp_min: 5.0,
                temp_max: 10.0,
                sunrise: None,
                sunset: None,
            },
        );

        let plan = assemble(&mut facts, &intent);
        assert!(plan.all_items().all(|item| !item.is_outdoor()));
        assert!(plan
            .warnings
            .iter()
            .any(|warning| warning.code == "weather_unsafe"));
        let report = validate(&plan, &facts, &intent);
        assert!(report.hard_ok(), "{:?}", report.violations);
    }

    #[test]
    fn fallback_fills_every_day_without_repeats() {
        let mut facts = Facts::default();
        let intent = Intent {
            region: "primorye".to_string(),
            start_date: NaiveDate::from_ymd_opt(2026, 10, 24).unwrap(),
            end_date: NaiveDate::from_ymd_opt(2026, 10, 30).unwrap(),
            max_days: 7,
            transport: TransportMode::Car,
            budget_rub: None,
            party_size: 1,
            with_children: false,
            interests: vec![],
            must_see: vec![],
            avoid: vec![],
            pace: Pace::Normal,
            assumptions: vec![],
        };
        let plan = assemble(&mut facts, &intent);
        let ids: Vec<&str> = plan
            .all_items()
            .map(|item| item.place_id.as_str())
            .collect();
        let unique: std::collections::HashSet<&str> = ids.iter().copied().collect();
        assert_eq!(plan.days.len(), 7);
        assert!(plan.days.iter().all(|day| !day.items.is_empty()));
        assert_eq!(ids.len(), unique.len(), "duplicate places: {ids:?}");
    }

    #[test]
    fn fallback_does_not_repeat_places_across_days() {
        let mut facts = Facts::default();
        let intent = Intent {
            region: "primorye".to_string(),
            start_date: NaiveDate::from_ymd_opt(2026, 10, 24).unwrap(),
            end_date: NaiveDate::from_ymd_opt(2026, 10, 26).unwrap(),
            max_days: 3,
            transport: TransportMode::Car,
            budget_rub: None,
            party_size: 1,
            with_children: false,
            interests: vec![],
            must_see: vec![],
            avoid: vec![],
            pace: Pace::Normal,
            assumptions: vec![],
        };
        let plan = assemble(&mut facts, &intent);
        let ids: Vec<&str> = plan
            .all_items()
            .map(|item| item.place_id.as_str())
            .collect();
        let unique: std::collections::HashSet<&str> = ids.iter().copied().collect();
        assert_eq!(plan.days.len(), 3);
        assert_eq!(ids.len(), unique.len(), "duplicate places: {ids:?}");
    }
}
