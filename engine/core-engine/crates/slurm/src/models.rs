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

/// The ordered `sacct --format` columns the watcher requests, `|`-joined for
/// `--parsable2`. Kept as a single source of truth next to [`SacctEntry::parse`]
/// so the column order and the positional parse can never drift.
///
/// Columns (0-indexed):
/// `0 JobID | 1 Comment | 2 State | 3 ExitCode | 4 NodeList | 5 Submit |
///  6 Start | 7 End | 8 Elapsed | 9 TotalCPU | 10 MaxRSS | 11 ReqTRES |
///  12 AllocTRES`
pub const SACCT_FORMAT: &str =
    "JobID,Comment,State,ExitCode,NodeList,Submit,Start,End,Elapsed,TotalCPU,MaxRSS,ReqTRES,AllocTRES";

/// A single entry from `sacct --parsable2 -o <SACCT_FORMAT> -n`.
///
/// The first five fields (JobID/Comment/State/ExitCode/NodeList) feed routing +
/// status detection; the rest are accounting telemetry flattened into the
/// terminal signal payload as [`AllocationMetrics`](petri_scheduler_bridge::AllocationMetrics).
/// Accounting fields can be empty (sacct emits `""` for sub-steps or when
/// accounting is partial) — parse keeps them as raw strings; the metrics build
/// step tolerates blanks.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
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
    /// Submit time (ISO-ish, local cluster TZ; e.g. `2026-05-29T12:00:00`).
    pub submit: String,
    /// Start time (when the job began running).
    pub start: String,
    /// End time (when the job reached terminal).
    pub end: String,
    /// Wall-clock elapsed (`[DD-]HH:MM:SS[.ffffff]`).
    pub elapsed: String,
    /// Total CPU time (`[DD-]HH:MM:SS[.ffffff]`).
    pub total_cpu: String,
    /// Peak resident set size (e.g. `1024K`, `2.5G`).
    pub max_rss: String,
    /// Requested TRES (e.g. `cpu=4,mem=16G,gres/gpu=1`).
    pub req_tres: String,
    /// Allocated TRES (e.g. `cpu=4,mem=16G,gres/gpu=1`).
    pub alloc_tres: String,
}

