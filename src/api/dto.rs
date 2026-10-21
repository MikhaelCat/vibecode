//! Request body for the plan endpoints.
//!
//! Only `query` is required. Optional date and transport fields are form/API
//! overrides: the server still parses the query, but explicit values are
//! authoritative, so the model cannot invent a different period.

use chrono::NaiveDate;
use serde::Deserialize;

use crate::domain::intent::TransportMode;

/// JSON body for `POST /api/plan` and `/api/plan/sync`.
///
/// `start_date`/`end_date` must be supplied together and are clamped by the
/// API before they reach the orchestrator; `transport` replaces the mode
/// inferred from the query.
#[derive(Debug, Deserialize)]
pub struct PlanRequest {
    pub query: String,
    #[serde(default)]
    pub start_date: Option<NaiveDate>,
    #[serde(default)]
    pub end_date: Option<NaiveDate>,
    #[serde(default)]
    pub transport: Option<TransportMode>,
}
