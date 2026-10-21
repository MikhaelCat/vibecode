use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct PlanRequest {
    pub query: String,
}
