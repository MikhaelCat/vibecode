use std::env;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};

#[derive(Debug, Clone)]
pub struct Config {
    pub bind: SocketAddr,
    pub openrouter_api_key: Option<String>,
    pub model: String,
    pub fallback_models: Vec<String>,
    pub llm_proxy_token: Option<String>,
    pub llm_proxy_url: String,
    pub llm_proxy_model: Option<String>,
    pub daily_budget_usd: f64,
    pub daily_token_budget: u64,
    pub app_token: Option<String>,
    pub price_in_per_m: f64,
    pub price_out_per_m: f64,
    pub database_url: String,
    pub prompts_dir: PathBuf,
    pub data_dir: PathBuf,
    pub web_dir: PathBuf,
    pub max_steps: usize,
    pub max_repairs: usize,
    pub max_completion_tokens: u32,
    pub request_timeout: Duration,
    pub tool_timeout: Duration,
    pub max_query_chars: usize,
    pub concurrent_runs: usize,
    pub nominatim_user_agent: String,
    pub ors_api_key: Option<String>,
    pub demo_mode: bool,
    pub mock_llm: bool,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let bind = env::var("BIND")
            .unwrap_or_else(|_| "0.0.0.0:8080".to_string())
            .parse()
            .context("BIND must be a socket address, e.g. 0.0.0.0:8080")?;

        let fallback_models = env::var("OPENROUTER_FALLBACK_MODELS")
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(ToOwned::to_owned)
            .collect();

        let cfg = Self {
            bind,
            openrouter_api_key: non_empty(env::var("OPENROUTER_API_KEY").ok()),
            model: non_empty(env::var("OPENROUTER_MODEL").ok())
                .unwrap_or_else(|| "openrouter/auto".to_string()),
            fallback_models,
            llm_proxy_token: non_empty(env::var("LLM_PROXY_TOKEN").ok()),
            llm_proxy_url: env::var("LLM_PROXY_URL")
                .unwrap_or_else(|_| "https://llm-proxy.spirit.t-education.com/v1".to_string()),
            llm_proxy_model: non_empty(env::var("LLM_PROXY_MODEL").ok()),
            daily_budget_usd: parse_f64("DAILY_BUDGET_USD", 25.0),
            daily_token_budget: parse_u64("DAILY_TOKEN_BUDGET", 1_000_000),
            app_token: non_empty(env::var("APP_TOKEN").ok()),
            price_in_per_m: parse_f64("PRICE_IN_PER_M", 0.0),
            price_out_per_m: parse_f64("PRICE_OUT_PER_M", 0.0),
            database_url: env::var("DATABASE_URL")
                .unwrap_or_else(|_| "sqlite://travel-ai.db".to_string()),
            prompts_dir: PathBuf::from(
                env::var("PROMPTS_DIR").unwrap_or_else(|_| "./src/prompts".to_string()),
            ),
            data_dir: PathBuf::from(env::var("DATA_DIR").unwrap_or_else(|_| "./data".to_string())),
            web_dir: PathBuf::from(
                env::var("WEB_DIR").unwrap_or_else(|_| "./src/frontend/dist".to_string()),
            ),
            max_steps: parse_usize("MAX_STEPS", 8),
            max_repairs: parse_usize("MAX_REPAIRS", 2),
            max_completion_tokens: parse_u32("MAX_COMPLETION_TOKENS", 4096),
            request_timeout: Duration::from_secs(parse_u64("REQUEST_TIMEOUT_SECS", 90)),
            tool_timeout: Duration::from_secs(parse_u64("TOOL_TIMEOUT_SECS", 8)),
            max_query_chars: parse_usize("MAX_QUERY_CHARS", 1000),
            concurrent_runs: parse_usize("CONCURRENT_RUNS", 4).max(1),
            nominatim_user_agent: env::var("NOMINATIM_USER_AGENT")
                .unwrap_or_else(|_| "travel-ai-hackathon/0.1 (team@example.com)".to_string()),
            ors_api_key: non_empty(env::var("ORS_API_KEY").ok()),
            demo_mode: parse_bool("DEMO_MODE", false),
            mock_llm: parse_bool("MOCK_LLM", false),
        };

        Ok(cfg)
    }

    pub fn has_real_llm(&self) -> bool {
        !self.mock_llm && (self.llm_proxy_token.is_some() || self.openrouter_api_key.is_some())
    }

    pub fn estimate_cost(&self, prompt_tokens: u32, completion_tokens: u32) -> f64 {
        let input = prompt_tokens as f64 / 1_000_000.0 * self.price_in_per_m;
        let output = completion_tokens as f64 / 1_000_000.0 * self.price_out_per_m;
        input + output
    }

    pub fn estimate_tokens(text: &str) -> u32 {
        // Rough for Russian: ~2.8 chars per token. Used only for budget pre-checks.
        (text.chars().count() as f64 / 2.8).ceil() as u32
    }
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.and_then(|v| {
        let trimmed = v.trim().to_string();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        }
    })
}

fn parse_f64(key: &str, default: f64) -> f64 {
    env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn parse_u64(key: &str, default: u64) -> u64 {
    env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn parse_u32(key: &str, default: u32) -> u32 {
    env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn parse_usize(key: &str, default: usize) -> usize {
    env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn parse_bool(key: &str, default: bool) -> bool {
    env::var(key)
        .ok()
        .map(|v| matches!(v.as_str(), "1" | "true" | "TRUE" | "yes" | "on"))
        .unwrap_or(default)
}
