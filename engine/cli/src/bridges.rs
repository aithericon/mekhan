//! `aithericon check-bridges` — validate cross-net bridge connections.

use crate::client::EngineClient;
use crate::report::{print_analysis_report, AnalysisReport};

pub fn run_check_bridges(client: &EngineClient) {
    let report: AnalysisReport = match client.get("/api/bridges/check") {
        Ok(r) => r,
        Err(e) => {
            use colored::Colorize;
            eprintln!("{} {}", "Error:".red().bold(), e);
            std::process::exit(1);
        }
    };

    if !print_analysis_report(&report) {
        std::process::exit(1);
    }
}
