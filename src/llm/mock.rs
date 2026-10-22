use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use chrono::NaiveDate;
use serde_json::{json, Value};

use crate::domain::dates::{add_days, next_saturday, today_vladivostok};
use crate::domain::geo::haversine_km;
use crate::domain::intent::{Intent, Pace, TransportMode};
use crate::domain::plan::{slot_times, DayDraft, ItemDraft, ItemKind, PlanDraft};
use crate::domain::trace::TokenUsage;
use crate::llm::{ChatRequest, ChatResponse, LlmClient, LlmError, LlmToolCall};

/// Deterministic offline LLM used for tests, local demo and when no API key is set.
///
/// Only the model is mocked: the agent still calls the regular tools, which hit
/// the live providers (OSM/Overpass, Wikipedia, Nominatim, Open-Meteo, VL.ru)
/// and fall back to `data/mock/*.json` only when a provider is unavailable.
pub struct MockLlm {
    counter: AtomicUsize,
    model: String,
}

impl Default for MockLlm {
    fn default() -> Self {
        Self::new()
    }
}

impl MockLlm {
    pub fn new() -> Self {
        Self {
            counter: AtomicUsize::new(0),
            model: "mock-llm".to_string(),
        }
    }

    fn call_id(&self) -> String {
        let index = self.counter.fetch_add(1, Ordering::Relaxed);
        format!("mock_call_{index}")
    }
}

#[async_trait]
impl LlmClient for MockLlm {
    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse, LlmError> {
        let intent = intent_from_messages(&request.messages);
        let has_tool_result = request
            .messages
            .iter()
            .any(|message| message.role == "tool");

        if !has_tool_result {
            let area = area_from_messages(&request.messages);
            // A bay, cape or small settlement is not in the settlements
            // gazetteer; its hub carries the coordinates of the area.
            let (area_lat, area_lon) = crate::domain::plan::city_center(&area)
                .or_else(|| crate::domain::hubs::find(&area).map(|hub| (hub.lat, hub.lon)))
                .unwrap_or((43.1155, 131.8855));
            let weather_args = json!({
                "lat": area_lat,
                "lon": area_lon,
                "start_date": intent.start_date.to_string(),
                "end_date": intent.end_date.to_string()
            });
            let lodging_args = json!({
                "settlement": area.clone(),
                "check_in": intent.start_date.to_string(),
                "check_out": intent.end_date.to_string(),
                "party": intent.party_size
            });
            let mut calls = vec![
                LlmToolCall::new(
                    self.call_id(),
                    "search_places",
                    json!({
                        "area": area.clone(),
                        "categories": categories_from_interests(&intent),
                        "limit": 30
                    })
                    .to_string(),
                ),
                LlmToolCall::new(self.call_id(), "get_weather", weather_args.to_string()),
                LlmToolCall::new(self.call_id(), "find_lodging", lodging_args.to_string()),
                LlmToolCall::new(
                    self.call_id(),
                    "find_events",
                    json!({
                        "area": area.clone(),
                        "start_date": intent.start_date.to_string(),
                        "end_date": intent.end_date.to_string()
                    })
                    .to_string(),
                ),
            ];
            if intent.transport == TransportMode::Transit {
                calls.push(LlmToolCall::new(
                    self.call_id(),
                    "find_transport",
                    json!({
                        "from": "Владивосток",
                        "to": area.clone(),
                        "date": intent.start_date.to_string()
                    })
                    .to_string(),
                ));
            }
            return Ok(ChatResponse {
                content: None,
                tool_calls: calls,
                usage: estimate_usage(&request.messages, 300),
                model: self.model.clone(),
                finish_reason: Some("tool_calls".to_string()),
            });
        }

        let places = select_places(
            collect_places(&request.messages),
            &intent,
            collect_bad_weather(&request.messages, &intent),
        );
        // One more tool round before the plan: the day needs a real route, both
        // for the distance numbers and for the line on the map. Without it the
        // offline model produced legs with `geometry: null` and the map stayed
        // empty, which the review reported as "нет отображения маршрутов".
        if !has_route_result(&request.messages) && places.len() >= 2 {
            let points: Vec<Value> = places
                .iter()
                .take(8)
                .map(|place| json!({"lat": place.lat, "lon": place.lon}))
                .collect();
            return Ok(ChatResponse {
                content: None,
                tool_calls: vec![LlmToolCall::new(
                    self.call_id(),
                    "get_route",
                    json!({"points": points, "mode": intent.transport.as_str()}).to_string(),
                )],
                usage: estimate_usage(&request.messages, 200),
                model: self.model.clone(),
                finish_reason: Some("tool_calls".to_string()),
            });
        }
        let lodging_ids = collect_lodging(&request.messages, intent.nights() > 0);
        let draft = build_draft(&intent, &places, &lodging_ids);
        let arguments =
            serde_json::to_string(&draft).map_err(|error| LlmError::Parse(error.to_string()))?;

        Ok(ChatResponse {
            content: None,
            tool_calls: vec![LlmToolCall::new(self.call_id(), "submit_plan", arguments)],
            usage: estimate_usage(&request.messages, 400),
            model: self.model.clone(),
            finish_reason: Some("tool_calls".to_string()),
        })
    }

