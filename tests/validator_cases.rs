use chrono::{NaiveDate, NaiveTime};
use travel_ai::domain::facts::{Facts, PlaceFact, SourceKind, WeatherDay};
use travel_ai::domain::intent::{Intent, Pace, TransportMode};
use travel_ai::domain::plan::{enrich, DayDraft, ItemDraft, ItemKind, Plan, PlanDraft};
use travel_ai::domain::validate::{validate, Severity, ValidationReport};

fn date(year: i32, month: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(year, month, day).expect("valid test date")
}

fn at(hour: u32, minute: u32) -> NaiveTime {
    NaiveTime::from_hms_opt(hour, minute, 0).expect("valid test time")
}

fn make_intent(start: NaiveDate, end: NaiveDate, max_days: u8) -> Intent {
    Intent {
        region: "primorye".to_string(),
        start_date: start,
        end_date: end,
        max_days,
        transport: TransportMode::Car,
        budget_rub: None,
        party_size: 1,
        with_children: false,
        interests: Vec::new(),
        must_see: Vec::new(),
        avoid: Vec::new(),
        pace: Pace::Normal,
        assumptions: Vec::new(),
    }
}

fn facts_for(intent: &Intent) -> Facts {
    let mut facts = Facts::default();
    facts.ensure_minimal(intent.start_date, intent.end_date);
    facts
}

fn place(id: &str, name: &str, lat: f64, lon: f64, opening_hours: Option<&str>) -> PlaceFact {
    PlaceFact {
        id: id.to_string(),
        name: name.to_string(),
        lat,
        lon,
        kind: "sight".to_string(),
        opening_hours: opening_hours.map(ToOwned::to_owned),
        source: SourceKind::Live,
    }
}

fn insert_weather(facts: &mut Facts, date: NaiveDate, precip_prob: f64) {
    facts.weather.insert(
        date,
        WeatherDay {
            date,
            precip_prob,
            wind_max: 5.0,
            temp_min: 10.0,
            temp_max: 16.0,
            sunrise: Some(at(7, 30)),
            sunset: Some(at(18, 30)),
        },
    );
}

fn item(place_id: &str, kind: ItemKind, start: Option<NaiveTime>) -> ItemDraft {
    ItemDraft {
        place_id: place_id.to_string(),
        name: String::new(),
        kind,
        start,
        end: None,
        note: None,
    }
}

fn day(date: NaiveDate, items: Vec<ItemDraft>) -> DayDraft {
    DayDraft { date, items }
}

fn make_plan(
    intent: &Intent,
    facts: &Facts,
    days: Vec<DayDraft>,
    lodging_ids: Vec<String>,
) -> Plan {
    enrich(
        PlanDraft {
            title: "Тестовый план".to_string(),
            summary: "Интеграционный тест валидатора".to_string(),
            days,
            lodging_ids,
            transport: Some(intent.transport),
        },
        intent,
        facts,
    )
}

fn one_day_plan(
    intent: &Intent,
    facts: &Facts,
    place_id: &str,
    kind: ItemKind,
    start: Option<NaiveTime>,
) -> Plan {
    make_plan(
        intent,
        facts,
        vec![day(intent.start_date, vec![item(place_id, kind, start)])],
        Vec::new(),
    )
}

fn has_hard(report: &ValidationReport, code: &str) -> bool {
    report
        .hard_violations()
        .any(|violation| violation.code == code)
}

#[test]
fn minimal_plan_from_facts_passes() {
    let intent = make_intent(date(2026, 10, 24), date(2026, 10, 24), 1);
    let mut facts = Facts::default();
    facts.ensure_minimal(intent.start_date, intent.end_date);

    let plan = one_day_plan(
        &intent,
        &facts,
        "mock:place:oceanarium",
        ItemKind::Sight,
        None,
    );
    let report = validate(&plan, &facts, &intent);

    assert!(
        report.hard_ok(),
        "minimal plan should pass, violations: {:?}",
        report.violations
    );
}

#[test]
fn sochi_point_triggers_v01() {
    let intent = make_intent(date(2026, 10, 24), date(2026, 10, 24), 1);
    let mut facts = facts_for(&intent);
    facts.places.insert(
        "test:sochi".to_string(),
        place("test:sochi", "Сочи", 43.5855, 39.7231, None),
    );

    let plan = one_day_plan(&intent, &facts, "test:sochi", ItemKind::Sight, None);
    let report = validate(&plan, &facts, &intent);

    let v01 = report
        .hard_violations()
        .find(|violation| violation.code == "V01")
        .expect("V01 hard violation expected");
    assert_eq!(v01.place_id.as_deref(), Some("test:sochi"));
    assert!(!report.hard_ok());
}

#[test]
fn too_many_days_triggers_v02() {
    let intent = make_intent(date(2026, 10, 24), date(2026, 10, 26), 2);
    let facts = facts_for(&intent);
    let days = vec![
        day(
            date(2026, 10, 24),
            vec![item("mock:place:oceanarium", ItemKind::Sight, None)],
        ),
        day(
            date(2026, 10, 25),
            vec![item("mock:place:oceanarium", ItemKind::Sight, None)],
        ),
        day(
            date(2026, 10, 26),
            vec![item("mock:place:oceanarium", ItemKind::Sight, None)],
        ),
    ];
    let plan = make_plan(&intent, &facts, days, Vec::new());
    let report = validate(&plan, &facts, &intent);

    assert!(
        report.hard_violations().any(|violation| {
            violation.code == "V02" && violation.message.contains("не больше 2")
        }),
        "V02 hard violation expected, got: {:?}",
        report.violations
    );
}

