//! Agent loop and the only path from a user query to a plan.
//!
//! Flow: deterministic `guard` → prompt assembly → LLM tool loop → tool
//! registry → `Facts::ingest` → `submit_plan` → `enrich` → `validate`. Hard
//! validation failures are fed back to the model up to `max_repairs`; when the
//! loop ends without a valid plan, `fallback::assemble` produces a degraded
//! one. Every tool result is ingested into `Facts` before submission, so the
//! validator's provenance checks (V10) cover all model output. Runs are
//! persisted to the store and mirrored as SSE events when a sink is present.

use std::sync::Arc;
use std::time::Instant;

use chrono::{NaiveDate, Utc};
use futures::future::join_all;
use serde_json::{json, Value};
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::agent::guard::{self, GuardDecision};
use crate::agent::prompt::PromptSet;
use crate::config::Config;
use crate::domain::dates::today_vladivostok;
use crate::domain::facts::Facts;
use crate::domain::fallback;
use crate::domain::intent::{Intent, Refusal, TransportMode, MAX_TRIP_DAYS};
use crate::domain::plan::{enrich, Generation, Plan, PlanDraft};
use crate::domain::trace::{Outcome, RunTrace, TokenUsage, ToolCallTrace, TraceStep};
use crate::domain::validate::{validate, ValidationReport};
use crate::llm::{ChatRequest, LlmClient, LlmMessage};
use crate::store::{RunRecord, Store};
use crate::tools::Registry;

/// One event emitted during a run: an SSE event name plus a JSON payload.
#[derive(Debug, Clone)]
pub struct StreamEvent {
    pub event: &'static str,
    pub data: Value,
}

/// Explicit form/API values that override the intent parsed from the query.
///
/// Overrides are applied once after the guard and are authoritative for the
/// rest of the run; the trip is clamped to `MAX_TRIP_DAYS`.
#[derive(Debug, Clone, Default)]
pub struct RunOverrides {
    pub start_date: Option<NaiveDate>,
    pub end_date: Option<NaiveDate>,
    pub transport: Option<TransportMode>,
}

/// Result of a finished run.
///
/// A refusal has `refusal` set and no plan; a normal or degraded run has
/// `intent`, `plan` and `report` set. `trace` records per-step timings and
/// tool calls, and `tools_used` excludes validator-only pseudo-calls.
#[derive(Debug, Clone)]
pub struct RunOutcome {
    pub run_id: Uuid,
    pub intent: Option<Intent>,
    pub plan: Option<Plan>,
    pub report: Option<ValidationReport>,
    pub refusal: Option<Refusal>,
    pub usage: TokenUsage,
    pub latency_ms: u64,
    pub tools_used: Vec<String>,
    pub trace: RunTrace,
    pub model: String,
}

/// Executes the guard/LLM/validate pipeline with shared config, LLM client,
/// tool registry, run store and prompts.
pub struct Orchestrator {
    cfg: Arc<Config>,
    llm: Arc<dyn LlmClient>,
    registry: Arc<Registry>,
    store: Store,
    prompts: PromptSet,
}

impl Orchestrator {
    /// Builds an orchestrator with prompts loaded from `cfg.prompts_dir`.
    pub fn new(
        cfg: Arc<Config>,
        llm: Arc<dyn LlmClient>,
        registry: Arc<Registry>,
        store: Store,
    ) -> Self {
        Self::new_with_prompt(cfg, llm, registry, store, None)
    }

    /// Like `new`, but loads the system prompt from an explicit file (evals).
    pub fn new_with_prompt(
        cfg: Arc<Config>,
        llm: Arc<dyn LlmClient>,
        registry: Arc<Registry>,
        store: Store,
        prompt_path: Option<std::path::PathBuf>,
    ) -> Self {
        let prompts = match prompt_path {
            Some(path) => PromptSet::load_file(&path),
            None => PromptSet::load(&cfg.prompts_dir),
        };
        Self {
            cfg,
            llm,
            registry,
            store,
            prompts,
        }
    }

    /// Model identifier reported by the LLM client.
    pub fn model(&self) -> &str {
        self.llm.model()
    }

