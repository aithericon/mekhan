//! The `&'static [BackendMeta]` slice — one entry per shipped backend.
//!
//! Carries the minimal cross-crate metadata both `mekhan-service` and
//! `aithericon-executor-service` need to agree on. The mekhan side layers
//! `validate`/`ref_scanner`/`default_editor_config` fn pointers on top via
//! `mekhan_service::backends::BackendDecl`; the executor side keys its
//! runtime trait registrations off [`BackendMeta::name`] + the
//! `DispatchMode::ExecutorJob` filter.
//!
//! Adding a new backend = add a `BackendMeta` entry here + the compile-time
//! decl in mekhan + the runtime impl in executor + the Svelte panel. The
//! cross-crate name list is no longer hand-mirrored — both sides read this
//! slice directly.

use crate::types::{DispatchMode, ExecutionBackendType, ResourceChannel};

/// Cross-crate metadata for one backend. Both the compile-time decl in
/// mekhan-service and the runtime registry in executor-service look the
/// same entry up by [`ExecutionBackendType`].
///
/// Kept deliberately minimal — anything that depends on service-internal
/// types (compiler errors, port shapes, JSON values) lives in
/// `mekhan_service::backends::BackendDecl` and references this struct via a
/// `&'static BackendMeta` field.
#[derive(Debug, Clone, Copy)]
pub struct BackendMeta {
    /// Discriminator + lookup key. Equal to the [`ExecutionBackendType`]
    /// variant whose [`ExecutionBackendType::as_wire_str`] matches
    /// [`Self::wire_name`].
    pub backend_type: ExecutionBackendType,
    /// Snake-case wire tag (`"smtp"`, `"python"`, …). Stored separately from
    /// `backend_type` so callers can match strings without round-tripping
    /// through the enum.
    pub wire_name: &'static str,
    /// Human label shown in the editor's backend picker.
    pub display_name: &'static str,
    /// Lucide-style icon name (frontend resolves to a component).
    pub icon: &'static str,
    /// How the compiler lowers a step of this backend into Petri. Drives
    /// the executor's `ExecutorJob` filter — backends with
    /// `DispatchMode::EngineEffect` are never registered on the executor.
    pub dispatch_mode: DispatchMode,
    /// Whether the editor should surface the Scheduled (Nomad/Slurm)
    /// deployment toggle. Engine-effect backends are inline-only.
    pub schedulable: bool,
    /// How a resolved resource envelope reaches the running backend.
    /// `None` for backends that don't bind workspace resources at all
    /// (Process, Docker, CatalogueQuery today).
    pub resource_channel: ResourceChannel,
    /// Whether this backend can be selected as an `AutomatedStep` backend in
    /// the editor's node-authoring surface. `false` hides it from the
    /// `/api/v1/backends` picker list while keeping the variant fully
    /// compilable + runnable — the channel for backends that are an internal
    /// lowering target rather than a user-authored node kind. `Llm` is
    /// `false`: inference is authored via the dedicated **Agent** node (whose
    /// degenerate single-shot path emits byte-identical `AutomatedStep(Llm)`
    /// IR), so a standalone "LLM step" is no longer a user-facing concept.
    pub user_authorable: bool,
}

// Per-backend constants. Service-side `BackendDecl` references these via
// `meta: &aithericon_backends::PYTHON_META` etc. Centralising the consts
// here lets the executor's runtime registration loop in Phase 2 walk the
// same data without re-declaring it.

pub const PYTHON_META: BackendMeta = BackendMeta {
    backend_type: ExecutionBackendType::Python,
    wire_name: "python",
    display_name: "Python",
    icon: "code",
    dispatch_mode: DispatchMode::ExecutorJob,
    schedulable: true,
    resource_channel: ResourceChannel::None,
    user_authorable: true,
};

pub const PROCESS_META: BackendMeta = BackendMeta {
    backend_type: ExecutionBackendType::Process,
    wire_name: "process",
    display_name: "Process",
    icon: "terminal",
    dispatch_mode: DispatchMode::ExecutorJob,
    schedulable: true,
    resource_channel: ResourceChannel::None,
    user_authorable: true,
};

pub const DOCKER_META: BackendMeta = BackendMeta {
    backend_type: ExecutionBackendType::Docker,
    wire_name: "docker",
    display_name: "Docker",
    icon: "container",
    dispatch_mode: DispatchMode::ExecutorJob,
    schedulable: true,
    resource_channel: ResourceChannel::None,
    user_authorable: true,
};

pub const HTTP_META: BackendMeta = BackendMeta {
    backend_type: ExecutionBackendType::Http,
    wire_name: "http",
    display_name: "HTTP Request",
    icon: "globe",
    dispatch_mode: DispatchMode::ExecutorJob,
    schedulable: true,
    // LLM-style: the backend's `prepare()` reads `<auth_resource>.json` and
    // fills the selected `AuthConfig` scheme's secret. Resource kinds:
    // `http_bearer` / `http_basic` / `http_api_key`.
    resource_channel: ResourceChannel::ConfigOverlay,
    user_authorable: true,
};

