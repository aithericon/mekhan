pub mod artifact_store;
/// Per-cluster connection manager (multi-cluster scheduling, docs/16). Gated on
/// the scheduler legs — it owns the per-cluster watchers + idle-teardown.
#[cfg(any(feature = "slurm", feature = "nomad"))]
pub mod cluster_registry;
/// First-class cluster/watcher management API (docs/16 §9). `GET /api/clusters`
/// + force-reconnect/drain over the live `ClusterRegistry`. Gated on the
/// scheduler legs (the registry only exists then).
#[cfg(any(feature = "slurm", feature = "nomad"))]
pub mod cluster_routes;
pub mod dto;
pub mod handlers;
pub mod net_registry;
pub mod nomad_allocator;
pub mod router;
pub mod scenario_bridge;
pub mod slurm_allocator;
pub mod snapshot_store_object;

pub use artifact_store::ArtifactStoreState;
pub use snapshot_store_object::ObjectSnapshotStore;
#[cfg(feature = "catalogue")]
pub use net_registry::CatalogueIntegrationConfig;
pub use net_registry::HumanIntegrationConfig;
#[cfg(feature = "executor")]
pub use net_registry::{ExecutorIntegrationConfig, HttpExecutorConfig};
pub use net_registry::{NetInstance, NetRegistry, OnNetCreated, SchedulerBackend, SchedulerConfig};
pub use router::{create_router, create_router_with_registry, ApiDoc, AppState};
pub use scenario_bridge::ScenarioBridge;

// Pre-dispatch hook re-exports — spec § 6: the public `NetRegistry`
// surface lives in this crate, and consumers register hooks here. The
// trait + outcome types are implemented in `petri-application` and
// re-exported so callers don't need a direct `petri-application` dep.
pub use petri_application::pre_dispatch::{
    evaluate_chain, ChainEvalInputs, ChainEvalOutcome, DeferBudgets, HttpPreDispatchHook,
    HttpPreDispatchRequest, PreDispatchChain, PreDispatchChainEntry, PreDispatchContext,
    PreDispatchError, PreDispatchHook, PreDispatchHookConfig, PreDispatchMetadata,
    PreDispatchOutcome, PreDispatchRuntime, PreDispatchTransport, RegistrationError,
    DEFAULT_MAX_DEFERS,
};
pub use petri_domain::{PreDispatchHookOutcome, PreDispatchOutcomeKind};