    /// Prompt version derived from the loaded system prompt file stem.
    pub fn prompt_version(&self) -> &str {
        &self.prompts.version
    }

    /// Checks daily spend/token budgets before a live LLM run. Always true
    /// when no real LLM is configured, so mock/demo runs are never blocked.
    pub async fn budget_ok(&self) -> bool {
        if !self.cfg.has_real_llm() {
            return true;
        }
        if self.cfg.daily_budget_usd > 0.0
            && self.store.spent_today().await >= self.cfg.daily_budget_usd
        {
            return false;
        }
        if self.cfg.daily_token_budget > 0
            && self.store.spent_tokens_today().await >= self.cfg.daily_token_budget
        {
            return false;
        }
        true
    }

    /// Runs a query without form overrides.
    pub async fn execute(
        &self,
        query: String,
        sink: Option<mpsc::Sender<StreamEvent>>,
    ) -> RunOutcome {
        self.execute_with(query, sink, RunOverrides::default())
            .await
    }

    /// Runs the full pipeline, streaming events to `sink` when supplied.
    ///
    /// Returns exactly one outcome: a refusal from the guard, a validated
    /// plan, or a degraded plan assembled from `Facts` after the loop ends.
    pub async fn execute_with(
        &self,
        query: String,
        sink: Option<mpsc::Sender<StreamEvent>>,
        overrides: RunOverrides,
    ) -> RunOutcome {
        let started = Instant::now();
        let run_id = Uuid::new_v4();
        let mut trace = RunTrace::new(
            run_id,
            query.clone(),
            self.prompts.version.clone(),
            self.llm.model().to_string(),
        );
        let today = today_vladivostok();

        self.emit(
            &sink,
            "stage",
            json!({"stage": "guard", "status": "running"}),
        )
        .await;

        let intent = match guard::check(&query, today, self.cfg.max_query_chars) {
            GuardDecision::Refuse(refusal) => {
                trace.outcome = Outcome::Refusal;
                trace.latency_ms = started.elapsed().as_millis() as u64;
                self.emit(
                    &sink,
                    "refusal",
                    json!({"category": refusal.category, "message": refusal.message}),
                )
                .await;
                self.emit(
                    &sink,
                    "done",
                    json!({
                        "run_id": run_id,
                        "latency_ms": trace.latency_ms,
                        "outcome": "refusal"
                    }),
                )
                .await;
                self.persist(
                    run_id,
                    &query,
                    "refusal",
                    None,
                    TokenUsage::default(),
                    0.0,
                    &trace,
                    trace.latency_ms,
                )
                .await;
                return RunOutcome {
                    run_id,
                    intent: None,
                    plan: None,
                    report: None,
                    refusal: Some(refusal),
                    usage: TokenUsage::default(),
                    latency_ms: trace.latency_ms,
                    tools_used: Vec::new(),
                    trace,
                    model: self.llm.model().to_string(),
                };
            }
            GuardDecision::Allow(intent) => apply_overrides(intent, &overrides),
        };

        self.emit(&sink, "stage", json!({"stage": "intent", "status": "ok"}))
            .await;
        self.emit(
            &sink,
            "intent",
            serde_json::to_value(&intent).unwrap_or(Value::Null),
        )
        .await;

        let mut messages = vec![
            LlmMessage::system(self.prompts.render_system(today)),
            LlmMessage::system(format!(
                "INTENT_JSON: {}",
                serde_json::to_string(&intent).unwrap_or_else(|_| "{}".to_string())
            )),
            LlmMessage::system(plan_window(&intent)),
            LlmMessage::user(query.clone()),
        ];

        let mut facts = Facts::default();
        // The poster is worth loading even if the model forgets to call the
        // tool: events are attached to the plan separately from places.
        for area in event_areas_for_query(&query) {
            let events_args = json!({
                "area": area,
                "start_date": intent.start_date.to_string(),
                "end_date": intent.end_date.to_string()
            });
            if let Ok(Ok(output)) = tokio::time::timeout(
                std::time::Duration::from_secs(6),
                self.registry.call("find_events", events_args, &intent),
            )
            .await
            {
                facts.ingest("find_events", &output);
            }
        }
        let mut usage = TokenUsage::default();
        let mut tools_used: Vec<String> = Vec::new();
        let mut repairs = 0usize;
        let mut pending_submit: Option<(Plan, ValidationReport)> = None;
        let mut submit_problem: Option<String> = None;
        let mut last_error: Option<String> = None;
        let mut search_calls = 0usize;

        'agent: for step in 0..self.cfg.max_steps.max(1) {
            let llm_started = Instant::now();
            let mut response = match self
                .llm
                .chat(ChatRequest {
                    messages: messages.clone(),
                    tools: self.registry.specs(),
                })
                .await
            {
                Ok(response) => response,
                Err(error) => {
                    tracing::warn!(run_id = %run_id, %error, "LLM call failed");
                    last_error = Some(error.to_string());
                    trace.error = Some(error.to_string());
                    break 'agent;
                }
            };
            // Some OpenAI-compatible providers omit tool-call ids; the next
            // request cannot reference such a call.
            for call in &mut response.tool_calls {
                if call.id.trim().is_empty() {
                    call.id = uuid::Uuid::new_v4().to_string();
                }
            }
            usage.add(response.usage);

            let tool_names: Vec<String> = response
                .tool_calls
                .iter()
                .map(|call| call.function.name.clone())
                .collect();
            trace.steps.push(TraceStep {
                step,
                llm_ms: llm_started.elapsed().as_millis() as u64,
                tool_names,
            });

            if matches!(
                response.finish_reason.as_deref(),
                Some("length" | "content_filter")
            ) {
                let reason = format!(
                    "LLM response stopped early: {}",
                    response.finish_reason.as_deref().unwrap_or("unknown")
                );
                tracing::warn!(run_id = %run_id, %reason, "LLM response truncated");
                last_error = Some(reason.clone());
                trace.error = Some(reason);
                break 'agent;
            }

            if response.tool_calls.is_empty() {
                if let Some(content) = response.content {
                    messages.push(LlmMessage::assistant(content));
                }
                messages.push(LlmMessage::user(
                    "Ты не вызвал инструмент. Вызови нужный тул или submit_plan, если данных достаточно.",
                ));
                continue;
            }

            messages.push(LlmMessage::assistant_tool_calls(
                response.tool_calls.clone(),
            ));

            // Parse and announce all calls first, then execute independent
            // tools concurrently. `submit_plan` stays last and sees all facts.
            let mut regular_calls = Vec::new();
            let mut submit_calls = Vec::new();
            for call in &response.tool_calls {
                let name = call.function.name.clone();
                let args = match serde_json::from_str::<Value>(&call.function.arguments) {
                    Ok(args) => args,
                    Err(error) => {
                        let content =
                            json!({"error": format!("bad tool arguments: {error}")}).to_string();
                        messages.push(LlmMessage::tool(call.id.clone(), content));
                        trace.tool_calls.push(ToolCallTrace {
                            id: call.id.clone(),
                            name: name.clone(),
                            args_summary: call.function.arguments.chars().take(200).collect(),
                            ms: 0,
                            ok: false,
                            cached: false,
                            source: "parse_error".to_string(),
                            summary: error.to_string(),
                        });
                        continue;
                    }
                };

                // Bound search tool usage per run. A multi-city plan needs
                // more than a couple of category searches, but still finite.
                if matches!(name.as_str(), "search_places" | "get_nearby_attractions") {
                    if search_calls >= 12 {
                        let content = json!({
                            "error": "search budget exhausted; use existing facts and call submit_plan"
                        })
                        .to_string();
                        messages.push(LlmMessage::tool(call.id.clone(), content));
                        trace.tool_calls.push(ToolCallTrace {
                            id: call.id.clone(),
                            name: name.clone(),
                            args_summary: truncate(&args.to_string(), 200),
                            ms: 0,
                            ok: false,
                            cached: false,
                            source: "budget".to_string(),
                            summary: "search budget exhausted".to_string(),
                        });
                        continue;
                    }
                    search_calls += 1;
                }

                self.emit(
                    &sink,
                    "tool_call",
                    json!({"id": call.id, "name": name, "args": args}),
                )
                .await;

                if name == "submit_plan" {
                    submit_calls.push((call.id.clone(), name, args));
                } else {
                    regular_calls.push((call.id.clone(), name, args));
                }
            }

            if !regular_calls.is_empty() {
                let registry = self.registry.clone();
                let intent = intent.clone();
                let futures = regular_calls.into_iter().map(|(call_id, name, mut args)| {
                    let registry = registry.clone();
                    let intent = intent.clone();
                    enforce_search_intent(&name, &mut args, &intent);
                    let args_summary = truncate(&args.to_string(), 200);
                    async move {
                        let started = Instant::now();
                        let result = registry.call(&name, args, &intent).await;
                        (
                            call_id,
                            name,
                            args_summary,
                            result,
                            started.elapsed().as_millis() as u64,
                        )
                    }
                });

                for (call_id, name, args_summary, result, ms) in join_all(futures).await {
                    match result {
                        Ok(output) => {
                            facts.ingest(&name, &output);
                            tools_used.push(name.clone());
                            let compact = compact_tool_data(&name, &output.data);
                            let content = json_for_model(&compact, 12_000);
                            trace.tool_calls.push(ToolCallTrace {
                                id: call_id.clone(),
                                name: name.clone(),
                                args_summary,
                                ms,
                                ok: output.ok,
                                cached: output.meta.cached,
                                source: output.meta.source.as_str().to_string(),
                                summary: output.summary.clone(),
                            });
                            self.emit(
                                &sink,
                                "tool_result",
                                json!({
                                    "id": call_id,
                                    "name": name,
                                    "ok": output.ok,
                                    "ms": ms,
                                    "cached": output.meta.cached,
                                    "source": output.meta.source.as_str(),
                                    "summary": output.summary
                                }),
                            )
                            .await;
                            messages.push(LlmMessage::tool(call_id, content));
                        }
                        Err(error) => {
                            let content = json!({"error": error.to_string()}).to_string();
                            trace.tool_calls.push(ToolCallTrace {
                                id: call_id.clone(),
                                name: name.clone(),
                                args_summary,
                                ms,
                                ok: false,
                                cached: false,
                                source: "error".to_string(),
                                summary: error.to_string(),
                            });
                            self.emit(
                                &sink,
                                "tool_result",
                                json!({
                                    "id": call_id,
                                    "name": name,
                                    "ok": false,
                                    "ms": ms,
                                    "error": error.to_string()
                                }),
                            )
                            .await;
                            messages.push(LlmMessage::tool(call_id, content));
                        }
                    }
                }
            }

            for (call_id, name, args) in submit_calls {
                let submit_started = Instant::now();
                let (content, ok) = self
                    .handle_submit(
                        &args,
                        &intent,
                        &facts,
                        &mut pending_submit,
                        &mut submit_problem,
                    )
                    .await;
                trace.tool_calls.push(ToolCallTrace {
                    id: call_id.clone(),
                    name: name.clone(),
                    args_summary: truncate(&args.to_string(), 200),
                    ms: submit_started.elapsed().as_millis() as u64,
                    ok,
                    cached: false,
                    source: "validator".to_string(),
                    summary: content.chars().take(200).collect(),
                });
                messages.push(LlmMessage::tool(call_id, content));
            }

            if let Some((mut plan, report)) = pending_submit.take() {
                let generation = if repairs > 0 {
                    Generation::Repaired
                } else {
                    Generation::Llm
                };
                self.ensure_plan_events(&mut plan, &mut facts, &intent)
                    .await;
                return self
                    .finish_plan(
                        run_id, query, intent, plan, report, usage, trace, started, sink,
                        generation,
                    )
                    .await;
            }

            if let Some(problem) = submit_problem.take() {
                if repairs < self.cfg.max_repairs {
                    repairs += 1;
                    messages.push(LlmMessage::user(problem));
                    continue;
                }
                last_error = Some("plan validation failed after repairs".to_string());
                break 'agent;
            }

            if facts.enough_for_plan() {
                messages.push(LlmMessage::user(
                    "Данных достаточно. Вызови submit_plan с планом, используя только place_id из результатов инструментов.",
                ));
            }
        }

