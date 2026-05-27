//! Compiler-derived, shape-aware token model — the **native, only**
//! representation of "what does the token look like here". This is no longer a
//! prototype: the old flat scope model (Rust `validate.rs::compute_scopes`,
//! the TS `guard-scope.ts::computeScopes`) is **deleted**; `compile_to_air`
//! itself emits the control/data split natively via
//! `compile.rs::apply_control_data_foundation`.
//!
//! Full architecture narrative: `docs/10-control-data-token-model.md`.
//! Supersedes parts of `docs/05-typed-ports.md` and
//! `docs/07-runtime-port-enforcement.md`.
//!
//! # The model (control token vs data token)
//!
//! A node's business output is **parked**, write-once, in a `p_{id}_data`
//! place; only a slim **control token** (`_`-prefixed metadata, `task_id`,
//! `status`, loop counter) is threaded by-move down the net. A guard / result
//! mapping that needs an upstream field gets a non-consuming **read-arc**
//! (`ScenarioArc{read:true}`) into the owning parked place. This is Rust's
//! ownership model: parked data ≡ a `let` owned by the place; a read-arc ≡ a
//! `&T` shared borrow; a consuming arc ≡ a move; the control token ≡ a
//! `let mut` threaded by-move. The compiler is the **borrow-checker**:
//! provenance proves which parked place owns a referenced field and
//! synthesizes the borrow; a reference nothing reachable owns is a hard
//! `CompileError`, not a silently-missed branch (the original bug class).
//!
//! Lowering (`lower.rs`): data-yielders (HumanTask / AutomatedStep) →
//! `split_outputs` (`p_{id}_data` parked + slim `p_{id}_ctrl` +
//! `t_{id}_yield` running [`YIELD_LOGIC`]). Start → `park_outputs`, an
//! *additive* fork (`p_{id}_data` for downstream borrows **plus**
//! `p_{id}_main` carrying the full token onward, so the next task can still
//! interpolate Start fields off the control token) — not a split.
//!
//! # References & scope (single source of truth)
//!
//! Borrowed data is addressed `<slug>.<field>`, where `slug` is the
//! producer node's user-defined, Rhai-safe key ([`slug_index`];
//! explicit collisions → [`CompileError::SlugConflict`]; unset slugs derive a
//! collision-suffixed default). `input.<path>` is reserved for genuinely
//! control-token-resident leaves (Start fields before any task, `_loop_*`,
//! `task_id`, `status`); control/identity leaves are attributed to a
//! synthetic "Process" group, not whichever node last forwarded the token.
//! Clean-cut: there is no legacy unqualified-`input` nearest-wins fallback.
//!
//! One resolver — [`guard_refs`] (scanner + `rhai_scope` gating) →
//! `resolve_ref` — is shared by [`reachable_scope`] (the editor picker),
//! `check_guard` (diagnostics) and [`guard_readarc_plan`] (read-arc
//! synthesis), so the picker offers exactly what the compiler binds and no
//! diagnostic contradicts it. Distinct producers of the same key are distinct
//! paths (`review.amount` vs `compliance.amount`) — no silent collision.
//!
//! # Runtime enforcement
//!
//! [`analyze`] derives a structural [`TokenShape`] per node; [`to_json_schema`]
//! lowers it to real AIR `#/definitions/*` (`Data__{id}` enforced producer
//! shape, `Ctrl__{id}` open object, `DynamicToken` permissive catch-all). The
//! engine `SchemaRegistry` validates every token crossing a schemed
//! place/port; an unresolvable `$ref` *fails* (not bypasses). [`analyze`] is
//! pure; [`compile_to_air_with_shapes`] additionally returns the
//! [`ShapeReport`] and annotates the AIR. The editor consumes the same
//! `analyze` via `surface_types` → `POST /api/v1/analyze`.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::compiler::error::CompileError;
use crate::compiler::graph::{topo_order, WorkflowDiGraph};
use crate::models::template::{
    FieldKind, JoinMode, MergeStrategy, Port, WorkflowGraph, WorkflowNode, WorkflowNodeData,
};

// ─── Structural token type ──────────────────────────────────────────────────

/// Leaf type of a token field. Deliberately small — the point is to model
/// *where a value lives and roughly what it is*, not a full JSON Schema.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScalarTy {
    String,
    Number,
    Bool,
    /// Catalogue file reference — at runtime an object `{ url, filename,
    /// content_type, .. }`, but commonly addressed as a scalar handle.
    FileRef,
    Timestamp,
    /// Opaque / dynamic — the `Json` escape-hatch field kind.
    Json,
}

impl ScalarTy {
    fn from_kind(k: FieldKind) -> ScalarTy {
        match k {
            FieldKind::Text
            | FieldKind::Textarea
            | FieldKind::Select
            | FieldKind::Signature => ScalarTy::String,
            FieldKind::Number => ScalarTy::Number,
            FieldKind::Bool => ScalarTy::Bool,
            FieldKind::File => ScalarTy::FileRef,
            FieldKind::Timestamp => ScalarTy::Timestamp,
            FieldKind::Json => ScalarTy::Json,
        }
    }

    pub(crate) fn label(&self) -> &'static str {
        match self {
            ScalarTy::String => "String",
            ScalarTy::Number => "Number",
            ScalarTy::Bool => "Bool",
            ScalarTy::FileRef => "FileRef",
            ScalarTy::Timestamp => "Timestamp",
            ScalarTy::Json => "Json",
        }
    }
}

/// Structural shape of a token (or sub-tree of one).
#[derive(Debug, Clone)]
pub enum TokenShape {
    Object(BTreeMap<String, Field>),
    Array(Box<TokenShape>),
    Scalar(ScalarTy),
    /// Unconstrained — an empty declared port ("accepts any token").
    Any,
    /// A modelled-but-deliberately-unexpanded envelope internal (e.g. an
    /// executor's `detail`: real, typed elsewhere, but not worth reproducing
    /// here). Carries a human note.
    Opaque(String),
}

/// A field of an [`TokenShape::Object`]: its shape plus *where it came from*.
#[derive(Debug, Clone)]
pub struct Field {
    pub shape: TokenShape,
    pub prov: Provenance,
}

/// Why/where a field exists — the thing the flat model throws away.
#[derive(Debug, Clone)]
pub struct Provenance {
    pub node_id: String,
    pub node_label: String,
    pub note: String,
    /// Set when the field is *both* a recursable container AND a usable
    /// scalar leaf of the given type — currently only File envelopes (the
    /// declared port-level `FieldKind::File`, which is a `FileRef` handle
    /// in its own right *and* exposes `{url, filename, content_type}`
    /// subkeys at runtime). `collect_leaves` emits the container path with
    /// this scalar type *and* continues into the children.
    pub anchor: Option<ScalarTy>,
}

impl Provenance {
    fn new(node: &WorkflowNode, note: impl Into<String>) -> Provenance {
        Provenance {
            node_id: node.id.clone(),
            node_label: node.data.label().to_string(),
            note: note.into(),
            anchor: None,
        }
    }

    fn with_anchor(mut self, anchor: ScalarTy) -> Provenance {
        self.anchor = Some(anchor);
        self
    }
}

impl TokenShape {
    fn object() -> TokenShape {
        TokenShape::Object(BTreeMap::new())
    }

    fn insert(&mut self, key: &str, shape: TokenShape, prov: Provenance) {
        if let TokenShape::Object(map) = self {
            map.insert(key.to_string(), Field { shape, prov });
        }
    }

    /// Shallow last-wins merge of `other` into `self` — mirrors the runtime
    /// `for k in signal.keys() { result[k] = signal[k] }` and the
    /// `ShallowLastWins` join. `DeepMerge` recurses on nested objects.
    fn merge_from(&mut self, other: &TokenShape, deep: bool) {
        match (self, other) {
            (TokenShape::Object(a), TokenShape::Object(b)) => {
                for (k, vf) in b {
                    match (deep, a.get_mut(k)) {
                        (true, Some(existing))
                            if matches!(existing.shape, TokenShape::Object(_))
                                && matches!(vf.shape, TokenShape::Object(_)) =>
                        {
                            existing.shape.merge_from(&vf.shape, true);
                        }
                        _ => {
                            a.insert(k.clone(), vf.clone());
                        }
                    }
                }
            }
            (slot, other) => {
                // Non-object on either side: last value wins (runtime parity).
                *slot = other.clone();
            }
        }
    }

    /// Resolve a dotted path (the segments *after* `input`). Returns the
    /// matched field's shape + provenance, or `None` if any segment is absent.
    pub(crate) fn resolve<'a>(&'a self, segs: &[String]) -> Option<(&'a TokenShape, Option<&'a Provenance>)> {
        let mut cur = self;
        let mut prov: Option<&Provenance> = None;
        for seg in segs {
            match cur {
                TokenShape::Object(map) => {
                    let f = map.get(seg)?;
                    cur = &f.shape;
                    prov = Some(&f.prov);
                }
                // Can't walk into a scalar/opaque/any/array by key.
                _ => return None,
            }
        }
        Some((cur, prov))
    }

    /// Depth-first search for any leaf whose *final* path segment equals
    /// `name`. Used to suggest "did you mean …" when a guard ref is
    /// unresolved. Returns (dotted_path, scalar/shape label, provenance).
    pub(crate) fn find_by_leaf(&self, name: &str) -> Option<(String, String, Provenance)> {
        fn walk(
            shape: &TokenShape,
            prefix: &str,
            name: &str,
        ) -> Option<(String, String, Provenance)> {
            if let TokenShape::Object(map) = shape {
                for (k, f) in map {
                    let path = if prefix.is_empty() {
                        k.clone()
                    } else {
                        format!("{prefix}.{k}")
                    };
                    if k == name {
                        return Some((path, f.shape.kind_label(), f.prov.clone()));
                    }
                    if let Some(hit) = walk(&f.shape, &path, name) {
                        return Some(hit);
                    }
                }
            }
            None
        }
        walk(self, "", name)
    }

    pub(crate) fn kind_label(&self) -> String {
        match self {
            TokenShape::Object(_) => "Object".to_string(),
            TokenShape::Array(_) => "Array".to_string(),
            TokenShape::Scalar(s) => s.label().to_string(),
            TokenShape::Any => "Any".to_string(),
            TokenShape::Opaque(n) => format!("Opaque({n})"),
        }
    }

    /// Structural shape → JSON Schema for the engine's `SchemaRegistry`.
    ///
    /// Known fields get a type constraint; `additionalProperties` stays open
    /// and nothing is `required`, so the schema *validates the shape we know*
    /// without rejecting extra/optional keys (the executor envelope, metadata
    /// the lowering stamps on, etc.). `Opaque`/`Any`/`FileRef` are permissive
    /// `{}` — undeclared executor outputs and catalogue refs must not be
    /// rejected (the declared→enforced ramp tightens these later).
    ///
    /// Scalar types also accept `null` — the executor backends (LLM,
    /// Python) legitimately set declared-but-unset optional outputs to
    /// `null` so downstream consumers (Python `<slug>.<field>` access)
    /// see `None` instead of `AttributeError`. Without the `null`
    /// alternative, the engine's `t_<id>_yield` schema validator rejects
    /// the parked envelope on the first nullable scalar and the whole
    /// instance fails.
    pub fn to_json_schema(&self) -> Value {
        match self {
            TokenShape::Object(map) => {
                let mut props = serde_json::Map::new();
                for (k, f) in map {
                    props.insert(k.clone(), f.shape.to_json_schema());
                }
                serde_json::json!({
                    "type": "object",
                    "properties": Value::Object(props),
                    "additionalProperties": true
                })
            }
            TokenShape::Array(inner) => serde_json::json!({
                "type": "array",
                "items": inner.to_json_schema()
            }),
            TokenShape::Scalar(ScalarTy::Number) => {
                serde_json::json!({ "type": ["number", "null"] })
            }
            TokenShape::Scalar(ScalarTy::Bool) => {
                serde_json::json!({ "type": ["boolean", "null"] })
            }
            TokenShape::Scalar(ScalarTy::String) | TokenShape::Scalar(ScalarTy::Timestamp) => {
                serde_json::json!({ "type": ["string", "null"] })
            }
            // FileRef is a catalogue handle (string or object at runtime),
            // Json/Any/Opaque are deliberately unconstrained.
            TokenShape::Scalar(ScalarTy::FileRef)
            | TokenShape::Scalar(ScalarTy::Json)
            | TokenShape::Any
            | TokenShape::Opaque(_) => serde_json::json!({}),
        }
    }

    /// Pretty multi-line render for the demo report.
    fn render(&self, indent: usize) -> String {
        let pad = "  ".repeat(indent);
        match self {
            TokenShape::Object(map) if map.is_empty() => "{}".to_string(),
            TokenShape::Object(map) => {
                let mut s = String::from("{\n");
                for (k, f) in map {
                    s.push_str(&format!(
                        "{pad}  {k}: {}{}\n",
                        f.shape.render(indent + 1),
                        format_args!("   « {} »", f.prov.note),
                    ));
                }
                s.push_str(&format!("{pad}}}"));
                s
            }
            TokenShape::Array(inner) => format!("[{}]", inner.render(indent)),
            TokenShape::Scalar(t) => t.label().to_string(),
            TokenShape::Any => "Any".to_string(),
            TokenShape::Opaque(n) => format!("Opaque<{n}>"),
        }
    }
}

