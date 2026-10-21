use async_trait::async_trait;
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
        let mock = load_mock(ctx.data_dir.as_path(), "events.json", fallback_events());
        let mut events: Vec<Value> = mock
            .get("events")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        events.truncate(5);
        Ok(ToolOutput::ok(
            json!({"events": events, "area": area}),
            SourceKind::Mock,
            format!("События: {} (демо-данные)", events.len()),
        ))
    }
}
