//! Host / hardware fingerprint probe (fleet visibility).
//!
//! Probes the local machine ONCE at runner startup and produces the `host`
//! block a runner self-reports in its presence heartbeat (see [`crate::presence`]).
//! It answers the operator's "which machine, what accelerator, which IP?" — the
//! detail the fleet board would otherwise be missing for an enrolled runner.
//!
//! **Advisory wire-truth, never trusted for control.** mekhan surfaces this for
//! visibility only; caps/namespace stay DB-authoritative. Every field is
//! best-effort: a probe that can't run (no `nvidia-smi`, a locked-down sandbox)
//! simply omits its field, and the heartbeat still publishes. The output JSON
//! shape MUST match `mekhan_service::models::runner::HostInfo` (same field names)
//! — they are wire-compatible by convention, like the `{backends, concurrency}`
//! facets already on the payload, with no shared crate.
//!
//! Subprocess-based, mirroring `executor-llm`'s `hardware_probe`: standard CLI
//! tools (`hostname`, `nvidia-smi`, `rocm-smi`, `system_profiler`, `sysctl`,
//! `nproc`) over native bindings, so a CPU-only runner pulls no GPU libraries.

use std::process::Command;

use serde::Serialize;

/// The probed `host` block. Serializes to the JSON object embedded under
/// `host` in the presence payload; field names mirror `HostInfo` on the mekhan
/// side. Absent fields are skipped so a sparse probe yields a compact object.
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct HostInfo {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hostname: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub os: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu_cores: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mem_gb: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accelerator: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gpu_count: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vram_gb: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compute_capability: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub ips: Vec<String>,
}

/// Probe the local host once and serialize to a JSON value ready to embed as the
/// presence payload's `host` field. Best-effort end-to-end: any probe that fails
/// leaves its field absent. The accelerator priority matches `executor-llm`'s
/// hardware probe (CUDA > ROCm > Metal > CPU).
pub fn probe_host() -> serde_json::Value {
    let info = probe_host_info();
    serde_json::to_value(&info).unwrap_or_else(|_| serde_json::json!({}))
}

/// The structured probe — split out so it is unit-testable without serializing.
fn probe_host_info() -> HostInfo {
    let mut info = HostInfo {
        hostname: probe_hostname(),
        os: Some(std::env::consts::OS.to_string()),
        arch: Some(std::env::consts::ARCH.to_string()),
        cpu_cores: probe_cpu_cores(),
        mem_gb: probe_mem_gb(),
        ips: probe_ips(),
        ..Default::default()
    };
    apply_accelerator(&mut info);
    info
}

/// Fold the accelerator probe (CUDA > ROCm > Metal > CPU) into the descriptor.
fn apply_accelerator(info: &mut HostInfo) {
    if let Some((count, vram_gb, cc)) = probe_cuda() {
        info.accelerator = Some("cuda".to_string());
        info.gpu_count = Some(count);
        info.vram_gb = Some(vram_gb);
        info.compute_capability = Some(cc);
    } else if let Some((count, vram_gb)) = probe_rocm() {
        info.accelerator = Some("rocm".to_string());
        info.gpu_count = Some(count);
        info.vram_gb = Some(vram_gb);
    } else if let Some(unified_gb) = probe_metal() {
        info.accelerator = Some("metal".to_string());
        // Metal is unified memory — surface it as VRAM for the fleet view.
        info.vram_gb = Some(unified_gb);
    } else {
        info.accelerator = Some("cpu".to_string());
    }
}

fn probe_hostname() -> Option<String> {
    if let Ok(h) = std::env::var("HOSTNAME") {
        let h = h.trim();
        if !h.is_empty() {
            return Some(h.to_string());
        }
    }
    let out = Command::new("hostname").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let h = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!h.is_empty()).then_some(h)
}

/// Primary outbound IPv4 via the UDP-connect trick: connecting a UDP socket sets
/// the default peer (no packets sent) so `local_addr` returns the source address
/// the OS would route through — the runner's real LAN/WAN IP, not loopback.
fn probe_ips() -> Vec<String> {
    let Ok(sock) = std::net::UdpSocket::bind("0.0.0.0:0") else {
        return Vec::new();
    };
    if sock.connect("8.8.8.8:80").is_err() {
        return Vec::new();
    }
    match sock.local_addr() {
        Ok(addr) if !addr.ip().is_loopback() && !addr.ip().is_unspecified() => {
            vec![addr.ip().to_string()]
        }
        _ => Vec::new(),
    }
}