// Shared schema-definition vocabulary so lowering (WS2) and the read-arc
// synthesis phase (WS3) agree on `#/definitions/*` names. Node ids can contain
// `-` (e.g. `check-amount`); JSON-pointer definition keys allow it.

/// Definition name for a data-yielding node's parked data token.
pub fn data_def_name(node_id: &str) -> String {
    format!("Data__{node_id}")
}

/// Definition name for a node's slim control token.
pub fn ctrl_def_name(node_id: &str) -> String {
    format!("Ctrl__{node_id}")
}

/// `#/definitions/<name>` ref for a definition name.
pub fn def_ref(name: &str) -> String {
    format!("#/definitions/{name}")
}

/// Permissive catch-all definition every non-split place/port keeps using so
/// the `SchemaRegistry` resolves their `#/definitions/DynamicToken` ref
/// (unresolvable refs *fail* validation) while constraining nothing.
pub fn dynamic_token_definition() -> (String, Value) {
    ("DynamicToken".to_string(), serde_json::json!({}))
}

fn port_to_shape(port: &Port, node: &WorkflowNode, note: &str) -> TokenShape {
    let mut o = TokenShape::object();
    for f in &port.fields {
        let (shape, prov) = match f.kind {
            // A File field is *both* a `FileRef` scalar handle (what
            // Kreuzberg/LLM consume via `{{ <slug>.<file> }}`) AND an
            // object exposing `{url, filename, content_type}` subkeys
            // (what HumanTask blocks interpolate via `{{ <slug>.<file>.filename }}`).
            // The outer field's provenance carries `anchor = FileRef` so
            // `collect_leaves` emits both the container leaf and its
            // children — and the picker can offer the full nested family.
            FieldKind::File => {
                let mut fo = TokenShape::object();
                let p = Provenance::new(node, "uploaded file (catalogue reference)");
                fo.insert("url", TokenShape::Scalar(ScalarTy::String), p.clone());
                fo.insert("filename", TokenShape::Scalar(ScalarTy::String), p.clone());
                fo.insert("content_type", TokenShape::Scalar(ScalarTy::String), p);
                (fo, Provenance::new(node, note).with_anchor(ScalarTy::FileRef))
            }
            k => (
                TokenShape::Scalar(ScalarTy::from_kind(k)),
                Provenance::new(node, note),
            ),
        };
        o.insert(&f.name, shape, prov);
    }
    o
}

/// A strict, SSOT-derived type violation of a declared port contract.
///
/// Complements [`Port::validate_token`], which is *lenient* for `File`/`Json`
/// (a `file` field accepts a bare string). This carries the typed shape the
/// foundation derives via [`port_to_shape`] — the same shape
/// [`TokenShape::to_json_schema`] feeds the engine's strict `Data__*`
/// schemas — so the trigger boundary can reject exactly what the net would
/// reject deep inside (e.g. a `file` field arriving as `"example"`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortShapeViolation {
    pub field: String,
    pub expected: String,
    pub actual: String,
}

