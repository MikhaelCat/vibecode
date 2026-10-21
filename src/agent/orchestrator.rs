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

#[derive(Debug, Clone)]
pub struct StreamEvent {
    pub event: &'static str,
    pub data: Value,
}

#[derive(Debug, Clone, Default)]
pub struct RunOverrides {
    pub start_date: Option<NaiveDate>,
    pub end_date: Option<NaiveDate>,
    pub transport: Option<TransportMode>,
}

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

pub struct Orchestrator {
    cfg: Arc<Config>,
    llm: Arc<dyn LlmClient>,
    registry: Arc<Registry>,
    store: Store,
    prompts: PromptSet,
}

impl Orchestrator {
    pub fn new(
        cfg: Arc<Config>,
        llm: Arc<dyn LlmClient>,
        registry: Arc<Registry>,
        store: Store,
    ) -> Self {
        Self::new_with_prompt(cfg, llm, registry, store, None)
    }

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

    pub fn model(&self) -> &str {
        self.llm.model()
    }

    pub fn prompt_version(&self) -> &str {
        &self.prompts.version
    }

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

    pub async fn execute(
        &self,
        query: String,
        sink: Option<mpsc::Sender<StreamEvent>>,
    ) -> RunOutcome {
        self.execute_with(query, sink, RunOverrides::default())
            .await
    }

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
        let mut usage = TokenUsage::default();
        let mut tools_used: Vec<String> = Vec::new();
        let mut repairs = 0usize;
        let mut pending_submit: Option<(Plan, ValidationReport)> = None;
        let mut submit_problem: Option<String> = None;
        let mut last_error: Option<String> = None;

        'agent: for step in 0..self.cfg.max_steps.max(1) {
            let llm_started = Instant::now();
            let response = match self
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
                messages.push(LlmMessage::system(
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
                let futures = regular_calls.into_iter().map(|(call_id, name, args)| {
                    let registry = registry.clone();
                    let intent = intent.clone();
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
                            let content = truncate(
                                &serde_json::to_string(&output.data).unwrap_or_default(),
                                24_000,
                            );
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

            if let Some((plan, report)) = pending_submit.take() {
                let generation = if repairs > 0 {
                    Generation::Repaired
                } else {
                    Generation::Llm
                };
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
                    messages.push(LlmMessage::system(problem));
                    continue;
                }
                last_error = Some("plan validation failed after repairs".to_string());
                break 'agent;
            }

            if facts.enough_for_plan() {
                messages.push(LlmMessage::system(
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
