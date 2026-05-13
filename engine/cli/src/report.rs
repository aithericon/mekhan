//! Shared types and formatting for `AnalysisReport` responses.
//!
//! Used by `check-bridges` and `activate` commands.

use colored::Colorize;
use serde::Deserialize;

#[derive(Deserialize)]
pub struct AnalysisReport {
    pub is_valid: bool,
    pub issues: Vec<ValidationIssue>,
    pub summary: AnalysisSummary,
}

#[derive(Deserialize)]
pub struct ValidationIssue {
    pub node_id: String,
    pub level: String,
    pub code: String,
    pub message: String,
    #[serde(default)]
    pub remote_net_id: Option<String>,
}

#[derive(Deserialize)]
pub struct AnalysisSummary {
    pub error_count: usize,
    pub warning_count: usize,
    pub info_count: usize,
}

/// Pretty-print an analysis report to stdout.
///
/// Returns `true` if the report is valid (no errors).
pub fn print_analysis_report(report: &AnalysisReport) -> bool {
    if report.issues.is_empty() {
        println!(
            "{}",
            "OK -- all bridge connections are valid.".green().bold()
        );
        return true;
    }

    for issue in &report.issues {
        let prefix = match issue.level.as_str() {
            "error" => format!("{}", "ERROR".red().bold()),
            "warning" => format!("{}", "WARN".yellow().bold()),
            _ => format!("{}", "INFO".blue()),
        };

        let location = if let Some(ref remote) = issue.remote_net_id {
            format!("[{} -> {}]", issue.node_id, remote)
        } else {
            format!("[{}]", issue.node_id)
        };

        println!("{} {}", prefix, location.dimmed());
        println!("  {}: {}", issue.code.bold(), issue.message);
    }

    println!();
    let summary = format!(
        "{} errors, {} warnings, {} info",
        report.summary.error_count, report.summary.warning_count, report.summary.info_count
    );
    if report.is_valid {
        println!("{} -- {}", "OK".green().bold(), summary);
    } else {
        println!("{} -- {}", "FAILED".red().bold(), summary);
    }

    report.is_valid
}
