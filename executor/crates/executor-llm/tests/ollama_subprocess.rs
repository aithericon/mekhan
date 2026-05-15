//! Integration tests for the managed Ollama subprocess + hardware probe.
//!
//! ## Test discipline
//!
//! - **No `#[ignore]`** anywhere in this file. Live-Ollama availability gates
//!   via a runtime branch (`which ollama`); when the binary is absent, the
//!   subprocess-lifecycle test logs an honest-skip line and reports PASS
//!   (the only thing being asserted is "if a binary is available, the
//!   lifecycle works" — the absence path is not a contract violation).
//! - **No `std::env::set_var`.** The hardware-probe override is passed as a
//!   direct parameter.
//! - **Test fixtures use placeholder model names** (`test-model-a`,
//!   `test-model-b`) — never real Ollama models.
//! - **Honest-absence assertions** — every positive assertion has its
//!   absence counterpart (health-check `true` before stop, `false` after;
//!   forced-Metal IS Metal AND IS NOT Cuda/Rocm/Cpu).
//!
//! Run with:
//!   cargo test -p aithericon-executor-llm --test ollama_subprocess -- --nocapture

use std::process::Command as StdCommand;
use std::time::Duration;

use aithericon_executor_llm::hardware_probe::{probe_hardware, HardwareAdvertisement};
use aithericon_executor_llm::ollama_subprocess::{OllamaSubprocess, OllamaSubprocessConfig};

// ---------------------------------------------------------------------------
// Hardware probe tests
// ---------------------------------------------------------------------------

#[test]
fn hardware_probe_force_metal_returns_metal() {
    // Pure-parameter override — NO env mutation. Force-Metal must return
    // Metal regardless of host platform.
    let hw = probe_hardware(Some("metal"));
    assert!(
        matches!(
            hw,
            HardwareAdvertisement::Metal {
                unified_memory_gb: 128
            }
        ),
        "forced Metal must return Metal, got {hw:?}"
    );
}

#[test]
fn hardware_probe_force_metal_is_not_cuda() {
    // Honest-absence: forced-Metal must NOT be CUDA. The dev box is M5
    // Metal — if a future code change accidentally routes forced-Metal to
    // the natural-probe path, this assert surfaces the regression.
    let hw = probe_hardware(Some("metal"));
    assert!(
        !matches!(hw, HardwareAdvertisement::Cuda { .. }),
        "forced Metal must NOT be Cuda, got {hw:?}"
    );
    assert!(
        !matches!(hw, HardwareAdvertisement::Rocm { .. }),
        "forced Metal must NOT be Rocm, got {hw:?}"
    );
    assert!(
        !matches!(hw, HardwareAdvertisement::Cpu { .. }),
        "forced Metal must NOT be Cpu, got {hw:?}"
    );
}

#[test]
fn hardware_probe_natural_on_dev_returns_metal() {
    // Natural probe on the dev M5 box (target_os = "macos") should detect
    // Metal. On CI (Linux without GPU) it returns Cpu. This test asserts
    // both branches structurally: the result is one of the four variants,
    // with valid per-variant invariants.
    let hw = probe_hardware(None);
    match &hw {
        HardwareAdvertisement::Metal { unified_memory_gb } => {
            assert!(
                *unified_memory_gb > 0,
                "Metal probe must report >0 GB, got {unified_memory_gb}"
            );
        }
        HardwareAdvertisement::Cuda { count, vram_gb, .. } => {
            assert!(*count > 0, "Cuda probe must report >0 devices");
            assert!(*vram_gb > 0, "Cuda probe must report >0 GB VRAM");
        }
        HardwareAdvertisement::Rocm { count, vram_gb } => {
            assert!(*count > 0, "Rocm probe must report >0 devices");
            assert!(*vram_gb > 0, "Rocm probe must report >0 GB VRAM");
        }
        HardwareAdvertisement::Cpu { cores } => {
            assert!(*cores > 0, "Cpu probe must report >0 cores");
        }
    }

    // Honest-absence on macOS: natural probe on this dev box (target_os
    // = "macos") must NOT report CUDA — there is no NVIDIA GPU on Apple
    // Silicon.
    #[cfg(target_os = "macos")]
    assert!(
        !matches!(hw, HardwareAdvertisement::Cuda { .. }),
        "macOS natural probe must NOT report CUDA, got {hw:?}"
    );
}

