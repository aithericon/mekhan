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

/// Parse the allocation/job id out of `salloc --no-shell` output.
///
/// `salloc` prints a line like `salloc: Granted job allocation 12345`
/// (usually to stderr, which we merge into stdout via `2>&1`). We scan all
/// lines for the `Granted job allocation <id>` marker and return the trailing
/// numeric id. Returns `None` if no such line is present (e.g. salloc failed
/// or is still pending without a grant).
pub fn parse_granted_job_id(output: &str) -> Option<String> {
    const MARKER: &str = "Granted job allocation";
    for line in output.lines() {
        if let Some(idx) = line.find(MARKER) {
            let rest = line[idx + MARKER.len()..].trim();
            // The id is the first whitespace-delimited token after the marker.
            let id = rest.split_whitespace().next().unwrap_or("").trim();
            if !id.is_empty() {
                return Some(id.to_string());
            }
        }
    }
    None
}

/// Extract a `key=value` field from a single-line `scontrol show ... -o` record.
///
/// `scontrol show job <id> -o` emits one record per line as space-separated
/// `Key=Value` pairs (e.g. `JobId=12345 JobName=petri-foo NodeList=node01 ...`).
/// The value runs up to the next whitespace; `scontrol` quotes values that
/// themselves contain spaces, but the fields we care about (`NodeList`,
/// `EndTime`, `JobState`) never do. Returns the value for the first matching
/// key, or `None` if the key is absent.
///
/// Slurm represents "no value yet" with sentinels like `(null)` or
/// `Unknown`/`None`; callers should treat those as absent.
pub fn scontrol_field(output: &str, key: &str) -> Option<String> {
    let needle = format!("{}=", key);
    for line in output.lines() {
        // Search token-by-token so we don't match `XNodeList=` for `NodeList`.
        for token in line.split_whitespace() {
            if let Some(value) = token.strip_prefix(&needle) {
                return Some(value.to_string());
            }
        }
    }
    None
}

/// Whether a `scontrol` field value is a real value (not a Slurm sentinel).
///
/// Slurm uses `(null)`, `None`, `Unknown`, and the empty string to mean
/// "not assigned yet" (e.g. `NodeList` on a still-pending allocation, or
/// `EndTime=Unknown`). This collapses those to `None`.
pub fn scontrol_value_present(value: &str) -> Option<String> {
    let v = value.trim();
    if v.is_empty() || v == "(null)" || v == "None" || v == "Unknown" {
        None
    } else {
        Some(v.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_granted_job_id() {
        let out = "salloc: Granted job allocation 12345\n";
        assert_eq!(parse_granted_job_id(out), Some("12345".to_string()));
    }

    #[test]
    fn test_parse_granted_job_id_with_noise() {
        let out = "salloc: Pending job allocation 12345\nsalloc: job 12345 queued and waiting for resources\nsalloc: Granted job allocation 12345\nsalloc: Nodes node01 are ready for job\n";
        assert_eq!(parse_granted_job_id(out), Some("12345".to_string()));
    }

    #[test]
    fn test_parse_granted_job_id_absent() {
        assert_eq!(parse_granted_job_id(""), None);
        assert_eq!(
            parse_granted_job_id("salloc: error: Job submit/allocate failed"),
            None
        );
    }

    #[test]
    fn test_scontrol_field() {
        let out = "JobId=12345 JobName=petri-foo UserId=testuser(1000) JobState=RUNNING NodeList=node01 EndTime=2026-05-29T12:00:00\n";
        assert_eq!(scontrol_field(out, "JobId"), Some("12345".to_string()));
        assert_eq!(scontrol_field(out, "NodeList"), Some("node01".to_string()));
        assert_eq!(scontrol_field(out, "JobState"), Some("RUNNING".to_string()));
        assert_eq!(
            scontrol_field(out, "EndTime"),
            Some("2026-05-29T12:00:00".to_string())
        );
        assert_eq!(scontrol_field(out, "Missing"), None);
    }

    #[test]
    fn test_scontrol_field_no_substring_match() {
        // `NodeList` must not be matched by a token like `ReqNodeList=...`.
        let out = "ReqNodeList=(null) NodeList=node07";
        assert_eq!(scontrol_field(out, "NodeList"), Some("node07".to_string()));
    }

    #[test]
    fn test_scontrol_field_pending() {
        let out = "JobId=1 JobState=PENDING NodeList=(null) EndTime=Unknown";
        assert_eq!(scontrol_field(out, "NodeList"), Some("(null)".to_string()));
        assert_eq!(
            scontrol_value_present(&scontrol_field(out, "NodeList").unwrap()),
            None
        );
        assert_eq!(
            scontrol_value_present(&scontrol_field(out, "EndTime").unwrap()),
            None
        );
    }

    #[test]
    fn test_scontrol_value_present() {
        assert_eq!(scontrol_value_present("node01"), Some("node01".to_string()));
        assert_eq!(scontrol_value_present(""), None);
        assert_eq!(scontrol_value_present("(null)"), None);
        assert_eq!(scontrol_value_present("None"), None);
        assert_eq!(scontrol_value_present("Unknown"), None);
    }

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
        // squeue -o '%i|%500k|%T' pads fields with trailing spaces.
        // Test fixture uses "scheduler-net" as an example net id in the JSON comment.
        let line = "    1|{\"petri_net_id\":\"scheduler-net\",\"petri_place\":\"inbox\"}                                          |RUNNING         ";
        let entry = SqueueEntry::parse(line).unwrap();
        assert_eq!(entry.job_id, "1");
        assert_eq!(
            entry.comment,
            // Expected parsed comment — "scheduler-net" here is test data, not a pattern reference.
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
