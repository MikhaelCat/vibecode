//! Business rules, independent of HTTP and providers.
//!
//! `intent` turns a free-form query into dates/transport/interests. `facts`
//! stores verified tool output by id. `plan` expands a model draft into
//! concrete days, deduplicates places and builds legs. `validate` applies the
//! hard V01–V15 rules; `fallback` assembles a safe plan when the model cannot.
//! `geo` and `dates` are shared deterministic helpers.
pub mod dates;
pub mod facts;
pub mod fallback;
pub mod geo;
pub mod intent;
pub mod plan;
pub mod settlements;
pub mod trace;
pub mod validate;

pub use dates::{next_saturday, today_vladivostok};
pub use facts::{
    EventFact, Facts, FuelStation, Lodging, PlaceFact, RouteFact, SourceKind, SourceRef, ToolMeta,
    ToolOutput, TransportOption, WeatherDay,
};
pub use intent::{Intent, Pace, Refusal, TransportMode};
pub use plan::{DayPlan, Generation, ItemDraft, ItemKind, Leg, Plan, PlanDraft, PlanItem, Warning};
pub use trace::{RunTrace, TokenUsage, ToolCallTrace, TraceStep};
pub use validate::{Severity, ValidationReport, Violation};
