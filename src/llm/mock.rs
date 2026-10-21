use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use chrono::NaiveDate;
use serde_json::{json, Value};

use crate::domain::dates::{add_days, next_saturday, today_vladivostok};
use crate::domain::intent::{Intent, Pace, TransportMode};
use crate::domain::plan::{DayDraft, ItemDraft, ItemKind, PlanDraft};
use crate::domain::trace::TokenUsage;
use crate::llm::{ChatRequest, ChatResponse, LlmClient, LlmError, LlmToolCall};

/// Deterministic offline LLM used for tests, local demo and when no API key is set.
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
            let weather_args = json!({
                "lat": 43.1155,
                "lon": 131.8855,
                "start_date": intent.start_date.to_string(),
                "end_date": intent.end_date.to_string()
            });
            let lodging_args = json!({
                "settlement": "Владивосток",
                "check_in": intent.start_date.to_string(),
                "check_out": intent.end_date.to_string(),
                "party": intent.party_size
            });
            let mut calls = vec![
                LlmToolCall::new(
                    self.call_id(),
                    "search_places",
                    json!({
                        "area": "Приморский край",
                        "categories": ["nature", "museum", "viewpoint"],
                        "limit": 12
                    })
                    .to_string(),
                ),
                LlmToolCall::new(self.call_id(), "get_weather", weather_args.to_string()),
                LlmToolCall::new(self.call_id(), "find_lodging", lodging_args.to_string()),
            ];
            if intent.transport == TransportMode::Transit {
                calls.push(LlmToolCall::new(
                    self.call_id(),
                    "find_transport",
                    json!({
                        "from": "Владивосток",
                        "to": "Приморский край",
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
            });
        }

        let places = collect_places(&request.messages);
        let lodging_ids = collect_lodging(&request.messages, intent.nights() > 0);
        let draft = build_draft(&intent, &places, &lodging_ids);
        let arguments =
            serde_json::to_string(&draft).map_err(|error| LlmError::Parse(error.to_string()))?;

        Ok(ChatResponse {
            content: None,
            tool_calls: vec![LlmToolCall::new(self.call_id(), "submit_plan", arguments)],
            usage: estimate_usage(&request.messages, 400),
            model: self.model.clone(),
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

fn collect_places(messages: &[crate::llm::LlmMessage]) -> Vec<MockPlace> {
    let mut places = Vec::new();
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
            let name = item.get("name").and_then(Value::as_str).unwrap_or("Место");
            let kind = item.get("kind").and_then(Value::as_str).unwrap_or("sight");
            places.push(MockPlace {
                id: id.to_string(),
                name: name.to_string(),
                kind: kind.to_string(),
            });
        }
    }
    places.truncate(4);
    places
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
    let mut index = 0;

    while date <= intent.end_date {
        let mut items = Vec::new();
        for slot in 0..2 {
            if places.is_empty() {
                break;
            }
            let place = &places[index % places.len()];
            index += 1;
            let (start, end) = if slot == 0 {
                ("10:00:00", "12:00:00")
            } else {
                ("14:00:00", "16:00:00")
            };
            items.push(ItemDraft {
                place_id: place.id.clone(),
                name: place.name.clone(),
                kind: kind_from_label(&place.kind),
                start: crate::domain::plan::parse_time(start),
                end: crate::domain::plan::parse_time(end),
                note: None,
            });
        }
        days.push(DayDraft { date, items });
        date = add_days(date, 1);
    }

    PlanDraft {
        title: "Демо-план по Приморью".to_string(),
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
        "food" => ItemKind::Food,
        _ => ItemKind::Sight,
    }
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
