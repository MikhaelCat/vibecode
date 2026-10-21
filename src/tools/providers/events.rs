use std::collections::HashSet;

use async_trait::async_trait;
use chrono::NaiveDate;
use serde_json::{json, Value};

use crate::domain::facts::{SourceKind, ToolOutput};
use crate::tools::providers::{fallback_events, load_mock};
use crate::tools::registry::{Tool, ToolCtx, ToolError};

pub struct EventsTool;

#[async_trait]
impl Tool for EventsTool {
    fn name(&self) -> &'static str {
        "find_events"
    }

    fn description(&self) -> &'static str {
        "События и экскурсии в районе на даты поездки. Демо-данные."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "area": {"type": "string"},
                "start_date": {"type": "string"},
                "end_date": {"type": "string"}
            },
            "required": ["area", "start_date", "end_date"]
        })
    }

    async fn call(&self, args: Value, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let area = args
            .get("area")
            .and_then(Value::as_str)
            .unwrap_or("Приморский край");
        let start = args
            .get("start_date")
            .and_then(Value::as_str)
            .and_then(|value| NaiveDate::parse_from_str(value, "%Y-%m-%d").ok());
        let end = args
            .get("end_date")
            .and_then(Value::as_str)
            .and_then(|value| NaiveDate::parse_from_str(value, "%Y-%m-%d").ok());

        let mock = load_mock(ctx.data_dir.as_path(), "events.json", fallback_events());
        let mut seen = HashSet::new();
        let mut events: Vec<Value> = Vec::new();
        for event in mock
            .get("events")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
        {
            let event_date = event
                .get("date")
                .and_then(Value::as_str)
                .and_then(|value| NaiveDate::parse_from_str(value, "%Y-%m-%d").ok());
            if let (Some(start), Some(end), Some(date)) = (start, end, event_date) {
                if date < start || date > end {
                    continue;
                }
            }
            let title = event
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_lowercase();
            let id = event
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            if !seen.insert((id, title.clone())) {
                continue;
            }
            events.push(event);
        }
        events.truncate(5);
        Ok(ToolOutput::ok(
            json!({"events": events, "area": area}),
            SourceKind::Mock,
            format!("События: {} (демо-данные)", events.len()),
        ))
    }
}
