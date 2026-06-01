//! End-to-end lease lifecycle for `SlurmAllocatorClient` against the local
//! Docker Slurm sandbox.
//!
//! These tests require:
//! - A running Slurm Docker container (`just slurm-up` / `just infra slurm-up`)
//! - The committed SSH key at `infra/slurm/ssh/slurm_test`
//!
//! Run with (CWD = `engine/`, so the relative key path resolves):
//!   `cargo test -p petri-api --features slurm --test slurm_alloc_lifecycle -- --ignored --test-threads=1`
//!
//! The whole file is gated on the `slurm` feature so a default `cargo test`
//! (feature off) compiles it away to nothing.

#![cfg(feature = "slurm")]

use petri_api::slurm_allocator::SlurmAllocatorClient;
use petri_application::resource_lease_handlers::AllocatorClient;
use petri_slurm::SlurmConfig;

/// Absolute path to the committed sandbox SSH key.
///
/// `cargo test` runs the test binary with CWD = the package manifest dir
/// (`core-engine/crates/api`), NOT the engine workspace root, so a bare
/// relative `infra/slurm/ssh/slurm_test` does not resolve. Anchor on
/// `CARGO_MANIFEST_DIR` and walk up to the engine root (`../../..`) where
/// `infra/` actually lives.
fn sandbox_ssh_key() -> String {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../infra/slurm/ssh/slurm_test")
        .to_string_lossy()
        .into_owned()
}

/// Config pointing at the local Docker Slurm sandbox (matches petri-slurm's
/// `integration_tests::sandbox_config`).
fn sandbox_config() -> SlurmConfig {
    SlurmConfig {
        ssh_host: "localhost".to_string(),
        ssh_port: 2222,
        ssh_user: "testuser".to_string(),
        ssh_key: sandbox_ssh_key(),
        ssh_known_hosts: "accept".to_string(),
        poll_interval_secs: 2,
        template_dir: "/opt/petri/templates".to_string(),
        lookback_window_secs: 3600,
        command_timeout_secs: 60,
    }
}

/// Full acquire → (node resolved) → release cycle through the trait surface.
///
/// Asserts the typed lease shape: a real `alloc_id`, a non-empty `node`
/// (CPU-only sandbox grants near-instantly), the `slurm` scheduler flavor, no
/// `gpu_uuid` (retired), and that `release` cancels the allocation. A second
/// `acquire` for the SAME grant reuses the live allocation (idempotency) rather
/// than allocating again.
#[tokio::test]
#[ignore] // requires live Slurm container
async fn slurm_allocator_acquire_release_lifecycle() {
    let client = SlurmAllocatorClient::new(sandbox_config());
    let grant_id = format!("petri-api-lease-test-{}", std::process::id());
    let request = serde_json::json!({});

    // acquire
    let lease = client
        .acquire("", "", &grant_id, &request)
        .await
        .expect("acquire lease");

    let alloc_id = lease
        .get("alloc_id")
        .and_then(|v| v.as_str())
        .expect("lease has alloc_id");
    assert!(!alloc_id.is_empty(), "alloc_id must be non-empty: {lease}");

    // Typed per-flavor scheduler detail; gpu_uuid is gone.
    assert_eq!(
        lease.get("scheduler").and_then(|s| s.get("flavor")).and_then(|v| v.as_str()),
        Some("slurm"),
        "lease must carry the slurm scheduler flavor: {lease}"
    );
    assert!(
        lease.get("gpu_uuid").is_none(),
        "retired gpu_uuid must not appear: {lease}"
    );

    // The node may briefly be null while pending; poll until assigned.
    let mut node = lease
        .get("node")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    for _ in 0..10 {
        if node.as_deref().is_some_and(|n| !n.is_empty()) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        let again = client
            .acquire("", "", &grant_id, &request)
            .await
            .expect("re-acquire (idempotent)");
        // idempotency: the same allocation id is reused.
        assert_eq!(
            again.get("alloc_id").and_then(|v| v.as_str()),
            Some(alloc_id),
            "re-acquire must reuse the same allocation"
        );
        node = again
            .get("node")
            .and_then(|v| v.as_str())
            .map(str::to_string);
    }
    assert!(
        node.as_deref().is_some_and(|n| !n.is_empty()),
        "node should be assigned on the CPU sandbox, got {node:?}"
    );

    // release
    client
        .release("", "", alloc_id)
        .await
        .expect("release lease");

    // release is idempotent: a second release of an already-gone alloc is fine.
    client
        .release("", "", alloc_id)
        .await
        .expect("second release tolerated");
}
