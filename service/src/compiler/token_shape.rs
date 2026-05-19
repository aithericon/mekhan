//! PROTOTYPE — compiler-derived, shape-aware per-place token schema.
//!
//! # Why this exists
//!
//! Today three independent representations of "what does the token look like
//! here" coexist and nothing forces them to agree:
//!
//! 1. The editor's design-time scope model (`app/.../guard-scope.ts`
//!    `computeScopes`) — a TS reimplementation that flattens *declared*
//!    upstream output-port fields into `input.<field>`.
//! 2. The compiler's lowering (`lower.rs` / `rhai_gen.rs`) — what actually
//!    happens to the token JSON (the `data` wrapper a human task introduces,
//!    the executor envelope an automated step wraps everything in, the
//!    `_`-prefixed metadata, the loop counter).
//! 3. The runtime token (`DynamicToken(serde_json::Value)`); places carry a
//!    `token_schema` but for every business place it is `DynamicToken` (=any).
//!
//! Phase-3 guard validation (`validate.rs::compute_scopes`) is a *flat union
//! of declared port field names*. It will happily accept
//! `input.invoice_amount > 5000` even though, at the place where that guard
//! runs, the token is the `extract` step's executor envelope and
//! `invoice_amount` only ever existed as a *human-task form field nested
//! under `.data`*. The guard silently never matches → the default branch is
//! taken → the run reports "completed" while having done the wrong thing.
//!
//! # What this module does
//!
//! Make the **compiler** the single source of truth. Walk the lowered graph
//! and, for each node, compute a *structural* [`TokenShape`] for the token
//! arriving at / leaving that node, modelling the **real** per-pattern JSON
//! transformations the lowerer performs (verified against `lower.rs`,
//! `rhai_gen.rs::build_merge_logic`, `effect_tokens.rs::HumanTaskResponse`,
//! and a live instance token dump). Then:
//!
//! * attach the derived shape to the matching AIR place's `token_schema`
//!   (replacing the useless `#/definitions/DynamicToken`), and
//! * re-validate every Decision/Loop guard against the *real* shape, with
//!   provenance-aware diagnostics ("`invoice_amount` is not present here; it
//!   last existed as `input.data.invoice_amount: String`, introduced by the
//!   'Review Invoice' human task, and was dropped when the 'Extract Data'
//!   automated step replaced the token with its executor envelope").
//!
//! This is a prototype: it lives alongside the existing Phase-3 pass (it does
//! not rip it out), `analyze()` is pure, and `compile_to_air` is unchanged
//! unless a caller opts in via [`compile_to_air_with_shapes`].

use std::collections::BTreeMap;

use serde_json::Value;

use crate::compiler::error::CompileError;
use crate::compiler::graph::{topo_order, WorkflowDiGraph};
use crate::models::template::{
    FieldKind, MergeStrategy, Port, WorkflowGraph, WorkflowNode, WorkflowNodeData,
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

    fn label(&self) -> &'static str {
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
}

