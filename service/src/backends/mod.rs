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

// Cross-crate metadata — the wire-name enum, dispatch mode, resource
// channel, and the per-backend `BackendMeta` consts. Re-exported so
// existing callers can keep importing from `crate::backends::*`.
pub use aithericon_backends::{
    BackendMeta, DispatchMode, ResourceChannel, CATALOGUE_QUERY_META, DOCKER_META, FILE_OPS_META,
    HTTP_META, KREUZBERG_META, LLM_META, PROCESS_META, PYTHON_META, SMTP_META,
};

pub mod catalogue_query;
pub mod docker;
pub mod file_ops;
pub mod http;
pub mod kreuzberg;
pub mod llm;
pub mod process;
pub mod python;
pub mod smtp;

/// Build a self-contained JSON Schema for a `ToSchema` config type `T`.
///
/// utoipa's `PartialSchema::schema()` gives the type's own schema, but any
/// nested type appears as a `{"$ref": "#/components/schemas/<Name>"}`. We
/// resolve those against the full `ApiDoc` components map (every config
/// sub-type is registered there) and inline them recursively so the value
/// the SPA receives on `BackendDescriptor.config_schema` needs no second
/// lookup. Cycle-guarded + depth-capped, mirroring
/// `compiler::schema_refs::inline_refs` (that one inlines `#/definitions/*`;
/// utoipa emits `#/components/schemas/*`, hence a sibling resolver here).
pub fn self_contained_config_schema<T: utoipa::PartialSchema>() -> Value {
    const COMPONENTS_PREFIX: &str = "#/components/schemas/";
    const DEPTH_CAP: usize = 64;

    // The full document's component schemas — every backend config type and
    // its sub-types are registered in `crate::openapi::ApiDoc`.
    use utoipa::OpenApi as _;
    let doc = serde_json::to_value(crate::openapi::ApiDoc::openapi())
        .expect("ApiDoc serialization cannot fail");
    let components = doc
        .get("components")
        .and_then(|c| c.get("schemas"))
        .and_then(|s| s.as_object())
        .cloned()
        .unwrap_or_default();

    let mut root =
        serde_json::to_value(T::schema()).expect("schema serialization cannot fail");

    fn inline(
        value: &mut Value,
        components: &serde_json::Map<String, Value>,
        in_flight: &mut std::collections::HashSet<String>,
        depth: usize,
        prefix: &str,
    ) {
        if depth > DEPTH_CAP {
            return;
        }
        // A node carrying a `{"$ref": "#/components/schemas/<name>"}`. utoipa
        // emits the ref alongside sibling keys (e.g. a field's `description`),
        // so we don't require the ref to be the sole key — we resolve it and
        // merge any siblings (description wins over the definition's own).
        let ref_name = value
            .as_object()
            .and_then(|m| m.get("$ref"))
            .and_then(|r| r.as_str())
            .and_then(|r| r.strip_prefix(prefix))
            .map(str::to_string);
        if let Some(name) = ref_name {
            if let Some(def) = components.get(&name) {
                if in_flight.insert(name.clone()) {
                    let mut resolved = def.clone();
                    inline(&mut resolved, components, in_flight, depth + 1, prefix);
                    in_flight.remove(&name);
                    // Carry over sibling keys from the ref site (notably the
                    // field-level `description`) onto the resolved schema.
                    if let (Some(resolved_obj), Some(site)) =
                        (resolved.as_object_mut(), value.as_object())
                    {
                        for (k, v) in site {
                            if k != "$ref" {
                                resolved_obj
                                    .entry(k.clone())
                                    .or_insert_with(|| v.clone());
                            }
                        }
                    }
                    *value = resolved;
                }
            }
            return;
        }
        match value {
            Value::Object(map) => {
                for (_, v) in map.iter_mut() {
                    inline(v, components, in_flight, depth + 1, prefix);
                }
            }
            Value::Array(arr) => {
                for v in arr.iter_mut() {
                    inline(v, components, in_flight, depth + 1, prefix);
                }
            }
            _ => {}
        }
    }

    let mut in_flight = std::collections::HashSet::new();
    inline(&mut root, &components, &mut in_flight, 0, COMPONENTS_PREFIX);
    root
}

