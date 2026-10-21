use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Parser;
use serde::{Deserialize, Serialize};
use serde_json::json;

use travel_ai::agent::Orchestrator;
use travel_ai::config::Config;
use travel_ai::domain::intent::TransportMode;
use travel_ai::domain::plan::Generation;
use travel_ai::llm::MockLlm;
use travel_ai::store::Store;
use travel_ai::tools::Registry;

#[derive(Debug, Parser)]
#[command(about = "Run the fixed eval dataset against one pipeline configuration")]
struct Args {
    #[arg(long)]
    config: PathBuf,
    #[arg(long)]
    dataset: PathBuf,
    #[arg(long)]
    out: Option<PathBuf>,
    #[arg(long)]
    markdown: Option<PathBuf>,
    #[arg(long)]
    limit: Option<usize>,
    #[arg(long, default_value_t = false)]
    live: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)]
struct EvalConfig {
    name: String,
    prompt: String,
    #[serde(default)]
    model: Option<String>,
    max_steps: usize,
    #[serde(default)]
    compression: String,
    #[serde(default)]
    parallel_tools: bool,
    #[serde(default)]
    repair_attempts: usize,
    #[serde(default)]
    fallback: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct EvalCase {
    id: String,
    query: String,
    #[serde(default)]
    expect: Expect,
    #[serde(default)]
    tools: Vec<String>,
    #[serde(default)]
    tags: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
struct Expect {
    #[serde(default)]
    in_scope: bool,
    #[serde(default)]
    transport: Option<TransportMode>,
    #[serde(default)]
    max_days: Option<u8>,
    #[serde(default)]
    budget_rub: Option<u32>,
}

#[derive(Debug, Serialize)]
struct CaseResult {
    id: String,
    query: String,
    in_scope_expected: bool,
    in_scope_actual: bool,
    hard_pass: Option<bool>,
    tool_choice: bool,
    intent_fit: bool,
    refusal_ok: bool,
    fallback: bool,
    tokens: u32,
    latency_ms: u64,
    tools_used: Vec<String>,
    tags: Vec<String>,
    error: Option<String>,
}

#[derive(Debug, Serialize)]
struct Summary {
    config: String,
    cases: usize,
    in_scope_cases: usize,
    negative_cases: usize,
    hard_pass: usize,
    hard_pass_rate: f64,
    tool_choice: usize,
    tool_choice_rate: f64,
    intent_fit: usize,
    intent_fit_rate: f64,
    refusal_ok: usize,
    refusal_rate: f64,
    fallback: usize,
    total_tokens: u64,
    avg_tokens: f64,
    p50_latency_ms: u64,
    p95_latency_ms: u64,
    errors: usize,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let config_text = std::fs::read_to_string(&args.config)
        .with_context(|| format!("cannot read {}", args.config.display()))?;
    let eval_config: EvalConfig = toml::from_str(&config_text)
        .with_context(|| format!("cannot parse {}", args.config.display()))?;

    let mut cases = read_cases(&args.dataset)?;
    if let Some(limit) = args.limit {
        cases.truncate(limit);
    }
    if cases.is_empty() {
        anyhow::bail!("dataset is empty");
    }

    let mut cfg = Config::from_env()?;
    cfg.max_steps = eval_config.max_steps;
    cfg.max_repairs = eval_config.repair_attempts;
    cfg.mock_llm = !args.live;
    cfg.database_url = "sqlite::memory:".to_string();
    if args.live {
        if let Some(model) = &eval_config.model {
            cfg.model = model.clone();
        }
    }
    let cfg = Arc::new(cfg);

    let store = Store::connect(&cfg.database_url).await?;
    let registry = Registry::new(&cfg)?;
    if args.live && !cfg.has_real_llm() {
        anyhow::bail!("--live requires LLM_PROXY_TOKEN or OPENROUTER_API_KEY");
    }
    let llm = if args.live {
        travel_ai::build_llm(&cfg)?
    } else {
        eprintln!("running with MockLlm; pass --live to spend LLM tokens");
        Arc::new(MockLlm::new())
    };

    let orchestrator = Orchestrator::new_with_prompt(
        cfg.clone(),
        llm,
        Arc::new(registry),
        store,
        Some(PathBuf::from(&eval_config.prompt)),
    );

    let mut results = Vec::new();
    for case in &cases {
        let outcome = orchestrator.run(case.query.clone(), None).await;
        let in_scope_actual = outcome.refusal.is_none();

        let hard_pass = outcome.report.as_ref().map(|report| !report.hard_failed);
        let tool_choice = case.tools.is_empty()
            || outcome
                .tools_used
                .iter()
                .any(|tool| case.tools.contains(tool));
        let intent_fit = intent_fit(case, &outcome);
        let refusal_ok = !case.expect.in_scope && outcome.refusal.is_some();
        let fallback = outcome
            .plan
            .as_ref()
            .map(|plan| plan.generation == Generation::Fallback)
            .unwrap_or(false);
        let error = if case.expect.in_scope && outcome.refusal.is_some() {
            Some("unexpected refusal".to_string())
        } else if !case.expect.in_scope && outcome.refusal.is_none() {
            Some("expected refusal".to_string())
        } else {
            None
        };

        results.push(CaseResult {
            id: case.id.clone(),
            query: case.query.clone(),
            in_scope_expected: case.expect.in_scope,
            in_scope_actual,
            hard_pass,
            tool_choice,
            intent_fit,
            refusal_ok,
            fallback,
            tokens: outcome.usage.total_tokens,
            latency_ms: outcome.latency_ms,
            tools_used: outcome.tools_used.clone(),
            tags: case.tags.clone(),
            error,
        });
    }

    let summary = summarize(&eval_config.name, &results);
    let payload = json!({
        "config": &eval_config,
        "summary": &summary,
        "cases": &results
    });

    let out = args
        .out
        .unwrap_or_else(|| PathBuf::from(format!("results/{}.json", eval_config.name)));
    let markdown = args
        .markdown
        .unwrap_or_else(|| PathBuf::from(format!("results/{}.md", eval_config.name)));
    if let Some(parent) = out.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    if let Some(parent) = markdown.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }

    std::fs::write(&out, serde_json::to_string_pretty(&payload)?)?;
    std::fs::write(&markdown, render_markdown(&summary, &results))?;
    println!("{}", serde_json::to_string_pretty(&summary)?);
    println!("wrote {} and {}", out.display(), markdown.display());
    Ok(())
}

fn read_cases(path: &Path) -> Result<Vec<EvalCase>> {
    let content =
        std::fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
    let mut cases = Vec::new();
    for (index, line) in content.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let case = serde_json::from_str::<EvalCase>(line)
            .with_context(|| format!("bad JSONL at line {}", index + 1))?;
        cases.push(case);
    }
    Ok(cases)
}

