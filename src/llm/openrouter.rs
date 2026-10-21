use async_trait::async_trait;
use std::time::Duration;

use reqwest::Client;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::domain::trace::TokenUsage;
use crate::llm::{ChatRequest, ChatResponse, LlmClient, LlmError, LlmToolCall};

const OPENROUTER_URL: &str = "https://openrouter.ai/api/v1/chat/completions";

pub struct OpenRouterClient {
    http: Client,
    api_key: String,
    model: String,
    fallbacks: Vec<String>,
    max_tokens: u32,
    endpoint: String,
    openrouter: bool,
}

impl OpenRouterClient {
    pub fn new(
        http: Client,
        api_key: String,
        model: String,
        fallbacks: Vec<String>,
        max_tokens: u32,
    ) -> Self {
        Self {
            http,
            api_key,
            model,
            fallbacks,
            max_tokens,
            endpoint: OPENROUTER_URL.to_string(),
            openrouter: true,
        }
    }

    /// OpenAI-compatible endpoint, e.g. T-Bank Spirit LLM Proxy.
    pub fn new_compatible(
        http: Client,
        api_key: String,
        endpoint: String,
        model: String,
        max_tokens: u32,
    ) -> Self {
        Self {
            http,
            api_key,
            model,
            fallbacks: Vec::new(),
            max_tokens,
            endpoint,
            openrouter: false,
        }
    }

    async fn request(&self, body: &Value) -> Result<Value, LlmError> {
        let mut last_error = None;
        for attempt in 0..3u32 {
            let mut request = self
                .http
                .post(&self.endpoint)
                .header("Authorization", format!("Bearer {}", self.api_key))
                .json(body);
            if self.openrouter {
                request = request
                    .header("HTTP-Referer", "https://github.com/MikhaelCat/vibecode")
                    .header("X-Title", "vibecode travel assistant");
            }

            match request.send().await {
                Ok(response) => {
                    let status = response.status();
                    let text = response
                        .text()
                        .await
                        .map_err(|error| LlmError::Upstream(error.to_string()))?;

                    if status.is_success() {
                        return serde_json::from_str(&text).map_err(|error| {
                            LlmError::Parse(format!("{error}; body={}", truncate(&text, 500)))
                        });
                    }

                    let retryable = status.as_u16() == 408
                        || status.as_u16() == 429
                        || status.is_server_error();
                    last_error = Some(format!("status {status}: {}", truncate(&text, 500)));
                    if retryable && attempt < 2 {
                        tokio::time::sleep(Duration::from_millis(500 * 2u64.pow(attempt))).await;
                        continue;
                    }
                    return Err(LlmError::Upstream(last_error.unwrap_or_default()));
                }
                Err(error) => {
                    last_error = Some(error.to_string());
                    if attempt < 2 {
                        tokio::time::sleep(Duration::from_millis(500 * 2u64.pow(attempt))).await;
                        continue;
                    }
                }
            }
        }
        Err(LlmError::Upstream(
            last_error.unwrap_or_else(|| "request failed".to_string()),
        ))
    }
}

#[async_trait]
impl LlmClient for OpenRouterClient {
    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse, LlmError> {
        let mut body = json!({
            "model": self.model,
            "messages": request.messages,
            "tools": request.tools,
            "tool_choice": "auto",
            "temperature": 0.2,
            "max_tokens": self.max_tokens
        });

        if self.openrouter {
            body["provider"] = json!({ "require_parameters": true, "sort": "throughput" });
            if !self.fallbacks.is_empty() {
                body["models"] = json!(self.fallbacks);
            }
        }

        let value = self.request(&body).await?;
        let parsed: OpenRouterResponse =
            serde_json::from_value(value).map_err(|error| LlmError::Parse(error.to_string()))?;

        let usage = parsed
            .usage
            .map(|usage| TokenUsage {
                prompt_tokens: usage.prompt_tokens,
                completion_tokens: usage.completion_tokens,
                total_tokens: usage.total_tokens,
                cost_usd: usage.cost.unwrap_or(0.0),
                estimated: false,
            })
            .unwrap_or_default();

        let choice = parsed
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| LlmError::Parse("empty choices".to_string()))?;

        Ok(ChatResponse {
            content: choice.message.content,
            tool_calls: choice.message.tool_calls.unwrap_or_default(),
            usage,
            model: parsed.model.unwrap_or_else(|| self.model.clone()),
        })
    }

    fn model(&self) -> &str {
        &self.model
    }
}

#[derive(Debug, Deserialize)]
struct OpenRouterResponse {
    #[serde(default)]
    choices: Vec<Choice>,
    #[serde(default)]
    usage: Option<Usage>,
    #[serde(default)]
    model: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Choice {
    message: ResponseMessage,
}

#[derive(Debug, Deserialize)]
struct ResponseMessage {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<LlmToolCall>>,
}

#[derive(Debug, Deserialize)]
struct Usage {
    #[serde(default)]
    prompt_tokens: u32,
    #[serde(default)]
    completion_tokens: u32,
    #[serde(default)]
    total_tokens: u32,
    #[serde(default)]
    cost: Option<f64>,
}

fn truncate(value: &str, max: usize) -> String {
    value.chars().take(max).collect()
}