/// `config_schema_fn` for backends with no executor config type (engine
/// effects like `catalogue_query`). Returns JSON `null`.
pub fn no_config_schema() -> Value {
    Value::Null
}

/// Per-backend declaration. Stored in a `&'static` slice so the registry has
/// zero runtime cost and trivially serializes the metadata subset for
/// `GET /api/v1/backends`.
///
/// The cross-crate metadata (wire name, display name, icon, dispatch mode,
/// schedulable, resource channel) lives on `meta` — a borrow into the
/// `aithericon-backends` crate. Everything else here is service-internal
/// (pulls in `CompileError`, `InputDeclaration`, the placeholder scanner
/// context) and stays inside `mekhan-service`.
pub struct BackendDecl {
    /// Cross-crate metadata block — wire name, display name, icon,
    /// dispatch mode, schedulable, resource channel. The conformance test
    /// asserts that `meta.backend_type == self.backend_type`.
    pub meta: &'static BackendMeta,
    /// Discriminator + lookup key. Equal to `meta.backend_type`; carried
    /// as a separate field so dispatch-site match statements can pattern
    /// against `decl.backend_type` without the indirection.
    pub backend_type: ExecutionBackendType,
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
    /// True for backends whose declared output port fields are emitted
    /// into the AIR as a Rhai `outputs:` constant (Python / Kreuzberg /
    /// Llm today). Drives `lower::declared_outputs_rhai`.
    pub consumes_declared_outputs: bool,
    /// True for backends that get `.pyi` introspection stubs generated
    /// on publish / on demand (Python only today).
    pub pyi_introspection: bool,
    /// How `ref_scanner` emissions are staged. Inert when
    /// `ref_scanner` is `None` (set to `Envelope` by convention).
    pub borrow_shape: BorrowShape,
    /// Per-site / per-kind validator called by the unified planner once
    /// per resolved ref. Default `accept_any_ref_kind` for backends
    /// without per-site constraints; LLM uses a custom validator to
    /// enforce `images[].path → File` and content-sites → not-File.
    pub validate_ref_kind: RefKindValidator,
    /// Who owns the output port shape — user, backend, or config-derived.
    /// Frontend branches its editor UI on this; the compiler still
    /// validates the persisted `output` field on publish either way.
    pub output_authoring: OutputAuthoring,
    /// Derive the output port from the step's `config`. Required when
    /// `output_authoring == Derived`; ignored otherwise. Pure function —
    /// called from the `POST /api/v1/backends/{name}/derive-output`
    /// endpoint and (potentially) compile-time validation hooks.
    pub derive_output_port: Option<DeriveOutputFn>,
    /// Produce the self-contained JSON Schema for this backend's
    /// `spec.config` shape (all `#/components/schemas/*` refs inlined).
    /// Surfaced on `BackendDescriptor.config_schema` so the SPA's generic
    /// schema-driven config form can render simple panels without a
    /// hand-written widget map. Mirrors `default_editor_config` as a
    /// fn pointer. Returns `Value::Null` for backends with no executor
    /// config type (e.g. the engine-effect `catalogue_query`).
    pub config_schema_fn: fn() -> Value,
    /// Field names within `config` that hold secrets (api keys, passwords).
    /// The generic form renders these with a masked widget. Empty when the
    /// backend carries no inline secret (credentials usually flow through a
    /// bound workspace resource instead).
    pub secret_fields: &'static [&'static str],
}