fn probe_cuda() -> Option<(u32, u32, String)> {
    let out = Command::new("nvidia-smi")
        .args([
            "--query-gpu=count,memory.total,compute_cap",
            "--format=csv,noheader,nounits",
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let line = stdout.lines().next()?;
    let parts: Vec<&str> = line.split(',').map(str::trim).collect();
    if parts.len() < 3 {
        return None;
    }
    let count: u32 = parts[0].parse().ok()?;
    // nvidia-smi reports MiB; convert to GB (rounding down).
    let vram_mib: u64 = parts[1].parse().ok()?;
    Some((count, (vram_mib / 1024) as u32, parts[2].to_string()))
}

fn probe_rocm() -> Option<(u32, u32)> {
    let out = Command::new("rocm-smi")
        .args(["--showmeminfo", "vram", "--csv"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let mut count: u32 = 0;
    let mut total_mib: u64 = 0;
    for line in stdout.lines().skip(1) {
        let parts: Vec<&str> = line.split(',').collect();
        if parts.len() >= 2 {
            if let Ok(mib) = parts[1].trim().parse::<u64>() {
                total_mib += mib;
                count += 1;
            }
        }
    }
    (count > 0).then_some((count, (total_mib / 1024) as u32))
}

#[cfg(target_os = "macos")]
fn probe_metal() -> Option<u32> {
    let out = Command::new("system_profiler")
        .arg("SPDisplaysDataType")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    if !stdout.lines().any(|l| l.contains("Metal")) {
        return None;
    }
    let mem_out = Command::new("sysctl").arg("hw.memsize").output().ok()?;
    let bytes: u64 = String::from_utf8_lossy(&mem_out.stdout)
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    Some((bytes / (1024 * 1024 * 1024)) as u32)
}

#[cfg(not(target_os = "macos"))]
fn probe_metal() -> Option<u32> {
    None
}

#[cfg(target_os = "linux")]
fn probe_cpu_cores() -> Option<u32> {
    let out = Command::new("nproc").output().ok()?;
    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
}

#[cfg(target_os = "macos")]
fn probe_cpu_cores() -> Option<u32> {
    let out = Command::new("sysctl").arg("hw.physicalcpu").output().ok()?;
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn probe_cpu_cores() -> Option<u32> {
    None
}

#[cfg(target_os = "linux")]
fn probe_mem_gb() -> Option<u32> {
    let text = std::fs::read_to_string("/proc/meminfo").ok()?;
    // `MemTotal:       263788000 kB`
    let kb: u64 = text
        .lines()
        .find_map(|l| l.strip_prefix("MemTotal:"))?
        .split_whitespace()
        .next()?
        .parse()
        .ok()?;
    Some((kb / (1024 * 1024)) as u32)
}

#[cfg(target_os = "macos")]
fn probe_mem_gb() -> Option<u32> {
    let out = Command::new("sysctl").arg("hw.memsize").output().ok()?;
    let bytes: u64 = String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())?;
    Some((bytes / (1024 * 1024 * 1024)) as u32)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn probe_mem_gb() -> Option<u32> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A natural probe on any host fills the always-available fields (os, arch,
    /// accelerator) and produces a JSON object whose field names match the
    /// mekhan-side `HostInfo` contract.
    #[test]
    fn probe_fills_static_fields_and_an_accelerator() {
        let info = probe_host_info();
        assert!(info.os.is_some(), "os from compile-time const");
        assert!(info.arch.is_some(), "arch from compile-time const");
        let accel = info.accelerator.expect("accelerator always classified");
        assert!(
            ["cuda", "rocm", "metal", "cpu"].contains(&accel.as_str()),
            "accelerator is one of the known kinds, got {accel}"
        );
    }

    /// The serialized object omits absent fields (compact wire) and is a JSON
    /// object — the exact shape the presence payload embeds under `host`.
    #[test]
    fn probe_host_serializes_to_compact_object() {
        let v = probe_host();
        assert!(v.is_object(), "host probe serializes to a JSON object");
        // os/arch/accelerator are always present; a value of `null` would mean a
        // skip_serializing_if regression.
        assert!(v.get("os").is_some());
        assert!(v.get("accelerator").is_some());
        assert!(
            !v.as_object().unwrap().values().any(|x| x.is_null()),
            "no field serializes as null (skip_serializing_if on every Option)"
        );
    }

    /// Field-name contract guard: the serialized keys are a SUBSET of the
    /// mekhan-side `HostInfo` field set. A rename here that drifts from mekhan
    /// would silently drop the field on parse — this pins the names.
    #[test]
    fn serialized_keys_match_mekhan_hostinfo_contract() {
        let allowed = [
            "hostname",
            "os",
            "arch",
            "cpu_cores",
            "mem_gb",
            "accelerator",
            "gpu_count",
            "vram_gb",
            "compute_capability",
            "ips",
        ];
        let v = probe_host();
        for key in v.as_object().unwrap().keys() {
            assert!(
                allowed.contains(&key.as_str()),
                "unexpected host field `{key}` — must match mekhan HostInfo"
            );
        }
    }
}
