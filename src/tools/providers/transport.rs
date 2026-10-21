use async_trait::async_trait;
use serde_json::{json, Value};

use crate::domain::facts::{SourceKind, ToolOutput};
use crate::tools::providers::{fallback_transport, load_mock};
use crate::tools::registry::{Tool, ToolCtx, ToolError};

pub struct TransportTool;

#[async_trait]
impl Tool for TransportTool {
    fn name(&self) -> &'static str {
        "find_transport"
    }

    fn description(&self) -> &'static str {
        "Расписания автобусов, поездов и паромов. Демо-данные: перед выездом проверьте перевозчика."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "from": {"type": "string"},
                "to": {"type": "string"},
                "date": {"type": "string"},
                "mode": {"type": "string"}
            },
            "required": ["from", "to"]
        })
    }

    async fn call(&self, args: Value, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let from = args
            .get("from")
            .and_then(Value::as_str)
            .unwrap_or("Владивосток")
            .to_lowercase();
        let to = args
            .get("to")
            .and_then(Value::as_str)
            .unwrap_or("Уссурийск")
            .to_lowercase();

        let mock = load_mock(
            ctx.data_dir.as_path(),
            "transport.json",
            fallback_transport(),
        );
        let all: Vec<Value> = mock
            .get("options")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        let mut options: Vec<Value> = all
            .iter()
            .filter(|option| {
                let option_from = option.get("from").and_then(Value::as_str).unwrap_or("");
                let option_to = option.get("to").and_then(Value::as_str).unwrap_or("");
                option_from.to_lowercase().contains(&from) && option_to.to_lowercase().contains(&to)
            })
            .cloned()
            .collect();
        if options.is_empty() {
            options = all;
        }
        options.truncate(5);

        Ok(ToolOutput::ok(
            json!({"options": options}),
            SourceKind::Mock,
            format!("Транспорт: {} вариантов (демо-данные)", options.len()),
        ))
    }
}