impl BackendDecl {
    /// Cross-crate display name (read through `meta`). Convenience for
    /// dispatch sites that previously inlined `decl.display_name`.
    pub fn display_name(&self) -> &'static str {
        self.meta.display_name
    }

    /// Cross-crate icon name.
    pub fn icon(&self) -> &'static str {
        self.meta.icon
    }

    /// Cross-crate dispatch mode.
    pub fn dispatch_mode(&self) -> DispatchMode {
        self.meta.dispatch_mode
    }

    /// Cross-crate resource channel.
    pub fn resource_channel(&self) -> ResourceChannel {
        self.meta.resource_channel
    }

    /// Cross-crate schedulable flag.
    pub fn schedulable(&self) -> bool {
        self.meta.schedulable
    }

    /// Cross-crate executor wire string. MUST equal
    /// `self.backend_type.as_wire_str()`; the conformance test
    /// double-checks.
    pub fn executor_wire_name(&self) -> &'static str {
        self.meta.wire_name
    }
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
///
/// `is_path_site` + `site_label` are only consulted for `BorrowShape::PerField`
/// backends (LLM, Kreuzberg) where the planner emits per-field staging and
/// the apply step rewrites `{{<head>.<attr>}}` placeholders to
/// `{{input:NAME}}` or `{{input_path:NAME}}` based on `is_path_site`. For
/// `BorrowShape::Envelope` backends (Python, SMTP) both fields are inert —
/// the apply step stages the whole envelope and the consumer reads fields
/// via its own template/runtime resolver.
#[derive(Debug, Clone)]
pub struct RefSite {
    pub head: String,
    pub attr: String,
    /// True when this ref site needs the producer's value as a filesystem
    /// path (Kreuzberg `file` / `files[i]`, LLM `images[].path`). False =
    /// content site (LLM `prompt` / `system_prompt` / `history[].content`,
    /// SMTP body/subject). Drives Raw-vs-StoragePath staging dispatch and
    /// the `{{input:NAME}}` vs `{{input_path:NAME}}` placeholder rewrite.
    pub is_path_site: bool,
    /// Author-facing site label for error attribution + per-field staging
    /// naming. Examples: `"prompt"`, `"images[2].path"`, `"subject"`,
    /// `"file"`, `"files[0]"`. For Envelope-shape backends carries the
    /// surface where the placeholder was found (informational only).
    pub site_label: String,
}

/// Who owns the AutomatedStep's output port shape — the user (free
/// authoring), the backend (fixed canonical shape), or the backend's
/// config (derived from `response_format` / schema / similar).
///
/// Frontend reads this off `BackendDescriptor` and either renders the
/// generic editable `PortsSection` (Free) or a read-only one whose fields
/// come from the registry (Fixed) or from a per-config server-side derive
/// call (Derived). The compiler still validates the persisted `output`
/// against the canonical shape on publish — the authoring flag is a UX
/// contract, not a security boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum OutputAuthoring {
    /// User defines the output port fields freely. Editable
    /// `PortsSection` + "Reset to default" button.
    Free,
    /// Backend's `default_output_fields` is the canonical shape. Read-only
    /// in the editor; persisted `output` is overwritten with the default
    /// on first paint.
    Fixed,
    /// Output fields are computed server-side from the step's config (and
    /// re-derived whenever it changes). Backends choosing this MUST set
    /// `derive_output_port` on their decl.
    Derived,
}

/// How the registry-driven borrow planner stages refs emitted by a
/// backend's [`RefScanner`]. Decided by the decl, intrinsic to the
/// backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum BorrowShape {
    /// Whole-envelope stage. Dedup per `(consumer, producer)`; apply step
    /// stages `<slug>.json` (with business fields hoisted to top-level)
    /// and the consumer's runtime — Python's `AccessibleDict`, SMTP's
    /// Tera context — surfaces fields without any source rewrite.
    /// Python, SMTP.
    Envelope,
    /// Per-field stage. Keep one borrow per `(consumer, slug, attr, site)`;
    /// apply step stages one input file per unique `(slug, attr)`,
    /// rewrites the `{{<slug>.<attr>}}` placeholder in the embedded
    /// config to `{{input:NAME}}` (content sites) or
    /// `{{input_path:NAME}}` (path sites). LLM, Kreuzberg.
    PerField,
}

