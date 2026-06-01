//! Hardware probe — executor-surya internal copy.
//!
//! Per-repo copy of the hardware-probe logic that originated in
//! `cloud-layer/cloud-layer-pool-ollama` and `cloud-layer/cloud-layer-compute-agent`
//! and was ported to `aithericon-executor-llm::hardware_probe`. Same per-repo
//! decision (sub-phase 2.2 Q6=A): avoids creating a cross-repo dependency on
//! `cloud-layer-common`; probe contracts are stable (CLI shapes); the small
//! surface (one function) is easy to audit.
//!
//! ## Why copy from executor-llm verbatim
//!
//! The hardware probe is identical regardless of OCR-vs-LLM backend — Surya's
//! PyTorch path detects MPS / CUDA / CPU the same way Ollama does at the
//! Python layer; the Rust side just advertises the discovered hardware to
//! cap-routing for load-scoring + capability gating. Copying verbatim
//! (instead of cross-crate dep on executor-llm) preserves the same per-repo
//! isolation rationale.

use std::process::Command;

use serde::{Deserialize, Serialize};

/// Probed hardware descriptor — advertised by the executor-surya pool.
///
/// Mirrors `cloud_layer_capability_routing::types::HardwareAdvertisement`'s
/// wire shape (tagged-enum with `kind` discriminator, PascalCase variants).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "PascalCase")]
pub enum HardwareAdvertisement {
    Cuda {
        count: u32,
        vram_gb: u32,
        compute_capability: String,
    },
    Rocm {
        count: u32,
        vram_gb: u32,
    },
    Metal {
        unified_memory_gb: u32,
    },
    Cpu {
        cores: u32,
    },
}

/// Probe hardware. First match wins (priority: CUDA > ROCm > Metal > CPU).
///
/// `force` parameter is a pure override (no env mutation). Tests pass
/// directly; production calls read `AITHERICON_FORCE_HARDWARE` at boot.
pub fn probe_hardware(force: Option<&str>) -> HardwareAdvertisement {
    if let Some(forced) = force {
        return match forced.to_lowercase().as_str() {
            "cuda" => HardwareAdvertisement::Cuda {
                count: 1,
                vram_gb: 24,
                compute_capability: "8.9".to_string(),
            },
            "rocm" => HardwareAdvertisement::Rocm {
                count: 1,
                vram_gb: 24,
            },
            "metal" => HardwareAdvertisement::Metal {
                unified_memory_gb: 128,
            },
            _ => HardwareAdvertisement::Cpu { cores: 4 },
        };
    }

    if let Some(hw) = probe_cuda() {
        return hw;
    }
    if let Some(hw) = probe_rocm() {
        return hw;
    }
    if let Some(hw) = probe_metal() {
        return hw;
    }
    probe_cpu()
}

fn probe_cuda() -> Option<HardwareAdvertisement> {
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
    let vram_mib: u64 = parts[1].parse().ok()?;
    let vram_gb = (vram_mib / 1024) as u32;
    let compute_capability = parts[2].to_string();
    Some(HardwareAdvertisement::Cuda {
        count,
        vram_gb,
        compute_capability,
    })
}

fn probe_rocm() -> Option<HardwareAdvertisement> {
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
    if count == 0 {
        return None;
    }
    Some(HardwareAdvertisement::Rocm {
        count,
        vram_gb: (total_mib / 1024) as u32,
    })
}

#[cfg(target_os = "macos")]
fn probe_metal() -> Option<HardwareAdvertisement> {
    let out = Command::new("system_profiler")
        .arg("SPDisplaysDataType")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let has_metal = stdout.lines().any(|l| l.contains("Metal"));
    if !has_metal {
        return None;
    }
    let mem_out = Command::new("sysctl").arg("hw.memsize").output().ok()?;
    let mem_str = String::from_utf8_lossy(&mem_out.stdout);
    let bytes: u64 = mem_str
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let unified_memory_gb = (bytes / (1024 * 1024 * 1024)) as u32;
    Some(HardwareAdvertisement::Metal { unified_memory_gb })
}

#[cfg(not(target_os = "macos"))]
fn probe_metal() -> Option<HardwareAdvertisement> {
    None
}

fn probe_cpu() -> HardwareAdvertisement {
    let cores = probe_cpu_cores().unwrap_or(1);
    HardwareAdvertisement::Cpu { cores }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn force_hardware_metal_override() {
        assert!(matches!(
            probe_hardware(Some("metal")),
            HardwareAdvertisement::Metal {
                unified_memory_gb: 128
            }
        ));
    }

    #[test]
    fn force_hardware_unknown_falls_back_to_cpu() {
        assert!(matches!(
            probe_hardware(Some("not-a-real-hardware-kind")),
            HardwareAdvertisement::Cpu { .. }
        ));
    }

    #[test]
    fn force_hardware_cuda_returns_cuda() {
        assert!(matches!(
            probe_hardware(Some("cuda")),
            HardwareAdvertisement::Cuda {
                count: 1,
                vram_gb: 24,
                ..
            }
        ));
    }
}