fn json_kind(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// Validate `token` against `port`'s declared, SSOT-typed shape.
///
/// This is the strict counterpart of [`Port::validate_token`]: it consumes the
/// foundation's own [`port_to_shape`] (the *single* place a `file` field is
/// defined as an object and a scalar field as its [`ScalarTy`]) rather than
/// reimplementing field-kind logic. Only *present, non-null* fields are
/// type-checked — required/absent stays [`Port::validate_token`]'s job, and
/// permissiveness mirrors [`TokenShape::to_json_schema`] (per-field top-level
/// type only; extra keys allowed; `Json`/`Any`/`Opaque` are escape hatches).
/// Returns the first mismatch with an actionable, field-named message.
pub fn validate_token_against_port(
    port: &Port,
    node: &WorkflowNode,
    token: &Value,
) -> Result<(), PortShapeViolation> {
    let TokenShape::Object(fields) = port_to_shape(port, node, "declared port field") else {
        return Ok(());
    };
    let Some(obj) = token.as_object() else {
        return Err(PortShapeViolation {
            field: port.id.clone(),
            expected: "object".to_string(),
            actual: json_kind(token).to_string(),
        });
    };
    for (name, f) in &fields {
        let Some(v) = obj.get(name) else {
            continue; // absent — required/missing is `validate_token`'s job
        };
        if v.is_null() {
            continue; // null — treated as absent (parity with `validate_token`)
        }
        let ok = match &f.shape {
            TokenShape::Object(_) => v.is_object(),
            TokenShape::Array(_) => v.is_array(),
            TokenShape::Scalar(ScalarTy::Number) => v.is_number(),
            TokenShape::Scalar(ScalarTy::Bool) => v.is_boolean(),
            TokenShape::Scalar(ScalarTy::String)
            | TokenShape::Scalar(ScalarTy::Timestamp) => v.is_string(),
            // Escape hatches — deliberately unconstrained, exactly as
            // `to_json_schema` emits `{}` for these.
            TokenShape::Scalar(ScalarTy::FileRef)
            | TokenShape::Scalar(ScalarTy::Json)
            | TokenShape::Any
            | TokenShape::Opaque(_) => true,
        };
        if !ok {
            let expected = match &f.shape {
                // `port_to_shape` maps a `file` field to this object triplet.
                TokenShape::Object(_) => {
                    "file reference object { url, filename, content_type }".to_string()
                }
                TokenShape::Array(_) => "array".to_string(),
                TokenShape::Scalar(s) => s.label().to_ascii_lowercase(),
                _ => "any".to_string(),
            };
            return Err(PortShapeViolation {
                field: name.clone(),
                expected,
                actual: json_kind(v).to_string(),
            });
        }
    }
    Ok(())
}

// ─── Per-node shape derivation ──────────────────────────────────────────────

/// One reachable, still-live reference the editor variable picker should
/// offer at a node — the producer-namespaced replacement for the flat TS
/// `computeScopes`. The `path` is what you'd actually type in a guard; the
/// producer attribution is the thing the flat model throws away.
#[derive(Debug, Clone)]
pub struct ScopeEntry {
    pub path: String,
    pub ty: String,
    pub producer_node: String,
    pub producer_label: String,
    pub note: String,
}

/// Result of [`analyze`]: the derived shapes, the per-node scope surface, and
/// guard diagnostics.
pub struct ShapeReport {
    /// Shape of the token *arriving* at each node (keyed by node id).
    pub node_in: BTreeMap<String, TokenShape>,
    /// Shape of the token each node *emits* downstream (keyed by node id).
    pub node_out: BTreeMap<String, TokenShape>,
    /// AIR place id → derived shape (the `token_schema` replacement).
    pub place_schemas: BTreeMap<String, String>,
    /// node id → the reference list the editor picker should show there.
    pub scopes: BTreeMap<String, Vec<ScopeEntry>>,
    pub diagnostics: Vec<ShapeDiagnostic>,
}

#[derive(Debug, Clone)]
pub enum ShapeDiagnostic {
    /// A guard references `input.<path>` that *was produced upstream* but is
    /// not present here because an intermediate node dropped it. Carries the
    /// full lineage + Petri-aware fixes — the decisive diagnostic.
    DroppedUpstream {
        node_id: String,
        node_label: String,
        guard: String,
        referenced: String,
        produced_by: String,
        produced_label: String,
        produced_path: String,
        produced_ty: String,
        dropped_by: Option<String>,
        drop_reason: String,
        fixes: Vec<String>,
    },
    /// A guard references `input.<path>` that no upstream node ever produced.
    UnresolvedGuardPath {
        node_id: String,
        node_label: String,
        guard: String,
        referenced: String,
    },
    /// The path resolves, but its scalar type can't satisfy the comparison
    /// it is used in (e.g. a `String` field compared `> 5000`).
    GuardTypeMismatch {
        node_id: String,
        node_label: String,
        guard: String,
        referenced: String,
        found: String,
        note: String,
    },
    /// The draft graph isn't structurally analyzable yet (no Start, a cycle,
    /// dangling edge). Reported instead of erroring so the editor still gets
    /// a response on every keystroke.
    GraphIncomplete { message: String },
}

impl ShapeDiagnostic {
    /// Flatten to `(kind, node_id, human message)` for the editor endpoint —
    /// the editor highlights `node_id` and shows `message`.
    pub fn dto(&self) -> (&'static str, String, String) {
        match self {
            ShapeDiagnostic::DroppedUpstream {
                node_id,
                referenced,
                produced_label,
                produced_path,
                produced_ty,
                drop_reason,
                ..
            } => (
                "dropped_upstream",
                node_id.clone(),
                format!(
                    "`{referenced}` is not present here. It is produced by \
                     '{produced_label}' as `{produced_path}: {produced_ty}` but \
                     {drop_reason}."
                ),
            ),
            ShapeDiagnostic::UnresolvedGuardPath {
                node_id,
                referenced,
                ..
            } => (
                "unresolved",
                node_id.clone(),
                format!("`{referenced}` is produced by no upstream node."),
            ),
            ShapeDiagnostic::GuardTypeMismatch {
                node_id,
                referenced,
                found,
                note,
                ..
            } => (
                "type_mismatch",
                node_id.clone(),
                format!("`{referenced}` is `{found}` but {note}."),
            ),
            ShapeDiagnostic::GraphIncomplete { message } => {
                ("graph_incomplete", String::new(), message.clone())
            }
        }
    }
}

/// Map a node to the AIR place id its inbound token lands on.
///
/// KNOWN LIMITATION (prototype): `compile_to_air` step 9 (`apply_merges` +
/// `resolve_aliases`) folds pass-through `p_{id}_input` places into the
/// upstream output place, so for routing nodes (Decision, Loop, …) the
/// `_input` id below does NOT survive into the final AIR. The production
/// version must derive shapes *inside* the compiler and resolve every place
/// through the same `fixups.merges` alias table the lowerer builds. We keep
/// the input mapping anyway (harmless — `annotate_air` only touches places
/// that exist) and additionally map the **output** places, which survive
/// merges and are what downstream consumers/guards actually read from.
fn input_place_id(node: &WorkflowNode) -> Option<String> {
    match &node.data {
        WorkflowNodeData::Start { .. } => Some(format!("p_{}_ready", node.id)),
        WorkflowNodeData::End { .. } => Some(format!("p_{}_done", node.id)),
        WorkflowNodeData::Scope { .. } | WorkflowNodeData::Trigger { .. } => None,
        _ => Some(format!("p_{}_input", node.id)),
    }
}

/// AIR place ids that carry this node's *outbound* token and survive the
/// merge pass (verified against the compiled invoice net). These are the
/// robust attachment points for the derived schema.
fn output_place_ids(node: &WorkflowNode) -> Vec<String> {
    let id = &node.id;
    match &node.data {
        // Start forks (`park_outputs`): the unchanged token continues on
        // `p_{id}_main`; `p_{id}_data` is schema'd by the foundation pass.
        WorkflowNodeData::Start { .. } => vec![format!("p_{id}_main")],
        WorkflowNodeData::HumanTask { .. } => vec![format!("p_{id}_output")],
        WorkflowNodeData::AutomatedStep { .. }
        | WorkflowNodeData::SubWorkflow { .. } => {
            vec![format!("p_{id}_output"), format!("p_{id}_error")]
        }
        WorkflowNodeData::Decision {
            conditions,
            default_branch,
            ..
        } => {
            let mut v: Vec<String> = (0..conditions.len())
                .map(|i| format!("p_{id}_out_{i}"))
                .collect();
            if default_branch.is_some() {
                v.push(format!("p_{id}_out_default"));
            }
            v
        }
        WorkflowNodeData::ParallelSplit { .. } => {
            // One out place per outgoing edge; enumerate generously, missing
            // ids are simply skipped by `annotate_air`.
            (0..8).map(|i| format!("p_{id}_out_{i}")).collect()
        }
        WorkflowNodeData::ParallelJoin { .. } => vec![format!("p_{id}_output")],
        WorkflowNodeData::Join { .. } => vec![format!("p_{id}_output")],
        WorkflowNodeData::Loop { .. } => vec![
            format!("p_{id}_body_in"),
            format!("p_{id}_body_out"),
            format!("p_{id}_output"),
        ],
        _ => vec![],
    }
}

/// The token a node emits downstream, given the token arriving at it.
/// This is the heart of the prototype: each arm encodes the *verified* JSON
/// transformation the corresponding `lower_*` performs.
fn out_shape(node: &WorkflowNode, in_shape: &TokenShape) -> TokenShape {
    match &node.data {
        // Start emits its declared `initial` port + the instance-seeded
        // `_instance_id`, plus `_process_name` when a process is registered.
        WorkflowNodeData::Start {
            initial,
            process_name,
            ..
        } => {
            let mut o = port_to_shape(initial, node, "Start input field (declared `initial` port)");
            o.insert(
                "_instance_id",
                TokenShape::Scalar(ScalarTy::String),
                Provenance::new(node, "injected at instance creation (seed)"),
            );
            if process_name.is_some() {
                o.insert(
                    "_process_name",
                    TokenShape::Scalar(ScalarTy::String),
                    Provenance::new(node, "process-name interpolation (t_*_proc_name)"),
                );
            }
            o
        }

        // Human task: `t_*_finalize` runs `build_merge_logic("state","signal")`
        // = `for k in signal.keys() { result[k] = signal[k] }`. The signal is
        // a `HumanTaskResponse` whose **form submission is nested under
        // `.data`** (effect_tokens.rs:365 — "The `data` field contains the
        // form submission"). So the output is the inbound token PLUS the
        // response envelope, with the user-entered fields under `data`. This
        // is the divergence the flat editor model erases.
        WorkflowNodeData::HumanTask { steps, .. } => {
            let mut o = in_shape.clone();
            o.insert(
                "task_id",
                TokenShape::Scalar(ScalarTy::String),
                Provenance::new(node, "human-task correlation id (HumanTaskResponse)"),
            );
            o.insert(
                "status",
                TokenShape::Scalar(ScalarTy::String),
                Provenance::new(node, "human-task outcome (HumanTaskResponse.status)"),
            );
            let form = port_to_shape(
                &crate::models::template::derive_human_task_output_port(steps),
                node,
                "HUMAN-TASK FORM FIELD — nested under `data` (HumanTaskResponse.data)",
            );
            o.insert(
                "data",
                form,
                Provenance::new(
                    node,
                    "form submission envelope — every form field lives in here",
                ),
            );
            // The request-injection (`build_human_task_injection_logic`) and
            // the human result listener also stamp these onto the token; model
            // them so the derived schema matches the observed live token.
            for (k, note) in [
                ("title", "human-task request scaffold"),
                ("instructions_mdsvex", "human-task request scaffold"),
                ("place", "human-task response envelope"),
                ("net_id", "human-task response envelope"),
                ("response_subject", "human-task response envelope"),
                ("completed_at", "human-task response envelope"),
            ] {
                o.insert(
                    k,
                    TokenShape::Scalar(ScalarTy::String),
                    Provenance::new(node, note),
                );
            }
            o.insert(
                "steps",
                TokenShape::Array(Box::new(TokenShape::Any)),
                Provenance::new(node, "human-task request scaffold"),
            );
            o
        }

        // Automated step: `prepare` snapshots the inbound token into
        // `spec.inputs["input.json"]` and the node forwards the **executor
        // result envelope** (`executor_lifecycle` → `to_output` = `#{ output:
        // done }`). The upstream business token is NOT propagated — anything
        // downstream sees `{ execution_id, job_id, run, status, source,
        // detail{ outputs, .. } }`. Business output (if the step declares an
        // output port) is under `detail.outputs`, never flattened back.
        WorkflowNodeData::AutomatedStep { .. } => {
            let mut o = TokenShape::object();
            let p = |n: &str| Provenance::new(node, n);
            o.insert(
                "execution_id",
                TokenShape::Scalar(ScalarTy::String),
                p("executor envelope"),
            );
            o.insert(
                "job_id",
                TokenShape::Scalar(ScalarTy::String),
                p("executor envelope"),
            );
            o.insert(
                "run",
                TokenShape::Scalar(ScalarTy::Number),
                p("executor envelope"),
            );
            o.insert(
                "status",
                TokenShape::Scalar(ScalarTy::String),
                p("executor envelope"),
            );
            o.insert(
                "source",
                TokenShape::Scalar(ScalarTy::String),
                p("executor envelope"),
            );
            // Declared success output port → detail.outputs; else opaque.
            let outputs = match node.data.output_ports().into_iter().next() {
                Some(port) if !port.fields.is_empty() => port_to_shape(
                    &port,
                    node,
                    "declared automated-step output (under detail.outputs)",
                ),
                _ => TokenShape::Opaque("executor outputs (undeclared)".to_string()),
            };
            let mut detail = TokenShape::object();
            detail.insert(
                "outputs",
                outputs,
                p("executor result — business output lives HERE, not at top level"),
            );
            detail.insert(
                "exit_code",
                TokenShape::Scalar(ScalarTy::Number),
                p("executor envelope"),
            );
            o.insert(
                "detail",
                detail,
                p("executor result envelope — upstream token was consumed into spec.inputs"),
            );
            o
        }

        // Loop: `t_*_enter` injects a declared `<slug>: { iteration: 0 }`
        // namespace on the control token; body re-entry increments
        // `<slug>.iteration`; the exit arm forwards the token unchanged so
        // post-loop nodes can still read the final count. The namespace is
        // first-class — `node_output_fields` declares `iteration: number`,
        // the picker / `.pyi` overlay surface it as `<slug>.iteration`, the
        // runner auto-promotes `<slug>` as a Python global, and Rhai
        // expressions in `loopCondition` / guards / End mappings reference it
        // as `input.<slug>.iteration` (or `<slug>.iteration` for the
        // slug-borrow rewrite path).
        WorkflowNodeData::Loop { .. } => {
            let mut o = in_shape.clone();
            let mut ns = TokenShape::object();
            ns.insert(
                "iteration",
                TokenShape::Scalar(ScalarTy::Number),
                Provenance::new(node, "loop iteration counter (declared producer field)"),
            );
            o.insert(
                &node.slug(),
                ns,
                Provenance::new(node, "loop namespace (`<slug>.iteration`)"),
            );
            o
        }

        // Sub-workflow: `t_*_join` maps the child's terminal result onto the
        // workflow token via the declared `output` port. With declared fields
        // downstream sees exactly those; otherwise the child result is opaque
        // here (we can't see across the spawned-child boundary at analyze time).
        WorkflowNodeData::SubWorkflow { output, .. } => {
            if output.fields.is_empty() {
                in_shape.clone()
            } else {
                port_to_shape(
                    output,
                    node,
                    "declared sub-workflow result (mapped at t_*_join)",
                )
            }
        }

        // Pure routing / pass-through patterns: token shape unchanged.
        WorkflowNodeData::Decision { .. }
        | WorkflowNodeData::ParallelSplit { .. }
        | WorkflowNodeData::ParallelJoin { .. }
        | WorkflowNodeData::Join { .. }
        | WorkflowNodeData::Scope { .. }
        | WorkflowNodeData::PhaseUpdate { .. }
        | WorkflowNodeData::ProgressUpdate { .. }
        | WorkflowNodeData::Failure { .. }
        | WorkflowNodeData::Trigger { .. } => in_shape.clone(),

        WorkflowNodeData::End { .. } => in_shape.clone(),
    }
}

/// Compute inbound + outbound shapes for every node, then validate guards
/// against the *real* inbound shape.
pub fn analyze(graph: &WorkflowGraph) -> Result<ShapeReport, CompileError> {
    use crate::compiler::borrow::planners::guard::{check_guard, reachable_scope};

    let wg = WorkflowDiGraph::build(graph)?;
    let order = topo_order(&wg)?;
    // Author-facing `<slug>.<field>` namespace — built once; a hard
    // `SlugConflict` here propagates out (the editor renders it via
    // `surface_types`'s `GraphIncomplete`, publish blocks via `validate_guards`).
    let slugs = slug_index(graph)?;

    let mut node_in: BTreeMap<String, TokenShape> = BTreeMap::new();
    let mut node_out: BTreeMap<String, TokenShape> = BTreeMap::new();

    for ni in &order {
        let node = *wg.dag.node_weight(*ni).unwrap();

        // Inbound = shallow-merge of every DAG predecessor's outbound shape.
        // (ParallelJoin / Join's strategy can be DeepMerge; honour it.)
        let deep = matches!(
            &node.data,
            WorkflowNodeData::ParallelJoin {
                merge_strategy: MergeStrategy::DeepMerge,
                ..
            }
        ) || matches!(
            &node.data,
            WorkflowNodeData::Join {
                mode: JoinMode::All,
                merge_strategy: Some(MergeStrategy::DeepMerge),
                ..
            }
        );
        let mut inbound = TokenShape::object();
        let mut had_pred = false;
        for pred_ni in wg
            .dag
            .neighbors_directed(*ni, petgraph::Direction::Incoming)
        {
            let pred = *wg.dag.node_weight(pred_ni).unwrap();
            if let Some(p_out) = node_out.get(&pred.id) {
                inbound.merge_from(p_out, deep);
                had_pred = true;
            }
        }
        let inbound = if had_pred { inbound } else { TokenShape::Any };

        let outbound = out_shape(node, &inbound);
        node_in.insert(node.id.clone(), inbound);
        node_out.insert(node.id.clone(), outbound);
    }

    // Place-schema mapping. Input places (pre-merge; only survivors get
    // annotated) carry the inbound shape; output places (merge-robust) carry
    // the outbound shape.
    let mut place_schemas = BTreeMap::new();
    for node in &graph.nodes {
        if let (Some(pid), Some(shape)) = (input_place_id(node), node_in.get(&node.id)) {
            place_schemas.insert(pid, shape.render(0));
        }
        if let Some(shape) = node_out.get(&node.id) {
            for pid in output_place_ids(node) {
                place_schemas.insert(pid, shape.render(0));
            }
        }
    }

    // Per-node scope surface: the *borrow-reachable* references — exactly the
    // set the compiler (`check_guard` / `guard_readarc_plan`) resolves, built
    // from the same `resolve` / `resolve_ref` primitives. The old
    // `flatten_scope(node_in)` only saw the linear control token, so every
    // upstream field was hidden behind a token-replacing automated step (the
    // picker showed the executor envelope, never the parked producer's data).
    let pos = topo_pos(&order, &wg);
    let mut scopes: BTreeMap<String, Vec<ScopeEntry>> = BTreeMap::new();
    let mut diagnostics = Vec::new();
    for node in &graph.nodes {
        scopes.insert(
            node.id.clone(),
            reachable_scope(node, graph, &node_in, &node_out, &order, &wg, &slugs),
        );
    }

    // Guard re-validation against the real shape.
    for node in &graph.nodes {
        let guards: Vec<(String, &str)> = match &node.data {
            WorkflowNodeData::Decision { conditions, .. } => conditions
                .iter()
                .filter(|c| !c.guard.trim().is_empty())
                .map(|c| (c.label.clone(), c.guard.as_str()))
                .collect(),
            WorkflowNodeData::Loop { loop_condition, .. }
                if !loop_condition.trim().is_empty() =>
            {
                vec![("loop".to_string(), loop_condition.as_str())]
            }
            _ => continue,
        };
        let in_shape = match node_in.get(&node.id) {
            Some(s) => s,
            None => continue,
        };
        for (_label, guard) in guards {
            check_guard(
                node,
                guard,
                &slugs,
                graph,
                in_shape,
                &node_out,
                &pos,
                &mut diagnostics,
            );
        }
    }

    Ok(ShapeReport {
        node_in,
        node_out,
        place_schemas,
        scopes,
        diagnostics,
    })
}

/// Every `(dotted_path, type_label, provenance)` leaf of a shape.
pub(crate) fn collect_leaves(
    shape: &TokenShape,
    prefix: &str,
    prov: Option<&Provenance>,
    out: &mut Vec<(String, String, Provenance)>,
) {
    match shape {
        TokenShape::Object(map) if !map.is_empty() => {
            // Two distinct kinds of Object live in node shapes:
            //   1. *Anchored* containers (currently: File envelopes) — the
            //      container itself is a pickable scalar leaf, AND its
            //      subkeys are addressable as nested leaves. Both are
            //      user-meaningful nesting and must be preserved verbatim
            //      (`start.document`, `start.document.filename`).
            //   2. Plain Objects — runtime envelopes (HumanTask metadata
            //      `{title, steps, data: {…}, …}`, AutomatedStep `{detail,
            //      execution_id, run, …}`). Their interior nesting is *not*
            //      part of the addressable surface the user typed — what
            //      users wrote is the leaf identifier (`amount`, not
            //      `data.amount`), so descendants RESET their prefix to the
            //      bare child key. This matches the prior behaviour of the
            //      now-removed `rsplit('.').next()` collapse in phase (2),
            //      while leaving anchored nesting intact.
            let anchored = prov.and_then(|p| p.anchor.clone());
            if let (Some(p), Some(anchor)) = (prov, &anchored) {
                out.push((prefix.to_string(), anchor.label().to_string(), p.clone()));
            }
            for (k, f) in map {
                let path = if anchored.is_some() {
                    if prefix.is_empty() {
                        k.clone()
                    } else {
                        format!("{prefix}.{k}")
                    }
                } else {
                    k.clone() // plain object: flatten — child is the new root
                };
                collect_leaves(&f.shape, &path, Some(&f.prov), out);
            }
        }
        // Leaf: scalar / opaque / any / array / empty object.
        _ => {
            if let Some(p) = prov {
                out.push((prefix.to_string(), shape.kind_label(), p.clone()));
            }
        }
    }
}

/// A node is a *parked producer* (its business output gets a write-once
/// `p_{id}_data` place that read-arcs can borrow) iff it is a HumanTask,
/// AutomatedStep, or SubWorkflow (`lower.rs::split_outputs`) **or a Start**
/// (`lower.rs::park_outputs`). Start forks rather than splits — it parks a
/// write-once copy of its declared inputs to `p_{id}_data` while still
/// forwarding the full token — so `start.<field>` is borrow-reachable
/// downstream exactly like `review.<field>`, and the immediately-following
/// task can still interpolate Start fields off the control token.
///
/// SubWorkflow uses the same split_outputs tail as AutomatedStep, so its
/// declared output fields ride the parked `p_{id}_data` place after the
/// join — `<sub_slug>.<field>` is the only addressable form downstream.
pub(crate) fn is_parked_producer(graph: &WorkflowGraph, id: &str) -> bool {
    graph.nodes.iter().any(|n| {
        n.id == id
            && matches!(
                n.data,
                WorkflowNodeData::HumanTask { .. }
                    | WorkflowNodeData::AutomatedStep { .. }
                    | WorkflowNodeData::SubWorkflow { .. }
                    | WorkflowNodeData::Start { .. }
                    | WorkflowNodeData::Loop { .. }
                    | WorkflowNodeData::Join { .. }
            )
    })
}

/// True if `id` names a `WorkflowNodeData::Loop` node. Loop counters live in a
/// parked `p_<loop>_data` place keyed flat (`{iteration: N}`), so
/// `<slug>.iteration` borrows resolve through the standard read-arc pipeline
/// (see `resolve_ref`'s Qualified branch).
pub(crate) fn is_loop_node(graph: &WorkflowGraph, id: &str) -> bool {
    graph
        .nodes
        .iter()
        .any(|n| n.id == id && matches!(n.data, WorkflowNodeData::Loop { .. }))
}

pub(crate) fn topo_pos(order: &[petgraph::graph::NodeIndex], wg: &WorkflowDiGraph) -> BTreeMap<String, usize> {
    let mut pos = BTreeMap::new();
    for (i, ni) in order.iter().enumerate() {
        pos.insert(wg.dag.node_weight(*ni).unwrap().id.clone(), i);
    }
    pos
}

// ─── Slug index: the `<slug>.<field>` ↔ node-id resolver ────────────────────

/// Author-facing slug ↔ node-id resolution, the single source of truth for
/// `<slug>.<field>` guard references. Built once per `analyze`/readarc pass.
pub(crate) struct SlugIndex {
    by_slug: BTreeMap<String, String>,
    by_node: BTreeMap<String, String>,
}

impl SlugIndex {
    pub(crate) fn node_for(&self, slug: &str) -> Option<&str> {
        self.by_slug.get(slug).map(String::as_str)
    }
    pub(crate) fn slug_for(&self, node_id: &str) -> Option<&str> {
        self.by_node.get(node_id).map(String::as_str)
    }
    /// Sorted list of every declared slug — used by backend-ref error
    /// messages to suggest alternatives for typo'd `{{<slug>.<field>}}`
    /// references.
    pub(crate) fn all_slugs(&self) -> Vec<&str> {
        self.by_slug.keys().map(String::as_str).collect()
    }
}

/// Resolve every node's author-facing slug. Explicit, user-set slugs claim
/// their (sanitized) name and a post-sanitize clash between two of them is a
/// hard [`CompileError::SlugConflict`]. Nodes without an explicit slug derive
/// one from their id, collision-suffixed (`_2`, `_3`, …) deterministically by
/// graph order so existing example templates load unchanged (clean-cut: no
/// stored templates to migrate).
///
/// **Loops are exempt from suffixing**: a Loop node's slug is embedded
/// *literally* in the engine's Rhai logic (see `lower::lower_loop`), so a
/// silent rename to `<slug>_2` would diverge from the picker / `<slug>.iteration`
/// resolution. Any collision where one side is a Loop — whether the colliding
/// slug is explicit or derived — is a hard [`CompileError::SlugConflict`].
/// Authors disambiguate by setting an explicit `slug` on one of the loops.
pub(crate) fn slug_index(graph: &WorkflowGraph) -> Result<SlugIndex, CompileError> {
    let mut by_slug: BTreeMap<String, String> = BTreeMap::new();
    let mut by_node: BTreeMap<String, String> = BTreeMap::new();

    for n in &graph.nodes {
        let explicit = n
            .slug
            .as_deref()
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false);
        if !explicit {
            continue;
        }
        let s = n.slug();
        if let Some(other) = by_slug.get(&s) {
            if other != &n.id {
                return Err(CompileError::SlugConflict {
                    slug: s,
                    node_a: other.clone(),
                    node_b: n.id.clone(),
                });
            }
        }
        by_slug.insert(s.clone(), n.id.clone());
        by_node.insert(n.id.clone(), s);
    }

    for n in &graph.nodes {
        if by_node.contains_key(&n.id) {
            continue;
        }
        let base = n.slug();
        // Loops never get silent suffixing — their slug is the literal
        // engine-Rhai key for the `<slug>: { iteration: N }` namespace. Any
        // collision (explicit or derived, peer is also a Loop or not) must
        // be a hard SlugConflict so the picker and engine stay aligned.
        let is_loop = matches!(n.data, WorkflowNodeData::Loop { .. });
        if is_loop {
            if let Some(other) = by_slug.get(&base) {
                if other != &n.id {
                    return Err(CompileError::SlugConflict {
                        slug: base,
                        node_a: other.clone(),
                        node_b: n.id.clone(),
                    });
                }
            }
            by_slug.insert(base.clone(), n.id.clone());
            by_node.insert(n.id.clone(), base);
            continue;
        }
        // For non-Loop producers a derived-slug collision still suffix-renames
        // — the read-arc resolver routes through the SlugIndex, so the suffix
        // is invisible to the engine. But if the colliding peer IS a Loop,
        // even a non-Loop derived collision has to be a hard error: the loop's
        // namespace would otherwise be ambiguous with a parked producer of
        // the same name.
        let mut s = base.clone();
        let mut k = 2usize;
        while let Some(holder) = by_slug.get(&s) {
            let holder_is_loop = graph
                .nodes
                .iter()
                .any(|m| &m.id == holder && matches!(m.data, WorkflowNodeData::Loop { .. }));
            if holder_is_loop {
                return Err(CompileError::SlugConflict {
                    slug: s,
                    node_a: holder.clone(),
                    node_b: n.id.clone(),
                });
            }
            s = format!("{base}_{k}");
            k += 1;
        }
        by_slug.insert(s.clone(), n.id.clone());
        by_node.insert(n.id.clone(), s);
    }

    Ok(SlugIndex { by_slug, by_node })
}

