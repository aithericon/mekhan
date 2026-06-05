use std::collections::BTreeMap;

use crate::compiler::error::CompileError;
use crate::models::template::{FieldKind, WorkflowGraph};

use super::*; // ─── Pre-publish editor entrypoint ──────────────────────────────────────────

/// What the editor needs on every (debounced) keystroke: per-place schemas,
/// the producer-namespaced scope per node, and diagnostics.
pub struct TypeSurface {
    pub place_schemas: BTreeMap<String, String>,
    pub scopes: BTreeMap<String, Vec<ScopeEntry>>,
    pub diagnostics: Vec<ShapeDiagnostic>,
    /// `false` when the draft isn't structurally analyzable yet (still
    /// returns — the editor gets the `GraphIncomplete` diagnostic, not an
    /// HTTP error).
    pub graph_ok: bool,
}

/// The DX lever: pure, graph-only, and **independent of `compile_to_air`
/// succeeding**. A draft with an unstaged Python step (unpublishable) still
/// gets full type surfacing here — feedback lands while editing, not at
/// publish when it's too late. This is what `POST /api/v1/compile` (or a sibling
/// `/api/v1/analyze`) should additionally return on every edit.
pub fn surface_types(
    graph: &WorkflowGraph,
    known_globals: &crate::compiler::named_global::KnownGlobals,
) -> TypeSurface {
    // Resolve Agent `response_format` `$ref`s so the variable picker / scope
    // sees the schema's fields (not the default envelope) — the same pre-pass
    // the compile path runs. Best-effort: a draft mid-edit may carry a
    // dangling ref, so fall back to the un-normalized graph rather than
    // blanking the surface (the real error still lands at publish).
    let normalized = crate::compiler::schema_refs::inline_agent_response_format_refs(graph)
        .unwrap_or(std::borrow::Cow::Borrowed(graph));
    let graph = normalized.as_ref();
    match analyze(graph, known_globals) {
        Ok(r) => TypeSurface {
            place_schemas: r.place_schemas,
            scopes: r.scopes,
            diagnostics: r.diagnostics,
            graph_ok: true,
        },
        Err(e) => TypeSurface {
            place_schemas: BTreeMap::new(),
            scopes: BTreeMap::new(),
            diagnostics: vec![ShapeDiagnostic::GraphIncomplete {
                message: e.to_string(),
            }],
            graph_ok: false,
        },
    }
}

// ─── Foundation: control/data split — guard read-arc planning ───────────────
//
// Borrow-model mapping (the spec): a *data token* is a `let` value produced
// once, **owned by a write-once parked place**; a *read-arc* is a `&T` shared
// borrow (non-consuming, many readers, `ScenarioArc{read:true}`); a consuming
// arc is a *move*; the *control token* is a `let mut` threaded by-move. The
// compiler plays borrow-checker: provenance proves which parked place owns a
// referenced field, and synthesizes the read-arc into the reader.

/// A control-token field = identity / routing only (`_`-prefixed metadata,
/// loop counter, plus correlation/outcome). Everything else is data.
pub(crate) fn is_control_leaf(path: &str) -> bool {
    // path looks like `input.<seg>...`
    let seg = path.strip_prefix("input.").unwrap_or(path);
    let head = seg.split('.').next().unwrap_or(seg);
    head.starts_with('_') || head == "task_id" || head == "status"
}

/// Canonical yield/park logic: park the producer's *whole* output as the
/// write-once `data` token (`let` owned by the parked place; read-arced by
/// downstream `&` borrows), forward only identity/routing keys as the slim
/// `ctrl` token (`let mut` threaded by-move). Input port `tok`, outputs
/// `data` + `ctrl`. Shared by native lowering (WS2) and any post-pass.
pub(crate) const YIELD_LOGIC: &str = "let d = tok; let c = #{}; \
     for k in d.keys() { if k.starts_with(\"_\") || k == \"task_id\" || k == \"status\" \
     { c[k] = d[k]; } } #{ data: d, ctrl: c }";

/// Map-body variant of [`YIELD_LOGIC`]: reconstructs the Map itemVar onto the
/// slim control token. A node whose `parent_id` is a Map runs once per scattered
/// element; the scatter stamps `#{ <itemVar>: <element>, __map_item: <element>, .. }`
/// onto each body token (namespace-on-token, v1) and the guard/borrow planner
/// resolves `<itemVar>.<field>` as token-resident (`RefResolution::Control`, no
/// read-arc). The bare `<itemVar>` does NOT survive an executor round-trip (the
/// lifecycle's `t_success` keeps only `_`-prefixed leaves), so a downstream
/// itemVar consumer in the same body (a Decision guard, a SubWorkflow input
/// mapping) would be stranded. The preserved `_`-prefixed `__map_item` copy DOES
/// survive, so we restore the bare `<itemVar>` from it here — keeping the
/// analyzer's "itemVar is always token-resident in the body" invariant true at
/// runtime. The data plane stays parked; this is one small routing leaf, not the
/// business payload.
pub(crate) fn yield_logic_keeping_item(item_var: &str) -> String {
    // itemVar names are validated identifiers, but escape defensively.
    let iv = item_var.replace('\\', "\\\\").replace('"', "\\\"");
    format!(
        "let d = tok; let c = #{{}}; \
         for k in d.keys() {{ if k.starts_with(\"_\") || k == \"task_id\" || k == \"status\" \
         {{ c[k] = d[k]; }} }} \
         let __it = d[\"__map_item\"]; if __it != () {{ c[\"{iv}\"] = __it; }} \
         #{{ data: d, ctrl: c }}"
    )
}