fn intent_fit(case: &EvalCase, outcome: &travel_ai::agent::RunOutcome) -> bool {
    if !case.expect.in_scope {
        return outcome.refusal.is_some();
    }
    let Some(intent) = &outcome.intent else {
        return false;
    };
    if let Some(transport) = case.expect.transport {
        if intent.transport != transport {
            return false;
        }
    }
    if let Some(max_days) = case.expect.max_days {
        if intent.max_days > max_days {
            return false;
        }
    }
    if let Some(budget) = case.expect.budget_rub {
        if intent.budget_rub != Some(budget) {
            return false;
        }
    }
    true
}

fn summarize(name: &str, results: &[CaseResult]) -> Summary {
    let in_scope: Vec<&CaseResult> = results
        .iter()
        .filter(|case| case.in_scope_expected)
        .collect();
    let negatives: Vec<&CaseResult> = results
        .iter()
        .filter(|case| !case.in_scope_expected)
        .collect();
    let hard_pass = in_scope
        .iter()
        .filter(|case| case.hard_pass == Some(true))
        .count();
    let tool_choice = in_scope.iter().filter(|case| case.tool_choice).count();
    let intent_fit = in_scope.iter().filter(|case| case.intent_fit).count();
    let refusal_ok = negatives.iter().filter(|case| case.refusal_ok).count();
    let fallback = results.iter().filter(|case| case.fallback).count();
    let total_tokens: u64 = results.iter().map(|case| case.tokens as u64).sum();
    let mut latencies: Vec<u64> = results.iter().map(|case| case.latency_ms).collect();
    latencies.sort_unstable();
    let p50 = percentile(&latencies, 50);
    let p95 = percentile(&latencies, 95);
    let errors = results.iter().filter(|case| case.error.is_some()).count();

    Summary {
        config: name.to_string(),
        cases: results.len(),
        in_scope_cases: in_scope.len(),
        negative_cases: negatives.len(),
        hard_pass,
        hard_pass_rate: rate(hard_pass, in_scope.len()),
        tool_choice,
        tool_choice_rate: rate(tool_choice, in_scope.len()),
        intent_fit,
        intent_fit_rate: rate(intent_fit, in_scope.len()),
        refusal_ok,
        refusal_rate: rate(refusal_ok, negatives.len()),
        fallback,
        total_tokens,
        avg_tokens: if results.is_empty() {
            0.0
        } else {
            total_tokens as f64 / results.len() as f64
        },
        p50_latency_ms: p50,
        p95_latency_ms: p95,
        errors,
    }
}

