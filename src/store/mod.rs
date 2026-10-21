use std::str::FromStr;

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::domain::dates::today_vladivostok;
use crate::domain::trace::TokenUsage;

#[derive(Debug, Clone)]
pub struct RunRecord {
    pub id: Uuid,
    pub created_at: DateTime<Utc>,
    pub query: String,
    pub status: String,
    pub latency_ms: u64,
    pub usage: TokenUsage,
    pub cost_usd: f64,
    pub plan_json: Option<String>,
    pub trace_json: Option<String>,
}

#[derive(Clone)]
pub struct Store {
    pool: SqlitePool,
}

impl Store {
    pub async fn connect(database_url: &str) -> Result<Self> {
        let options = SqliteConnectOptions::from_str(database_url)
            .map_err(|error| anyhow::anyhow!("invalid DATABASE_URL: {error}"))?
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal);

        let max_connections = if database_url.contains(":memory:") {
            1
        } else {
            5
        };
        let pool = SqlitePoolOptions::new()
            .max_connections(max_connections)
            .connect_with(options)
            .await?;

        sqlx::raw_sql(include_str!("../../migrations/0001_init.sql"))
            .execute(&pool)
            .await?;

        Ok(Self { pool })
    }

    pub async fn record_run(&self, record: &RunRecord) -> Result<()> {
        sqlx::query(
            "INSERT INTO runs (
                id, created_at, query, status, latency_ms,
                prompt_tokens, completion_tokens, cost_usd, plan_json, trace_json
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(record.id.to_string())
        .bind(record.created_at.to_rfc3339())
        .bind(&record.query)
        .bind(&record.status)
        .bind(record.latency_ms as i64)
        .bind(record.usage.prompt_tokens as i64)
        .bind(record.usage.completion_tokens as i64)
        .bind(record.cost_usd)
        .bind(&record.plan_json)
        .bind(&record.trace_json)
        .execute(&self.pool)
        .await?;

        let day = today_vladivostok().format("%Y-%m-%d").to_string();
        sqlx::query(
            "INSERT INTO daily_budget (day, prompt_tokens, completion_tokens, cost_usd, runs)
             VALUES (?, ?, ?, ?, 1)
             ON CONFLICT(day) DO UPDATE SET
                prompt_tokens = prompt_tokens + excluded.prompt_tokens,
                completion_tokens = completion_tokens + excluded.completion_tokens,
                cost_usd = cost_usd + excluded.cost_usd,
                runs = runs + 1",
        )
        .bind(day)
        .bind(record.usage.prompt_tokens as i64)
        .bind(record.usage.completion_tokens as i64)
        .bind(record.cost_usd)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    pub async fn spent_today(&self) -> f64 {
        let day = today_vladivostok().format("%Y-%m-%d").to_string();
        let row: Option<(f64,)> = sqlx::query_as(
            "SELECT COALESCE(SUM(cost_usd), 0) FROM runs WHERE substr(created_at, 1, 10) = ?",
        )
        .bind(day)
        .fetch_optional(&self.pool)
        .await
        .ok()
        .flatten();
        row.map(|(cost,)| cost).unwrap_or(0.0)
    }

    pub async fn stats_today(&self) -> Value {
        let day = today_vladivostok().format("%Y-%m-%d").to_string();
        let row: Option<(i64, i64, i64, f64)> = sqlx::query_as(
            "SELECT COUNT(*), COALESCE(SUM(prompt_tokens), 0),
                    COALESCE(SUM(completion_tokens), 0), COALESCE(SUM(cost_usd), 0)
             FROM runs WHERE substr(created_at, 1, 10) = ?",
        )
        .bind(&day)
        .fetch_optional(&self.pool)
        .await
        .ok()
        .flatten();

        let (runs, prompt_tokens, completion_tokens, cost_usd) =
            row.unwrap_or((0, 0, 0, 0.0));
        json!({
            "date": day,
            "runs": runs,
            "prompt_tokens": prompt_tokens,
            "completion_tokens": completion_tokens,
            "cost_usd": cost_usd
        })
    }

    pub async fn get_run(&self, id: &str) -> Option<Value> {
        let row: Option<(
            String,
            String,
            String,
            String,
            i64,
            i64,
            i64,
            f64,
            Option<String>,
            Option<String>,
        )> = sqlx::query_as(
            "SELECT id, created_at, query, status, latency_ms,
                    prompt_tokens, completion_tokens, cost_usd, plan_json, trace_json
             FROM runs WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .ok()
        .flatten();

        row.map(
            |(
                id,
                created_at,
                query,
                status,
                latency_ms,
                prompt_tokens,
                completion_tokens,
                cost_usd,
                plan_json,
                trace_json,
            )| {
                json!({
                    "id": id,
                    "created_at": created_at,
                    "query": query,
                    "status": status,
                    "latency_ms": latency_ms,
                    "prompt_tokens": prompt_tokens,
                    "completion_tokens": completion_tokens,
                    "cost_usd": cost_usd,
                    "plan": plan_json.and_then(|value| serde_json::from_str::<Value>(&value).ok()),
                    "trace": trace_json.and_then(|value| serde_json::from_str::<Value>(&value).ok())
                })
            },
        )
    }
}