        self.emit(
            &sink,
            "stage",
            json!({"stage": "fallback", "status": last_error.is_some()}),
        )
        .await;

        let mut plan = fallback::assemble(&mut facts, &intent);
        let report = validate(&plan, &facts, &intent);
        plan.generation = Generation::Fallback;
        self.ensure_plan_events(&mut plan, &mut facts, &intent)
            .await;
        self.finish_plan(
            run_id,
            query,
            intent,
            plan,
            report,
            usage,
            trace,
            started,
            sink,
            Generation::Fallback,
        )
        .await
    }

    /// Loads the poster for every city the finished plan actually visits, so
    /// a Ussuriysk/Artyom/Fokino day is not left without events just because
    /// the model forgot to call find_events for that city.
    async fn ensure_plan_events(&self, plan: &mut Plan, facts: &mut Facts, intent: &Intent) {
        let mut areas: Vec<&'static str> = Vec::new();
        for day in &plan.days {
            for item in &day.items {
                if let Some(city) = crate::domain::plan::city_for_point(item.lat, item.lon) {
                    if !areas.contains(&city) {
                        areas.push(city);
                    }
                }
            }
        }
        for area in areas {
            let already = facts.events.iter().any(|event| {
                event.area.to_lowercase().contains(&area.to_lowercase())
                    || area.to_lowercase().contains(&event.area.to_lowercase())
            });
            if already {
                continue;
            }
            let args = json!({
                "area": area,
                "start_date": intent.start_date.to_string(),
                "end_date": intent.end_date.to_string()
            });
            if let Ok(Ok(output)) = tokio::time::timeout(
                std::time::Duration::from_secs(6),
                self.registry.call("find_events", args, intent),
            )
            .await
            {
                facts.ingest("find_events", &output);
            }
        }
        for day in &mut plan.days {
            day.events = crate::domain::plan::plan_events_for_day(day.date, &day.items, facts);
        }
    }

    /// Parses and enriches a `submit_plan` call, then validates it.
    ///
    /// The plan is accepted only when validation has no hard violations;
    /// otherwise the returned text is the repair feedback for the model.
    async fn handle_submit(
        &self,
        args: &Value,
        intent: &Intent,
        facts: &Facts,
        pending_submit: &mut Option<(Plan, ValidationReport)>,
        submit_problem: &mut Option<String>,
    ) -> (String, bool) {
        let draft = match serde_json::from_value::<PlanDraft>(args.clone()) {
            Ok(draft) => draft,
            Err(error) => {
                let message = format!("Не удалось разобрать план: {error}");
                *submit_problem = Some(message.clone());
                return (message, false);
            }
        };

        let plan = enrich(draft, intent, facts);
        let report = validate(&plan, facts, intent);
        if report.hard_ok() {
            *pending_submit = Some((plan, report));
            ("План принят.".to_string(), true)
        } else {
            let feedback = report.feedback();
            *submit_problem = Some(feedback.clone());
            (feedback, false)
        }
    }

    /// Emits terminal events, persists the run and builds the `RunOutcome`.
    #[allow(clippy::too_many_arguments)]
    async fn finish_plan(
        &self,
        run_id: Uuid,
        query: String,
        intent: Intent,
        mut plan: Plan,
        report: ValidationReport,
        usage: TokenUsage,
        mut trace: RunTrace,
        started: Instant,
        sink: Option<mpsc::Sender<StreamEvent>>,
        generation: Generation,
    ) -> RunOutcome {
        plan.generation = generation;
        trace.validation = Some(report.clone());
        trace.outcome = if generation == Generation::Fallback {
            Outcome::Degraded
        } else {
            Outcome::Plan
        };
        trace.latency_ms = started.elapsed().as_millis() as u64;

        self.emit(
            &sink,
            "plan",
            serde_json::to_value(&plan).unwrap_or(Value::Null),
        )
        .await;
        self.emit(
            &sink,
            "validation",
            serde_json::to_value(&report).unwrap_or(Value::Null),
        )
        .await;
        self.emit(
            &sink,
            "usage",
            serde_json::to_value(usage).unwrap_or(Value::Null),
        )
        .await;
        self.emit(
            &sink,
            "done",
            json!({
                "run_id": run_id,
                "latency_ms": trace.latency_ms,
                "outcome": if generation == Generation::Fallback { "degraded" } else { "plan" },
                "generation": generation,
                "usage": usage
            }),
        )
        .await;

        let cost = if usage.cost_usd > 0.0 {
            usage.cost_usd
        } else {
            self.cfg
                .estimate_cost(usage.prompt_tokens, usage.completion_tokens)
        };
        self.persist(
            run_id,
            &query,
            if generation == Generation::Fallback {
                "degraded"
            } else {
                "plan"
            },
            Some(&plan),
            usage,
            cost,
            &trace,
            trace.latency_ms,
        )
        .await;

        RunOutcome {
            run_id,
            intent: Some(intent),
            plan: Some(plan),
            report: Some(report),
            refusal: None,
            usage,
            latency_ms: trace.latency_ms,
            tools_used: trace
                .tool_calls
                .iter()
                .filter(|call| call.source != "validator")
                .map(|call| call.name.clone())
                .collect(),
            trace,
            model: self.llm.model().to_string(),
        }
    }

    /// Stores a run record. Usage and cost are zeroed for mock LLM runs so
    /// they do not consume the daily budget.
    #[allow(clippy::too_many_arguments)]
    async fn persist(
        &self,
        run_id: Uuid,
        query: &str,
        status: &str,
        plan: Option<&Plan>,
        usage: TokenUsage,
        cost: f64,
        trace: &RunTrace,
        latency_ms: u64,
    ) {
        let stored_usage = if self.cfg.has_real_llm() {
            usage
        } else {
            TokenUsage::default()
        };
        let record = RunRecord {
            id: run_id,
            created_at: Utc::now(),
            query: query.to_string(),
            status: status.to_string(),
            latency_ms,
            usage: stored_usage,
            cost_usd: if self.cfg.has_real_llm() { cost } else { 0.0 },
            plan_json: plan.and_then(|plan| serde_json::to_string(plan).ok()),
            trace_json: serde_json::to_string(trace).ok(),
        };
        if let Err(error) = self.store.record_run(&record).await {
            tracing::warn!(%error, "failed to persist run");
        }
    }

    /// Sends a stream event if the caller is streaming; a closed receiver is
    /// ignored because the run still needs to finish and persist.
    async fn emit(
        &self,
        sink: &Option<mpsc::Sender<StreamEvent>>,
        event: &'static str,
        data: Value,
    ) {
        if let Some(tx) = sink {
            let _ = tx.send(StreamEvent { event, data }).await;
        }
    }
}

