//! Data parsing and outbound integrations.
//!
//! Every provider implements `registry::Tool` and returns `ToolOutput` with a
//! provenance label. `providers` contains the parsers for places, Wikipedia,
//! geocoding, weather, routes, events, lodging, transport and fuel; `cache`
//! is an optional shared Redis/in-memory cache for tool results. Providers
//! only return verified places with finite coordinates inside Primorye, and
//! water centroids are treated as search areas rather than map pins.
pub mod cache;
pub mod providers;
pub mod registry;

pub use cache::Cache;
pub use registry::{Registry, Tool, ToolCtx, ToolError};
