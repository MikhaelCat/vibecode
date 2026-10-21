use std::collections::HashSet;

use chrono::{Datelike, NaiveDate, NaiveTime, Weekday};
use serde::{Deserialize, Serialize};

use crate::domain::facts::Facts;
use crate::domain::geo::in_primorye;
use crate::domain::intent::{Intent, TransportMode};
use crate::domain::plan::{parse_time, Plan};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Hard,
    Soft,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Violation {
    pub code: String,
    pub severity: Severity,
    pub message: String,
    pub hint: Option<String>,
    pub place_id: Option<String>,
}

impl Violation {
    fn hard(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_string(),
            severity: Severity::Hard,
            message: message.into(),
            hint: None,
            place_id: None,
        }
    }

    fn soft(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_string(),
            severity: Severity::Soft,
            message: message.into(),
            hint: None,
            place_id: None,
        }
    }

    fn with_place(mut self, place_id: &str) -> Self {
        self.place_id = Some(place_id.to_string());
        self
    }

    fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationReport {
    pub violations: Vec<Violation>,
    pub hard_failed: bool,
    pub checks_run: u32,
    pub checks_passed: u32,
    pub score: f32,
}

impl ValidationReport {
    pub fn hard_ok(&self) -> bool {
        !self.hard_failed
    }

    pub fn hard_violations(&self) -> impl Iterator<Item = &Violation> {
        self.violations
            .iter()
            .filter(|violation| violation.severity == Severity::Hard)
    }

    pub fn feedback(&self) -> String {
        let mut lines = Vec::new();
        for violation in self.hard_violations().take(5) {
            let hint = violation
                .hint
                .as_deref()
                .map(|hint| format!(" Подсказка: {hint}"))
                .unwrap_or_default();
            lines.push(format!(
                "- {}: {}{hint}",
                violation.code, violation.message
            ));
        }
        if lines.is_empty() {
            return "План прошёл проверку.".to_string();
        }
        format!(
            "Проверка плана не прошла. Исправь только перечисленные нарушения и снова вызови submit_plan. Не выдумывай новые места — используй только place_id из результатов инструментов.\n{}",
            lines.join("\n")
        )
    }
}

