use chrono::NaiveDate;
use serde::Deserialize;

use crate::domain::intent::TransportMode;

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
