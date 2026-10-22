//! HTTP boundary.
//!
//! `POST /api/plan` streams SSE events from the orchestrator; `/api/plan/sync`
//! exists for evals and debugging. The router also serves the built SPA and
//! read-only diagnostics (`/api/health`, `/api/stats`, `/api/runs/{id}`).
//! Request parsing lives in `dto`; SSE framing lives in `sse`.
pub mod dto;
pub mod sse;

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};
use tokio::sync::Semaphore;
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::services::{ServeDir, ServeFile};
use tower_http::trace::TraceLayer;

use crate::agent::{Orchestrator, RunOverrides, StreamEvent};
use crate::config::Config;
use crate::domain::intent::MAX_TRIP_DAYS;
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
    if !index.exists() {
        tracing::error!(
            path = %index.display(),
            "frontend index.html is missing; run `just web` or set WEB_DIR"
        );
    }

    Router::new()
        .route("/api/plan", post(plan_sse))
        .route("/api/plan/sync", post(plan_sync))
        .route("/api/health", get(health))
        .route("/api/stats", get(stats))
        .route("/api/runs/{id}", get(run_by_id))
        .fallback_service(ServeDir::new(web_dir).fallback(ServeFile::new(index)))
        // The SPA has no build hashes in its file names, so a stale bundle in
        // the browser cache hid new map layers ("нет маршрутов и заправок").
        // Static answers revalidate; API responses are untouched.
        .layer(axum::middleware::from_fn(static_no_cache))
        .layer(RequestBodyLimitLayer::new(16 * 1024))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

async fn plan_sse(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<PlanRequest>,
) -> Result<Response, ApiError> {
    check_auth(&state, &headers)?;
    let overrides = validate_overrides(&request)?;
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
        let run = tokio::select! {
            run = tokio::time::timeout(
                Duration::from_secs(300),
                orchestrator.execute_with(query, Some(tx.clone()), overrides),
            ) => run,
            // The browser tab closed: drop the run instead of spending
            // tokens on a stream nobody reads.
            _ = tx.closed() => return,
        };
        if run.is_err() {
            // The orchestrator was dropped mid-run. Tell the client why the
            // stream ends and close it; do not fabricate a plan.
            let _ = tx
                .send(StreamEvent {
                    event: "refusal",
                    data: json!({
                        "category": "timeout",
                        "message": "Не удалось собрать план за 300 секунд. Попробуйте более короткий запрос."
                    }),
                })
                .await;
            let _ = tx
                .send(StreamEvent {
                    event: "done",
                    data: json!({"outcome": "refusal"}),
                })
                .await;
        }
    });

    Ok(plan_stream(rx).into_response())
}

async fn plan_sync(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<PlanRequest>,
) -> Result<Json<Value>, ApiError> {
    check_auth(&state, &headers)?;
    let overrides = validate_overrides(&request)?;
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
    let outcome = tokio::time::timeout(
        Duration::from_secs(300),
        state.orchestrator.execute_with(query, None, overrides),
    )
    .await
    .map_err(|_| ApiError {
        status: StatusCode::GATEWAY_TIMEOUT,
        code: "upstream_timeout",
        message: "Запрос не успел завершиться за 300 секунд".to_string(),
        hint: Some("Попробуйте более короткий запрос".to_string()),
    })?;
    drop(permit);
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
        "auth_required": state.cfg.app_token.is_some(),
        "uptime_secs": state.started_at.elapsed().as_secs(),
        "budget": {
            "daily_usd": state.cfg.daily_budget_usd,
            "spent_today_usd": spent
        }
    }))
}

async fn stats(State(state): State<AppState>, headers: HeaderMap) -> Result<Json<Value>, ApiError> {
    check_auth(&state, &headers)?;
    Ok(Json(state.store.stats_today().await))
}

async fn run_by_id(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    check_auth(&state, &headers)?;
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

/// Marks the served SPA files as revalidated: the browser must not reuse an
/// old `app.js` blindly. API responses keep their own headers.
async fn static_no_cache(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let is_api = request.uri().path().starts_with("/api/");
    let mut response = next.run(request).await;
    if !is_api {
        response.headers_mut().insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("no-cache, must-revalidate"),
        );
    }
    response
}

fn check_auth(state: &AppState, headers: &HeaderMap) -> Result<(), ApiError> {
    let Some(expected) = state.cfg.app_token.as_deref() else {
        return Ok(());
    };
    let header_token = headers
        .get("x-app-token")
        .and_then(|value| value.to_str().ok())
        .or_else(|| {
            headers
                .get(axum::http::header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.strip_prefix("Bearer "))
        });
    if header_token == Some(expected) {
        Ok(())
    } else {
        Err(ApiError::unauthorized(
            "Неверный или отсутствующий APP_TOKEN",
        ))
    }
}

fn validate_overrides(request: &PlanRequest) -> Result<RunOverrides, ApiError> {
    let start = request.start_date;
    let end = request.end_date;
    if start.is_some() != end.is_some() {
        return Err(ApiError::bad_request(
            "start_date и end_date нужно передавать вместе",
        ));
    }
    if let (Some(start), Some(end)) = (start, end) {
        if end < start {
            return Err(ApiError::bad_request("end_date раньше start_date"));
        }
        let days = (end - start).num_days() + 1;
        if !(1..=i64::from(MAX_TRIP_DAYS)).contains(&days) {
            return Err(ApiError::bad_request(format!(
                "план поддерживает от 1 до {MAX_TRIP_DAYS} дней"
            )));
        }
    }
    Ok(RunOverrides {
        start_date: start,
        end_date: end,
        transport: request.transport,
    })
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