/// Applies form/API overrides after guard parsing and clamps the trip to
/// `MAX_TRIP_DAYS`, recording any assumption shown to the user.
fn apply_overrides(mut intent: Intent, overrides: &RunOverrides) -> Intent {
    if let Some(start) = overrides.start_date {
        intent.start_date = start;
    }
    if let Some(end) = overrides.end_date {
        intent.end_date = end;
    }
    if let Some(transport) = overrides.transport {
        intent.transport = transport;
    }
    if overrides.start_date.is_some() || overrides.end_date.is_some() {
        let days = (intent.end_date - intent.start_date).num_days() + 1;
        if (1..=i64::from(MAX_TRIP_DAYS)).contains(&days) {
            intent.max_days = days as u8;
        } else if days > i64::from(MAX_TRIP_DAYS) {
            intent.end_date =
                intent.start_date + chrono::Duration::days(i64::from(MAX_TRIP_DAYS) - 1);
            intent.max_days = MAX_TRIP_DAYS;
            intent
                .assumptions
                .push(format!("период ограничен {MAX_TRIP_DAYS} днями"));
        }
    }
    if overrides.transport.is_some() {
        intent
            .assumptions
            .push("транспорт выбран в форме".to_string());
    }
    intent
}

/// Authoritative trip window. The form/API or the server-side query parser already
/// resolved relative dates, so the model must not invent its own period.
fn plan_window(intent: &Intent) -> String {
    let mut dates = Vec::new();
    let mut date = intent.start_date;
    while date <= intent.end_date {
        dates.push(date.to_string());
        date += chrono::Duration::days(1);
    }
    format!(
        "PLAN_WINDOW (авторитетно, уже вычислено сервером):\n\
         start_date: {start}\n\
         end_date: {end}\n\
         days_count: {count}\n\
         dates: {dates}\n\
         В submit_plan должно быть ровно {count} элементов days — строго эти даты, \
         по одному разу, в этом порядке. Не добавляй дни вне периода и не пропускай даты. \
         Каждый place_id используй в плане не более одного раза: дубликаты, один и тот же \
         объект под разными id и повторные события недопустимы. Данные инструментов могут \
         содержать повторы — отфильтруй их самостоятельно. Если find_events вернул события \
         на даты поездки, упомяни 1–2 уместных в summary или note.",
        start = intent.start_date,
        end = intent.end_date,
        count = dates.len(),
        dates = dates.join(", ")
    )
}

