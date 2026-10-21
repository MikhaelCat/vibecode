//! Agent loop.
//!
//! `orchestrator` owns the only path from a user query to a plan: guard →
//! tool calls → `submit_plan` → enrich → validator → repair/fallback. Tool
//! results are converted into `domain::Facts`, so later steps use verified
//! data instead of raw model memory. `guard` rejects prompt injection and
//! off-topic input, `prompt` builds compact context, and `repair` asks the
//! model to fix validator errors.
pub mod guard;
pub mod orchestrator;
pub mod prompt;
pub mod repair;

pub use orchestrator::{Orchestrator, RunOutcome, RunOverrides, StreamEvent};