impl ScalarTy {
    fn to_field_kind(&self) -> FieldKind {
        match self {
            ScalarTy::String => FieldKind::Text,
            ScalarTy::Number => FieldKind::Number,
            ScalarTy::Bool => FieldKind::Bool,
            ScalarTy::FileRef => FieldKind::File,
            ScalarTy::Timestamp => FieldKind::Timestamp,
            ScalarTy::Json => FieldKind::Json,
        }
    }
}

/// Per-node inbound scope as `top-level field → FieldKind`, derived from the
/// shape-aware model (the single source of truth). Replaces the old flat
/// `compute_scopes`. Nested objects collapse to `Json` (the Python stub
/// generator wants valid identifiers; deeper typed nesting is a follow-up).
/// Keyed by node id.
pub fn node_input_field_kinds(
    graph: &WorkflowGraph,
) -> Result<std::collections::HashMap<String, BTreeMap<String, FieldKind>>, CompileError> {
    let report = analyze(graph, &Default::default())?;
    let mut out = std::collections::HashMap::new();
    for (nid, shape) in &report.node_in {
        let mut m: BTreeMap<String, FieldKind> = BTreeMap::new();
        if let TokenShape::Object(map) = shape {
            for (k, f) in map {
                let kind = match &f.shape {
                    TokenShape::Scalar(s) => s.to_field_kind(),
                    _ => FieldKind::Json,
                };
                m.insert(k.clone(), kind);
            }
        }
        out.insert(nid.clone(), m);
    }
    // Unreachable nodes still need an entry (callers `.get().unwrap_or_default`).
    for n in &graph.nodes {
        out.entry(n.id.clone()).or_default();
    }
    Ok(out)
}

// ─── Borrow planners (moved) ─────────────────────────────────────────────────
// `ReadArcBind`, `guard_readarc_plan`, `AutomatedStepDataBorrow`,
// `automated_step_borrow_plan`, `AutomatedStepResourceBorrow`,
// `automated_step_resource_borrow_plan`, `HumanTaskDataBorrow`,
// `human_task_borrow_plan`, and `resolve_backend_ref` live under
// `crate::compiler::borrow::planners`. Re-exported here so external callers
// (notably `crate::compiler::validate`) keep working with the same path.

// `guard_readarc_plan` is consumed by `crate::compiler::validate` via this
// re-export — kept in non-test builds. The other planners are referenced
// only by this module's own tests; gate them on `cfg(test)` to avoid
// dead-import warnings in non-test builds.
pub(crate) use crate::compiler::borrow::planners::guard::guard_readarc_plan;

#[cfg(test)]
pub(crate) use crate::compiler::borrow::planners::automated_step::{
    automated_step_borrow_plan, AutomatedStepDataBorrow,
};
#[cfg(test)]
pub(crate) use crate::compiler::borrow::planners::human_task::human_task_borrow_plan;

/// Per-node, per-slug field map — the picker model pivoted from a flat
/// list to `slug → fields`. Drives the Python `.pyi` overlay's one
/// `class _<Slug>NS:` per upstream producer so the IDE autocompletes
/// `review.invoice_amount` against the same shape the borrow planner
/// will resolve at compile time.
///
/// Skips entries that aren't slug-qualified (the legacy `input.<path>`
/// control-token references and the synthetic `Process` bucket — those
/// are emitted as direct `Token` class attrs in the existing flat path,
/// not as their own namespace).
#[allow(clippy::type_complexity)]
pub fn node_namespace_scopes(
    graph: &WorkflowGraph,
) -> Result<
    std::collections::HashMap<String, BTreeMap<String, BTreeMap<String, FieldKind>>>,
    CompileError,
> {
    let report = analyze(graph, &Default::default())?;
    let slugs = slug_index(graph)?;
    let mut out: std::collections::HashMap<String, BTreeMap<String, BTreeMap<String, FieldKind>>> =
        std::collections::HashMap::new();
    for (node_id, entries) in &report.scopes {
        let mut by_slug: BTreeMap<String, BTreeMap<String, FieldKind>> = BTreeMap::new();
        for e in entries {
            if e.path.starts_with("input.") || e.producer_label == "Process" {
                continue;
            }
            // Prefer the slug index over splitting the path — keeps this
            // robust when a producer's slug differs from the path prefix
            // (e.g. a future collision-suffix rule).
            let slug = slugs
                .slug_for(&e.producer_node)
                .map(str::to_string)
                .unwrap_or_else(|| {
                    e.path
                        .split_once('.')
                        .map(|(s, _)| s.to_string())
                        .unwrap_or_default()
                });
            if slug.is_empty() {
                continue;
            }
            let field_path = e.path.strip_prefix(&format!("{slug}.")).unwrap_or(&e.path);
            let leaf = field_path
                .split('.')
                .next()
                .unwrap_or(field_path)
                .to_string();
            if leaf.is_empty() {
                continue;
            }
            let kind = ty_label_to_field_kind(&e.ty.kind_label());
            by_slug.entry(slug).or_default().insert(leaf, kind);
        }
        out.insert(node_id.clone(), by_slug);
    }
    // Unreachable nodes still need an entry (callers may .get().unwrap_or_default).
    for n in &graph.nodes {
        out.entry(n.id.clone()).or_default();
    }
    Ok(out)
}

fn ty_label_to_field_kind(ty: &str) -> FieldKind {
    match ty {
        "Number" => FieldKind::Number,
        "Boolean" | "Bool" => FieldKind::Bool,
        "Json" | "Object" | "Array" | "Any" => FieldKind::Json,
        _ => FieldKind::Text,
    }
}