fn truncate(value: &str, max: usize) -> String {
    value.chars().take(max).collect()
}

fn event_areas_for_query(query: &str) -> Vec<&'static str> {
    let query = query.to_lowercase();
    let mut areas = Vec::new();
    if query.contains("владивост") || query.contains("русск") {
        areas.push("Владивосток");
    }
    if query.contains("находк") {
        areas.push("Находка");
    }
    if query.contains("арсеньев") {
        areas.push("Арсеньев");
    }
    if query.contains("уссурийск") || query.contains("ussuriisk") {
        areas.push("Уссурийск");
    }
    if query.contains("больш") && query.contains("камен") {
        areas.push("Большой Камень");
    }
    if query.contains("артем") || query.contains("артём") || query.contains("artem") {
        areas.push("Артём");
    }
    if query.contains("спасск") {
        areas.push("Спасск");
    }
    if query.contains("партизанск") {
        areas.push("Партизанск");
    }
    if areas.is_empty() {
        if query.contains("примор") {
            areas.extend(["Владивосток", "Находка", "Уссурийск"]);
        } else {
            // A query without a city still starts in Vladivostok in the
            // default plan; load its poster so the day has a full event stack.
            areas.push("Владивосток");
        }
    }
    areas
}

/// Makes `search_places` follow the parsed request even when the model picks
/// generic categories or forgets a named venue. This is what turns "бары",
/// "пицца" or "докерпаб" into a targeted Yandex/OSM category search.
fn enforce_search_intent(name: &str, args: &mut Value, intent: &Intent) {
    if name != "search_places" {
        return;
    }
    let Some(object) = args.as_object_mut() else {
        return;
    };
    let wanted = search_categories(intent);
    if !wanted.is_empty() {
        let current: Vec<String> = object
            .get("categories")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        let already_requested = current.iter().any(|category| {
            wanted
                .iter()
                .any(|needle| category == needle || category.contains(needle))
        });
        if current.is_empty() || !already_requested {
            object.insert("categories".to_string(), json!(wanted));
        }
    }
    let query_is_empty = object
        .get("query")
        .and_then(Value::as_str)
        .map(|query| query.trim().is_empty())
        .unwrap_or(true);
    if query_is_empty && !intent.must_see.is_empty() {
        object.insert("query".to_string(), json!(intent.must_see.join(" ")));
    }
    if wanted.is_empty() && intent.must_see.is_empty() {
        return;
    }
    object.insert("limit".to_string(), json!(40));
}

