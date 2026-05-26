//! Declarative backend registry — one `BackendDecl` per `ExecutionBackendType`,
//! stored in a static `&[BackendDecl]` slice.
//!
//! Replaces the per-backend match arms scattered through `compiler/`,
//! `models/template.rs::default_output_port`, and the frontend's hardcoded
//! ladders in `AutomatedStepSection.svelte`. Each decl bundles everything the
//! platform needs to know about a backend: how to validate its config, how to
//! scan its placeholder surfaces, what its default output port looks like,
//! whether it dispatches an executor job or runs as an engine effect, and
//! whether it binds workspace resources by staged file or by inline config
//! overlay.
//!
//! Adding a new backend is one entry in [`BACKENDS`] plus the backend-specific
//! module (e.g. `backends/smtp.rs`). Dispatch sites do `backends::lookup(bt)`
//! and call into the decl's fn pointers.
//!
//! The legacy [`ExecutionBackendType`] enum stays as the snake_case wire tag
//! (OpenAPI discriminator, Y.Doc-stored string, executor wire name); the
//! registry replaces the enum's role as a dispatch source-of-truth.

use std::collections::HashMap;

use serde::Serialize;
use serde_json::Value;
use utoipa::ToSchema;

use aithericon_executor_domain::{InputDeclaration, InputSource};

use crate::compiler::CompileError;
use crate::models::template::{ExecutionBackendType, FieldKind, Port, PortField};

pub mod docker;
pub mod file_ops;
pub mod http;
pub mod process;
pub mod smtp;

/// Per-backend declaration. Stored in a `&'static` slice so the registry has
/// zero runtime cost and trivially serializes the metadata subset for
/// `GET /api/backends`.
pub struct BackendDecl {
    /// Discriminator + lookup key. Must be unique across [`BACKENDS`].
    pub backend_type: ExecutionBackendType,
    /// Human label shown in the editor's backend picker.
    pub display_name: &'static str,
    /// Lucide icon name (frontend resolves to a component).
    pub icon: &'static str,
    /// Canonical output port fields. Mirrors what
    /// `default_output_port(bt)` returns; emitted in the
    /// `BackendDescriptor` so the frontend can stop duplicating the list.
    pub default_output_fields: &'static [DefaultPortField],
    /// Seed config the editor inserts when a step's backend is first set
    /// to this kind. The frontend has historically owned this map — moving
    /// it to the decl kills the TS/Rust drift surface.
    pub default_editor_config: fn() -> Value,
    /// Validate + transform the editor's JSON config into the canonical
    /// executor-facing config. Returns the validated `Value` plus the
    /// list of staged inputs (`InputDeclaration`s) the publisher will
    /// resolve to S3 paths.
    pub validate: ValidateFn,
    /// Optional placeholder scanner. Scans every config surface that can
    /// carry `<head>.<attr>` references (Tera templates, Python source,
    /// prompt strings) and returns the union of `(head, attr)` pairs.
    /// Drives both data-borrow planning (`<slug>.<field>`) and resource
    /// binding (heads that match workspace resources).
    pub ref_scanner: Option<RefScanner>,
    /// Static config paths whose string value names a workspace resource.
    /// Each `&[&str]` is a JSON path (e.g. `&["resource_alias"]`,
    /// `&["storage", "resource_alias"]`). Empty for backends whose
    /// resource references live only inside templates/source (see
    /// `ref_scanner`).
    pub resource_alias_paths: &'static [&'static [&'static str]],
    /// How a resolved resource envelope reaches the backend at runtime.
    pub resource_channel: ResourceChannel,
    /// How the compiler lowers a step of this backend into Petri.
    pub dispatch_mode: DispatchMode,
    /// True for backends whose declared output port fields are emitted
    /// into the AIR as a Rhai `outputs:` constant (Python / Kreuzberg /
    /// Llm today). Drives `lower::declared_outputs_rhai`.
    pub consumes_declared_outputs: bool,
    /// True for backends that get `.pyi` introspection stubs generated
    /// on publish / on demand (Python only today).
    pub pyi_introspection: bool,
    /// True if this backend can run via `DeploymentModel::Scheduled`.
    /// Engine-effect backends (e.g. CatalogueQuery) and any inherently
    /// inline-only future backends set this `false` so the editor hides
    /// the Scheduled toggle and the compiler rejects the combination.
    pub schedulable: bool,
    /// Snake-case wire string the executor uses to match `ExecutionSpec.backend`.
    /// MUST equal `backend_type.as_wire_str()` — enforced by the
    /// conformance test.
    pub executor_wire_name: &'static str,
}

/// Validation context passed to a backend's `validate` fn. Bundles the small
/// set of inputs the existing `validate_and_transform` body needs.
pub struct ValidationCtx<'a> {
    pub node_id: &'a str,
    pub node_files: &'a HashMap<String, InputSource>,
}

