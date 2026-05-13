//! Parseable models for Slurm CLI output.
//!
//! `squeue` uses `-o` format strings with `|` as field delimiter.
//! `sacct` supports `--parsable2` which uses `|` as field delimiter with no trailing separator.

/// A single entry from `squeue -o '%i|%k|%T' -h`.
///
/// Fields: JobID | Comment | State
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqueueEntry {
    /// Slurm job ID.
    pub job_id: String,
    /// Job comment (contains JSON routing metadata).
    pub comment: String,
    /// Current job state (e.g., PENDING, RUNNING).
    pub state: String,
}

impl SqueueEntry {
    /// Parse a single `--parsable2` line from squeue output.
    ///
    /// Expected format: `job_id|comment|state`
    pub fn parse(line: &str) -> Option<Self> {
        let line = line.trim();
        if line.is_empty() {
            return None;
        }

        let parts: Vec<&str> = line.splitn(3, '|').collect();
        if parts.len() < 3 {
            tracing::warn!(line = %line, "squeue line has fewer than 3 fields");
            return None;
        }

        Some(Self {
            job_id: parts[0].trim().to_string(),
            comment: parts[1].trim().to_string(),
            state: parts[2].trim().to_string(),
        })
    }

    /// Parse multiple lines of squeue output.
    pub fn parse_all(output: &str) -> Vec<Self> {
        output.lines().filter_map(Self::parse).collect()
    }
}

/// A single entry from `sacct --parsable2 -o "JobID,Comment,State,ExitCode,NodeList" -n`.
///
/// Fields: JobID | Comment | State | ExitCode | NodeList
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SacctEntry {
    /// Slurm job ID (may include `.batch` or `.extern` suffix for job steps).
    pub job_id: String,
    /// Job comment (contains JSON routing metadata).
    pub comment: String,
    /// Job state (e.g., COMPLETED, FAILED).
    pub state: String,
    /// Exit code in `exit:signal` format (e.g., `0:0`).
    pub exit_code: String,
    /// Comma-separated list of nodes.
    pub node_list: String,
}

impl SacctEntry {
    /// Parse a single `--parsable2` line from sacct output.
    ///
    /// Expected format: `job_id|comment|state|exit_code|node_list`
    pub fn parse(line: &str) -> Option<Self> {
        let line = line.trim();
        if line.is_empty() {
            return None;
        }

        let parts: Vec<&str> = line.splitn(5, '|').collect();
        if parts.len() < 5 {
            tracing::warn!(line = %line, "sacct line has fewer than 5 fields");
            return None;
        }

        Some(Self {
            job_id: parts[0].trim().to_string(),
            comment: parts[1].trim().to_string(),
            state: parts[2].trim().to_string(),
            exit_code: parts[3].trim().to_string(),
            node_list: parts[4].trim().to_string(),
        })
    }

    /// Parse multiple lines of sacct output.
    pub fn parse_all(output: &str) -> Vec<Self> {
        output.lines().filter_map(Self::parse).collect()
    }

    /// Whether this is the main job entry (not a sub-step like `.batch` or `.extern`).
    ///
    /// sacct often returns multiple rows per job (the main job plus sub-steps).
    /// We only want the main entry for state tracking.
    pub fn is_main_job(&self) -> bool {
        !self.job_id.contains('.')
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_squeue_parse() {
        let line = r#"12345|{"petri_net_id":"test-net","petri_place":"inbox"}|RUNNING"#;
        let entry = SqueueEntry::parse(line).unwrap();
        assert_eq!(entry.job_id, "12345");
        assert!(entry.comment.contains("petri_net_id"));
        assert_eq!(entry.state, "RUNNING");
    }

    #[test]
    fn test_squeue_parse_empty_comment() {
        let line = "12345||PENDING";
        let entry = SqueueEntry::parse(line).unwrap();
        assert_eq!(entry.job_id, "12345");
        assert_eq!(entry.comment, "");
        assert_eq!(entry.state, "PENDING");
    }

    #[test]
    fn test_squeue_parse_empty_line() {
        assert!(SqueueEntry::parse("").is_none());
        assert!(SqueueEntry::parse("  ").is_none());
    }

    #[test]
    fn test_squeue_parse_insufficient_fields() {
        assert!(SqueueEntry::parse("12345|RUNNING").is_none());
    }

    #[test]
    fn test_squeue_parse_all() {
        let output = "100|comment1|RUNNING\n200|comment2|PENDING\n";
        let entries = SqueueEntry::parse_all(output);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].job_id, "100");
        assert_eq!(entries[1].job_id, "200");
    }

    #[test]
    fn test_sacct_parse() {
        let line = r#"12345|{"petri_net_id":"test"}|COMPLETED|0:0|node01"#;
        let entry = SacctEntry::parse(line).unwrap();
        assert_eq!(entry.job_id, "12345");
        assert_eq!(entry.state, "COMPLETED");
        assert_eq!(entry.exit_code, "0:0");
        assert_eq!(entry.node_list, "node01");
    }

    #[test]
    fn test_sacct_parse_empty_fields() {
        let line = "12345|||0:0|";
        let entry = SacctEntry::parse(line).unwrap();
        assert_eq!(entry.comment, "");
        assert_eq!(entry.state, "");
        assert_eq!(entry.node_list, "");
    }

    #[test]
    fn test_sacct_parse_empty_line() {
        assert!(SacctEntry::parse("").is_none());
    }

    #[test]
    fn test_sacct_is_main_job() {
        let make = |id: &str| SacctEntry {
            job_id: id.to_string(),
            comment: String::new(),
            state: String::new(),
            exit_code: String::new(),
            node_list: String::new(),
        };

        assert!(make("12345").is_main_job());
        assert!(!make("12345.batch").is_main_job());
        assert!(!make("12345.extern").is_main_job());
        assert!(!make("12345.0").is_main_job());
    }

    #[test]
    fn test_squeue_parse_padded_wide_format() {
        // squeue -o '%i|%500k|%T' pads fields with trailing spaces
        let line = "    1|{\"petri_net_id\":\"scheduler-net\",\"petri_place\":\"inbox\"}                                          |RUNNING         ";
        let entry = SqueueEntry::parse(line).unwrap();
        assert_eq!(entry.job_id, "1");
        assert_eq!(
            entry.comment,
            r#"{"petri_net_id":"scheduler-net","petri_place":"inbox"}"#
        );
        assert_eq!(entry.state, "RUNNING");
    }

    #[test]
    fn test_sacct_parse_all() {
        let output = "100|c|COMPLETED|0:0|n1\n100.batch|c|COMPLETED|0:0|n1\n200|c|FAILED|1:0|n2\n";
        let entries = SacctEntry::parse_all(output);
        assert_eq!(entries.len(), 3);

        let main_entries: Vec<_> = entries.iter().filter(|e| e.is_main_job()).collect();
        assert_eq!(main_entries.len(), 2);
    }
}