impl Provenance {
    fn new(node: &WorkflowNode, note: impl Into<String>) -> Provenance {
        Provenance {
            node_id: node.id.clone(),
            node_label: node.data.label().to_string(),
            note: note.into(),
        }
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
    fn resolve<'a>(&'a self, segs: &[String]) -> Option<(&'a TokenShape, Option<&'a Provenance>)> {
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
    fn find_by_leaf(&self, name: &str) -> Option<(String, String, Provenance)> {
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

    fn kind_label(&self) -> String {
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
            TokenShape::Scalar(ScalarTy::Number) => serde_json::json!({ "type": "number" }),
            TokenShape::Scalar(ScalarTy::Bool) => serde_json::json!({ "type": "boolean" }),
            TokenShape::Scalar(ScalarTy::String) | TokenShape::Scalar(ScalarTy::Timestamp) => {
                serde_json::json!({ "type": "string" })
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
        let shape = match f.kind {
            // A File field is an object at runtime (`{{ invoice_file.url }}`
            // etc. is used in the fixture), so model the addressable subkeys.
            FieldKind::File => {
                let mut fo = TokenShape::object();
                let p = Provenance::new(node, "uploaded file (catalogue reference)");
                fo.insert("url", TokenShape::Scalar(ScalarTy::String), p.clone());
                fo.insert("filename", TokenShape::Scalar(ScalarTy::String), p.clone());
                fo.insert("content_type", TokenShape::Scalar(ScalarTy::String), p);
                fo
            }
            k => TokenShape::Scalar(ScalarTy::from_kind(k)),
        };
        o.insert(&f.name, shape, Provenance::new(node, note));
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
    /// Two different producers contribute the same leaf name with different
    /// types into one node's accumulator — the flat last-writer-wins
    /// ambiguity, surfaced instead of silently resolved.
    ScopeCollision {
        node_id: String,
        node_label: String,
        leaf: String,
        a: String,
        b: String,
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
            ShapeDiagnostic::ScopeCollision {
                node_id, leaf, a, b, ..
            } => (
                "scope_collision",
                node_id.clone(),
                format!("`{leaf}` is ambiguous: {a} vs {b} (last-writer-wins)."),
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
        WorkflowNodeData::HumanTask { .. } => vec![format!("p_{id}_output")],
        WorkflowNodeData::AutomatedStep { .. } => {
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

        // Loop: `t_*_enter` injects `_loop_<id>_count`, body re-enters with it
        // incremented; the exit arm forwards the token (counter still present).
        WorkflowNodeData::Loop { .. } => {
            let mut o = in_shape.clone();
            o.insert(
                &format!("_loop_{}_count", node.id),
                TokenShape::Scalar(ScalarTy::Number),
                Provenance::new(node, "loop iteration counter (injected by t_*_enter)"),
            );
            o
        }

        // Pure routing / pass-through patterns: token shape unchanged.
        WorkflowNodeData::Decision { .. }
        | WorkflowNodeData::ParallelSplit { .. }
        | WorkflowNodeData::ParallelJoin { .. }
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
    let wg = WorkflowDiGraph::build(graph)?;
    let order = topo_order(&wg)?;

    let mut node_in: BTreeMap<String, TokenShape> = BTreeMap::new();
    let mut node_out: BTreeMap<String, TokenShape> = BTreeMap::new();

    for ni in &order {
        let node = *wg.dag.node_weight(*ni).unwrap();

        // Inbound = shallow-merge of every DAG predecessor's outbound shape.
        // (ParallelJoin's strategy can be DeepMerge; honour it.)
        let deep = matches!(
            node.data,
            WorkflowNodeData::ParallelJoin {
                merge_strategy: MergeStrategy::DeepMerge,
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
    // from the same `resolve` / `locate_field` primitives. The old
    // `flatten_scope(node_in)` only saw the linear control token, so every
    // upstream field was hidden behind a token-replacing automated step (the
    // picker showed the executor envelope, never the parked producer's data).
    let mut scopes: BTreeMap<String, Vec<ScopeEntry>> = BTreeMap::new();
    let mut diagnostics = Vec::new();
    for node in &graph.nodes {
        scopes.insert(
            node.id.clone(),
            reachable_scope(node, graph, &node_in, &node_out, &order, &wg),
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
            check_guard(node, guard, in_shape, &node_out, &order, &wg, &mut diagnostics);
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
fn collect_leaves(
    shape: &TokenShape,
    prefix: &str,
    prov: Option<&Provenance>,
    out: &mut Vec<(String, String, Provenance)>,
) {
    match shape {
        TokenShape::Object(map) if !map.is_empty() => {
            for (k, f) in map {
                let path = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
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
/// `p_{id}_data` place that read-arcs can borrow) iff it is a HumanTask or
/// AutomatedStep — the only patterns `lower.rs::split_outputs` splits. Start
/// parks `ProcessStarted`, not its business input, so a Start field is only
/// reachable while it is still *on the control token* (the control-resident
/// branch below), never via a synthesized read-arc.
fn is_parked_producer(graph: &WorkflowGraph, id: &str) -> bool {
    graph.nodes.iter().any(|n| {
        n.id == id
            && matches!(
                n.data,
                WorkflowNodeData::HumanTask { .. } | WorkflowNodeData::AutomatedStep { .. }
            )
    })
}

/// Borrow-reachable scope at a node: exactly the references the compiler
/// (`check_guard` / `guard_readarc_plan`) resolves — (1) every leaf still on
/// the node's own inbound control token (resolvable with no read-arc), plus
/// (2) every leaf an upstream *parked producer* owns, resolved through the
/// SAME `locate_field` the read-arc synthesis uses (nearest producer wins).
/// The user types the bare `input.<leaf>`; the compiler rebinds it to the
/// producer's parked place. Replaces the old `flatten_scope(node_in)`, which
/// only saw the linear control token and so hid every field produced before a
/// token-replacing automated step.
fn reachable_scope(
    node: &WorkflowNode,
    graph: &WorkflowGraph,
    node_in: &BTreeMap<String, TokenShape>,
    node_out: &BTreeMap<String, TokenShape>,
    order: &[petgraph::graph::NodeIndex],
    wg: &WorkflowDiGraph,
) -> Vec<ScopeEntry> {
    // leaf -> entry. A control-resident leaf wins: it needs no read-arc and
    // `check_guard` tries `resolve()` (control token) before `locate_field`.
    let mut by_leaf: BTreeMap<String, ScopeEntry> = BTreeMap::new();

    // (1) Control-token-resident — Start fields before any task, the slim
    //     control keys, the envelope an automated step actually forwards.
    if let Some(in_shape) = node_in.get(&node.id) {
        let mut leaves = Vec::new();
        collect_leaves(in_shape, "", None, &mut leaves);
        for (dotted, ty, prov) in leaves {
            let leaf = dotted.rsplit('.').next().unwrap_or(&dotted).to_string();
            by_leaf.insert(
                leaf,
                ScopeEntry {
                    path: format!("input.{dotted}"),
                    ty,
                    producer_node: prov.node_id,
                    producer_label: prov.node_label,
                    note: prov.note,
                },
            );
        }
    }

    // (2) Read-arc-reachable — every leaf a strictly-upstream parked producer
    //     owns. Resolve through `locate_field` (nearest-producer-wins, the
    //     identical call `guard_readarc_plan` makes) so the picker can never
    //     offer a path the compiler won't bind, nor hide one it would.
    let pos = topo_pos(order, wg);
    let self_pos = pos.get(&node.id).copied();
    let mut candidates: Vec<String> = Vec::new();
    if let Some(self_pos) = self_pos {
        for ni in order.iter() {
            let prod = *wg.dag.node_weight(*ni).unwrap();
            if pos.get(&prod.id).copied().unwrap_or(usize::MAX) >= self_pos {
                continue;
            }
            if let Some(shape) = node_out.get(&prod.id) {
                let mut leaves = Vec::new();
                collect_leaves(shape, "", None, &mut leaves);
                for (dotted, _ty, _prov) in leaves {
                    candidates.push(dotted.rsplit('.').next().unwrap_or(&dotted).to_string());
                }
            }
        }
    }
    candidates.sort();
    candidates.dedup();
    for leaf in candidates {
        if by_leaf.contains_key(&leaf) {
            continue; // already control-resident (resolves without a read-arc)
        }
        if is_control_leaf(&format!("input.{leaf}")) {
            continue; // identity/routing — stays on the slim control token
        }
        if let Some(fl) = locate_field(&leaf, node, node_out, order, wg) {
            if !is_parked_producer(graph, &fl.producer_id) {
                continue; // owner has no parked place — not borrow-reachable
            }
            by_leaf.insert(
                leaf.clone(),
                ScopeEntry {
                    path: format!("input.{leaf}"),
                    ty: fl.ty,
                    producer_node: fl.producer_id,
                    producer_label: fl.producer_label,
                    note: fl.note,
                },
            );
        }
    }

    by_leaf.into_values().collect()
}

/// Where an upstream-produced field actually lives (which node owns it, its
/// path within that producer's parked token, and its type).
struct FieldLocation {
    producer_id: String,
    producer_label: String,
    path: String,
    ty: String,
    note: String,
}

fn topo_pos(order: &[petgraph::graph::NodeIndex], wg: &WorkflowDiGraph) -> BTreeMap<String, usize> {
    let mut pos = BTreeMap::new();
    for (i, ni) in order.iter().enumerate() {
        pos.insert(wg.dag.node_weight(*ni).unwrap().id.clone(), i);
    }
    pos
}

/// Find the nearest strictly-upstream producer of `leaf` (the node whose
/// parked output a read-arc would borrow it from).
fn locate_field(
    leaf: &str,
    node: &WorkflowNode,
    node_out: &BTreeMap<String, TokenShape>,
    order: &[petgraph::graph::NodeIndex],
    wg: &WorkflowDiGraph,
) -> Option<FieldLocation> {
    let pos = topo_pos(order, wg);
    let self_pos = *pos.get(&node.id)?;

    let mut producer: Option<(usize, String, String, Provenance)> = None;
    for ni in order.iter() {
        let prod = *wg.dag.node_weight(*ni).unwrap();
        let pp = pos[&prod.id];
        if pp >= self_pos {
            continue;
        }
        if let Some(shape) = node_out.get(&prod.id) {
            if let Some((dotted, ty, prov)) = shape.find_by_leaf(leaf) {
                if producer.as_ref().map(|(b, ..)| pp > *b).unwrap_or(true) {
                    producer = Some((pp, dotted, ty, prov));
                }
            }
        }
    }
    let (_pp, dotted, ty, prov) = producer?;
    Some(FieldLocation {
        producer_id: prov.node_id.clone(),
        producer_label: prov.node_label.clone(),
        path: format!("input.{dotted}"),
        ty,
        note: prov.note.clone(),
    })
}

#[allow(clippy::too_many_arguments)]
fn check_guard(
    node: &WorkflowNode,
    guard: &str,
    in_shape: &TokenShape,
    node_out: &BTreeMap<String, TokenShape>,
    order: &[petgraph::graph::NodeIndex],
    wg: &WorkflowDiGraph,
    out: &mut Vec<ShapeDiagnostic>,
) {
    for (segs, cmp) in extract_input_paths(guard) {
        let referenced = format!("input.{}", segs.join("."));
        match in_shape.resolve(&segs) {
            Some((shape, _prov)) => {
                // Resolved — opportunistic scalar/comparison type check.
                if let (TokenShape::Scalar(ty), Some(lit)) = (shape, cmp) {
                    if !scalar_satisfies(ty, &lit) {
                        out.push(ShapeDiagnostic::GuardTypeMismatch {
                            node_id: node.id.clone(),
                            node_label: node.data.label().to_string(),
                            guard: guard.to_string(),
                            referenced,
                            found: ty.label().to_string(),
                            note: format!("compared against a {} literal", lit.label()),
                        });
                    }
                }
            }
            None => {
                // Not on the inbound control token. Post-foundation this is
                // the *normal, correct* case if an upstream parked producer
                // owns it: `guard_readarc_plan` synthesizes a non-consuming
                // read-arc into that producer's `p_{id}_data` and rebinds the
                // reference. Emitting `DroppedUpstream` here would directly
                // contradict the compiler (and the picker, which now surfaces
                // exactly these). Only a reference no upstream node produces
                // at all is genuinely unresolved. This mirrors
                // `guard_readarc_plan` so picker ↔ read-arc synthesis ↔ this
                // diagnostic can never disagree.
                let leaf = segs.last().cloned().unwrap_or_default();
                if locate_field(&leaf, node, node_out, order, wg).is_none() {
                    out.push(ShapeDiagnostic::UnresolvedGuardPath {
                        node_id: node.id.clone(),
                        node_label: node.data.label().to_string(),
                        guard: guard.to_string(),
                        referenced,
                    });
                }
            }
        }
    }
}

// ─── Tiny guard expression scanner ──────────────────────────────────────────
//
// `rhai_scope::extract_qualified_refs` only yields 2-segment `ident.field`
// refs; we need full dotted paths *and* the comparison literal for the type
// check. This is a deliberately small scanner — not a Rhai parser.

#[derive(Debug, Clone)]
enum LitTy {
    Number,
    Bool,
    Str,
}

impl LitTy {
    fn label(&self) -> &'static str {
        match self {
            LitTy::Number => "number",
            LitTy::Bool => "bool",
            LitTy::Str => "string",
        }
    }
}

fn scalar_satisfies(ty: &ScalarTy, lit: &LitTy) -> bool {
    matches!(
        (ty, lit),
        (ScalarTy::Number, LitTy::Number)
            | (ScalarTy::Bool, LitTy::Bool)
            | (ScalarTy::String, LitTy::Str)
            | (ScalarTy::Timestamp, LitTy::Str)
            | (ScalarTy::Json, _)
    )
}

/// Returns each `input.<a>.<b>...` path (segments after `input`) found in the
/// guard, paired with the literal it is compared against on the immediate RHS
/// (best-effort, for the type check).
fn extract_input_paths(src: &str) -> Vec<(Vec<String>, Option<LitTy>)> {
    let bytes: Vec<char> = src.chars().collect();
    let mut i = 0;
    let mut out = Vec::new();
    while i < bytes.len() {
        if src[byte_idx(&bytes, i)..].starts_with("input")
            && (i + 5 >= bytes.len() || bytes[i + 5] == '.')
            && (i == 0 || !is_ident(bytes[i - 1]))
        {
            i += 5;
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
                out.push((segs, lit));
            }
        } else {
            i += 1;
        }
    }
    out
}

fn byte_idx(chars: &[char], char_i: usize) -> usize {
    chars[..char_i].iter().map(|c| c.len_utf8()).sum()
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
                ShapeDiagnostic::ScopeCollision {
                    node_label,
                    node_id,
                    leaf,
                    a,
                    b,
                } => {
                    s.push_str(&format!(
                        "✖ COLLISION   [{node_label} ({node_id})]\n  `{leaf}` is ambiguous: \
                         {a} vs {b} (flat accumulator is last-writer-wins).\n\n"
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
/// publish when it's too late. This is what `POST /api/compile` (or a sibling
/// `/api/analyze`) should additionally return on every edit.
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
fn is_control_leaf(path: &str) -> bool {
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

/// One guard reference that must be lowered to a physical read-arc into a
/// producer's parked data place. The compiler-as-borrow-checker output.
pub(crate) struct ReadArcBind {
    /// Node whose Decision/Loop guard holds the reference.
    pub consumer_node_id: String,
    /// Literal text in the guard, e.g. `input.invoice_amount`.
    pub referenced: String,
    /// Data-yielding node that owns the field (its `p_{producer}_data`).
    pub producer_node: String,
    /// Path within that producer's parked token, e.g. `data.invoice_amount`.
    pub producer_path: String,
}

/// For every Decision/Loop guard, resolve each non-control `input.<path>`
/// reference to the parked data place that owns it (via shape provenance).
/// This is the compiler playing borrow-checker: it proves which `let`-owned
/// data token holds the value and emits the `&`-borrow plan. A reference that
/// no upstream data-yielding node produces *and* isn't on the pre-yield
/// control token is a hard `CompileError`.
pub(crate) fn guard_readarc_plan(
    graph: &WorkflowGraph,
) -> Result<Vec<ReadArcBind>, CompileError> {
    let report = analyze(graph)?;
    let wg = WorkflowDiGraph::build(graph)?;
    let order = topo_order(&wg)?;
    let mut binds = Vec::new();

    for node in &graph.nodes {
        let guards: Vec<String> = match &node.data {
            WorkflowNodeData::Decision { conditions, .. } => conditions
                .iter()
                .filter(|c| !c.guard.trim().is_empty())
                .map(|c| c.guard.clone())
                .collect(),
            WorkflowNodeData::Loop { loop_condition, .. }
                if !loop_condition.trim().is_empty() =>
            {
                vec![loop_condition.clone()]
            }
            // Result-mapping expressions (End/Failure, added on main)
            // reference `input.<path>` in transition logic — same shape
            // resolution + read-arc synthesis as guards.
            WorkflowNodeData::End { result_mapping, .. } => result_mapping
                .iter()
                .map(|m| m.expression.clone())
                .filter(|s| !s.trim().is_empty())
                .collect(),
            WorkflowNodeData::Failure {
                error_result_mapping,
                ..
            } => error_result_mapping
                .iter()
                .map(|m| m.expression.clone())
                .filter(|s| !s.trim().is_empty())
                .collect(),
            _ => continue,
        };
        for guard in &guards {
            for (segs, _lit) in extract_input_paths(guard) {
                let referenced = format!("input.{}", segs.join("."));
                if is_control_leaf(&referenced) {
                    continue; // stays on the slim control token
                }
                let leaf = segs.last().cloned().unwrap_or_default();
                match locate_field(&leaf, node, &report.node_out, &order, &wg) {
                    Some(fl) => {
                        let producer_path = fl
                            .path
                            .strip_prefix("input.")
                            .unwrap_or(&fl.path)
                            .to_string();
                        binds.push(ReadArcBind {
                            consumer_node_id: node.id.clone(),
                            referenced,
                            producer_node: fl.producer_id,
                            producer_path,
                        });
                    }
                    None => {
                        // No upstream data-yielding producer. If it still
                        // resolves on the pre-yield control token (a Start
                        // field before any task), leave it as `input.*`.
                        let on_control = report
                            .node_in
                            .get(&node.id)
                            .map(|s| s.resolve(&segs).is_some())
                            .unwrap_or(false);
                        if !on_control {
                            let available = report
                                .scopes
                                .get(&node.id)
                                .map(|v| v.iter().map(|e| e.path.clone()).collect())
                                .unwrap_or_default();
                            return Err(CompileError::GuardUnresolved {
                                node_id: node.id.clone(),
                                identifier: referenced,
                                available,
                            });
                        }
                    }
                }
            }
        }
    }
    Ok(binds)
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
                "url": "/api/files/blob/abc",
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

        // (1) Picker offers the upstream parked producer's field, attributed
        //     to `review` — not the `extract` envelope it physically arrives
        //     wrapped in.
        let scope = report.scopes.get("check-amount").expect("decision scope");
        let amt = scope
            .iter()
            .find(|e| e.path == "input.invoice_amount")
            .unwrap_or_else(|| {
                panic!(
                    "invoice_amount must be pickable at the decision; offered: {:?}",
                    scope.iter().map(|e| &e.path).collect::<Vec<_>>()
                )
            });
        assert_eq!(amt.producer_node, "review");
        assert_eq!(amt.ty, "Number");

        // (2) The read-arc synthesis resolves the IDENTICAL reference to the
        //     same producer (the compiler-as-borrow-checker).
        let binds = guard_readarc_plan(&g).expect("readarc plan");
        assert!(
            binds.iter().any(|b| b.consumer_node_id == "check-amount"
                && b.referenced == "input.invoice_amount"
                && b.producer_node == "review"),
            "guard_readarc_plan must bind input.invoice_amount -> review, got {:?}",
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
                    referenced, "input.invoice_amount",
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
}
