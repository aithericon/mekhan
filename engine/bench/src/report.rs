//! Run-provenance capture and JSON artifact emission.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::metrics::{ResultRecord, RunMeta};

/// Capture environment provenance for the current process.
///
/// - `git_sha`: `git rev-parse --short HEAD`, falling back to `"unknown"`.
/// - `timestamp_ms`: Unix epoch milliseconds.
/// - `host`: `$HOSTNAME`, else the `hostname` command, else `"unknown"`.
/// - `profile`: `"debug"` or `"release"` per `cfg!(debug_assertions)`.
pub fn run_meta() -> RunMeta {
    RunMeta {
        git_sha: git_sha(),
        timestamp_ms: timestamp_ms(),
        host: host(),
        profile: if cfg!(debug_assertions) {
            "debug".to_string()
        } else {
            "release".to_string()
        },
    }
}

/// Write `record` as pretty JSON into the crate `results/` dir.
///
/// The directory is resolved relative to `CARGO_MANIFEST_DIR` (CWD-independent)
/// and created if missing. The filename is `<timestamp_ms>-<scenario>.json`
/// with the scenario sanitized to filename-safe characters. The written path
/// is returned and also printed to stderr.
pub fn emit(record: &ResultRecord) -> std::io::Result<PathBuf> {
    let dir = results_dir();
    std::fs::create_dir_all(&dir)?;

    let filename = format!(
        "{}-{}.json",
        record.run.timestamp_ms,
        sanitize(&record.scenario)
    );
    let path = dir.join(filename);

    let json = serde_json::to_string_pretty(record)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

    let mut file = std::fs::File::create(&path)?;
    file.write_all(json.as_bytes())?;
    file.write_all(b"\n")?;

    eprintln!("wrote {}", path.display());
    Ok(path)
}

/// The `results/` output directory, resolved relative to this crate's root.
fn results_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("results")
}

/// Replace any non-`[A-Za-z0-9._-]` character with `_`.
fn sanitize(scenario: &str) -> String {
    scenario
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn git_sha() -> String {
    Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| {
            let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if s.is_empty() {
                None
            } else {
                Some(s)
            }
        })
        .unwrap_or_else(|| "unknown".to_string())
}

fn timestamp_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn host() -> String {
    if let Ok(h) = std::env::var("HOSTNAME") {
        if !h.trim().is_empty() {
            return h.trim().to_string();
        }
    }

    Command::new("hostname")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| {
            let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if s.is_empty() {
                None
            } else {
                Some(s)
            }
        })
        .unwrap_or_else(|| "unknown".to_string())
}
