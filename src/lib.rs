pub mod agent;
pub mod api;
pub mod config;
pub mod domain;
pub mod error;
pub mod llm;
pub mod obs;
pub mod store;
pub mod tools;

use std::sync::Arc;
use std::time::Instant;

use anyhow::Context;
use reqwest::Client;
use tokio::sync::Semaphore;

use crate::agent::Orchestrator;
use crate::api::AppState;
use crate::config::Config;
use crate::llm::{LlmClient, MockLlm, OpenRouterClient};
use crate::store::Store;
use crate::tools::Registry;

pub async fn build_state(cfg: Arc<Config>) -> anyhow::Result<AppState> {
    let store = Store::connect(&cfg.database_url)
        .await
        .context("failed to initialize SQLite store")?;

    let registry = Arc::new(Registry::new(&cfg).context("failed to initialize tools")?);

    let llm: Arc<dyn LlmClient> = if cfg.has_real_llm() {
        let http = Client::builder()
            .timeout(cfg.request_timeout)
            .user_agent("travel-ai/0.1")
            .build()
            .context("failed to build OpenRouter HTTP client")?;
        Arc::new(OpenRouterClient::new(
            http,
            cfg.openrouter_api_key
                .clone()
                .context("OPENROUTER_API_KEY is missing")?,
            cfg.model.clone(),
            cfg.fallback_models.clone(),
            cfg.max_completion_tokens,
        ))
    } else {
        tracing::warn!("OPENROUTER_API_KEY is empty or MOCK_LLM=1: using deterministic mock LLM");
        Arc::new(MockLlm::new())
    };

    let orchestrator = Arc::new(Orchestrator::new(
        cfg.clone(),
        llm,
        registry,
        store.clone(),
    ));

    Ok(AppState {
        cfg: cfg.clone(),
        orchestrator,
        store,
        started_at: Instant::now(),
        semaphore: Arc::new(Semaphore::new(cfg.concurrent_runs)),
    })
}
