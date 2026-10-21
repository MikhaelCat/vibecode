//! Deterministic pre-LLM gate.
//!
//! Rejects oversized queries and off-topic, other-region or prompt-injection
//! requests via `intent::parse`, then yields the parsed `Intent`. This runs
//! before any model or tool call, so refusals are cheap and reproducible.

use chrono::NaiveDate;

use crate::domain::intent::{self, Intent, Refusal};

/// Outcome of the pre-flight check: a parsed intent or a user-facing refusal.
pub enum GuardDecision {
    Allow(Intent),
    Refuse(Refusal),
}

/// Enforces the query length limit and parses intent. Never calls the LLM.
pub fn check(query: &str, today: NaiveDate, max_chars: usize) -> GuardDecision {
    if query.chars().count() > max_chars {
        return GuardDecision::Refuse(Refusal::too_long());
    }
    match intent::parse(query, today) {
        Ok(intent) => GuardDecision::Allow(intent),
        Err(refusal) => GuardDecision::Refuse(refusal),
    }
}