pub type ValidateFn =
    fn(&Value, &ValidationCtx<'_>) -> Result<(Value, Vec<InputDeclaration>), CompileError>;

/// Reference-scanning context. Bundles the inputs a backend's [`RefScanner`]
/// needs: the step's config, the node id (for ref attribution / dedupe), the
/// inline source map (for backends that scan attached files like Python),
/// and the entrypoint filename (for Python).
///
/// Identical shape to the legacy `compiler::resource_binding::ScanCtx` — the
/// registry now owns the type and `resource_binding` re-imports it.
pub struct ScanCtx<'a> {
    pub config: &'a Value,
    pub node_id: &'a str,
    pub inline_sources: &'a HashMap<String, HashMap<String, String>>,
    pub entrypoint: Option<&'a str>,
}

pub type RefScanner = fn(&ScanCtx<'_>) -> Vec<RefSite>;

/// One `<head>.<attr>` access discovered by a backend's scanner. The platform
/// resolves heads against three namespaces (in order): graph slugs (data
/// borrows), workspace resources (resource borrows), and `input.*` (control
/// token leaves). A single scanner is allowed to emit references that resolve
/// in any namespace; the caller filters by context.
#[derive(Debug, Clone)]
pub struct RefSite {
    pub head: String,
    pub attr: String,
}

/// How a resolved resource envelope reaches the running backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ResourceChannel {
    /// SMTP-style. Compiler emits a `ResourceEnvelope` borrow; the publisher
    /// stages `<alias>.json` as an `InputDeclaration`; the executor reads
    /// the file at run time via `load_resource::<T>`.
    StagedFile,
    /// LLM-style. The backend's `prepare()` reads `<alias>.json` and merges
    /// fields into the resolved config (per-step values win). The runtime
    /// never sees a separate envelope file — everything is in `resolved_config`.
    ConfigOverlay,
    /// Backend doesn't bind a workspace resource (Process, Docker, …).
    None,
}

/// Lowering mode — intrinsic to the backend, decided at the decl, NOT the
/// step. Orthogonal to `DeploymentModel` (Inline / Scheduled) which is a
/// per-step author choice on any `ExecutorJob` backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DispatchMode {
    /// Standard executor dispatch. The compiler emits an executor job; the
    /// step's `DeploymentModel` decides whether it's inline via NATS or
    /// submitted to a scheduler-net.
    ExecutorJob,
    /// Engine builtin effect (e.g. CatalogueQuery → `catalogue_lookup`). The
    /// compiler skips executor lowering entirely and emits an effect handler
    /// invocation directly into the Petri transition.
    EngineEffect {
        #[serde(rename = "handler")]
        handler: &'static str,
    },
}

/// A canonical default-port field. Mirrors [`PortField`]'s frontend-visible
/// shape but uses `&'static str` so the decl can live in a `const`.
#[derive(Debug, Clone, Copy)]
pub struct DefaultPortField {
    pub name: &'static str,
    pub label: &'static str,
    pub kind: FieldKind,
}

impl DefaultPortField {
    pub fn into_port_field(self) -> PortField {
        PortField {
            name: self.name.to_string(),
            label: self.label.to_string(),
            kind: self.kind,
            required: false,
            options: None,
            description: None,
            accept: None,
        }
    }
}

// ─── Registry ───────────────────────────────────────────────────────────────