// ─── One guard-reference resolver ───────────────────────────────────────────
// (`RefRoot`, `GuardRef`, `guard_refs`, `RefResolution`, `resolve_ref`,
// `reachable_scope`, and `check_guard` moved to
// `crate::compiler::borrow::planners::guard`.)


// ─── Tiny guard expression scanner ──────────────────────────────────────────
//
// `rhai_scope::extract_qualified_refs` only yields 2-segment `ident.field`
// refs; we need full dotted paths *and* the comparison literal for the type
// check. This is a deliberately small scanner — not a Rhai parser.

#[derive(Debug, Clone)]
pub(crate) enum LitTy {
    Number,
    Bool,
    Str,
}

impl LitTy {
    pub(crate) fn label(&self) -> &'static str {
        match self {
            LitTy::Number => "number",
            LitTy::Bool => "bool",
            LitTy::Str => "string",
        }
    }
}

pub(crate) fn scalar_satisfies(ty: &ScalarTy, lit: &LitTy) -> bool {
    matches!(
        (ty, lit),
        (ScalarTy::Number, LitTy::Number)
            | (ScalarTy::Bool, LitTy::Bool)
            | (ScalarTy::String, LitTy::Str)
            | (ScalarTy::Timestamp, LitTy::Str)
            | (ScalarTy::Json, _)
    )
}

/// Scan every contiguous `<root>.<a>.<b>...` dotted reference in `src`, paired
/// with the literal it is compared against on the immediate RHS (best-effort,
/// for the type check). `<root>` is any identifier — `input` (the control
/// token) or a node slug (`<slug>.<field>`, borrowed parked-producer data).
/// This is the single scanner feeding `guard_refs` (and through it
/// `reachable_scope`, `check_guard` and `guard_readarc_plan`) so the picker,
/// the read-arc synthesis and the diagnostics can never disagree.
pub(crate) fn scan_dotted_refs(src: &str) -> Vec<(String, Vec<String>, Option<LitTy>)> {
    let bytes: Vec<char> = src.chars().collect();
    let mut i = 0;
    let mut out = Vec::new();
    while i < bytes.len() {
        // A root starts an identifier that is not itself the field half of a
        // longer chain (`a.b` must not also yield root `b`).
        let root_start = (bytes[i].is_ascii_alphabetic() || bytes[i] == '_')
            && (i == 0 || (!is_ident(bytes[i - 1]) && bytes[i - 1] != '.'));
        if !root_start {
            i += 1;
            continue;
        }
        let rs = i;
        while i < bytes.len() && is_ident(bytes[i]) {
            i += 1;
        }
        let root: String = bytes[rs..i].iter().collect();
        let mut segs = Vec::new();
        while i < bytes.len() && bytes[i] == '.' {
            i += 1;
            let start = i;
            while i < bytes.len() && is_ident(bytes[i]) {
                i += 1;
            }
            if i > start {
                segs.push(bytes[start..i].iter().collect::<String>());
            } else {
                break;
            }
        }
        if !segs.is_empty() {
            let lit = sniff_rhs_literal(&bytes, i);
            out.push((root, segs, lit));
        }
    }
    out
}