    fn model(&self) -> &str {
        &self.model
    }
}

#[derive(Debug, Clone)]
struct MockPlace {
    id: String,
    name: String,
    kind: String,
    lat: f64,
    lon: f64,
}

fn intent_from_messages(messages: &[crate::llm::LlmMessage]) -> Intent {
    for message in messages {
        let Some(content) = message.content.as_deref() else {
            continue;
        };
        let Some(index) = content.find("INTENT_JSON:") else {
            continue;
        };
        let json_part = content[index + "INTENT_JSON:".len()..].trim();
        if let Ok(intent) = serde_json::from_str::<Intent>(json_part) {
            return intent;
        }
    }
    default_intent()
}

fn default_intent() -> Intent {
    let today = today_vladivostok();
    let start = next_saturday(today);
    Intent {
        region: "primorye".to_string(),
        start_date: start,
        end_date: add_days(start, 1),
        max_days: 2,
        transport: TransportMode::Car,
        budget_rub: None,
        party_size: 1,
        with_children: false,
        interests: vec![],
        must_see: vec![],
        avoid: vec![],
        pace: Pace::Normal,
        assumptions: vec!["mock".to_string()],
    }
}

fn area_from_messages(messages: &[crate::llm::LlmMessage]) -> String {
    const AREAS: &[(&str, &str)] = &[
        ("уссурийск", "Уссурийск"),
        ("находк", "Находка"),
        ("арсеньев", "Арсеньев"),
        ("артём", "Артём"),
        ("артем", "Артём"),
        ("дальнегорск", "Дальнегорск"),
        ("дальнереченск", "Дальнереченск"),
        ("славянк", "Славянка"),
        ("андреевк", "Андреевка"),
        ("терней", "Терней"),
        ("кавалерово", "Кавалерово"),
        ("лесозаводск", "Лесозаводск"),
        ("большой камень", "Большой Камень"),
        ("фокино", "Фокино"),
        ("партизанск", "Партизанск"),
        ("спасск", "Спасск-Дальний"),
        ("русск", "Владивосток"),
        ("тобизин", "Тобизина"),
        ("пидан", "Пидан"),
        ("ханк", "озеро Ханка"),
        ("владивосток", "Владивосток"),
    ];
    let query = messages
        .iter()
        .rev()
        .find(|message| message.role == "user")
        .and_then(|message| message.content.as_deref())
        .unwrap_or("")
        .to_lowercase();
    AREAS
        .iter()
        .find(|(needle, _)| query.contains(needle))
        .map(|(_, area)| (*area).to_string())
        .or_else(|| {
            // "Южно-Морской", "бухта Триозёрье", "Пидан": keep the user's own
            // wording as the area, the tools resolve the agglomeration.
            crate::domain::hubs::matched_trigger(&query).map(|(_, trigger)| trigger.to_string())
        })
        .unwrap_or_else(|| "Владивосток".to_string())
}

fn collect_places(messages: &[crate::llm::LlmMessage]) -> Vec<MockPlace> {
    let mut places = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for message in messages {
        if message.role != "tool" {
            continue;
        }
        let Some(content) = message.content.as_deref() else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<Value>(content) else {
            continue;
        };
        let Some(array) = value.get("places").and_then(Value::as_array) else {
            continue;
        };
        for item in array {
            let Some(id) = item.get("id").and_then(Value::as_str) else {
                continue;
            };
            if !seen.insert(id.to_string()) {
                continue;
            }
            let name = item.get("name").and_then(Value::as_str).unwrap_or("Место");
            let kind = item.get("kind").and_then(Value::as_str).unwrap_or("sight");
            let lat = item.get("lat").and_then(Value::as_f64).unwrap_or(0.0);
            let lon = item.get("lon").and_then(Value::as_f64).unwrap_or(0.0);
            places.push(MockPlace {
                id: id.to_string(),
                name: name.to_string(),
                kind: kind.to_string(),
                lat,
                lon,
            });
        }
    }
    places
}