pub const LLM_META: BackendMeta = BackendMeta {
    backend_type: ExecutionBackendType::Llm,
    wire_name: "llm",
    display_name: "LLM (AI Model)",
    icon: "sparkles",
    dispatch_mode: DispatchMode::ExecutorJob,
    schedulable: true,
    resource_channel: ResourceChannel::ConfigOverlay,
    // Not user-authorable: inference is authored via the Agent node. The
    // variant stays fully compilable/runnable as the Agent degenerate path's
    // IR target — it's just no longer offered in the backend picker.
    user_authorable: false,
};

pub const FILE_OPS_META: BackendMeta = BackendMeta {
    backend_type: ExecutionBackendType::FileOps,
    wire_name: "file_ops",
    display_name: "File Operations",
    icon: "folder-open",
    dispatch_mode: DispatchMode::ExecutorJob,
    schedulable: true,
    resource_channel: ResourceChannel::StagedFile,
    user_authorable: true,
};

pub const KREUZBERG_META: BackendMeta = BackendMeta {
    backend_type: ExecutionBackendType::Kreuzberg,
    wire_name: "kreuzberg",
    display_name: "Document Extraction",
    icon: "file-search",
    dispatch_mode: DispatchMode::ExecutorJob,
    schedulable: true,
    resource_channel: ResourceChannel::None,
    user_authorable: true,
};

pub const SURYA_META: BackendMeta = BackendMeta {
    backend_type: ExecutionBackendType::Surya,
    wire_name: "surya",
    display_name: "Surya OCR",
    icon: "scan-text",
    dispatch_mode: DispatchMode::ExecutorJob,
    schedulable: true,
    resource_channel: ResourceChannel::None,
    user_authorable: true,
};

pub const SMTP_META: BackendMeta = BackendMeta {
    backend_type: ExecutionBackendType::Smtp,
    wire_name: "smtp",
    display_name: "SMTP (Email)",
    icon: "mail",
    dispatch_mode: DispatchMode::ExecutorJob,
    schedulable: true,
    resource_channel: ResourceChannel::StagedFile,
    user_authorable: true,
};

pub const CATALOGUE_QUERY_META: BackendMeta = BackendMeta {
    backend_type: ExecutionBackendType::CatalogueQuery,
    wire_name: "catalogue_query",
    display_name: "Catalogue Query",
    icon: "database-zap",
    dispatch_mode: DispatchMode::EngineEffect {
        handler: "catalogue_lookup",
    },
    schedulable: false,
    resource_channel: ResourceChannel::None,
    user_authorable: true,
};

/// Every shipped backend. One entry per [`ExecutionBackendType`] variant;
/// the conformance test in `mekhan-service` asserts bijection.
pub static BACKENDS: &[&BackendMeta] = &[
    &PYTHON_META,
    &PROCESS_META,
    &DOCKER_META,
    &HTTP_META,
    &LLM_META,
    &FILE_OPS_META,
    &KREUZBERG_META,
    &SURYA_META,
    &SMTP_META,
    &CATALOGUE_QUERY_META,
];

/// Look up the cross-crate metadata for a backend. Returns `None` only if
/// [`BACKENDS`] is out of sync with the enum, which the conformance test
/// in `mekhan-service` catches.
pub fn lookup(backend_type: ExecutionBackendType) -> Option<&'static BackendMeta> {
    BACKENDS
        .iter()
        .copied()
        .find(|m| m.backend_type == backend_type)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_variant_has_meta() {
        for bt in [
            ExecutionBackendType::Python,
            ExecutionBackendType::Process,
            ExecutionBackendType::Docker,
            ExecutionBackendType::Http,
            ExecutionBackendType::Llm,
            ExecutionBackendType::FileOps,
            ExecutionBackendType::Kreuzberg,
            ExecutionBackendType::Surya,
            ExecutionBackendType::Smtp,
            ExecutionBackendType::CatalogueQuery,
        ] {
            let m = lookup(bt).unwrap_or_else(|| panic!("BACKENDS missing entry for {bt:?}"));
            assert_eq!(m.wire_name, bt.as_wire_str());
        }
    }

    #[test]
    fn wire_names_unique() {
        let mut names: Vec<&str> = BACKENDS.iter().map(|m| m.wire_name).collect();
        names.sort_unstable();
        let len_before = names.len();
        names.dedup();
        assert_eq!(names.len(), len_before, "wire_name collision in BACKENDS");
    }

    #[test]
    fn catalogue_query_is_engine_effect() {
        let m = lookup(ExecutionBackendType::CatalogueQuery).unwrap();
        assert!(matches!(
            m.dispatch_mode,
            DispatchMode::EngineEffect {
                handler: "catalogue_lookup"
            }
        ));
        assert!(!m.schedulable);
    }
}
