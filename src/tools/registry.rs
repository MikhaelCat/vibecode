//! Tool registry and the single provider call path.
//!
//! `Registry` owns all `Tool` implementations, converts them to OpenAI-style
//! function specs and executes calls with a timeout and a disposable
//! tool-result cache. Every result is a `ToolOutput` with provenance; the
//! orchestrator ingests it into `Facts`, so tools never bypass the validator.
//! Provider credentials come from `Config`, which reads them from the
//! environment.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use reqwest::Client;
use serde_json::{json, Value};

use crate::config::Config;
use crate::domain::facts::ToolOutput;
use crate::domain::intent::Intent;
use crate::domain::SourceKind;
use crate::tools::cache::Cache;
use crate::tools::providers::{
    EventsTool, FuelTool, GeocodeTool, LodgingTool, PlacesTool, RouteTool, TransportTool,
    WeatherTool, WikipediaTool,
};

/// Errors a tool may report; the orchestrator turns them into tool messages
/// and keeps the agent loop alive.
#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("bad arguments: {0}")]
    BadArgs(String),
    #[error("upstream error: {0}")]
    Upstream(String),
    #[error("tool timeout")]
    Timeout,
}

/// Shared per-call context: parsed intent (for date defaults), HTTP client,
/// mock-data directory and Nominatim user agent.
pub struct ToolCtx {
    pub intent: Intent,
    pub http: Client,
    pub data_dir: PathBuf,
    pub nominatim_user_agent: String,
}

/// One external data source exposed to the LLM.
///
/// Implementations must return provenance in `ToolOutput::meta` and must not
/// write to `Facts`; ingestion is the orchestrator's job.
#[async_trait]
pub trait Tool: Send + Sync {
    /// Stable function name used by the model and cache keys.
    fn name(&self) -> &'static str;
    /// Russian description shown to the model.
    fn description(&self) -> &'static str;
    /// JSON schema for the tool arguments.
    fn parameters(&self) -> Value;
    /// Executes the tool with raw model-supplied JSON arguments.
    async fn call(&self, args: Value, ctx: &ToolCtx) -> Result<ToolOutput, ToolError>;
}

/// Owns the tool set, shared HTTP client, cache and per-call timeout.
pub struct Registry {
    tools: HashMap<&'static str, Arc<dyn Tool>>,
    http: Client,
    cache: Arc<Cache>,
    data_dir: PathBuf,
    nominatim_user_agent: String,
    timeout: Duration,
}

impl Registry {
    /// Builds all providers and their shared HTTP client from configuration.
    pub fn new(config: &Config) -> anyhow::Result<Self> {
        let http = Client::builder()
            .timeout(config.tool_timeout + Duration::from_secs(2))
            .user_agent(config.nominatim_user_agent.clone())
            .build()?;

        let mut tools: HashMap<&'static str, Arc<dyn Tool>> = HashMap::new();
        let implementations: Vec<Arc<dyn Tool>> = vec![
            Arc::new(WeatherTool),
            Arc::new(PlacesTool),
            Arc::new(WikipediaTool),
            Arc::new(GeocodeTool),
            Arc::new(RouteTool),
            Arc::new(LodgingTool),
            Arc::new(TransportTool),
            Arc::new(FuelTool),
            Arc::new(EventsTool),
        ];
        for tool in implementations {
            tools.insert(tool.name(), tool);
        }

        Ok(Self {
            tools,
            http,
            cache: Arc::new(Cache::new(config.redis_url.clone())),
            data_dir: config.data_dir.clone(),
            nominatim_user_agent: config.nominatim_user_agent.clone(),
            timeout: config.tool_timeout,
        })
    }

    /// Returns OpenAI function specs for all tools plus `submit_plan`.
    pub fn specs(&self) -> Vec<Value> {
        let mut tools: Vec<&Arc<dyn Tool>> = self.tools.values().collect();
        tools.sort_by_key(|tool| tool.name());
        let mut specs: Vec<Value> = tools
            .into_iter()
            .map(|tool| {
                json!({
                    "type": "function",
                    "function": {
                        "name": tool.name(),
                        "description": tool.description(),
                        "parameters": tool.parameters()
                    }
                })
            })
            .collect();
        specs.push(submit_plan_spec());
        specs
    }

