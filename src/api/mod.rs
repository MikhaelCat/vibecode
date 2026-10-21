pub mod dto;
pub mod sse;

use std::sync::Arc;
use std::time::Instant;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};
use tokio::sync::Semaphore;
use tower_http::cors::CorsLayer;
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::services::{ServeDir, ServeFile};
use tower_http::trace::TraceLayer;

use crate::agent::Orchestrator;
use crate::config::Config;
use crate::error::ApiError;
use crate::store::Store;

pub use dto::PlanRequest;
pub use sse::plan_stream;

#[derive(Clone)]
pub struct AppState {
    pub cfg: Arc<Config>,
    pub orchestrator: Arc<Orchestrator>,
    pub store: Store,
    pub started_at: Instant,
    pub semaphore: Arc<Semaphore>,
}

pub fn router(state: AppState) -> Router {
    let web_dir = state.cfg.web_dir.clone();
    let index = web_dir.join("index.html");

    Router::new()
        .route("/api/plan", post(plan_sse))
        .route("/api/plan/sync", post(plan_sync))
        .route("/api/health", get(health))
        .route("/api/stats", get(stats))
        .route("/api/runs/{id}", get(run_by_id))
        .fallback_service(ServeDir::new(web_dir).fallback(ServeFile::new(index)))
        .layer(RequestBodyLimitLayer::new(16 * 1024))
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

async fn plan_sse(
    State(state): State<AppState>,
    Json(request): Json<PlanRequest>,
) -> Result<Response, ApiError> {
    let query = validate_query(&state, request.query)?;
    if !state.orchestrator.budget_ok().await {
        return Err(ApiError::budget_exhausted(
            "Дневной лимит токенов исчерпан. Попробуйте завтра.",
        ));
    }

    let permit = state
        .semaphore
        .clone()
        .try_acquire_owned()
        .map_err(|_| ApiError::rate_limited("Слишком много одновременных запросов"))?;

    let (tx, rx) = tokio::sync::mpsc::channel(64);
    let orchestrator = state.orchestrator.clone();
    tokio::spawn(async move {
        let _permit = permit;
        orchestrator.run(query, Some(tx)).await;
    });

    Ok(plan_stream(rx).into_response())
}

async fn plan_sync(
    State(state): State<AppState>,
    Json(request): Json<PlanRequest>,
) -> Result<Json<Value>, ApiError> {
    let query = validate_query(&state, request.query)?;
    if !state.orchestrator.budget_ok().await {
        return Err(ApiError::budget_exhausted(
            "Дневной лимит токенов исчерпан. Попробуйте завтра.",
        ));
    }

    let outcome = state.orchestrator.run(query, None).await;
    Ok(Json(json!({
        "run_id": outcome.run_id,
        "model": outcome.model,
        "intent": outcome.intent,
        "plan": outcome.plan,
        "report": outcome.report,
        "refusal": outcome.refusal,
        "usage": outcome.usage,
        "latency_ms": outcome.latency_ms,
        "tools_used": outcome.tools_used,
        "trace": outcome.trace
    })))
}

async fn health(State(state): State<AppState>) -> Json<Value> {
    let spent = state.store.spent_today().await;
    Json(json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
        "model": state.orchestrator.model(),
        "mock_llm": !state.cfg.has_real_llm(),
        "prompt_version": state.orchestrator.prompt_version(),
        "uptime_secs": state.started_at.elapsed().as_secs(),
        "budget": {
            "daily_usd": state.cfg.daily_budget_usd,
            "spent_today_usd": spent
        }
    }))
}

async fn stats(State(state): State<AppState>) -> Json<Value> {
    Json(state.store.stats_today().await)
}

async fn run_by_id(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    match state.store.get_run(&id).await {
        Some(run) => Ok(Json(run)),
        None => Err(ApiError {
            status: StatusCode::NOT_FOUND,
            code: "not_found",
            message: format!("run {id} not found"),
            hint: None,
        }),
    }
}

fn validate_query(state: &AppState, query: String) -> Result<String, ApiError> {
    let query = query.trim().to_string();
    if query.is_empty() {
        return Err(ApiError::bad_request("query must not be empty"));
    }
    if query.chars().count() > state.cfg.max_query_chars {
        return Err(ApiError::payload_too_large(format!(
            "query is longer than {} characters",
            state.cfg.max_query_chars
        )));
    }
    Ok(query)
}