pub fn validate(plan: &Plan, facts: &Facts, intent: &Intent) -> ValidationReport {
    let mut violations = Vec::new();
    const CHECKS: u32 = 15;

    // V01 — регион
    for item in plan.all_items() {
        if !in_primorye(item.lat, item.lon) {
            violations.push(
                Violation::hard(
                    "V01",
                    format!("«{}» вне Приморского края", item.name),
                )
                .with_place(&item.place_id)
                .with_hint("Уберите точку или замените на приморскую"),
            );
        }
    }

    // V02 — сроки
    if plan.days.is_empty() {
        violations.push(Violation::hard("V02", "В плане нет ни одного дня"));
    } else {
        if plan.days.len() as u8 > intent.max_days {
            violations.push(Violation::hard(
                "V02",
                format!(
                    "В плане {} дн., а просили не больше {}",
                    plan.days.len(),
                    intent.max_days
                ),
            ));
        }
        for day in &plan.days {
            if day.date < intent.start_date || day.date > intent.end_date {
                violations.push(Violation::hard(
                    "V02",
                    format!("Дата {} выходит за запрошенный период", day.date),
                ));
            }
        }
        let first = plan.days.first().map(|day| day.date);
        let last = plan.days.last().map(|day| day.date);
        if first != Some(intent.start_date) || last != Some(intent.end_date) {
            violations.push(Violation::soft(
                "V02",
                "План покрывает не весь запрошенный период",
            ));
        }
    }

    // V03 — транспорт
    for leg in plan.all_legs() {
        if leg.mode != intent.transport {
            violations.push(Violation::hard(
                "V03",
                format!(
                    "Перегон указан как {}, а запрошен {}",
                    leg.mode,
                    intent.transport
                ),
            ));
        }
    }

    // V04 — часы работы
    for day in &plan.days {
        for item in &day.items {
            let Some(start) = item.start else {
                continue;
            };
            if let Some(hours) = item.opening_hours.as_deref() {
                match check_open(hours, day.date, start) {
                    Some(false) => {
                        violations.push(
                            Violation::hard(
                                "V04",
                                format!("«{}» закрыто в это время", item.name),
                            )
                            .with_place(&item.place_id)
                            .with_hint(format!("Часы работы: {hours}")),
                        );
                    }
                    None => {
                        violations.push(Violation::soft(
                            "V04",
                            format!("Часы работы «{}» не удалось проверить", item.name),
                        ));
                    }
                    Some(true) => {}
                }
            }
        }
    }

    // V05 — погода
    for day in &plan.days {
        match facts.weather_for(day.date) {
            Some(weather) => {
                for item in &day.items {
                    if !item.is_outdoor() {
                        continue;
                    }
                    if weather.precip_prob >= 70.0 {
                        violations.push(
                            Violation::hard(
                                "V05",
                                format!(
                                    "{}: дождь {}%, «{}» на улице",
                                    day.date, weather.precip_prob, item.name
                                ),
                            )
                            .with_place(&item.place_id)
                            .with_hint("Предложите indoor-альтернативу или перенесите визит"),
                        );
                    }
                    if matches!(intent.transport, TransportMode::Bike | TransportMode::Foot)
                        && weather.wind_max >= 12.0
                    {
                        violations.push(Violation::hard(
                            "V05",
                            format!(
                                "{}: ветер {} м/с — не для {}",
                                day.date,
                                weather.wind_max,
                                intent.transport.label_ru()
                            ),
                        ));
                    }
                    if intent.transport == TransportMode::Bike && weather.temp_min < 0.0 {
                        violations.push(Violation::soft(
                            "V05",
                            format!("{}: температура ниже нуля для велосипеда", day.date),
                        ));
                    }
                }
            }
            None => violations.push(Violation::soft(
                "V05",
                format!("Прогноз погоды на {} недоступен", day.date),
            )),
        }
    }

    // V06 — дневной пробег
    for day in &plan.days {
        let distance: f64 = day.legs.iter().map(|leg| leg.distance_km).sum();
        let (soft, hard) = match intent.transport {
            TransportMode::Foot => (15.0, 25.0),
            TransportMode::Bike => (60.0, 80.0),
            TransportMode::Car => (350.0, 450.0),
            TransportMode::Transit => (400.0, 500.0),
        };
        if distance > hard {
            violations.push(Violation::hard(
                "V06",
                format!(
                    "{}: {:.0} км за день — слишком много для {}",
                    day.date,
                    distance,
                    intent.transport.label_ru()
                ),
            ));
        } else if distance > soft {
            violations.push(Violation::soft(
                "V06",
                format!("{}: {:.0} км за день — большой пробег", day.date, distance),
            ));
        }
    }

    // V07 — светлое время
    if matches!(intent.transport, TransportMode::Foot | TransportMode::Bike) {
        for day in &plan.days {
            let Some(weather) = facts.weather_for(day.date) else {
                continue;
            };
            let Some(sunset) = weather.sunset else {
                continue;
            };
            for item in &day.items {
                if let Some(end) = item.end {
                    if end > sunset {
                        violations.push(Violation::soft(
                            "V07",
                            format!(
                                "{}: «{}» заканчивается после заката ({})",
                                day.date, item.name, sunset
                            ),
                        ));
                    }
                }
            }
        }
    }

    // V08 — расписания
    if intent.transport == TransportMode::Transit {
        if facts.transport.is_empty() {
            violations.push(Violation::soft(
                "V08",
                "Расписания транспорта не проверены — нет данных",
            ));
        } else {
            let last = facts
                .transport
                .iter()
                .filter_map(|option| parse_time(&option.arrive))
                .max();
            if let Some(last) = last {
                if last > NaiveTime::from_hms_opt(23, 0, 0).unwrap() {
                    violations.push(Violation::hard(
                        "V08",
                        "Последний рейс прибывает после 23:00 — план не сходится",
                    ));
                }
            }
        }
    }

    // V09 — топливо
    if intent.transport == TransportMode::Car {
        for day in &plan.days {
            let distance: f64 = day.legs.iter().map(|leg| leg.distance_km).sum();
            if distance > 250.0 && facts.fuel.is_empty() {
                violations.push(Violation::soft(
                    "V09",
                    format!("{}: длинный перегон, АЗС не проверены", day.date),
                ));
            }
        }
    }

    // V10 — провенанс
    for item in plan.all_items() {
        if !facts.contains_place(&item.place_id) {
            violations.push(
                Violation::hard(
                    "V10",
                    format!("Место «{}» не найдено в данных инструментов", item.name),
                )
                .with_place(&item.place_id)
                .with_hint("Используйте только place_id из результатов инструментов"),
            );
        }
    }
    for lodging in &plan.lodging {
        if !facts.lodging.contains_key(&lodging.id) {
            violations.push(Violation::hard(
                "V10",
                format!("Ночёвка «{}» не найдена в данных инструментов", lodging.name),
            ));
        }
    }

    // V11 — бюджет
    if let (Some(budget), Some(cost)) = (intent.budget_rub, plan.cost_estimate_rub) {
        if cost > budget {
            violations.push(Violation::soft(
                "V11",
                format!("Оценка {cost} ₽ выше бюджета {budget} ₽"),
            ));
        }
    }

    // V12 — ночёвка
    if plan.nights() > 0 && plan.lodging.is_empty() {
        violations.push(Violation::hard(
            "V12",
            "В плане есть ночёвка, но не выбрано жильё",
        ));
    }

    // V13 — дубли
    let mut seen = HashSet::new();
    for item in plan.all_items() {
        if !seen.insert(item.place_id.clone()) {
            violations.push(Violation::soft(
                "V13",
                format!("«{}» встречается в плане дважды", item.name),
            ));
        }
    }

    // V14 — время за рулём
    for day in &plan.days {
        let minutes: u32 = day.legs.iter().map(|leg| leg.duration_min).sum();
        if minutes > 360 {
            violations.push(Violation::soft(
                "V14",
                format!("{}: {} ч за рулём/в пути — тяжело", day.date, minutes / 60),
            ));
        }
    }

    // V15 — пустые дни
    for day in &plan.days {
        if day.items.is_empty() {
            violations.push(Violation::soft(
                "V15",
                format!("{}: в дне нет ни одной точки", day.date),
            ));
        }
    }

    let hard_failed = violations
        .iter()
        .any(|violation| violation.severity == Severity::Hard);
    let checks_passed = CHECKS.saturating_sub(violations.len() as u32);
    let score = checks_passed as f32 / CHECKS as f32;

    ValidationReport {
        violations,
        hard_failed,
        checks_run: CHECKS,
        checks_passed,
        score,
    }
}

