//! Repair-loop hand-off.
//!
//! After a hard validation failure the orchestrator sends this feedback back
//! to the model and lets it call `submit_plan` again. The wording lives in the
//! domain validator so every entry point produces the same repair prompt.

use crate::domain::validate::ValidationReport;

/// Returns the repair instructions for the report's hard violations.
pub fn feedback(report: &ValidationReport) -> String {
    report.feedback()
}