fn collect_bad_weather(messages: &[crate::llm::LlmMessage], intent: &Intent) -> bool {
    let mut rainy = false;
    for message in messages {
        if message.role != "tool" {
            continue;
        }
        let Some(content) = message.content.as_deref() else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<Value>(content) else {
            continue;
        };
        let Some(days) = value.get("days").and_then(Value::as_array) else {
            continue;
        };
        for day in days {
            let rain = day
                .get("precip_prob")
                .and_then(Value::as_f64)
                .map(|prob| prob >= 70.0)
                .unwrap_or(false);
            let wind = day
                .get("wind_max")
                .and_then(Value::as_f64)
                .map(|wind| wind >= 12.0)
                .unwrap_or(false);
            if rain
                || (wind && matches!(intent.transport, TransportMode::Foot | TransportMode::Bike))
            {
                rainy = true;
            }
        }
    }
    rainy
}

fn select_places(mut places: Vec<MockPlace>, intent: &Intent, rainy: bool) -> Vec<MockPlace> {
    if places.is_empty() {
        return places;
    }
    if rainy {
        let indoor: Vec<MockPlace> = places
            .iter()
            .filter(|place| matches!(place.kind.as_str(), "museum" | "food" | "rest"))
            .cloned()
            .collect();
        // No verified indoor option means an empty day, not an unsafe outdoor plan.
        places = indoor;
    }
    let base = if places.is_empty() {
        (43.1155, 131.8855)
    } else {
        let lat = places.iter().map(|place| place.lat).sum::<f64>() / places.len() as f64;
        let lon = places.iter().map(|place| place.lon).sum::<f64>() / places.len() as f64;
        (lat, lon)
    };
    // Same radius rule as the deterministic fallback: the offline model must
    // not build a two-day route across the whole region.
    let radius_km = crate::domain::plan::trip_radius_km(intent.transport, intent.days());
    let nearby: Vec<MockPlace> = places
        .iter()
        .filter(|place| haversine_km(base, (place.lat, place.lon)) <= radius_km)
        .cloned()
        .collect();
    if !nearby.is_empty() {
        places = nearby;
    }
    if let Some(city) = intent.city() {
        if let Some(center) = crate::domain::plan::city_center(city) {
            places.retain(|place| haversine_km(center, (place.lat, place.lon)) <= 30.0);
        }
    }
    // Requested kinds first, and each group walked as a route: the offline
    // model must not hand the plan a day that jumps between two clusters.
    {
        let points: Vec<(f64, f64)> = places.iter().map(|place| (place.lat, place.lon)).collect();
        let ranks: Vec<u8> = places
            .iter()
            .map(|place| u8::from(!matches_interest(place, intent)))
            .collect();
        let order = crate::domain::routing::priority_route(&points, &ranks, base);
        let mut reordered: Vec<Option<MockPlace>> = places.drain(..).map(Some).collect();
        for index in order {
            if let Some(place) = reordered.get_mut(index).and_then(Option::take) {
                places.push(place);
            }
        }
        places.extend(reordered.into_iter().flatten());
    }
    places.truncate(30);
    places
}

fn matches_interest(place: &MockPlace, intent: &Intent) -> bool {
    let name = place.name.to_lowercase();
    if intent
        .must_see
        .iter()
        .any(|venue| !venue.is_empty() && name.contains(&venue.to_lowercase()))
    {
        return true;
    }
    let kind = place.kind.as_str();
    intent
        .interests
        .iter()
        .any(|interest| match interest.as_str() {
            "food" => kind == "food",
            "nightlife" => matches!(kind, "bar" | "club"),
            "nature" => matches!(
                kind,
                "nature" | "park" | "beach" | "lake" | "river" | "viewpoint"
            ),
            "sea" | "beach" => matches!(kind, "beach" | "nature" | "lake" | "river"),
            "museum" | "history" => matches!(kind, "museum" | "historic"),
            "attractions" => !matches!(kind, "food" | "bar" | "club" | "rest"),
            "cinema" => kind == "cinema",
            "theater" => kind == "theater",
            "concert" => matches!(kind, "theater" | "sight"),
            "market" => kind == "market",
            "clothes" | "shopping" => matches!(kind, "shop" | "market"),
            "mountains" => matches!(kind, "nature" | "viewpoint"),
            "fishing" => matches!(kind, "beach" | "nature" | "viewpoint"),
            _ => false,
        })
}

/// True when a route has already been measured in this run.
fn has_route_result(messages: &[crate::llm::LlmMessage]) -> bool {
    messages.iter().any(|message| {
        message.role == "tool"
            && message
                .content
                .as_deref()
                .map(|content| content.contains("\"distance_km\""))
                .unwrap_or(false)
    })
}

