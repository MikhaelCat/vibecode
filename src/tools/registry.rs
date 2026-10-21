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

#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("bad arguments: {0}")]
    BadArgs(String),
    #[error("upstream error: {0}")]
    Upstream(String),
    #[error("tool timeout")]
    Timeout,
}

pub struct ToolCtx {
    pub intent: Intent,
    pub http: Client,
    pub data_dir: PathBuf,
    pub nominatim_user_agent: String,
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &'static str;
    fn description(&self) -> &'static str;
    fn parameters(&self) -> Value;
    async fn call(&self, args: Value, ctx: &ToolCtx) -> Result<ToolOutput, ToolError>;
}

pub struct Registry {
    tools: HashMap<&'static str, Arc<dyn Tool>>,
    http: Client,
    cache: Arc<Cache>,
    data_dir: PathBuf,
    nominatim_user_agent: String,
    timeout: Duration,
}

impl Registry {
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
            cache: Arc::new(Cache::new()),
            data_dir: config.data_dir.clone(),
            nominatim_user_agent: config.nominatim_user_agent.clone(),
            timeout: config.tool_timeout,
        })
    }

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

    pub fn tool_names(&self) -> Vec<&'static str> {
        let mut names: Vec<&'static str> = self.tools.keys().copied().collect();
        names.sort_unstable();
        names
    }

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

        let cache_key = format!(
            "{name}:{}:{}",
            intent.transport,
            serde_json::to_string(&args).unwrap_or_default()
        );
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

fn mark_cached_sources(data: &mut Value) {
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
                                                "enum": ["sight", "museum", "nature", "viewpoint", "park", "historic", "food", "transfer", "rest"]
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