/// Context for [`RefKindValidator`] — bundles everything a per-backend
/// validator needs to either accept the resolved ref or construct a
/// targeted [`CompileError`] with full attribution.
pub struct RefKindCtx<'a> {
    pub node_id: &'a str,
    pub site_label: &'a str,
    pub is_path_site: bool,
    pub slug: &'a str,
    pub attr: &'a str,
    pub kind: FieldKind,
}

/// Per-backend kind validator. Called by the unified planner once per
/// resolved ref with the producer's field kind and the ref site's
/// `is_path_site` flag. Returns `Ok(())` if the kind is acceptable at
/// the site, or a targeted [`CompileError`] (e.g.
/// [`CompileError::LlmImageRefNotFileKind`]).
pub type RefKindValidator = fn(&RefKindCtx<'_>) -> Result<(), CompileError>;

/// Default ref-kind validator — accepts every `FieldKind` at every site.
/// Used by backends without per-site kind constraints (Kreuzberg accepts
/// any kind at any path site because non-File kinds stage as Raw temp
/// files; SMTP has no per-site constraints because the whole envelope is
/// in scope).
pub fn accept_any_ref_kind(_: &RefKindCtx<'_>) -> Result<(), CompileError> {
    Ok(())
}

/// Output-port deriver for `OutputAuthoring::Derived` backends. Maps the
/// step's `config` to its canonical output [`Port`]. Pure: no I/O, no
/// global state, called per editor keystroke (with debouncing on the
/// frontend) from `POST /api/v1/backends/{name}/derive-output`.
///
/// Implementations should be permissive — partial/invalid configs are
/// expected at edit time, so return the closest valid port shape rather
/// than erroring. Hard validation belongs in [`BackendDecl::validate`].
pub type DeriveOutputFn = fn(&Value) -> Port;

// `DispatchMode` and `ResourceChannel` moved to `aithericon-backends` and
// are re-exported at the top of this module.

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

/// Static slice of every backend. Covers all 9 `ExecutionBackendType`
/// variants — the registry-coverage test enforces it. Every dispatch site
/// (compiler validation, ref scanning, lowering, `default_output_port`,
/// frontend metadata) reads from this slice; there are no remaining
/// per-backend match arms in the platform.
pub static BACKENDS: &[&BackendDecl] = &[
    &catalogue_query::CATALOGUE_QUERY_DECL,
    &docker::DOCKER_DECL,
    &file_ops::FILE_OPS_DECL,
    &http::HTTP_DECL,
    &kreuzberg::KREUZBERG_DECL,
    &llm::LLM_DECL,
    &process::PROCESS_DECL,
    &python::PYTHON_DECL,
    &smtp::SMTP_DECL,
];

/// Look up the decl for a backend type. Returns `None` only if `BACKENDS`
/// is missing the variant — and the registry-coverage test makes that a
/// build-time failure, so every dispatch site can safely `.expect()` or
/// early-out without a legacy fallback.
pub fn lookup(backend_type: ExecutionBackendType) -> Option<&'static BackendDecl> {
    BACKENDS
        .iter()
        .find(|d| d.backend_type == backend_type)
        .copied()
}

// ─── Wire descriptor (frontend metadata via `GET /api/v1/backends`) ────────────

/// Frontend-visible metadata for one backend. Returned by `GET /api/v1/backends`.
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
    /// Who owns the output port shape — user (free), backend (fixed), or
    /// derived from config. Drives the editor's port-section rendering.
    pub output_authoring: OutputAuthoring,
    /// Self-contained JSON Schema for this backend's `spec.config` shape
    /// (`#/components/schemas/*` refs inlined). The SPA's generic
    /// schema-driven form renders simple panels off this. `null` for
    /// backends with no executor config type.
    pub config_schema: Value,
    /// `config` field names that hold secrets (rendered masked by the
    /// generic form).
    pub secret_fields: Vec<String>,
}