fn check_open(hours: &str, date: NaiveDate, time: NaiveTime) -> Option<bool> {
    let trimmed = hours.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("off") {
        return Some(false);
    }
    if trimmed.contains("24/7") {
        return Some(true);
    }

    let mut parsed_any = false;
    let mut open = false;

    for segment in trimmed.split(';') {
        let mut parts = segment.split_whitespace();
        let Some(days_spec) = parts.next() else {
            continue;
        };
        let Some(times_spec) = parts.next() else {
            continue;
        };
        let Some(days) = parse_days(days_spec) else {
            continue;
        };
        parsed_any = true;
        if !days.contains(&date.weekday()) {
            continue;
        }
        for range in times_spec.split(',') {
            let Some((from, to)) = range.split_once('-') else {
                continue;
            };
            let (Some(from), Some(to)) = (parse_time(from), parse_time(to)) else {
                continue;
            };
            if time >= from && time <= to {
                open = true;
            }
        }
    }

    if parsed_any {
        Some(open)
    } else {
        None
    }
}

fn parse_days(spec: &str) -> Option<Vec<Weekday>> {
    let mut days = Vec::new();
    for part in spec.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if let Some((start, end)) = part.split_once('-') {
            let start = day_index(start)?;
            let end = day_index(end)?;
            let mut current = start;
            loop {
                days.push(weekday_from_index(current));
                if current == end {
                    break;
                }
                current = (current + 1) % 7;
            }
        } else {
            days.push(weekday_from_index(day_index(part)?));
        }
    }
    if days.is_empty() {
        None
    } else {
        Some(days)
    }
}

fn day_index(code: &str) -> Option<u32> {
    match code.to_lowercase().as_str() {
        "mo" => Some(0),
        "tu" => Some(1),
        "we" => Some(2),
        "th" => Some(3),
        "fr" => Some(4),
        "sa" => Some(5),
        "su" => Some(6),
        _ => None,
    }
}

fn weekday_from_index(index: u32) -> Weekday {
    match index {
        0 => Weekday::Mon,
        1 => Weekday::Tue,
        2 => Weekday::Wed,
        3 => Weekday::Thu,
        4 => Weekday::Fri,
        5 => Weekday::Sat,
        _ => Weekday::Sun,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::facts::Facts;
    use crate::domain::intent::{Intent, Pace};
    use crate::domain::plan::PlanDraft;

    fn intent() -> Intent {
        Intent {
            region: "primorye".to_string(),
            start_date: NaiveDate::from_ymd_opt(2026, 10, 24).unwrap(),
            end_date: NaiveDate::from_ymd_opt(2026, 10, 24).unwrap(),
            max_days: 1,
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
    fn opening_hours_weekday() {
        let sat = NaiveDate::from_ymd_opt(2026, 10, 24).unwrap();
        let time = NaiveTime::from_hms_opt(12, 0, 0).unwrap();
        assert_eq!(check_open("Tu-Su 10:00-18:00", sat, time), Some(true));
        let mon = NaiveDate::from_ymd_opt(2026, 10, 26).unwrap();
        assert_eq!(check_open("Tu-Su 10:00-18:00", mon, time), Some(false));
    }

    #[test]
    fn unknown_place_is_hard() {
        let mut facts = Facts::default();
        facts.ensure_minimal(intent().start_date, intent().end_date);
        let known = facts.places.keys().next().unwrap().clone();
        let draft = PlanDraft {
            title: "t".to_string(),
            summary: "s".to_string(),
            days: vec![crate::domain::plan::DayDraft {
                date: intent().start_date,
                items: vec![crate::domain::plan::ItemDraft {
                    place_id: known,
                    name: "known".to_string(),
                    kind: crate::domain::plan::ItemKind::Sight,
                    start: None,
                    end: None,
                    note: None,
                }],
            }],
            lodging_ids: vec![],
            transport: None,
        };
        let plan = crate::domain::plan::enrich(draft, &intent(), &facts);
        let report = validate(&plan, &facts, &intent());
        assert!(report.hard_ok(), "{:?}", report.violations);
    }
}