fn is_ident(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// Peek past whitespace + a comparison operator and classify the next literal.
fn sniff_rhs_literal(b: &[char], mut i: usize) -> Option<LitTy> {
    while i < b.len() && b[i].is_whitespace() {
        i += 1;
    }
    // skip a comparison operator
    let ops = ['<', '>', '=', '!'];
    if i < b.len() && ops.contains(&b[i]) {
        i += 1;
        if i < b.len() && b[i] == '=' {
            i += 1;
        }
    } else {
        return None;
    }
    while i < b.len() && b[i].is_whitespace() {
        i += 1;
    }
    if i >= b.len() {
        return None;
    }
    if b[i] == '"' || b[i] == '\'' {
        return Some(LitTy::Str);
    }
    let rest: String = b[i..].iter().collect();
    if rest.starts_with("true") || rest.starts_with("false") {
        return Some(LitTy::Bool);
    }
    if b[i].is_ascii_digit() || b[i] == '-' {
        return Some(LitTy::Number);
    }
    None
}

// ─── AIR integration + reporting ────────────────────────────────────────────

/// Replace `token_schema` on every AIR place we have a derived shape for.
/// Today those places carry `"#/definitions/DynamicToken"`; this swaps in the
/// structural shape so the contract is visible to the engine and the editor.
pub fn annotate_air(air: &mut Value, report: &ShapeReport) {
    let Some(places) = air.get_mut("places").and_then(|p| p.as_array_mut()) else {
        return;
    };
    for place in places {
        let Some(id) = place.get("id").and_then(|v| v.as_str()) else {
            continue;
        };
        if let Some(shape) = report.place_schemas.get(id) {
            place["token_schema"] = Value::String(format!("inline:{shape}"));
        }
    }
}

/// Convenience wrapper: compile as usual, then annotate places with derived
/// shapes. `compile_to_air` itself is left untouched.
pub fn compile_to_air_with_shapes(
    graph: &WorkflowGraph,
    name: &str,
    description: &str,
    files: &crate::compiler::lower::NodeFiles,
) -> Result<(Value, ShapeReport), CompileError> {
    let mut air = crate::compiler::compile_to_air(graph, name, description, files)?;
    let report = analyze(graph)?;
    annotate_air(&mut air, &report);
    Ok((air, report))
}

impl ShapeReport {
    /// Human-readable demo dump.
    pub fn render(&self) -> String {
        let mut s = String::new();
        s.push_str("══ Derived token shape at each node's input place ══\n\n");
        for (nid, shape) in &self.node_in {
            s.push_str(&format!("● {nid}\n{}\n\n", shape.render(0)));
        }
        s.push_str("══ Editor scope surface (what the variable picker should show) ══\n\n");
        for (nid, entries) in &self.scopes {
            if entries.is_empty() {
                continue;
            }
            s.push_str(&format!("● {nid}\n"));
            for e in entries {
                s.push_str(&format!(
                    "    {} : {}   (from {} — {})\n",
                    e.path, e.ty, e.producer_label, e.note
                ));
            }
            s.push('\n');
        }
        s.push_str("══ Guard diagnostics (shape-aware) ══\n\n");
        if self.diagnostics.is_empty() {
            s.push_str("(none)\n");
        }
        for d in &self.diagnostics {
            match d {
                ShapeDiagnostic::DroppedUpstream {
                    node_label,
                    node_id,
                    guard,
                    referenced,
                    produced_by,
                    produced_label,
                    produced_path,
                    produced_ty,
                    dropped_by,
                    drop_reason,
                    fixes,
                } => {
                    s.push_str(&format!(
                        "✖ DROPPED     [{node_label} ({node_id})]\n  guard: {guard}\n  \
                         `{referenced}` is not present here.\n  produced by '{produced_label}' \
                         ({produced_by}) as `{produced_path}: {produced_ty}`\n  dropped at {}: \
                         {drop_reason}\n  fixes:\n",
                        dropped_by.as_deref().unwrap_or("(upstream)")
                    ));
                    for f in fixes {
                        s.push_str(&format!("    • {f}\n"));
                    }
                    s.push('\n');
                }
                ShapeDiagnostic::UnresolvedGuardPath {
                    node_label,
                    node_id,
                    guard,
                    referenced,
                } => {
                    s.push_str(&format!(
                        "✖ UNRESOLVED  [{node_label} ({node_id})]\n  guard: {guard}\n  \
                         `{referenced}` is produced by no upstream node.\n\n"
                    ));
                }
                ShapeDiagnostic::GuardTypeMismatch {
                    node_label,
                    node_id,
                    guard,
                    referenced,
                    found,
                    note,
                } => {
                    s.push_str(&format!(
                        "✖ TYPE        [{node_label} ({node_id})]\n  guard: {guard}\n  \
                         `{referenced}` is `{found}` but {note}.\n\n"
                    ));
                }
                ShapeDiagnostic::GraphIncomplete { message } => {
                    s.push_str(&format!("… GRAPH       not analyzable yet: {message}\n\n"));
                }
            }
        }
        s
    }
}

// ─── Pre-publish editor entrypoint ──────────────────────────────────────────

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
pub fn surface_types(graph: &WorkflowGraph) -> TypeSurface {
    match analyze(graph) {
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
    let report = analyze(graph)?;
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
#[cfg(test)]
pub(crate) use crate::compiler::borrow::planners::resource::automated_step_resource_borrow_plan;

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
pub fn node_namespace_scopes(
    graph: &WorkflowGraph,
) -> Result<
    std::collections::HashMap<String, BTreeMap<String, BTreeMap<String, FieldKind>>>,
    CompileError,
> {
    let report = analyze(graph)?;
    let slugs = slug_index(graph)?;
    let mut out: std::collections::HashMap<
        String,
        BTreeMap<String, BTreeMap<String, FieldKind>>,
    > = std::collections::HashMap::new();
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
            let field_path = e
                .path
                .strip_prefix(&format!("{slug}."))
                .unwrap_or(&e.path);
            let leaf = field_path.split('.').next().unwrap_or(field_path).to_string();
            if leaf.is_empty() {
                continue;
            }
            let kind = ty_label_to_field_kind(&e.ty);
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

#[cfg(test)]
mod port_contract_tests {
    use super::*;
    use crate::models::template::{PortField, Position};
    use serde_json::json;

    fn start_node(fields: Vec<PortField>) -> WorkflowNode {
        WorkflowNode {
            id: "start".to_string(),
            node_type: "start".to_string(),
            slug: None,
            position: Position { x: 0.0, y: 0.0 },
            data: WorkflowNodeData::Start {
                label: "Start".to_string(),
                description: None,
                initial: Port {
                    id: "in".to_string(),
                    label: "Input".to_string(),
                    fields,
                },
                process_name: None,
            },
            parent_id: None,
            width: None,
            height: None,
        }
    }

    fn field(name: &str, kind: FieldKind, required: bool) -> PortField {
        PortField {
            name: name.to_string(),
            label: name.to_string(),
            kind,
            required,
            options: None,
            description: None,
            accept: None,
        }
    }

    fn invoice_port() -> Port {
        Port {
            id: "in".to_string(),
            label: "Input".to_string(),
            fields: vec![
                field("invoice_file", FieldKind::File, true),
                field("invoice_id", FieldKind::Text, true),
            ],
        }
    }

    // The live incident: `invoice_file` resolved to the JSON scalar `"example"`
    // instead of an uploaded file ref. The lenient `Port::validate_token`
    // accepts it (a `file` field accepts a string); the strict SSOT gate must
    // reject it with a field-named, type-specific message — at ingestion,
    // before any net is created.
    #[test]
    fn file_field_as_scalar_string_is_rejected() {
        let node = start_node(invoice_port().fields);
        let token = json!({ "invoice_file": "example", "invoice_id": "example" });
        let v = validate_token_against_port(&invoice_port(), &node, &token)
            .expect_err("a string for a `file` field must be rejected");
        assert_eq!(v.field, "invoice_file");
        assert_eq!(v.actual, "string");
        assert!(
            v.expected.contains("file reference object"),
            "message should name the expected file shape, got: {}",
            v.expected
        );
        // Sanity: the lenient gate is exactly why this slipped to the net.
        assert!(invoice_port().validate_token(&token).is_ok());
    }

    #[test]
    fn valid_uploaded_file_ref_passes() {
        let node = start_node(invoice_port().fields);
        let token = json!({
            "invoice_file": {
                "key": "blob/abc",
                "url": "/api/v1/files/blob/abc",
                "filename": "invoice.png",
                "content_type": "image/png",
                "size": 1234
            },
            "invoice_id": "INV-1"
        });
        assert!(validate_token_against_port(&invoice_port(), &node, &token).is_ok());
    }

    #[test]
    fn number_field_as_string_is_rejected() {
        let port = Port {
            id: "in".to_string(),
            label: "Input".to_string(),
            fields: vec![field("amount", FieldKind::Number, true)],
        };
        let node = start_node(port.fields.clone());
        let v = validate_token_against_port(&port, &node, &json!({ "amount": "13" }))
            .expect_err("a string for a `number` field must be rejected");
        assert_eq!(v.field, "amount");
        assert_eq!(v.expected, "number");
        assert_eq!(v.actual, "string");
    }

    #[test]
    fn absent_field_is_not_a_type_error() {
        // Required/absent is `Port::validate_token`'s job — this strict gate
        // is type-only and must stay silent on absence so the two layers
        // compose without double-reporting.
        let node = start_node(invoice_port().fields);
        assert!(
            validate_token_against_port(&invoice_port(), &node, &json!({ "invoice_id": "x" }))
                .is_ok()
        );
    }

    #[test]
    fn non_object_token_is_rejected() {
        let node = start_node(invoice_port().fields);
        let v = validate_token_against_port(&invoice_port(), &node, &json!("not an object"))
            .expect_err("a non-object token cannot satisfy a field-keyed port");
        assert_eq!(v.field, "in");
        assert_eq!(v.actual, "string");
    }

    #[test]
    fn json_escape_hatch_accepts_anything() {
        let port = Port {
            id: "in".to_string(),
            label: "Input".to_string(),
            fields: vec![field("blob", FieldKind::Json, true)],
        };
        let node = start_node(port.fields.clone());
        assert!(validate_token_against_port(&port, &node, &json!({ "blob": "anything" })).is_ok());
        assert!(validate_token_against_port(&port, &node, &json!({ "blob": 42 })).is_ok());
    }
}

#[cfg(test)]
mod scope_reachability_tests {
    //! Task #20: the editor picker scope, the read-arc synthesis, and the
    //! drop diagnostic must be three views of ONE borrow-reachable model.
    //! Before the fix, `check-amount` (a Decision sitting after the
    //! token-replacing `extract` automated step) only saw extract's executor
    //! envelope — `review`'s `invoice_amount` was invisible in the picker yet
    //! the compiler happily read-arced it, and `check_guard` flagged it
    //! `DroppedUpstream`: three layers, three answers.
    use super::*;

    fn invoice_graph() -> WorkflowGraph {
        // Same fixture the foundation e2e proves the net enforces — so the
        // picker can't drift from what the compiler binds.
        let s = std::fs::read_to_string("tests/fixtures/graphs/invoice-processing.json")
            .expect("read invoice fixture");
        serde_json::from_str(&s).expect("deser invoice fixture")
    }

    #[test]
    fn decision_scope_agrees_with_readarc_synthesis_and_diagnostics() {
        let g = invoice_graph();
        let report = analyze(&g).expect("analyze");

        // (1) Picker offers the upstream parked producer's field,
        //     producer-namespaced as `review.invoice_amount` — not the
        //     `extract` envelope it physically arrives wrapped in, and not the
        //     provenance-erasing flat `input.invoice_amount`.
        let scope = report.scopes.get("check-amount").expect("decision scope");
        let amt = scope
            .iter()
            .find(|e| e.path == "review.invoice_amount")
            .unwrap_or_else(|| {
                panic!(
                    "review.invoice_amount must be pickable at the decision; offered: {:?}",
                    scope.iter().map(|e| &e.path).collect::<Vec<_>>()
                )
            });
        assert_eq!(amt.producer_node, "review");
        assert_eq!(amt.ty, "Number");
        // The flat, provenance-erasing form is gone.
        assert!(
            !scope.iter().any(|e| e.path == "input.invoice_amount"),
            "borrowed data must be slug-qualified, not flat input.*"
        );

        // (2) The read-arc synthesis resolves the IDENTICAL reference to the
        //     same producer (the compiler-as-borrow-checker).
        let binds = guard_readarc_plan(&g).expect("readarc plan");
        assert!(
            binds.iter().any(|b| b.consumer_node_id == "check-amount"
                && b.referenced == "review.invoice_amount"
                && b.producer_node == "review"),
            "guard_readarc_plan must bind review.invoice_amount -> review, got {:?}",
            binds
                .iter()
                .map(|b| (&b.consumer_node_id, &b.referenced, &b.producer_node))
                .collect::<Vec<_>>()
        );

        // (3) No diagnostic contradicts the compiler.
        for d in &report.diagnostics {
            if let ShapeDiagnostic::DroppedUpstream { referenced, .. }
            | ShapeDiagnostic::UnresolvedGuardPath { referenced, .. } = d
            {
                assert_ne!(
                    referenced, "review.invoice_amount",
                    "borrow-reachable ref wrongly flagged dropped/unresolved"
                );
            }
        }

        // Global invariant: nothing the picker offers is, at that same node,
        // reported unresolved — the picker never lies about resolvability.
        for (nid, entries) in &report.scopes {
            for e in entries {
                let contradicted = report.diagnostics.iter().any(|d| matches!(d,
                    ShapeDiagnostic::UnresolvedGuardPath { node_id, referenced, .. }
                        if node_id == nid && referenced == &e.path));
                assert!(
                    !contradicted,
                    "picker offered {} at {} but it is reported unresolved",
                    e.path, nid
                );
            }
        }
    }

    /// Two upstream parked producers contributing the *same* leaf no longer
    /// collapse to one nearest-wins entry (the #20 regression): producer
    /// namespacing makes them distinct paths, an unqualified `input.<key>`
    /// is unbindable (must qualify), and a nearer non-parked node never masks
    /// a farther parked one.
    fn two_producer_graph(decision_guard: &str) -> WorkflowGraph {
        // Start → reviewA → reviewB → decision. Both human tasks emit a form
        // field `amount`; `reviewA` is the *farther* parked producer.
        let step = |field: &str| {
            format!(
                r#"{{"id":"s","title":"S","blocks":[{{"type":"input","field":{{"name":"{field}","label":"Amt","kind":"number","required":true}}}}]}}"#
            )
        };
        let ht = |id: &str, slug: &str| {
            format!(
                r#"{{"id":"{id}","type":"human_task","slug":"{slug}","position":{{"x":0,"y":0}},"data":{{"type":"human_task","label":"{id}","taskTitle":"{id}","steps":[{}]}}}}"#,
                step("amount")
            )
        };
        let json = format!(
            r#"{{"nodes":[
              {{"id":"start","type":"start","position":{{"x":0,"y":0}},"data":{{"type":"start","label":"Start"}}}},
              {ha},
              {hb},
              {{"id":"dec","type":"decision","position":{{"x":0,"y":0}},"data":{{"type":"decision","label":"D","conditions":[{{"edgeId":"hi","label":"hi","guard":"{decision_guard}"}}],"defaultBranch":"default"}}}},
              {{"id":"end1","type":"end","position":{{"x":0,"y":0}},"data":{{"type":"end","label":"E1"}}}},
              {{"id":"end2","type":"end","position":{{"x":0,"y":0}},"data":{{"type":"end","label":"E2"}}}}
            ],"edges":[
              {{"id":"e1","source":"start","target":"reviewA","type":"sequence"}},
              {{"id":"e2","source":"reviewA","target":"reviewB","type":"sequence"}},
              {{"id":"e3","source":"reviewB","target":"dec","type":"sequence"}},
              {{"id":"e4","source":"dec","target":"end1","sourceHandle":"hi","type":"sequence"}},
              {{"id":"e5","source":"dec","target":"end2","sourceHandle":"default","type":"sequence"}}
            ]}}"#,
            ha = ht("reviewA", "rev_a"),
            hb = ht("reviewB", "rev_b"),
        );
        serde_json::from_str(&json).expect("deser two-producer graph")
    }

    #[test]
    fn collision_distinct_parked_producers_get_distinct_qualified_paths() {
        let g = two_producer_graph("rev_a.amount > 0");
        let report = analyze(&g).expect("analyze");
        let scope = report.scopes.get("dec").expect("decision scope");
        let paths: std::collections::BTreeSet<&str> =
            scope.iter().map(|e| e.path.as_str()).collect();

        // Same key, two parked owners → two DISTINCT producer-namespaced
        // entries (no nearest-wins collapse, no silent loss).
        assert!(
            paths.contains("rev_a.amount") && paths.contains("rev_b.amount"),
            "both producers' `amount` must be distinctly pickable, got: {paths:?}"
        );
        let a = scope.iter().find(|e| e.path == "rev_a.amount").unwrap();
        let b = scope.iter().find(|e| e.path == "rev_b.amount").unwrap();
        assert_eq!(a.producer_node, "reviewA");
        assert_eq!(b.producer_node, "reviewB");
        // The flat form that erased the producer is gone entirely.
        assert!(
            !paths.contains("input.amount"),
            "unqualified borrowed key must not be offered: {paths:?}"
        );

        // The qualified guard binds to its named producer — the farther one,
        // proving a nearer parked/forwarding node does not mask it.
        let binds = guard_readarc_plan(&g).expect("readarc plan");
        assert!(
            binds
                .iter()
                .any(|x| x.referenced == "rev_a.amount" && x.producer_node == "reviewA"),
            "rev_a.amount must bind to reviewA, got {:?}",
            binds
                .iter()
                .map(|x| (&x.referenced, &x.producer_node))
                .collect::<Vec<_>>()
        );

        // An unqualified, non-control `input.amount` is unbindable: hard
        // error at compile, naming the qualified forms to use; and the same
        // node reports it unresolved for the editor.
        let g2 = two_producer_graph("input.amount > 0");
        match guard_readarc_plan(&g2) {
            Err(CompileError::GuardUnresolved {
                node_id,
                identifier,
                available,
            }) => {
                assert_eq!(node_id, "dec");
                assert_eq!(identifier, "input.amount");
                assert!(
                    available.iter().any(|p| p == "rev_a.amount")
                        && available.iter().any(|p| p == "rev_b.amount"),
                    "the error must name both qualified forms, got: {available:?}"
                );
            }
            other => panic!("expected GuardUnresolved, got {other:?}"),
        }
        let report2 = analyze(&g2).expect("analyze g2");
        assert!(
            report2.diagnostics.iter().any(|d| matches!(d,
                ShapeDiagnostic::UnresolvedGuardPath { node_id, referenced, .. }
                    if node_id == "dec" && referenced == "input.amount")),
            "editor must see input.amount as unresolved at the decision"
        );
    }

    /// Start is a parked producer (`lower.rs::park_outputs`): its declared
    /// inputs are borrow-reachable downstream as `<start-slug>.<field>`,
    /// exactly like a human task's, and genuine control/identity leaves are
    /// attributed to the synthetic "Process" group instead of whichever node
    /// last forwarded the token (the `input.status`-under-Extract-Data bug).
    fn start_producer_graph(decision_guard: &str) -> WorkflowGraph {
        let v = serde_json::json!({
            "nodes": [
                {"id":"start","type":"start","position":{"x":0,"y":0},
                 "data":{"type":"start","label":"Start",
                    "initial":{"id":"in","label":"Intake","fields":[
                        {"name":"note","label":"Note","kind":"text","required":true}]}}},
                {"id":"dec","type":"decision","position":{"x":0,"y":0},
                 "data":{"type":"decision","label":"D",
                    "conditions":[{"edgeId":"hi","label":"hi","guard":decision_guard}],
                    "defaultBranch":"default"}},
                {"id":"end1","type":"end","position":{"x":0,"y":0},"data":{"type":"end","label":"E1"}},
                {"id":"end2","type":"end","position":{"x":0,"y":0},"data":{"type":"end","label":"E2"}}
            ],
            "edges": [
                {"id":"e1","source":"start","target":"dec","type":"sequence"},
                {"id":"e4","source":"dec","target":"end1","sourceHandle":"hi","type":"sequence"},
                {"id":"e5","source":"dec","target":"end2","sourceHandle":"default","type":"sequence"}
            ]
        });
        serde_json::from_value(v).expect("deser start-producer graph")
    }

    #[test]
    fn start_is_parked_producer_and_control_leaves_grouped_as_process() {
        let g = start_producer_graph("start.note == \"ok\"");
        let report = analyze(&g).expect("analyze");
        let scope = report.scopes.get("dec").expect("decision scope");

        // (1) Start's declared input is borrow-reachable, namespaced by the
        //     Start's slug (derived from node id `start`) — never flat.
        let note = scope.iter().find(|e| e.path == "start.note").unwrap_or_else(|| {
            panic!(
                "start.note must be pickable at the decision; offered: {:?}",
                scope.iter().map(|e| &e.path).collect::<Vec<_>>()
            )
        });
        assert_eq!(note.producer_node, "start");
        assert_eq!(note.ty, "String");
        assert!(
            !scope.iter().any(|e| e.path == "input.note"),
            "Start data must be slug-qualified, not flat input.*"
        );

        // (2) Genuine control/identity leaves (`_instance_id`) go to the
        //     synthetic "Process" group, not a business producer.
        let proc = scope
            .iter()
            .find(|e| e.path == "input._instance_id")
            .expect("control leaf input._instance_id must be offered");
        assert_eq!(proc.producer_label, "Process");
        assert_eq!(proc.producer_node, "");
        assert!(
            !scope
                .iter()
                .any(|e| e.path.starts_with("input.") && e.producer_label != "Process"),
            "every control leaf must group under Process, got {:?}",
            scope
                .iter()
                .map(|e| (&e.path, &e.producer_label))
                .collect::<Vec<_>>()
        );

        // (3) The read-arc synthesis binds the IDENTICAL ref to the Start's
        //     parked data place (`apply_control_data_foundation` borrows
        //     `p_start_data`) — picker == compiler.
        let binds = guard_readarc_plan(&g).expect("readarc plan");
        assert!(
            binds.iter().any(|b| b.consumer_node_id == "dec"
                && b.referenced == "start.note"
                && b.producer_node == "start"),
            "guard_readarc_plan must bind start.note -> start, got {:?}",
            binds
                .iter()
                .map(|b| (&b.consumer_node_id, &b.referenced, &b.producer_node))
                .collect::<Vec<_>>()
        );

        // (4) The picker never lies: nothing it offers is, at that node,
        //     reported unresolved.
        for e in scope {
            let contradicted = report.diagnostics.iter().any(|d| matches!(d,
                ShapeDiagnostic::UnresolvedGuardPath { node_id, referenced, .. }
                    if node_id == "dec" && referenced == &e.path));
            assert!(
                !contradicted,
                "picker offered {} but it is reported unresolved",
                e.path
            );
        }
    }

    /// A Python AutomatedStep that reads `review.invoice_amount` in its
    /// source must produce exactly one [`AutomatedStepDataBorrow`] from
    /// the consumer (the AutomatedStep) to the producer (the upstream
    /// HumanTask `review`) — the same borrow-checker model the
    /// Decision/Loop branch already uses, just sourced from Python AST
    /// instead of Rhai.
    #[test]
    fn python_automated_step_review_field_emits_borrow() {
        use crate::compiler::lower::NodeFiles;
        use aithericon_executor_domain::InputSource;
        use std::collections::HashMap;

        // Start → review (HumanTask, slug "review", produces `invoice_amount`)
        //       → extract (Python AutomatedStep) → end
        let json = r#"{
          "nodes":[
            {"id":"start","type":"start","position":{"x":0,"y":0},
             "data":{"type":"start","label":"Start"}},
            {"id":"review","type":"human_task","slug":"review","position":{"x":0,"y":0},
             "data":{"type":"human_task","label":"Review","taskTitle":"Review",
                     "steps":[{"id":"s","title":"S","blocks":[
                       {"type":"input","field":{"name":"invoice_amount","label":"Amt","kind":"number","required":true}}
                     ]}]}},
            {"id":"extract","type":"automated_step","slug":"extract","position":{"x":0,"y":0},
             "data":{"type":"automated_step","label":"Extract",
                     "executionSpec":{"backendType":"python","entrypoint":"main.py","config":{"entrypoint":"main.py","python":"python3","sdk":true}},
                     "retryPolicy":{"maxRetries":0,"strategy":{"type":"immediate"}},
                     "deploymentModel":{"mode":"inline"}}},
            {"id":"end","type":"end","position":{"x":0,"y":0},
             "data":{"type":"end","label":"End"}}
          ],
          "edges":[
            {"id":"e1","source":"start","target":"review","type":"sequence"},
            {"id":"e2","source":"review","target":"extract","type":"sequence"},
            {"id":"e3","source":"extract","target":"end","type":"sequence"}
          ]
        }"#;
        let g: WorkflowGraph = serde_json::from_str(json).expect("deser borrow graph");

        let mut inline: std::collections::HashMap<String, std::collections::HashMap<String, String>> =
            HashMap::new();
        let mut step_files = HashMap::new();
        step_files.insert(
            "main.py".to_string(),
            "amount = review.invoice_amount\nprint(amount)\n".to_string(),
        );
        inline.insert("extract".to_string(), step_files);

        let borrows = automated_step_borrow_plan(&g, &inline).expect("borrow plan");
        assert_eq!(
            borrows.len(),
            1,
            "exactly one borrow expected; got: {borrows:?}"
        );
        assert_eq!(borrows[0].consumer_node_id(), "extract");
        assert_eq!(borrows[0].slug(), "review");
        assert_eq!(borrows[0].producer_node(), "review");
    }

    /// Multiple accesses to the SAME producer collapse to one borrow per
    /// `(consumer, producer)` pair — the runtime stages the whole
    /// envelope once and the user reads any number of fields off it.
    #[test]
    fn python_borrow_dedupes_per_producer() {
        use std::collections::HashMap;

        let json = r#"{
          "nodes":[
            {"id":"start","type":"start","position":{"x":0,"y":0},
             "data":{"type":"start","label":"Start"}},
            {"id":"review","type":"human_task","slug":"review","position":{"x":0,"y":0},
             "data":{"type":"human_task","label":"Review","taskTitle":"Review",
                     "steps":[{"id":"s","title":"S","blocks":[
                       {"type":"input","field":{"name":"invoice_amount","label":"Amt","kind":"number","required":true}},
                       {"type":"input","field":{"name":"vendor_name","label":"V","kind":"text","required":true}}
                     ]}]}},
            {"id":"extract","type":"automated_step","slug":"extract","position":{"x":0,"y":0},
             "data":{"type":"automated_step","label":"Extract",
                     "executionSpec":{"backendType":"python","entrypoint":"main.py","config":{"entrypoint":"main.py","python":"python3","sdk":true}},
                     "retryPolicy":{"maxRetries":0,"strategy":{"type":"immediate"}},
                     "deploymentModel":{"mode":"inline"}}},
            {"id":"end","type":"end","position":{"x":0,"y":0},
             "data":{"type":"end","label":"End"}}
          ],
          "edges":[
            {"id":"e1","source":"start","target":"review","type":"sequence"},
            {"id":"e2","source":"review","target":"extract","type":"sequence"},
            {"id":"e3","source":"extract","target":"end","type":"sequence"}
          ]
        }"#;
        let g: WorkflowGraph = serde_json::from_str(json).expect("deser borrow graph");

        let mut inline: HashMap<String, HashMap<String, String>> = HashMap::new();
        let mut step_files = HashMap::new();
        step_files.insert(
            "main.py".to_string(),
            "a = review.invoice_amount\nb = review.vendor_name\nc = review.invoice_amount\n"
                .to_string(),
        );
        inline.insert("extract".to_string(), step_files);

        let borrows = automated_step_borrow_plan(&g, &inline).expect("borrow plan");
        // Three accesses on `review` → one borrow.
        assert_eq!(
            borrows.len(),
            1,
            "borrow plan must dedupe per (consumer, producer); got: {borrows:?}"
        );
    }

    /// An identifier that isn't a known slug (stdlib module, local var,
    /// typo) is silently ignored — no borrow, no hard error, no false
    /// positive against `os.path` and friends.
    #[test]
    fn python_unknown_head_is_silently_ignored() {
        use std::collections::HashMap;

        let json = r#"{
          "nodes":[
            {"id":"start","type":"start","position":{"x":0,"y":0},
             "data":{"type":"start","label":"Start"}},
            {"id":"extract","type":"automated_step","slug":"extract","position":{"x":0,"y":0},
             "data":{"type":"automated_step","label":"Extract",
                     "executionSpec":{"backendType":"python","entrypoint":"main.py","config":{"entrypoint":"main.py","python":"python3","sdk":true}},
                     "retryPolicy":{"maxRetries":0,"strategy":{"type":"immediate"}},
                     "deploymentModel":{"mode":"inline"}}},
            {"id":"end","type":"end","position":{"x":0,"y":0},
             "data":{"type":"end","label":"End"}}
          ],
          "edges":[
            {"id":"e1","source":"start","target":"extract","type":"sequence"},
            {"id":"e2","source":"extract","target":"end","type":"sequence"}
          ]
        }"#;
        let g: WorkflowGraph = serde_json::from_str(json).expect("deser graph");

        let mut inline: HashMap<String, HashMap<String, String>> = HashMap::new();
        let mut step_files = HashMap::new();
        step_files.insert(
            "main.py".to_string(),
            "import os\np = os.path.join('a', 'b')\nlocal_var = {'k': 1}\nv = local_var.get('k')\n"
                .to_string(),
        );
        inline.insert("extract".to_string(), step_files);

        let borrows = automated_step_borrow_plan(&g, &inline).expect("borrow plan");
        assert!(
            borrows.is_empty(),
            "stdlib + locals must not become borrows; got: {borrows:?}"
        );
    }

    /// One model: a HumanTask's `{{ <slug>.<field> }}` placeholder
    /// resolves to a single borrow against the upstream parked place,
    /// exactly like a Python AutomatedStep's `<slug>.<field>` source
    /// access.
    #[test]
    fn human_task_borrow_simple() {
        // Start(slug=start, with invoice_id) → review (HumanTask) → end.
        // The HumanTask title interpolates `{{ start.invoice_id }}`.
        let json = r#"{
          "nodes":[
            {"id":"s","type":"start","slug":"start","position":{"x":0,"y":0},
             "data":{"type":"start","label":"Start",
                     "initial":{"id":"in","label":"Initial","fields":[
                       {"name":"invoice_id","label":"Invoice","kind":"text","required":true}
                     ]}}},
            {"id":"review","type":"human_task","position":{"x":0,"y":0},
             "data":{"type":"human_task","label":"Review",
                     "taskTitle":"Review {{ start.invoice_id }}",
                     "steps":[]}},
            {"id":"end","type":"end","position":{"x":0,"y":0},
             "data":{"type":"end","label":"End"}}
          ],
          "edges":[
            {"id":"e1","source":"s","target":"review","type":"sequence"},
            {"id":"e2","source":"review","target":"end","type":"sequence"}
          ]
        }"#;
        let g: WorkflowGraph = serde_json::from_str(json).expect("deser borrow graph");
        let borrows = human_task_borrow_plan(&g).expect("borrow plan");
        assert_eq!(borrows.len(), 1, "expected exactly one borrow; got: {borrows:?}");
        assert_eq!(borrows[0].consumer_node_id, "review");
        assert_eq!(borrows[0].slug, "start");
        assert_eq!(borrows[0].producer_node, "s");
    }

    /// Multiple placeholders against the same producer collapse to one
    /// borrow per `(consumer, producer)` pair — mirrors the Python
    /// dedupe rule. The runtime read-arc reaches the whole envelope, the
    /// Rhai `__pluck` walks down to the individual field per call site.
    #[test]
    fn human_task_borrow_dedupes_per_producer() {
        let json = r#"{
          "nodes":[
            {"id":"s","type":"start","slug":"start","position":{"x":0,"y":0},
             "data":{"type":"start","label":"Start",
                     "initial":{"id":"in","label":"Initial","fields":[
                       {"name":"invoice_id","label":"I","kind":"text","required":true},
                       {"name":"vendor_name","label":"V","kind":"text","required":true}
                     ]}}},
            {"id":"review","type":"human_task","position":{"x":0,"y":0},
             "data":{"type":"human_task","label":"Review",
                     "taskTitle":"Pay {{ start.vendor_name }} for {{ start.invoice_id }}",
                     "instructionsMdsvex":"Re: {{ start.invoice_id }}",
                     "steps":[]}},
            {"id":"end","type":"end","position":{"x":0,"y":0},
             "data":{"type":"end","label":"End"}}
          ],
          "edges":[
            {"id":"e1","source":"s","target":"review","type":"sequence"},
            {"id":"e2","source":"review","target":"end","type":"sequence"}
          ]
        }"#;
        let g: WorkflowGraph = serde_json::from_str(json).expect("deser borrow graph");
        let borrows = human_task_borrow_plan(&g).expect("borrow plan");
        assert_eq!(
            borrows.len(),
            1,
            "three placeholders on `start` → one borrow; got: {borrows:?}"
        );
    }

    /// An unknown head identifier (typo, root-level control-token
    /// field like `{{ status }}`, or a placeholder pointing nowhere)
    /// is silently ignored — same posture as Python's
    /// `python_unknown_head_is_silently_ignored`. The interpolation
    /// stays in place and `__pluck` degrades to `()` at runtime.
    #[test]
    fn human_task_unknown_slug_ignored() {
        let json = r#"{
          "nodes":[
            {"id":"s","type":"start","position":{"x":0,"y":0},
             "data":{"type":"start","label":"Start"}},
            {"id":"review","type":"human_task","position":{"x":0,"y":0},
             "data":{"type":"human_task","label":"Review",
                     "taskTitle":"{{ mystery.field }} or {{ also_unknown }}",
                     "steps":[]}},
            {"id":"end","type":"end","position":{"x":0,"y":0},
             "data":{"type":"end","label":"End"}}
          ],
          "edges":[
            {"id":"e1","source":"s","target":"review","type":"sequence"},
            {"id":"e2","source":"review","target":"end","type":"sequence"}
          ]
        }"#;
        let g: WorkflowGraph = serde_json::from_str(json).expect("deser graph");
        let borrows = human_task_borrow_plan(&g).expect("borrow plan");
        assert!(
            borrows.is_empty(),
            "unknown slugs and root-level placeholders must not become borrows; got: {borrows:?}"
        );
    }

    // ──────────────────────────────────────────────────────────────────
    // LLM / Kreuzberg borrow planner tests
    // ──────────────────────────────────────────────────────────────────

    /// Fixture: Start → review (HumanTask: invoice_amount:number, vendor_name:text)
    ///          → ocr_step (Kreuzberg, attached PDF, outputs content:text)
    ///          → classify (LLM, prompt references {{review.vendor_name}} +
    ///                      {{ocr_step.content}})
    ///          → end
    fn ocr_classify_graph(prompt: &str) -> WorkflowGraph {
        let json = format!(
            r#"{{
              "nodes": [
                {{"id":"s","type":"start","slug":"start","position":{{"x":0,"y":0}},
                 "data":{{"type":"start","label":"Start"}}}},
                {{"id":"review","type":"human_task","slug":"review","position":{{"x":0,"y":0}},
                 "data":{{"type":"human_task","label":"Review","taskTitle":"R",
                         "steps":[{{"id":"s1","title":"S","blocks":[
                           {{"type":"input","field":{{"name":"invoice_amount","label":"A","kind":"number","required":true}}}},
                           {{"type":"input","field":{{"name":"vendor_name","label":"V","kind":"text","required":true}}}},
                           {{"type":"input","field":{{"name":"invoice_pdf","label":"P","kind":"file","required":true}}}}
                         ]}}]}}}},
                {{"id":"ocr_step","type":"automated_step","slug":"ocr_step","position":{{"x":0,"y":0}},
                 "data":{{"type":"automated_step","label":"OCR",
                         "executionSpec":{{"backendType":"kreuzberg","config":{{"file":"sample.pdf"}}}},
                         "retryPolicy":{{"maxRetries":0,"strategy":{{"type":"immediate"}}}},
                         "deploymentModel":{{"mode":"inline"}},
                         "output":{{"id":"out","label":"out","fields":[
                           {{"name":"content","label":"Content","kind":"text","required":true}}
                         ]}}}}}},
                {{"id":"classify","type":"automated_step","slug":"classify","position":{{"x":0,"y":0}},
                 "data":{{"type":"automated_step","label":"Classify",
                         "executionSpec":{{"backendType":"llm","config":{{
                            "provider":"openai","model":"gpt-4o-mini",
                            "prompt":{prompt}
                         }}}},
                         "retryPolicy":{{"maxRetries":0,"strategy":{{"type":"immediate"}}}},
                         "deploymentModel":{{"mode":"inline"}},
                         "output":{{"id":"out","label":"out","fields":[
                           {{"name":"klass","label":"K","kind":"text","required":true}}
                         ]}}}}}},
                {{"id":"end","type":"end","position":{{"x":0,"y":0}},
                 "data":{{"type":"end","label":"End"}}}}
              ],
              "edges":[
                {{"id":"e1","source":"s","target":"review","type":"sequence"}},
                {{"id":"e2","source":"review","target":"ocr_step","type":"sequence"}},
                {{"id":"e3","source":"ocr_step","target":"classify","type":"sequence"}},
                {{"id":"e4","source":"classify","target":"end","type":"sequence"}}
              ]
            }}"#,
            prompt = prompt
        );
        serde_json::from_str(&json).expect("deser ocr_classify graph")
    }

    #[test]
    fn llm_prompt_simple_borrow() {
        use std::collections::HashMap;
        let g = ocr_classify_graph(r#""Classify: {{ ocr_step.content }} for {{ review.vendor_name }}""#);
        let borrows = automated_step_borrow_plan(&g, &HashMap::new()).expect("borrow plan");

        let pairs: Vec<(String, String)> = borrows
            .iter()
            .filter_map(|b| match b {
                AutomatedStepDataBorrow::PerField {
                    consumer_node_id,
                    slug,
                    attr,
                    ..
                } if consumer_node_id == "classify" => Some((slug.clone(), attr.clone())),
                _ => None,
            })
            .collect();
        assert!(pairs.contains(&("ocr_step".into(), "content".into())));
        assert!(pairs.contains(&("review".into(), "vendor_name".into())));
        // All classify borrows must be content sites (is_path_site=false) —
        // the prompt is a content surface.
        for b in &borrows {
            if let AutomatedStepDataBorrow::PerField {
                consumer_node_id,
                is_path_site,
                ..
            } = b
            {
                if consumer_node_id == "classify" {
                    assert!(!*is_path_site, "prompt site must be content (is_path_site=false)");
                }
            }
        }
    }

    #[test]
    fn llm_unknown_slug_is_hard_error() {
        use std::collections::HashMap;
        let g = ocr_classify_graph(r#""Classify: {{ typo_slug.content }}""#);
        let err = automated_step_borrow_plan(&g, &HashMap::new())
            .expect_err("unknown slug must error");
        match err {
            CompileError::BackendRefUnresolved {
                backend,
                kind,
                name,
                slug,
                ..
            } => {
                assert_eq!(backend, "llm");
                assert_eq!(kind, "slug");
                assert_eq!(name, "typo_slug");
                assert_eq!(slug, "typo_slug");
            }
            other => panic!("expected BackendRefUnresolved(slug), got {other:?}"),
        }
    }

    #[test]
    fn llm_unknown_field_on_known_slug_is_hard_error() {
        use std::collections::HashMap;
        let g = ocr_classify_graph(r#""Classify: {{ ocr_step.no_such_field }}""#);
        let err = automated_step_borrow_plan(&g, &HashMap::new())
            .expect_err("unknown field must error");
        match err {
            CompileError::BackendRefUnresolved {
                kind,
                name,
                slug,
                field,
                available,
                ..
            } => {
                assert_eq!(kind, "field");
                assert_eq!(name, "no_such_field");
                assert_eq!(slug, "ocr_step");
                assert_eq!(field, "no_such_field");
                assert!(
                    available.contains(&"content".to_string()),
                    "available fields must include 'content', got {available:?}"
                );
            }
            other => panic!("expected BackendRefUnresolved(field), got {other:?}"),
        }
    }

    #[test]
    fn llm_content_site_rejects_file_kind_producer() {
        use std::collections::HashMap;
        // Interpolating a File-kind upstream into a text prompt is nonsense.
        let g = ocr_classify_graph(r#""Inline PDF? {{ review.invoice_pdf }}""#);
        let err = automated_step_borrow_plan(&g, &HashMap::new())
            .expect_err("file-kind in prompt must error");
        assert!(matches!(err, CompileError::LlmImageRefNotFileKind { .. }));
    }

    #[test]
    fn llm_no_placeholders_yields_no_borrows() {
        use std::collections::HashMap;
        let g = ocr_classify_graph(r#""Just a static prompt, no placeholders""#);
        let borrows = automated_step_borrow_plan(&g, &HashMap::new()).expect("borrow plan");
        // No borrows for the classify LLM consumer specifically.
        let classify_borrows: Vec<_> = borrows
            .iter()
            .filter(|b| b.consumer_node_id() == "classify")
            .collect();
        assert!(classify_borrows.is_empty(), "got: {classify_borrows:?}");
    }

    #[test]
    fn kreuzberg_borrow_resolves_file_kind() {
        // Two AutomatedSteps:
        //   1. uploader (HumanTask, slug=uploader, file field "pdf")
        //   2. ocr (Kreuzberg, file: "{{uploader.pdf}}")
        let json = r#"{
          "nodes": [
            {"id":"s","type":"start","slug":"start","position":{"x":0,"y":0},
             "data":{"type":"start","label":"Start"}},
            {"id":"uploader","type":"human_task","slug":"uploader","position":{"x":0,"y":0},
             "data":{"type":"human_task","label":"U","taskTitle":"U",
                     "steps":[{"id":"s1","title":"S","blocks":[
                       {"type":"input","field":{"name":"pdf","label":"P","kind":"file","required":true}}
                     ]}]}},
            {"id":"ocr","type":"automated_step","slug":"ocr","position":{"x":0,"y":0},
             "data":{"type":"automated_step","label":"OCR",
                     "executionSpec":{"backendType":"kreuzberg","config":{"file":"{{ uploader.pdf }}"}},
                     "retryPolicy":{"maxRetries":0,"strategy":{"type":"immediate"}},
                     "deploymentModel":{"mode":"inline"}}},
            {"id":"end","type":"end","position":{"x":0,"y":0},
             "data":{"type":"end","label":"End"}}
          ],
          "edges":[
            {"id":"e1","source":"s","target":"uploader","type":"sequence"},
            {"id":"e2","source":"uploader","target":"ocr","type":"sequence"},
            {"id":"e3","source":"ocr","target":"end","type":"sequence"}
          ]
        }"#;
        use std::collections::HashMap;
        let g: WorkflowGraph = serde_json::from_str(json).expect("deser graph");
        let borrows = automated_step_borrow_plan(&g, &HashMap::new()).expect("borrow plan");
        assert_eq!(borrows.len(), 1, "got: {borrows:?}");
        match &borrows[0] {
            AutomatedStepDataBorrow::PerField {
                consumer_node_id,
                slug,
                producer_node,
                attr,
                is_path_site,
                producer_field_kind,
            } => {
                assert_eq!(consumer_node_id, "ocr");
                assert_eq!(slug, "uploader");
                assert_eq!(producer_node, "uploader");
                assert_eq!(attr, "pdf");
                assert!(*is_path_site);
                assert_eq!(*producer_field_kind, crate::models::template::FieldKind::File);
            }
            other => panic!("Kreuzberg borrow must be PerField, got {other:?}"),
        }
    }

    #[test]
    fn kreuzberg_allows_text_kind_fields() {
        // Kreuzberg over an LLM's text output — temp-file path of the
        // stringified content. Compiler accepts; foundation pass handles
        // the Raw staging.
        let json = r#"{
          "nodes": [
            {"id":"s","type":"start","slug":"start","position":{"x":0,"y":0},
             "data":{"type":"start","label":"Start"}},
            {"id":"genreport","type":"automated_step","slug":"genreport","position":{"x":0,"y":0},
             "data":{"type":"automated_step","label":"Gen",
                     "executionSpec":{"backendType":"llm","config":{"provider":"openai","model":"x","prompt":"hello"}},
                     "retryPolicy":{"maxRetries":0,"strategy":{"type":"immediate"}},
                     "deploymentModel":{"mode":"inline"},
                     "output":{"id":"out","label":"out","fields":[
                       {"name":"narrative","label":"N","kind":"text","required":true}
                     ]}}},
            {"id":"reocr","type":"automated_step","slug":"reocr","position":{"x":0,"y":0},
             "data":{"type":"automated_step","label":"ReOCR",
                     "executionSpec":{"backendType":"kreuzberg","config":{"file":"{{ genreport.narrative }}"}},
                     "retryPolicy":{"maxRetries":0,"strategy":{"type":"immediate"}},
                     "deploymentModel":{"mode":"inline"}}},
            {"id":"end","type":"end","position":{"x":0,"y":0},
             "data":{"type":"end","label":"End"}}
          ],
          "edges":[
            {"id":"e1","source":"s","target":"genreport","type":"sequence"},
            {"id":"e2","source":"genreport","target":"reocr","type":"sequence"},
            {"id":"e3","source":"reocr","target":"end","type":"sequence"}
          ]
        }"#;
        use std::collections::HashMap;
        let g: WorkflowGraph = serde_json::from_str(json).expect("deser graph");
        let borrows = automated_step_borrow_plan(&g, &HashMap::new()).expect("borrow plan");
        assert_eq!(borrows.len(), 1);
        match &borrows[0] {
            AutomatedStepDataBorrow::PerField {
                producer_field_kind,
                ..
            } => assert_eq!(*producer_field_kind, crate::models::template::FieldKind::Text),
            other => panic!("Kreuzberg borrow must be PerField, got {other:?}"),
        }
    }

    /// File envelope nesting: a Start field `document: File` must surface
    /// downstream as *both* a `FileRef` leaf (`start.document`, what Kreuzberg
    /// and LLM borrow) *and* its three metadata subkeys (`start.document.url`,
    /// `.filename`, `.content_type`, the dotted form HumanTask blocks
    /// interpolate). Before the picker fix, the container leaf was missing
    /// and the subkeys were truncated to `start.{url,filename,content_type}`.
    #[test]
    fn file_envelope_exposes_container_leaf_and_nested_subkeys() {
        let json = r#"{
          "nodes": [
            {"id":"s","type":"start","slug":"start","position":{"x":0,"y":0},
             "data":{"type":"start","label":"Start",
                     "initial":{"id":"in","label":"Input","fields":[
                       {"name":"document","label":"Doc","kind":"file","required":true}
                     ]}}},
            {"id":"ocr","type":"automated_step","slug":"ocr","position":{"x":0,"y":0},
             "data":{"type":"automated_step","label":"OCR",
                     "executionSpec":{"backendType":"kreuzberg","config":{"file":"{{ start.document }}"}},
                     "retryPolicy":{"maxRetries":0,"strategy":{"type":"immediate"}},
                     "deploymentModel":{"mode":"inline"}}},
            {"id":"end","type":"end","position":{"x":0,"y":0},
             "data":{"type":"end","label":"End"}}
          ],
          "edges":[
            {"id":"e1","source":"s","target":"ocr","type":"sequence"},
            {"id":"e2","source":"ocr","target":"end","type":"sequence"}
          ]
        }"#;
        let g: WorkflowGraph = serde_json::from_str(json).expect("deser file-envelope graph");
        let report = analyze(&g).expect("analyze");
        let scope = report.scopes.get("ocr").expect("ocr scope");
        let by_path: std::collections::BTreeMap<&str, &str> =
            scope.iter().map(|e| (e.path.as_str(), e.ty.as_str())).collect();

        assert_eq!(
            by_path.get("start.document").copied(),
            Some("FileRef"),
            "container leaf must be a pickable FileRef; offered: {:?}",
            by_path
        );
        assert_eq!(
            by_path.get("start.document.url").copied(),
            Some("String"),
            "metadata subkey `url` must be nested under the file field, not flat at `start.url`; offered: {:?}",
            by_path
        );
        assert_eq!(by_path.get("start.document.filename").copied(), Some("String"));
        assert_eq!(by_path.get("start.document.content_type").copied(), Some("String"));

        // The pre-fix flat form (the bug from the screenshot) must be gone:
        // `start.url` would imply Start declared a top-level `url` field.
        assert!(
            !by_path.contains_key("start.url"),
            "flat `start.url` must not be offered — that path lives under `document`: {:?}",
            by_path
        );
        assert!(!by_path.contains_key("start.filename"));
        assert!(!by_path.contains_key("start.content_type"));
    }
}