    /// Sorted tool names, used by diagnostics and tests.
    pub fn tool_names(&self) -> Vec<&'static str> {
        let mut names: Vec<&'static str> = self.tools.keys().copied().collect();
        names.sort_unstable();
        names
    }

    /// Runs a named tool with caching and a timeout.
    ///
    /// Cache hits are re-labelled as `cache`; only `live` results are stored,
    /// for ten minutes. A miss or cache error simply falls through to the
    /// provider.
    pub async fn call(
        &self,
        name: &str,
        args: Value,
        intent: &Intent,
    ) -> Result<ToolOutput, ToolError> {
        let tool = self
            .tools
            .get(name)
            .ok_or_else(|| ToolError::BadArgs(format!("unknown tool: {name}")))?;

        // get_weather/find_events fall back to the intent dates, so their key
        // must include the date window. Other tools only depend on the intent
        // transport plus their own args, which lets places/lodging results be
        // reused across dates.
        let args_json = serde_json::to_string(&args).unwrap_or_default();
        let cache_key = if matches!(name, "get_weather" | "find_events") {
            format!(
                "{name}:{}:{}:{}:{args_json}",
                intent.transport, intent.start_date, intent.end_date
            )
        } else {
            format!("{name}:{}:{args_json}", intent.transport)
        };
        if let Some(cached) = self.cache.get(&cache_key).await {
            if let Ok(mut output) = serde_json::from_value::<ToolOutput>(cached) {
                output.meta.cached = true;
                output.meta.source = SourceKind::Cache;
                mark_cached_sources(&mut output.data);
                return Ok(output);
            }
        }

        let ctx = ToolCtx {
            intent: intent.clone(),
            http: self.http.clone(),
            data_dir: self.data_dir.clone(),
            nominatim_user_agent: self.nominatim_user_agent.clone(),
        };

        let output = tokio::time::timeout(self.timeout, tool.call(args, &ctx))
            .await
            .map_err(|_| ToolError::Timeout)??;

        if output.meta.source == SourceKind::Live {
            if let Ok(value) = serde_json::to_value(&output) {
                self.cache
                    .put(cache_key, value, Duration::from_secs(600))
                    .await;
            }
        }

        Ok(output)
    }
}

/// Rewrites nested `source` fields so the model can see the result came from
/// the cache rather than a live provider.
fn mark_cached_sources(data: &mut Value) {
    if let Some(source) = data.get_mut("source") {
        if source.is_string() {
            *source = Value::String("cache".to_string());
        }
    }
    for key in ["places", "options", "stations", "events"] {
        if let Some(items) = data.get_mut(key).and_then(Value::as_array_mut) {
            for item in items {
                if item.get("source").is_some() {
                    item["source"] = Value::String("cache".to_string());
                }
            }
        }
    }
}

/// Schema for the terminal `submit_plan` call. It is not a provider, so it
/// lives outside the registry map.
fn submit_plan_spec() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "submit_plan",
            "description": "Финальный план поездки. Вызывай только когда собраны места, погода и маршрут, и только с place_id из результатов инструментов.",
            "parameters": {
                "type": "object",
                "properties": {
                    "title": {"type": "string"},
                    "summary": {"type": "string"},
                    "transport": {
                        "type": "string",
                        "enum": ["car", "foot", "bike", "transit"]
                    },
                    "days": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "date": {"type": "string", "description": "YYYY-MM-DD"},
                                "items": {
                                    "type": "array",
                                    "items": {
                                        "type": "object",
                                        "properties": {
                                            "place_id": {"type": "string"},
                                            "name": {"type": "string"},
                                            "kind": {
                                                "type": "string",
                                                "enum": ["sight", "museum", "nature", "viewpoint", "park", "historic", "beach", "lake", "river", "food", "transfer", "rest"]
                                            },
                                            "start": {"type": "string", "description": "HH:MM или HH:MM:SS"},
                                            "end": {"type": "string", "description": "HH:MM или HH:MM:SS"},
                                            "note": {"type": "string"}
                                        },
                                        "required": ["place_id"]
                                    }
                                }
                            },
                            "required": ["date"]
                        }
                    },
                    "lodging_ids": {
                        "type": "array",
                        "items": {"type": "string"}
                    }
                },
                "required": ["days"]
            }
        }
    })
}