fn collect_lodging(messages: &[crate::llm::LlmMessage], need_lodging: bool) -> Vec<String> {
    if !need_lodging {
        return Vec::new();
    }
    for message in messages {
        if message.role != "tool" {
            continue;
        }
        let Some(content) = message.content.as_deref() else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<Value>(content) else {
            continue;
        };
        let Some(array) = value.get("options").and_then(Value::as_array) else {
            continue;
        };
        for item in array {
            if item.get("price_per_night").is_none() {
                continue;
            }
            if let Some(id) = item.get("id").and_then(Value::as_str) {
                return vec![id.to_string()];
            }
        }
    }
    Vec::new()
}

fn build_draft(intent: &Intent, places: &[MockPlace], lodging_ids: &[String]) -> PlanDraft {
    let mut days = Vec::new();
    let mut date: NaiveDate = intent.start_date;
    let target_per_day = intent.per_day().map(|count| count.min(20) as usize);
    let mut used = std::collections::HashSet::new();
    let mut remaining_days = (intent.end_date - intent.start_date).num_days() as usize + 1;

    while date <= intent.end_date {
        let mut items = Vec::new();
        let available = places
            .iter()
            .filter(|place| !used.contains(&place.id))
            .count();
        let slots = match target_per_day {
            Some(target) => target.min(available),
            None => ((available + remaining_days - 1) / remaining_days.max(1)).clamp(1, 20),
        };
        for slot in 0..slots {
            let Some(place) = places.iter().find(|place| !used.contains(&place.id)) else {
                break;
            };
            used.insert(place.id.clone());
            let kind = kind_from_label(&place.kind);
            let (start, end) = slot_times(kind, slot);
            items.push(ItemDraft {
                place_id: place.id.clone(),
                name: place.name.clone(),
                kind,
                start,
                end,
                note: None,
            });
        }
        days.push(DayDraft { date, items });
        remaining_days = remaining_days.saturating_sub(1);
        date = add_days(date, 1);
    }

    PlanDraft {
        title: "План по Приморью".to_string(),
        summary: format!("{} дн. — офлайн-план для проверки пайплайна", days.len()),
        days,
        lodging_ids: lodging_ids.to_vec(),
        transport: Some(intent.transport),
    }
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
        "cinema" => ItemKind::Cinema,
        "theater" => ItemKind::Theater,
        "market" => ItemKind::Market,
        "shop" => ItemKind::Shop,
        "food" => ItemKind::Food,
        "bar" => ItemKind::Bar,
        "club" => ItemKind::Club,
        _ => ItemKind::Sight,
    }
}

fn categories_from_interests(intent: &Intent) -> Vec<String> {
    let mut categories: Vec<String> = Vec::new();
    for interest in &intent.interests {
        let wanted: &[&str] = match interest.as_str() {
            "food" => &["restaurant", "cafe"],
            "nightlife" => &["bar", "club"],
            "nature" => &["nature", "park", "beach"],
            "sea" | "beach" => &["beach", "nature", "viewpoint"],
            "museum" | "history" => &["museum", "historic"],
            "attractions" => &["attraction", "museum", "historic", "viewpoint", "park"],
            // "Погулять" in a coastal village means the village itself: its
            // embankment, viewpoints and sights, not only beaches.
            "walk" => &["attraction", "nature", "viewpoint", "park", "historic"],
            "cinema" => &["cinema"],
            "theater" => &["theater"],
            "concert" => &["theatre", "arts_centre"],
            "market" => &["marketplace"],
            "clothes" => &["clothes"],
            "shopping" => &["clothes", "mall", "department_store", "shoes"],
            "mountains" => &["nature", "viewpoint"],
            "fishing" => &["beach", "nature", "viewpoint"],
            _ => &[],
        };
        categories.extend(wanted.iter().map(|category| (*category).to_string()));
    }
    if categories.is_empty() {
        categories.extend(
            [
                "nature",
                "museum",
                "viewpoint",
                "park",
                "beach",
                "lake",
                "river",
            ]
            .iter()
            .map(|category| (*category).to_string()),
        );
    }
    categories.sort();
    categories.dedup();
    categories
}

fn estimate_usage(messages: &[crate::llm::LlmMessage], completion: u32) -> TokenUsage {
    let chars: usize = messages
        .iter()
        .map(|message| message.content.as_deref().unwrap_or("").chars().count())
        .sum();
    let prompt_tokens = (chars as f64 / 2.8).ceil() as u32;
    TokenUsage {
        prompt_tokens,
        completion_tokens: completion,
        total_tokens: prompt_tokens + completion,
        cost_usd: 0.0,
        estimated: true,
    }
}