#[test]
fn unknown_place_triggers_v10() {
    let intent = make_intent(date(2026, 10, 24), date(2026, 10, 24), 1);
    let facts = facts_for(&intent);
    let mut plan = one_day_plan(&intent, &facts, "test:missing-place", ItemKind::Sight, None);
    // enrich leaves unknown coordinates at (0, 0); keep this test focused on V10.
    plan.days[0].items[0].lat = 43.1155;
    plan.days[0].items[0].lon = 131.8855;

    let report = validate(&plan, &facts, &intent);

    assert!(
        has_hard(&report, "V10"),
        "V10 hard violation expected, got: {:?}",
        report.violations
    );
    assert!(
        !has_hard(&report, "V01"),
        "V01 must not fire for the relocated test point"
    );
}

#[test]
fn closed_on_visit_day_triggers_v04() {
    let monday = date(2026, 10, 26);
    let intent = make_intent(monday, monday, 1);
    let mut facts = facts_for(&intent);
    facts.places.insert(
        "test:closed-monday".to_string(),
        place(
            "test:closed-monday",
            "Музей выходного дня",
            43.1155,
            131.8855,
            Some("Tu-Su 10:00-18:00"),
        ),
    );

    let plan = one_day_plan(
        &intent,
        &facts,
        "test:closed-monday",
        ItemKind::Museum,
        Some(at(12, 0)),
    );
    let report = validate(&plan, &facts, &intent);

    let v04 = report
        .hard_violations()
        .find(|violation| violation.code == "V04")
        .expect("V04 hard violation expected");
    assert_eq!(v04.place_id.as_deref(), Some("test:closed-monday"));
}

#[test]
fn rain_triggers_v05_only_for_outdoor_items() {
    let visit_date = date(2026, 10, 24);
    let intent = make_intent(visit_date, visit_date, 1);
    let mut facts = facts_for(&intent);
    facts.places.insert(
        "test:outdoor".to_string(),
        place(
            "test:outdoor",
            "Смотровая площадка",
            43.1155,
            131.8855,
            None,
        ),
    );
    let plan = one_day_plan(&intent, &facts, "test:outdoor", ItemKind::Nature, None);

    insert_weather(&mut facts, visit_date, 90.0);
    let wet_report = validate(&plan, &facts, &intent);
    assert!(
        has_hard(&wet_report, "V05"),
        "V05 hard violation expected for 90% rain, got: {:?}",
        wet_report.violations
    );

    insert_weather(&mut facts, visit_date, 10.0);
    let dry_report = validate(&plan, &facts, &intent);
    assert!(
        !dry_report
            .violations
            .iter()
            .any(|violation| violation.code == "V05"),
        "V05 must not fire for 10% rain, got: {:?}",
        dry_report.violations
    );
    assert!(
        dry_report.hard_ok(),
        "10% rain plan should be hard-valid, got: {:?}",
        dry_report.violations
    );
}

#[test]
fn two_day_plan_without_lodging_triggers_v12() {
    let intent = make_intent(date(2026, 10, 24), date(2026, 10, 25), 2);
    let mut facts = facts_for(&intent);
    facts.lodging.clear();
    let days = vec![
        day(
            date(2026, 10, 24),
            vec![item("mock:place:oceanarium", ItemKind::Sight, None)],
        ),
        day(
            date(2026, 10, 25),
            vec![item("mock:place:fortress", ItemKind::Sight, None)],
        ),
    ];
    let plan = make_plan(&intent, &facts, days, Vec::new());

    assert!(plan.lodging.is_empty());
    assert_eq!(plan.nights(), 1);
    let report = validate(&plan, &facts, &intent);

    assert!(
        has_hard(&report, "V12"),
        "V12 hard violation expected, got: {:?}",
        report.violations
    );
}

#[test]
fn over_budget_is_soft_v11_and_still_hard_valid() {
    let mut intent = make_intent(date(2026, 10, 24), date(2026, 10, 25), 2);
    intent.budget_rub = Some(1000);
    let facts = facts_for(&intent);
    let days = vec![
        day(
            date(2026, 10, 24),
            vec![item("mock:place:oceanarium", ItemKind::Sight, None)],
        ),
        day(
            date(2026, 10, 25),
            vec![item("mock:place:fortress", ItemKind::Sight, None)],
        ),
    ];
    let plan = make_plan(
        &intent,
        &facts,
        days,
        vec!["mock:lodge:vladivostok".to_string()],
    );
    assert!(plan.cost_estimate_rub.expect("lodging cost") > 1000);

    let report = validate(&plan, &facts, &intent);
    let v11 = report
        .violations
        .iter()
        .find(|violation| violation.code == "V11")
        .expect("V11 soft violation expected");
    assert_eq!(v11.severity, Severity::Soft);
    assert!(
        report.hard_ok(),
        "budget overflow must not fail hard checks, got: {:?}",
        report.violations
    );
}