/// Static slice of every backend. Phase 1 ships SMTP only; the legacy match
/// arms in `backend_configs.rs`, `token_shape.rs`, `compile.rs` and
/// `template.rs` cover the other 8 backends and fall through when
/// `lookup(bt)` returns `None`.
pub static BACKENDS: &[&BackendDecl] = &[
    &docker::DOCKER_DECL,
    &file_ops::FILE_OPS_DECL,
    &http::HTTP_DECL,
    &process::PROCESS_DECL,
    &smtp::SMTP_DECL,
];

/// Look up the decl for a backend type. Returns `None` for backends not yet
/// migrated to the registry — callers then fall through to their legacy
/// match arm.
pub fn lookup(backend_type: ExecutionBackendType) -> Option<&'static BackendDecl> {
    BACKENDS
        .iter()
        .find(|d| d.backend_type == backend_type)
        .copied()
}

// ─── Wire descriptor (frontend metadata via `GET /api/backends`) ────────────

/// Frontend-visible metadata for one backend. Returned by `GET /api/backends`.
///
/// The Svelte component map (`backend-panels.ts`) stays hand-written — TS
/// can't import components dynamically from a JSON tag at runtime without
/// defeating Vite chunking — but every other per-backend constant
/// (display name, icon, default config, default output fields, dispatch
/// mode, resource channel) flows from here. This is what kills the
/// `automated-ports.ts` ↔ `default_output_port()` drift hazard.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BackendDescriptor {
    /// Snake-case wire tag (`"smtp"`, `"python"`, …). Matches
    /// [`ExecutionBackendType`]'s wire encoding and the executor's
    /// `ExecutionSpec.backend` string.
    pub name: String,
    pub display_name: String,
    pub icon: String,
    /// Canonical output port shape. Frontend uses this for the "Reset to
    /// default" button on the output port editor.
    pub default_output_port: Port,
    /// Seed config inserted into a fresh step when this backend is
    /// selected. Opaque JSON — the backend's Svelte config panel decodes
    /// its own structure.
    pub default_editor_config: Value,
    pub dispatch_mode: DispatchMode,
    pub resource_channel: ResourceChannel,
    /// Whether the editor should show the Scheduled deployment toggle.
    pub schedulable: bool,
    /// Whether this backend's declared output port fields drive a Rhai
    /// `outputs:` constant (mostly informational for the frontend).
    pub consumes_declared_outputs: bool,
}

impl BackendDecl {
    pub fn to_descriptor(&self) -> BackendDescriptor {
        BackendDescriptor {
            name: self.backend_type.as_wire_str().to_string(),
            display_name: self.display_name.to_string(),
            icon: self.icon.to_string(),
            default_output_port: Port {
                id: "out".to_string(),
                label: "Output".to_string(),
                fields: self
                    .default_output_fields
                    .iter()
                    .map(|f| f.into_port_field())
                    .collect(),
            },
            default_editor_config: (self.default_editor_config)(),
            dispatch_mode: self.dispatch_mode,
            resource_channel: self.resource_channel,
            schedulable: self.schedulable,
            consumes_declared_outputs: self.consumes_declared_outputs,
        }
    }
}

/// Serialize every registered backend for `GET /api/backends`.
pub fn descriptors() -> Vec<BackendDescriptor> {
    BACKENDS.iter().map(|d| d.to_descriptor()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_finds_smtp() {
        let decl = lookup(ExecutionBackendType::Smtp).expect("smtp registered");
        assert_eq!(decl.executor_wire_name, "smtp");
        assert_eq!(decl.backend_type, ExecutionBackendType::Smtp);
    }

    #[test]
    fn lookup_returns_none_for_unmigrated() {
        // Phase 1 only ships SMTP. Other backends still go through the
        // legacy match arms.
        assert!(lookup(ExecutionBackendType::Python).is_none());
        assert!(lookup(ExecutionBackendType::Llm).is_none());
    }

    #[test]
    fn descriptors_includes_smtp() {
        let all = descriptors();
        assert!(all.iter().any(|d| d.name == "smtp"));
    }
}