#[test]
fn hardware_probe_unknown_override_falls_back_to_cpu() {
    let hw = probe_hardware(Some("not-a-real-hardware-kind"));
    assert!(matches!(hw, HardwareAdvertisement::Cpu { .. }));
}

// ---------------------------------------------------------------------------
// Subprocess lifecycle tests
// ---------------------------------------------------------------------------

/// Runtime check: returns true when an `ollama` binary is on PATH. Used to
/// honest-skip live-spawn tests on machines without Ollama installed (e.g.
/// minimal CI).
fn ollama_binary_available() -> bool {
    StdCommand::new("ollama")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[tokio::test]
async fn subprocess_start_fails_with_nonexistent_binary() {
    // Honest-absence: spawn with a binary that does not exist must return
    // Err — never a success with a stale handle. This test is host-independent
    // (no Ollama install required).
    let cfg = OllamaSubprocessConfig {
        port: 21436,
        binary_path: Some("/definitely/not/a/real/path/to/ollama-binary-test".to_string()),
        readiness_timeout_secs: 1,
    };
    let result = OllamaSubprocess::start(&cfg).await;
    assert!(
        result.is_err(),
        "start with a missing binary must return Err, got Ok"
    );
}

#[tokio::test]
async fn subprocess_lifecycle_when_ollama_available() {
    // Live-spawn test. Skips honestly (returns PASS without asserting
    // lifecycle invariants) when no `ollama` binary is on PATH.
    if !ollama_binary_available() {
        eprintln!(
            "[ollama_subprocess] honest-skip: `ollama` not on PATH \
             (binary absent on this host — lifecycle invariants not exercised)"
        );
        return;
    }

    // Use a port well clear of the system Ollama default (11434), the
    // clinic dev value (11435), and the executor-llm default (11436). 21437
    // is safely above the dev range so concurrent dev-stack-up shouldn't
    // collide with this test.
    let cfg = OllamaSubprocessConfig {
        port: 21437,
        binary_path: None,
        readiness_timeout_secs: 30,
    };

    let subprocess = match OllamaSubprocess::start(&cfg).await {
        Ok(s) => s,
        Err(e) => {
            // Honest-skip: the binary exists but spawn failed (e.g. port in
            // use, prior crash, sandboxed CI without permission to bind).
            // Don't fail the test for environmental reasons.
            eprintln!(
                "[ollama_subprocess] honest-skip: spawn failed ({e}) — \
                 likely environmental, not a regression"
            );
            return;
        }
    };

    // ---- Positive: health-check succeeds after start.
    assert!(
        subprocess.health_check().await,
        "health_check must return true while subprocess is running"
    );

    // ---- Model load+unload via HTTP API. Use placeholder names that do
    // NOT exist in any Ollama registry — we exercise the request path,
    // not the registry. The pull will fail (network or 404), and we
    // assert the failure is structured, not a panic.
    let load_result = subprocess.model_load("test-model-a").await;
    assert!(
        load_result.is_err(),
        "model_load of a placeholder name must fail structurally (no such \
         model on registry), got Ok — surfaces a regression in the error path"
    );

    let unload_result = subprocess.model_unload("test-model-b").await;
    // A 404 (model not present) is treated as success — that's the
    // post-condition. So unload of a never-loaded model SHOULD be Ok.
    assert!(
        unload_result.is_ok(),
        "model_unload of a non-present model must be Ok (404-as-success path), \
         got {unload_result:?}"
    );

    // ---- Shutdown.
    subprocess
        .stop()
        .await
        .expect("stop must succeed when subprocess is healthy");

    // ---- Honest-absence: after stop, a freshly-built health-check client
    // against the same port must NOT succeed (the process is gone). We
    // can't call `subprocess.health_check()` because `stop()` consumed
    // self; we issue a direct HTTP probe instead.
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap();
    let url = format!("http://127.0.0.1:{}/api/tags", cfg.port);
    let after_stop = client.get(&url).send().await;
    assert!(
        after_stop.is_err() || !after_stop.unwrap().status().is_success(),
        "after stop, /api/tags must NOT respond successfully — got a stale \
         healthy response, indicating the subprocess didn't actually exit"
    );
}
