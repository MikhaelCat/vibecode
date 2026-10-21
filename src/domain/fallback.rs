use crate::domain::dates::add_days;
use crate::domain::facts::Facts;
use crate::domain::intent::Intent;
use crate::domain::plan::{enrich, DayDraft, Generation, ItemDraft, ItemKind, Plan, PlanDraft};

/// Deterministic plan from already collected facts. Never calls the LLM.
pub fn assemble(facts: &mut Facts, intent: &Intent) -> Plan {
    facts.ensure_minimal(intent.start_date, intent.end_date);

    let mut places: Vec<_> = facts.places.values().cloned().collect();
    places.sort_by(|a, b| a.name.cmp(&b.name));

    let mut cursor = 0;
    let mut days = Vec::new();
    let mut date = intent.start_date;

    while date <= intent.end_date {
        let mut items = Vec::new();
        for slot in 0..2 {
            if places.is_empty() {
                break;
            }
            let place = &places[cursor % places.len()];
            cursor += 1;
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
        date = add_days(date, 1);
    }

    let lodging_ids = if days.len() > 1 {
        facts.lodging.keys().next().cloned().into_iter().collect()
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
        "food" => ItemKind::Food,
        _ => ItemKind::Sight,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::intent::{Pace, TransportMode};
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
}