fn percentile(values: &[u64], percent: usize) -> u64 {
    if values.is_empty() {
        return 0;
    }
    let index = ((values.len() * percent) / 100).min(values.len() - 1);
    values[index]
}

fn rate(part: usize, total: usize) -> f64 {
    if total == 0 {
        0.0
    } else {
        part as f64 / total as f64
    }
}

fn render_markdown(summary: &Summary, results: &[CaseResult]) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "# Eval: {}", summary.config);
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "| Case | In scope | Hard pass | Tool choice | Intent fit | Refusal | Tokens | ms | Fallback |"
    );
    let _ = writeln!(out, "|---|---|---|---|---|---|---|---|---|");
    for case in results {
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} |",
            case.id,
            case.in_scope_actual,
            case.hard_pass
                .map(|value| value.to_string())
                .unwrap_or_else(|| "—".to_string()),
            case.tool_choice,
            case.intent_fit,
            case.refusal_ok,
            case.tokens,
            case.latency_ms,
            case.fallback
        );
    }
    let _ = writeln!(out);
    let _ = writeln!(out, "## Summary");
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "- Cases: {} (in-scope {}, negative {})",
        summary.cases, summary.in_scope_cases, summary.negative_cases
    );
    let _ = writeln!(
        out,
        "- Hard-pass: {}/{} ({:.1}%)",
        summary.hard_pass,
        summary.in_scope_cases,
        summary.hard_pass_rate * 100.0
    );
    let _ = writeln!(
        out,
        "- Tool-choice: {}/{} ({:.1}%)",
        summary.tool_choice,
        summary.in_scope_cases,
        summary.tool_choice_rate * 100.0
    );
    let _ = writeln!(
        out,
        "- Intent fit: {}/{} ({:.1}%)",
        summary.intent_fit,
        summary.in_scope_cases,
        summary.intent_fit_rate * 100.0
    );
    let _ = writeln!(
        out,
        "- Refusal: {}/{} ({:.1}%)",
        summary.refusal_ok,
        summary.negative_cases,
        summary.refusal_rate * 100.0
    );
    let _ = writeln!(out, "- Fallback plans: {}", summary.fallback);
    let _ = writeln!(
        out,
        "- Tokens: total {}, avg {:.1}",
        summary.total_tokens, summary.avg_tokens
    );
    let _ = writeln!(
        out,
        "- Latency: p50 {} ms, p95 {} ms",
        summary.p50_latency_ms, summary.p95_latency_ms
    );
    let _ = writeln!(out, "- Errors: {}", summary.errors);
    out
}