impl BackendDecl {
    pub fn to_descriptor(&self) -> BackendDescriptor {
        BackendDescriptor {
            name: self.meta.wire_name.to_string(),
            display_name: self.meta.display_name.to_string(),
            icon: self.meta.icon.to_string(),
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
            dispatch_mode: self.meta.dispatch_mode,
            resource_channel: self.meta.resource_channel,
            schedulable: self.meta.schedulable,
            consumes_declared_outputs: self.consumes_declared_outputs,
            output_authoring: self.output_authoring,
            config_schema: (self.config_schema_fn)(),
            secret_fields: self.secret_fields.iter().map(|s| s.to_string()).collect(),
        }
    }
}

/// Serialize every registered backend for `GET /api/v1/backends`.
pub fn descriptors() -> Vec<BackendDescriptor> {
    BACKENDS.iter().map(|d| d.to_descriptor()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_finds_smtp() {
        let decl = lookup(ExecutionBackendType::Smtp).expect("smtp registered");
        assert_eq!(decl.executor_wire_name(), "smtp");
        assert_eq!(decl.backend_type, ExecutionBackendType::Smtp);
        assert_eq!(decl.meta.backend_type, decl.backend_type);
    }

    #[test]
    fn lookup_covers_every_backend() {
        // Every ExecutionBackendType variant must have a registered decl —
        // the unified compiler/borrow-planner paths are pure registry-driven
        // (no legacy fallbacks).
        for bt in [
            ExecutionBackendType::Python,
            ExecutionBackendType::Process,
            ExecutionBackendType::Docker,
            ExecutionBackendType::Http,
            ExecutionBackendType::Llm,
            ExecutionBackendType::FileOps,
            ExecutionBackendType::Kreuzberg,
            ExecutionBackendType::Smtp,
            ExecutionBackendType::CatalogueQuery,
        ] {
            assert!(
                lookup(bt).is_some(),
                "registry must cover every backend; missing: {bt:?}"
            );
        }
    }

    #[test]
    fn descriptors_includes_smtp() {
        let all = descriptors();
        assert!(all.iter().any(|d| d.name == "smtp"));
    }

    /// Walk a JSON value and assert no `#/components/schemas/*` ref survives.
    fn assert_no_component_refs(v: &Value, ctx: &str) {
        match v {
            Value::Object(map) => {
                if let Some(r) = map.get("$ref").and_then(|r| r.as_str()) {
                    assert!(
                        !r.starts_with("#/components/schemas/"),
                        "{ctx}: unresolved ref {r}"
                    );
                }
                for (_, child) in map {
                    assert_no_component_refs(child, ctx);
                }
            }
            Value::Array(arr) => {
                for child in arr {
                    assert_no_component_refs(child, ctx);
                }
            }
            _ => {}
        }
    }

    #[test]
    fn config_schemas_are_self_contained() {
        for d in descriptors() {
            if d.config_schema.is_null() {
                continue; // engine-effect backends (catalogue_query) carry no config
            }
            assert!(
                d.config_schema.is_object(),
                "{}: config_schema should be a schema object",
                d.name
            );
            assert_no_component_refs(&d.config_schema, &d.name);
        }
    }

    #[test]
    fn nested_config_types_inline() {
        // file_ops references StorageConfig (which nests StorageCredentials /
        // RetryConfig / StorageBackend); http references AuthConfig. If
        // inlining is wired correctly these resolve to inline objects, not
        // dangling refs — covered by `config_schemas_are_self_contained`, but
        // assert the nested types are non-trivially present too.
        let file_ops = descriptors()
            .into_iter()
            .find(|d| d.name == "file_ops")
            .expect("file_ops registered");
        let s = serde_json::to_string(&file_ops.config_schema).unwrap();
        // StorageConfig's `backend` / `credentials` leaves survive inlining.
        assert!(s.contains("credentials"), "StorageConfig should be inlined");

        let llm = descriptors()
            .into_iter()
            .find(|d| d.name == "llm")
            .expect("llm registered");
        assert!(
            llm.secret_fields.iter().any(|f| f == "api_key"),
            "llm must flag api_key as secret"
        );
    }
}