fn search_categories(intent: &Intent) -> Vec<String> {
    let mut categories: Vec<String> = Vec::new();
    for interest in &intent.interests {
        let wanted: &[&str] = match interest.as_str() {
            "food" => &["restaurant", "cafe"],
            "nightlife" => &["bar", "club"],
            "nature" => &["nature", "park", "beach"],
            "sea" | "beach" => &["beach", "nature"],
            "museum" | "history" => &["museum", "historic"],
            "cinema" => &["cinema"],
            "theater" => &["theater"],
            "concert" => &["theatre", "arts_centre"],
            "market" => &["marketplace"],
            "clothes" => &["clothes"],
            "shopping" => &["clothes", "mall", "department_store", "shoes"],
            "mountains" => &["nature", "viewpoint"],
            "fishing" => &["beach", "nature", "viewpoint"],
            _ => &[],
        };
        categories.extend(wanted.iter().map(|category| (*category).to_string()));
    }
    categories.sort();
    categories.dedup();
    categories
}

/// Serialize a tool result for the model, shrinking arrays and strings until
/// it fits. Unlike a raw character cut this never leaves broken JSON in the
/// tool message.
fn json_for_model(value: &Value, max_chars: usize) -> String {
    let mut value = value.clone();
    loop {
        let text = serde_json::to_string(&value).unwrap_or_else(|_| "{}".to_string());
        if text.chars().count() <= max_chars {
            return text;
        }
        if !shrink_json(&mut value) {
            return json!({
                "truncated": true,
                "note": "tool result too large; rely on the summary and existing facts"
            })
            .to_string();
        }
    }
}

