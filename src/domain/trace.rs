use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::domain::validate::ValidationReport;

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct TokenUsage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
    #[serde(default)]
    pub cost_usd: f64,
    #[serde(default)]
    pub estimated: bool,
}

impl TokenUsage {
    pub fn add(&mut self, other: TokenUsage) {
        self.prompt_tokens = self.prompt_tokens.saturating_add(other.prompt_tokens);
        self.completion_tokens = self
            .completion_tokens
            .saturating_add(other.completion_tokens);
        self.total_tokens = self.total_tokens.saturating_add(other.total_tokens);
        self.cost_usd += other.cost_usd;
        self.estimated |= other.estimated;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Plan,
    Refusal,
    Error,
    Degraded,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceStep {
    pub step: usize,
    pub llm_ms: u64,
    pub tool_names: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallTrace {
    pub id: String,
    pub name: String,
    pub args_summary: String,
    pub ms: u64,
    pub ok: bool,
    pub cached: bool,
    pub source: String,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunTrace {
    pub run_id: Uuid,
    pub query: String,
    pub prompt_version: String,
    pub model: String,
    pub steps: Vec<TraceStep>,
    pub tool_calls: Vec<ToolCallTrace>,
    pub validation: Option<ValidationReport>,
    pub outcome: Outcome,
    pub latency_ms: u64,
    #[serde(default)]
    pub error: Option<String>,
}

impl RunTrace {
    pub fn new(
        run_id: Uuid,
        query: impl Into<String>,
        prompt_version: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        Self {
            run_id,
            query: query.into(),
            prompt_version: prompt_version.into(),
            model: model.into(),
            steps: Vec::new(),
            tool_calls: Vec::new(),
            validation: None,
            outcome: Outcome::Error,
            latency_ms: 0,
            error: None,
        }
    }
}
