use std::time::Instant;

/// Test execution reporter for tracking relayer commands, container events, etc.
///
/// Analogous to Go ICT's `testreporter` package.
///
/// Full implementation in Phase 5.

/// Records the result of a relayer or container command execution.
#[derive(Debug, Clone)]
pub struct ExecReport {
    pub container_name: String,
    pub command: Vec<String>,
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i64,
    pub started_at: Instant,
    pub duration: std::time::Duration,
}

/// Collects execution reports for a test run.
#[derive(Debug, Default)]
pub struct TestReporter {
    reports: Vec<ExecReport>,
}

impl TestReporter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record an execution report.
    pub fn record(&mut self, report: ExecReport) {
        self.reports.push(report);
    }

    /// Get all recorded reports.
    pub fn reports(&self) -> &[ExecReport] {
        &self.reports
    }
}