fn shrink_json(value: &mut Value) -> bool {
    match value {
        Value::Array(items) => {
            if items.len() > 1 {
                items.truncate(items.len().div_ceil(2));
                return true;
            }
            let mut changed = false;
            for item in items.iter_mut() {
                changed |= shrink_json(item);
            }
            changed
        }
        Value::Object(map) => {
            let mut changed = false;
            for item in map.values_mut() {
                changed |= shrink_json(item);
            }
            changed
        }
        Value::String(text) => {
            const MAX_TEXT: usize = 600;
            if text.chars().count() > MAX_TEXT {
                *text = text.chars().take(MAX_TEXT).collect::<String>() + "…";
                true
            } else {
                false
            }
        }
        _ => false,
    }
}

/// Keep tool results small for the LLM context: only the fields the model needs
/// to build a plan. Facts are ingested from the full output before this runs.
fn compact_tool_data(name: &str, data: &Value) -> Value {
    match name {
        "search_places" | "geocode" | "get_nearby_attractions" => project_array(
            data,
            "places",
            &["id", "name", "kind", "lat", "lon", "opening_hours"],
        ),
        "find_lodging" => project_array(
            data,
            "options",
            &["id", "name", "settlement", "price_per_night", "kind"],
        ),
        "find_events" => project_array(
            data,
            "events",
            &["id", "title", "date", "end_date", "place"],
        ),
        "get_weather" => project_array(
            data,
            "days",
            &[
                "date",
                "precip_prob",
                "wind_max",
                "temp_min",
                "temp_max",
                "sunrise",
                "sunset",
            ],
        ),
        "get_route" => project_fields(data, &["from", "to", "distance_km", "duration_min"]),
        "find_fuel" => project_array(data, "stations", &["id", "name", "lat", "lon"]),
        _ => data.clone(),
    }
}

fn project_array(data: &Value, key: &str, fields: &[&str]) -> Value {
    let Some(items) = data.get(key).and_then(Value::as_array) else {
        return data.clone();
    };
    let mut compact = data.clone();
    compact[key] = Value::Array(
        items
            .iter()
            .map(|item| project_fields(item, fields))
            .collect(),
    );
    compact
}

fn project_fields(item: &Value, fields: &[&str]) -> Value {
    let mut object = serde_json::Map::new();
    for field in fields {
        object.insert(
            (*field).to_string(),
            item.get(*field).cloned().unwrap_or(Value::Null),
        );
    }
    Value::Object(object)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oversized_tool_result_remains_valid_json() {
        let value = json!({
            "places": (0..500)
                .map(|index| json!({"id": index, "name": "x".repeat(120)}))
                .collect::<Vec<_>>()
        });
        let text = json_for_model(&value, 2_000);
        assert!(
            text.chars().count() <= 2_000,
            "{} chars",
            text.chars().count()
        );
        assert!(serde_json::from_str::<Value>(&text).is_ok());
    }
}
