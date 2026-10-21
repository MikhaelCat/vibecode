use chrono::NaiveDate;

use crate::domain::intent::{self, Intent, Refusal};

pub enum GuardDecision {
    Allow(Intent),
    Refuse(Refusal),
}

pub fn check(query: &str, today: NaiveDate, max_chars: usize) -> GuardDecision {
    if query.chars().count() > max_chars {
        return GuardDecision::Refuse(Refusal::too_long());
    }
    match intent::parse(query, today) {
        Ok(intent) => GuardDecision::Allow(intent),
        Err(refusal) => GuardDecision::Refuse(refusal),
    }
}
