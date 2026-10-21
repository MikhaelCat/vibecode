use async_trait::async_trait;
use serde_json::{json, Value};

use crate::domain::facts::{SourceKind, ToolOutput};
use crate::tools::providers::{fallback_lodging, limit_from, load_mock};
use crate::tools::registry::{Tool, ToolCtx, ToolError};

pub struct LodgingTool;

#[async_trait]
impl Tool for LodgingTool {
    fn name(&self) -> &'static str {
        "find_lodging"
    }

    fn description(&self) -> &'static str {
        "Варианты ночёвки в населённом пункте. Демо-данные: цены и наличие не гарантированы."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "settlement": {"type": "string"},
                "check_in": {"type": "string"},
                "check_out": {"type": "string"},
                "party": {"type": "integer"},
                "limit": {"type": "integer"}
            },
            "required": ["settlement"]
        })
    }

    async fn call(&self, args: Value, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let settlement = args
            .get("settlement")
            .and_then(Value::as_str)
            .unwrap_or("Владивосток")
            .to_lowercase();
        let limit = limit_from(&args, 3, 10);
        let mock = load_mock(ctx.data_dir.as_path(), "lodging.json", fallback_lodging());
        let mut options: Vec<Value> = mock
            .get("options")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        let filtered: Vec<Value> = options
            .iter()
            .filter(|option| {
                option
                    .get("settlement")
                    .and_then(Value::as_str)
                    .map(|value| value.to_lowercase().contains(&settlement))
                    .unwrap_or(false)
            })
            .cloned()
            .collect();
        if !filtered.is_empty() {
            options = filtered;
        } else {
            options.clear();
        }
        options.truncate(limit);

        Ok(ToolOutput::ok(
            json!({"options": options}),
            SourceKind::Mock,
            format!("Жильё: {} вариантов (демо-данные)", options.len()),
        ))
    }
}