impl SacctEntry {
    /// Parse a single `--parsable2` line from sacct output.
    ///
    /// Expected format: the `|`-joined [`SACCT_FORMAT`] columns. Older callers
    /// requesting only the first five columns still parse (trailing accounting
    /// fields default to empty).
    pub fn parse(line: &str) -> Option<Self> {
        let line = line.trim();
        if line.is_empty() {
            return None;
        }

        // Split into all columns; the first 5 are required for routing/status.
        let parts: Vec<&str> = line.split('|').collect();
        if parts.len() < 5 {
            tracing::warn!(line = %line, "sacct line has fewer than 5 fields");
            return None;
        }

        // Positional accessor with empty-string default for the optional
        // accounting columns (back-compat with a 5-column sacct request).
        let at = |i: usize| {
            parts
                .get(i)
                .map(|s| s.trim().to_string())
                .unwrap_or_default()
        };

        Some(Self {
            job_id: at(0),
            comment: at(1),
            state: at(2),
            exit_code: at(3),
            node_list: at(4),
            submit: at(5),
            start: at(6),
            end: at(7),
            elapsed: at(8),
            total_cpu: at(9),
            max_rss: at(10),
            req_tres: at(11),
            alloc_tres: at(12),
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

/// Parse a Slurm `ExitCode` field (`exit:signal`, e.g. `0:0`, `1:0`, `0:9`)
/// into the numeric process exit code (the part before `:`).
///
/// Returns `None` for an empty / unparseable field. The signal half is ignored
/// (the `job_status` already captures cancellation/timeout).
pub fn parse_exit_code(field: &str) -> Option<i64> {
    let field = field.trim();
    if field.is_empty() {
        return None;
    }
    field.split(':').next()?.trim().parse::<i64>().ok()
}

/// Parse a Slurm duration field (`[DD-]HH:MM:SS[.ffffff]`) into milliseconds.
///
/// Used for `Elapsed` (wall clock) and `TotalCPU` (cpu time). Slurm renders
/// days with a `DD-` prefix and optional fractional seconds. Returns `None` for
/// an empty field or `INVALID`/`UNLIMITED` sentinels.
pub fn parse_duration_ms(field: &str) -> Option<i64> {
    let field = field.trim();
    if field.is_empty()
        || field.eq_ignore_ascii_case("INVALID")
        || field.eq_ignore_ascii_case("UNLIMITED")
    {
        return None;
    }

    // Optional leading `DD-`.
    let (days, hms) = match field.split_once('-') {
        Some((d, rest)) => (d.trim().parse::<i64>().ok()?, rest),
        None => (0, field),
    };

    // hms is `HH:MM:SS[.ffffff]` or `MM:SS[.ffffff]` or `SS[.ffffff]`.
    let comps: Vec<&str> = hms.split(':').collect();
    let (h, m, s_frac) = match comps.as_slice() {
        [h, m, s] => (h.parse::<i64>().ok()?, m.parse::<i64>().ok()?, *s),
        [m, s] => (0, m.parse::<i64>().ok()?, *s),
        [s] => (0, 0, *s),
        _ => return None,
    };

    // Seconds may carry a fractional part — parse as f64 then ms.
    let secs_f = s_frac.parse::<f64>().ok()?;
    let total_ms = ((days * 86_400 + h * 3_600 + m * 60) * 1_000) as f64 + secs_f * 1_000.0;
    Some(total_ms.round() as i64)
}

/// Parse a Slurm duration field into floating-point seconds (for `TotalCPU` →
/// `cpu_seconds`). Built on [`parse_duration_ms`].
pub fn parse_duration_secs(field: &str) -> Option<f64> {
    parse_duration_ms(field).map(|ms| ms as f64 / 1_000.0)
}

/// Parse a Slurm memory size (`MaxRSS`, e.g. `1024K`, `2.5G`, `512M`, `100`)
/// into bytes. A bare number is bytes; a trailing K/M/G/T (case-insensitive)
/// is a binary (1024-based) multiplier, matching sacct's default rendering.
pub fn parse_mem_bytes(field: &str) -> Option<i64> {
    let field = field.trim();
    if field.is_empty() {
        return None;
    }
    let (num_part, mult): (&str, f64) = match field.chars().last() {
        Some(c) if c.is_ascii_alphabetic() => {
            let m = match c.to_ascii_uppercase() {
                'K' => 1024.0,
                'M' => 1024.0 * 1024.0,
                'G' => 1024.0 * 1024.0 * 1024.0,
                'T' => 1024.0 * 1024.0 * 1024.0 * 1024.0,
                _ => return None,
            };
            (&field[..field.len() - 1], m)
        }
        _ => (field, 1.0),
    };
    let value = num_part.trim().parse::<f64>().ok()?;
    Some((value * mult).round() as i64)
}

/// A parsed TRES (trackable resources) spec from `ReqTRES`/`AllocTRES`.
///
/// Slurm renders TRES as `key=value` pairs joined by commas, e.g.
/// `cpu=4,mem=16G,node=1,gres/gpu=1` or `cpu=4,mem=16G,gres/gpu:a100=2`.
/// We extract cpu count, gpu count (`gres/gpu` or `gres/gpu:<type>`), gpu type,
/// and memory (normalised to GiB).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ParsedTres {
    /// CPU count (`cpu=`).
    pub cpu_count: Option<i64>,
    /// GPU count (`gres/gpu=` or `gres/gpu:<type>=`).
    pub gpu_count: Option<i64>,
    /// GPU type when the gres key is typed (`gres/gpu:a100=`).
    pub gpu_type: Option<String>,
    /// Memory in gibibytes (`mem=`, converted from K/M/G/T).
    pub memory_gb: Option<f64>,
}

impl ParsedTres {
    /// Whether nothing was extracted.
    pub fn is_empty(&self) -> bool {
        *self == ParsedTres::default()
    }
}

/// Parse a Slurm TRES field (`cpu=4,mem=16G,gres/gpu=1`) into a [`ParsedTres`].
///
/// Returns an all-`None` struct for an empty field (caller decides whether to
/// drop it). Memory values use the same K/M/G/T binary multipliers as
/// [`parse_mem_bytes`], then divide to GiB.
pub fn parse_tres(field: &str) -> ParsedTres {
    let mut out = ParsedTres::default();
    let field = field.trim();
    if field.is_empty() {
        return out;
    }
    for pair in field.split(',') {
        let Some((key, value)) = pair.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();
        if key == "cpu" {
            out.cpu_count = value.parse::<i64>().ok();
        } else if key == "mem" {
            out.memory_gb = parse_mem_bytes(value).map(|b| b as f64 / (1024.0 * 1024.0 * 1024.0));
        } else if key == "gres/gpu" || key.starts_with("gres/gpu:") {
            out.gpu_count = value.parse::<i64>().ok();
            if let Some(typ) = key.strip_prefix("gres/gpu:") {
                if !typ.is_empty() {
                    out.gpu_type = Some(typ.to_string());
                }
            }
        }
    }
    out
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
            ..Default::default()
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

    #[test]
    fn test_sacct_parse_full_format() {
        // The 13-column SACCT_FORMAT row.
        let line = "12345|{\"petri_net_id\":\"n\"}|COMPLETED|0:0|node01|2026-05-29T12:00:00|2026-05-29T12:00:05|2026-05-29T12:00:20|00:00:15|00:00:30|2048K|cpu=4,mem=16G,gres/gpu=1|cpu=4,mem=16G,gres/gpu=1";
        let e = SacctEntry::parse(line).unwrap();
        assert_eq!(e.job_id, "12345");
        assert_eq!(e.exit_code, "0:0");
        assert_eq!(e.node_list, "node01");
        assert_eq!(e.submit, "2026-05-29T12:00:00");
        assert_eq!(e.start, "2026-05-29T12:00:05");
        assert_eq!(e.end, "2026-05-29T12:00:20");
        assert_eq!(e.elapsed, "00:00:15");
        assert_eq!(e.total_cpu, "00:00:30");
        assert_eq!(e.max_rss, "2048K");
        assert_eq!(e.req_tres, "cpu=4,mem=16G,gres/gpu=1");
        assert_eq!(e.alloc_tres, "cpu=4,mem=16G,gres/gpu=1");
    }

    #[test]
    fn test_sacct_parse_backcompat_five_columns() {
        // A 5-column request still parses; accounting fields default empty.
        let e = SacctEntry::parse("12345|c|COMPLETED|0:0|node01").unwrap();
        assert_eq!(e.elapsed, "");
        assert_eq!(e.req_tres, "");
    }

    #[test]
    fn test_parse_exit_code() {
        assert_eq!(parse_exit_code("0:0"), Some(0));
        assert_eq!(parse_exit_code("1:0"), Some(1));
        assert_eq!(parse_exit_code("127:0"), Some(127));
        assert_eq!(parse_exit_code("0:9"), Some(0));
        assert_eq!(parse_exit_code("5"), Some(5));
        assert_eq!(parse_exit_code(""), None);
        assert_eq!(parse_exit_code("x:y"), None);
    }

    #[test]
    fn test_parse_duration_ms() {
        assert_eq!(parse_duration_ms("00:00:15"), Some(15_000));
        assert_eq!(parse_duration_ms("00:01:30"), Some(90_000));
        assert_eq!(parse_duration_ms("01:00:00"), Some(3_600_000));
        assert_eq!(parse_duration_ms("1-00:00:00"), Some(86_400_000));
        assert_eq!(parse_duration_ms("00:00:00.500"), Some(500));
        assert_eq!(parse_duration_ms("30"), Some(30_000));
        assert_eq!(parse_duration_ms("01:30"), Some(90_000));
        assert_eq!(parse_duration_ms(""), None);
        assert_eq!(parse_duration_ms("UNLIMITED"), None);
    }

    #[test]
    fn test_parse_duration_secs() {
        assert_eq!(parse_duration_secs("00:00:30"), Some(30.0));
        assert_eq!(parse_duration_secs("00:00:00.250"), Some(0.25));
    }

    #[test]
    fn test_parse_mem_bytes() {
        assert_eq!(parse_mem_bytes("1024K"), Some(1024 * 1024));
        assert_eq!(parse_mem_bytes("1M"), Some(1024 * 1024));
        assert_eq!(parse_mem_bytes("2G"), Some(2 * 1024 * 1024 * 1024));
        assert_eq!(
            parse_mem_bytes("2.5G"),
            Some((2.5 * 1024.0 * 1024.0 * 1024.0) as i64)
        );
        assert_eq!(parse_mem_bytes("100"), Some(100));
        assert_eq!(parse_mem_bytes(""), None);
    }

    #[test]
    fn test_parse_tres() {
        let t = parse_tres("cpu=4,mem=16G,node=1,gres/gpu=2");
        assert_eq!(t.cpu_count, Some(4));
        assert_eq!(t.gpu_count, Some(2));
        assert_eq!(t.gpu_type, None);
        assert_eq!(t.memory_gb, Some(16.0));

        let typed = parse_tres("cpu=8,gres/gpu:a100=1");
        assert_eq!(typed.cpu_count, Some(8));
        assert_eq!(typed.gpu_count, Some(1));
        assert_eq!(typed.gpu_type, Some("a100".to_string()));

        assert!(parse_tres("").is_empty());
    }
}
