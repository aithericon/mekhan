pub mod artifact_store;
pub mod dto;
pub mod handlers;
pub mod net_registry;
pub mod router;
pub mod scenario_bridge;

pub use artifact_store::ArtifactStoreState;
#[cfg(feature = "catalogue")]
pub use net_registry::CatalogueIntegrationConfig;
#[cfg(feature = "executor")]
pub use net_registry::ExecutorIntegrationConfig;
pub use net_registry::HumanIntegrationConfig;
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
