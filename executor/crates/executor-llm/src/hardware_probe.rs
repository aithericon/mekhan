//! Hardware probe — executor-llm internal copy.
//!
//! Probes the local machine at backend startup to determine the available
//! hardware kind (CUDA / ROCm / Metal / CPU). The probe path is per-platform;
//! the inference request path is hardware-agnostic (no cfg gates on fn
//! dispatch).
//!
//! Subprocess-based: uses standard CLI tools (`nvidia-smi`, `rocm-smi`,
//! `system_profiler`, `sysctl`, `nproc`) rather than native library bindings.
//! Rationale: avoids native-library dependencies that would complicate
//! cross-platform builds; CLI contracts are stable and predictable.
//!
//! **Per-repo copy.** This module is a per-repo copy of the hardware-probe
//! logic that originated in cloud-layer's `cloud-layer-pool-ollama` and
//! `cloud-layer-compute-agent` crates. The per-repo decision (sub-phase 2.2
//! Q6=A) avoids creating a cross-repo dependency on `cloud-layer-common`; the
//! executor and cloud-layer evolve independently. Drift is acceptable: probe
//! contracts are stable (CLI shapes) and the small surface (one function)
//! is easy to audit.

use std::process::Command;

use serde::{Deserialize, Serialize};

/// Probed hardware descriptor — advertised by the executor's LlmBackend.
///
/// This type intentionally mirrors `cloud_layer_capability_routing::types::
/// HardwareAdvertisement`'s wire shape (tagged-enum with `kind` discriminator,
/// PascalCase variants). Drift between this enum and the cloud-layer one is
/// the operator's responsibility to keep in sync — same rationale as any
/// other hand-maintained wire-compatible type pair across repository
/// boundaries.
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
/// The `force` parameter is an explicit override: when `Some("cuda" | "rocm" |
/// "metal" | "cpu")`, the function skips system probing and returns a fixed
/// descriptor for that hardware kind. Callers read the
/// `AITHERICON_FORCE_HARDWARE` (or equivalent) env once at boot and pass the
/// result here.
///
/// **Why a parameter, not an env read inside this fn:** tests need to assert
/// the override path, but `std::env::set_var` is process-global and races
/// across parallel test threads. Pure-function shape eliminates the test-env
/// mutation entirely. Pattern enforced by the wave-supervision conventions
/// document ("no global-state mutation in test bodies") and re-iterated as a
/// trip-wire in sub-phase 2.1b-adapter defect #2.
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
    // nvidia-smi returns MiB; convert to GB (rounding down).
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
    // Count lines that contain numeric VRAM data; sum total VRAM in MiB.
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

// Metal probe is only compiled on macOS — cfg gate is ONLY on the probe path,
// never on the inference request path (hardware-agnosticism invariant).
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

    // Unified memory via sysctl (bytes -> GB).
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

    // Tests pass `force` as a direct parameter — NO `std::env::set_var` anywhere
    // in this module. Process-global env mutation races across parallel test
    // threads; the parameter shape makes these tests deterministic regardless
    // of `--test-threads`.

    #[test]
    fn force_hardware_cpu_override() {
        assert!(matches!(
            probe_hardware(Some("cpu")),
            HardwareAdvertisement::Cpu { .. }
        ));
    }

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
    fn force_hardware_cuda_override() {
        assert!(matches!(
            probe_hardware(Some("cuda")),
            HardwareAdvertisement::Cuda {
                count: 1,
                vram_gb: 24,
                ..
            }
        ));
    }

    #[test]
    fn force_hardware_rocm_override() {
        assert!(matches!(
            probe_hardware(Some("rocm")),
            HardwareAdvertisement::Rocm {
                count: 1,
                vram_gb: 24
            }
        ));
    }

    #[test]
    fn force_hardware_unknown_falls_back_to_cpu() {
        assert!(matches!(
            probe_hardware(Some("totally-not-a-real-hardware-kind")),
            HardwareAdvertisement::Cpu { .. }
        ));
    }

    #[test]
    fn force_metal_is_not_cuda_or_rocm() {
        // Honest-absence: forcing Metal MUST NOT return any GPU variant.
        let hw = probe_hardware(Some("metal"));
        assert!(
            !matches!(hw, HardwareAdvertisement::Cuda { .. }),
            "forced Metal must not be Cuda"
        );
        assert!(
            !matches!(hw, HardwareAdvertisement::Rocm { .. }),
            "forced Metal must not be Rocm"
        );
        assert!(
            !matches!(hw, HardwareAdvertisement::Cpu { .. }),
            "forced Metal must not be Cpu"
        );
    }

    #[test]
    fn natural_probe_returns_valid_kind() {
        // No override: probe the actual hardware. On the dev M5 box this
        // returns Metal; on CI (CPU-only Linux) it returns Cpu. Either is
        // valid — we just check the structural invariants.
        let hw = probe_hardware(None);
        match &hw {
            HardwareAdvertisement::Metal { unified_memory_gb } => {
                assert!(*unified_memory_gb > 0, "Metal probe must report >0 GB");
            }
            HardwareAdvertisement::Cuda { count, .. } => assert!(*count > 0),
            HardwareAdvertisement::Rocm { count, .. } => assert!(*count > 0),
            HardwareAdvertisement::Cpu { cores } => assert!(*cores > 0),
        }
    }
}
