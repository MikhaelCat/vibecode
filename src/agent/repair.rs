use crate::domain::validate::ValidationReport;

pub fn feedback(report: &ValidationReport) -> String {
    report.feedback()
}
