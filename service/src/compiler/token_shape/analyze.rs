use std::collections::BTreeMap;

use crate::compiler::error::CompileError;
use crate::compiler::graph::{topo_order, WorkflowDiGraph};
use crate::models::template::{
    ChannelJoin, ChannelPlane, DeploymentModel, ElementType, JoinMode, MergeStrategy,
    TaskBlockConfig, WorkflowEdge, WorkflowGraph, WorkflowNode, WorkflowNodeData,
};

use super::*; // ─── Per-node shape derivation ──────────────────────────────────────────────

/// One reachable, still-live reference the editor variable picker should
/// offer at a node — the producer-namespaced replacement for the flat TS
/// `computeScopes`. The `path` is what you'd actually type in a guard; the
/// producer attribution is the thing the flat model throws away.
#[derive(Debug, Clone)]
pub struct ScopeEntry {
    pub path: String,
    pub ty: TyDescriptor,
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
        WorkflowNodeData::AutomatedStep { .. } | WorkflowNodeData::SubWorkflow { .. } => {
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
///
/// Registry-driven: the per-variant transform lives behind
/// `NodeDecl::token_shape` (one `out_shape_<kind>` free fn per variant, below).
/// This dispatcher just looks up the decl and calls through. Every variant
/// declares a `token_shape` hook — a missing one is a `None` here, which is a
/// hard panic (the `token_shape_hook_declared_for_every_variant` conformance
/// test in `nodes/mod.rs` makes that a test failure first), NOT a silent
/// pass-through default.
fn out_shape(node: &WorkflowNode, in_shape: &TokenShape) -> TokenShape {
    let decl = crate::nodes::lookup_by_variant(&node.data)
        .expect("every WorkflowNodeData variant is registered in crate::nodes::NODES");
    let f = decl.token_shape.unwrap_or_else(|| {
        panic!(
            "registry bug: variant `{}` has no token_shape hook (out_shape would silently \
             default to pass-through)",
            decl.wire_name
        )
    });
    f(node, in_shape)
}

// ── Per-variant outbound shape transforms (registry hooks) ──────────────────
// One `pub(crate) fn out_shape_<kind>` per variant, referenced by the matching
// `NodeDecl::token_shape`. Bodies are byte-identical to the pre-refactor
// `out_shape` match arms; they live here (not in `nodes/<kind>.rs`) because the
// `pub(super)` shape constructors (`TokenShape::object`/`insert`,
// `Provenance::new`, `port_to_shape`, `repeater_element_to_shape`) are scoped to
// this module — the same reason the `lower_*` fns live in `compiler/lower/`.

/// Start emits its declared `initial` port + the instance-seeded
/// `_instance_id`, plus `_process_name` when a process is registered.
pub(crate) fn out_shape_start(node: &WorkflowNode, _in_shape: &TokenShape) -> TokenShape {
    let WorkflowNodeData::Start {
        initial,
        process_name,
        ..
    } = &node.data
    else {
        unreachable!("out_shape_start on non-Start variant");
    };
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

/// Human task: `t_*_finalize` runs `build_merge_logic("state","signal")`
/// = `for k in signal.keys() { result[k] = signal[k] }`. The signal is
/// a `HumanTaskResponse` whose **form submission is nested under
/// `.data`** (effect_tokens.rs:365 — "The `data` field contains the
/// form submission"). So the output is the inbound token PLUS the
/// response envelope, with the user-entered fields under `data`. This
/// is the divergence the flat editor model erases.
pub(crate) fn out_shape_human_task(node: &WorkflowNode, in_shape: &TokenShape) -> TokenShape {
    let WorkflowNodeData::HumanTask {
        steps, steps_ref, ..
    } = &node.data
    else {
        unreachable!("out_shape_human_task on non-HumanTask variant");
    };
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
    // Dynamic form: the form block list (and thus its output field names) is
    // sourced at runtime from a producer ref, so the result envelope is opaque
    // at compile time. Keep the title/steps request-scaffold leaves, but the
    // `data` submission envelope degrades to `Any` (no per-Input-field union).
    if steps_ref.is_some() {
        o.insert(
            "data",
            TokenShape::Any,
            Provenance::new(
                node,
                "dynamic form submission envelope — field names unknown at compile time",
            ),
        );
        for (k, note) in [
            ("title", "human-task request scaffold"),
            ("instructions_mdsvex", "human-task request scaffold"),
        ] {
            o.insert(k, TokenShape::Any, Provenance::new(node, note));
        }
        o.insert(
            "steps",
            TokenShape::Array(Box::new(TokenShape::Any)),
            Provenance::new(node, "human-task request scaffold (dynamic steps)"),
        );
        return o;
    }
    let mut form = port_to_shape(
        &crate::models::template::derive_human_task_output_port(steps),
        node,
        "HUMAN-TASK FORM FIELD — nested under `data` (HumanTaskResponse.data)",
    );
    // Feature B: each Repeater block in this HumanTask contributes a
    // typed array `<output_slug>: Array<{<sub_fields>}>` to the
    // form envelope. Downstream consumers pick
    // `<human_task_slug>.<output_slug>[*].<sub_field>` via the same
    // `[*]` synthetic-child picker affordance as any other array.
    // Validation (collision with form-field names, malformed refs)
    // runs in `validate_repeaters`; here we just emit the shape
    // assuming the config is well-formed.
    for step in steps {
        for block in &step.blocks {
            if let TaskBlockConfig::Repeater {
                output_slug,
                blocks: repeater_blocks,
                ..
            } = block
            {
                let key = output_slug.trim();
                if key.is_empty() {
                    continue;
                }
                let elem = repeater_element_to_shape(repeater_blocks, node);
                form.insert(
                    key,
                    TokenShape::Array(Box::new(elem)),
                    Provenance::new(
                        node,
                        "Repeater typed array output — one element per sub-form row",
                    ),
                );
            }
        }
    }
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

/// Automated step: `prepare` snapshots the inbound token into
/// `spec.inputs["input.json"]` and the node forwards the **executor
/// result envelope** (`executor_lifecycle` → `to_output` = `#{ output:
/// done }`). The upstream business token is NOT propagated — anything
/// downstream sees `{ execution_id, job_id, run, status, source,
/// detail{ outputs, .. } }`. Business output (if the step declares an
/// output port) is under `detail.outputs`, never flattened back.
///
/// Shared by AutomatedStep and Agent (same executor-envelope shape — see the
/// `agent_degenerate_lowers_byte_identical_to_llm_automated_step` contract);
/// both decls point `token_shape` at this fn.
pub(crate) fn out_shape_automated_step(node: &WorkflowNode, _in_shape: &TokenShape) -> TokenShape {
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
    // Lease-bearing steps merge the granted typed lease into the parked data
    // envelope under `lease` (see `lower_pooled_body`'s `t_<id>_to_output`).
    // Surface a `lease` field so a downstream `<slug>.lease.<field>` borrow
    // resolves through the standard read-arc pipeline. Opaque (not the kind's
    // typed lease schema) because shape analysis has no `known_resources` to
    // resolve the kind here — the leaf is findable + the nested `.field` is
    // appended verbatim, which is all the borrow resolver needs. Emitted for
    // BOTH lease-bearing paths (they share `lower_pooled_body`):
    //   - `Executor { capacity: Some }` (concurrency_limit admission, R2/R3), and
    //   - `Scheduled { operation: Lease }` (datacenter lease, R4).
    // Plain executor dispatch + `Scheduled { operation: Submit }` stage no lease
    // and stay byte-identical.
    let holds_lease = matches!(
        &node.data,
        WorkflowNodeData::AutomatedStep {
            deployment_model: DeploymentModel::Executor {
                capacity: Some(_),
                ..
            },
            ..
        }
    );
    if holds_lease {
        o.insert(
            "lease",
            TokenShape::Opaque("typed pool lease (Lease__<backend>)".to_string()),
            p("granted pool lease, staged into the body + parked"),
        );
    }
    o
}

/// Loop: `t_*_enter` injects a declared `<slug>: { iteration: 0 }`
/// namespace on the control token; body re-entry increments
/// `<slug>.iteration`; the exit arm forwards the token unchanged so
/// post-loop nodes can still read the final count. The namespace is
/// first-class — `node_output_fields` declares `iteration: number`,
/// the picker / `.pyi` overlay surface it as `<slug>.iteration`, the
/// runner auto-promotes `<slug>` as a Python global, and Rhai
/// expressions in `loopCondition` / guards / End mappings reference it
/// as `input.<slug>.iteration` (or `<slug>.iteration` for the
/// slug-borrow rewrite path).
pub(crate) fn out_shape_loop(node: &WorkflowNode, in_shape: &TokenShape) -> TokenShape {
    let WorkflowNodeData::Loop { accumulators, .. } = &node.data else {
        unreachable!("out_shape_loop on non-Loop variant");
    };
    let mut o = in_shape.clone();
    let mut ns = TokenShape::object();
    ns.insert(
        "iteration",
        TokenShape::Scalar(ScalarTy::Number),
        Provenance::new(node, "loop iteration counter (declared producer field)"),
    );
    // Each accumulator is an additional parked field. `init` is opaque
    // Rhai (could be any JSON shape), so the declared shape is `Any`.
    for acc in accumulators {
        ns.insert(
            &acc.var,
            TokenShape::Any,
            Provenance::new(node, "loop accumulator (declared producer field)"),
        );
    }
    o.insert(
        &node.slug(),
        ns,
        Provenance::new(node, "loop namespace (`<slug>.iteration` + accumulators)"),
    );
    o
}

/// LeaseScope: holds one allocation across its interior and parks the held
/// grant write-once at `p_<id>_data` under a `lease` key. The held grant is
/// opaque to the compiler (the allocator fills it at runtime), so the `lease`
/// namespace is declared `Any` — body steps + downstream blocks borrow
/// `<scope>.lease.<field>` (e.g. `<scope>.lease.executor_namespace`) through the
/// standard read-arc pipeline (`resolve_ref`'s `resolves_under_opaque` path).
/// Mirrors a leased `Loop`'s `<slug>.lease` namespace, minus the iteration
/// counter / accumulators (a LeaseScope only ever parks the lease).
pub(crate) fn out_shape_lease_scope(node: &WorkflowNode, in_shape: &TokenShape) -> TokenShape {
    let WorkflowNodeData::LeaseScope { .. } = &node.data else {
        unreachable!("out_shape_lease_scope on non-LeaseScope variant");
    };
    let mut o = in_shape.clone();
    let mut ns = TokenShape::object();
    ns.insert(
        "lease",
        TokenShape::Any,
        Provenance::new(node, "lease-scope held lease (`<scope>.lease.<field>`)"),
    );
    o.insert(
        &node.slug(),
        ns,
        Provenance::new(node, "lease-scope namespace (`<scope>.lease`)"),
    );
    o
}

pub(crate) fn out_shape_map(node: &WorkflowNode, in_shape: &TokenShape) -> TokenShape {
    let WorkflowNodeData::Map { output, .. } = &node.data else {
        unreachable!("out_shape_map on non-Map variant");
    };
    let elem = match output {
        Some(p) if !p.fields.is_empty() => {
            port_to_shape(p, node, "map gathered element (declared output port)")
        }
        _ => TokenShape::Any,
    };
    let mut o = in_shape.clone();
    o.insert(
        &node.slug(),
        TokenShape::Array(Box::new(elem)),
        Provenance::new(node, "map gathered collection (`<slug>[*].<field>`)"),
    );
    o
}

/// Sub-workflow: `t_*_join` maps the child's terminal result onto the
/// workflow token via the declared `output` port. With declared fields
/// downstream sees exactly those; otherwise the child result is opaque
/// here (we can't see across the spawned-child boundary at analyze time).
pub(crate) fn out_shape_sub_workflow(node: &WorkflowNode, in_shape: &TokenShape) -> TokenShape {
    let WorkflowNodeData::SubWorkflow { output, .. } = &node.data else {
        unreachable!("out_shape_sub_workflow on non-SubWorkflow variant");
    };
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

/// Pure routing / pass-through patterns: token shape unchanged. Shared by
/// every control-flow / marker variant (Decision, ParallelSplit, Join, Scope,
/// PhaseUpdate, ProgressUpdate, Failure, Delay, Timeout, End). Trigger also
/// reuses it for completeness (Trigger never lowers, so its `out_shape` is
/// never consulted in practice, but declaring the hook keeps the conformance
/// test honest).
pub(crate) fn out_shape_passthrough(_node: &WorkflowNode, in_shape: &TokenShape) -> TokenShape {
    in_shape.clone()
}

/// Compute inbound + outbound shapes for every node, then validate guards
/// against the *real* inbound shape.
///
/// `known_globals` is the per-template named-global registry (resources +
/// assets); it is threaded into `reachable_scope` (the picker offers a
/// "Globals" group of `<name>.<field>` entries) and `check_guard` (a
/// `<name>.<field>` ref to a known global resolves instead of flagging a false
/// `UnresolvedGuardPath`). Pass an empty map (`&Default::default()`) on the
/// internal compile paths that don't carry one yet — the surface degrades to
/// the producer-only scope, exactly as before this registry existed.
pub fn analyze(
    graph: &WorkflowGraph,
    known_globals: &crate::compiler::named_global::KnownGlobals,
) -> Result<ShapeReport, CompileError> {
    use crate::compiler::borrow::planners::guard::{check_guard, reachable_scope};

    let wg = WorkflowDiGraph::build(graph)?;
    let order = topo_order(&wg)?;
    // Author-facing `<slug>.<field>` namespace — built once; a hard
    // `SlugConflict` here propagates out (the editor renders it via
    // `surface_types`'s `GraphIncomplete`, publish blocks via `validate_guards`).
    let slugs = slug_index(graph)?;

    let mut node_in: BTreeMap<String, TokenShape> = BTreeMap::new();
    let mut node_out: BTreeMap<String, TokenShape> = BTreeMap::new();

    // Make the workflow's `definitions` available to `port_to_shape`'s
    // schema-override arm for the whole derivation pass, so `$ref`s inside a
    // `port.schema` override resolve into the structural shadow. See
    // [`super::port::with_definitions`] for why this is a thread-local bracket
    // rather than an extra hook argument.
    crate::compiler::token_shape::port::with_definitions(&graph.definitions, || {
        for ni in &order {
            let node = *wg.dag.node_weight(*ni).unwrap();

            // Inbound = shallow-merge of every DAG predecessor's outbound shape.
            // (Join's strategy can be DeepMerge; honour it.)
            let deep = matches!(
                &node.data,
                WorkflowNodeData::Join {
                    mode: JoinMode::All,
                    merge_strategy: Some(MergeStrategy::DeepMerge),
                    ..
                }
            );
            // EDGE-AWARE propagation: a CHANNEL edge (one whose `source_handle`
            // names a declared channel on the producer) contributes the channel
            // PAYLOAD shape — the `each`/`gather` projection IS the consumer's
            // input token (`lower/channels.rs`) — NOT the producer's whole
            // outbound envelope (which never reaches the consumer's input place
            // on that edge). Non-channel edges keep the existing
            // whole-envelope merge byte-identical.
            //
            // LIMITATION: a channel-`each` edge with a NON-OBJECT element (a
            // scalar / `Any` payload) makes the consumer's whole input token
            // that bare value. With a sole inbound edge we model it directly;
            // under a multi-predecessor merge there is no key to merge it
            // under, so the inbound degrades to `Any` rather than inventing
            // one.
            let mut inbound = TokenShape::object();
            let mut had_pred = false;
            let mut contributions = 0usize;
            let mut non_object_channel: Option<TokenShape> = None;
            for eref in wg.dag.edges_directed(*ni, petgraph::Direction::Incoming) {
                use petgraph::visit::EdgeRef;
                let pred = *wg.dag.node_weight(eref.source()).unwrap();
                let edge: &WorkflowEdge = eref.weight();
                if let Some(contrib) = channel_edge_contribution(pred, edge, &graph.definitions) {
                    had_pred = true;
                    contributions += 1;
                    match contrib {
                        TokenShape::Object(_) => inbound.merge_from(&contrib, deep),
                        other => non_object_channel = Some(other),
                    }
                } else if let Some(p_out) = node_out.get(&pred.id) {
                    inbound.merge_from(p_out, deep);
                    had_pred = true;
                    contributions += 1;
                }
            }
            let inbound = if !had_pred {
                TokenShape::Any
            } else if let Some(payload) = non_object_channel {
                if contributions == 1 {
                    payload
                } else {
                    TokenShape::Any // see the LIMITATION note above
                }
            } else {
                inbound
            };

            let outbound = out_shape(node, &inbound);
            node_in.insert(node.id.clone(), inbound);
            node_out.insert(node.id.clone(), outbound);
        }
    });

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
            reachable_scope(
                node,
                graph,
                &node_in,
                &node_out,
                &order,
                &wg,
                &slugs,
                known_globals,
            ),
        );
    }

    // Guard re-validation against the real shape. The set of Rhai-bearing
    // sources whose *real-shape* diagnostics surface in the editor is
    // intentionally narrower than the publish-time `guard_rhai_sources` set:
    // Decision/Loop/Delay/Timeout only (End/Failure result mappings are
    // syntax+ref-checked at publish, but not shape-diagnosed inline). Preserved
    // byte-identically — this loop feeds only the editor `analyze` endpoint's
    // `diagnostics`, so widening it would change the editor contract.
    for node in &graph.nodes {
        let guards: Vec<&str> = match &node.data {
            WorkflowNodeData::Decision { conditions, .. } => conditions
                .iter()
                .filter(|c| !c.guard.trim().is_empty())
                .map(|c| c.guard.as_str())
                .collect(),
            WorkflowNodeData::Loop { loop_condition, .. } if !loop_condition.trim().is_empty() => {
                vec![loop_condition.as_str()]
            }
            // Delay/Timeout duration expressions borrow upstream refs just
            // like a Loop condition — re-validate against the real shape so
            // an unresolved `<slug>.<field>` in the duration surfaces as an
            // inline editor diagnostic, not a runtime failure.
            WorkflowNodeData::Delay {
                duration_ms_expr, ..
            }
            | WorkflowNodeData::Timeout {
                duration_ms_expr, ..
            } if !duration_ms_expr.trim().is_empty() => {
                vec![duration_ms_expr.as_str()]
            }
            _ => continue,
        };
        let in_shape = match node_in.get(&node.id) {
            Some(s) => s,
            None => continue,
        };
        for guard in guards {
            check_guard(
                node,
                guard,
                &slugs,
                graph,
                in_shape,
                &node_out,
                &pos,
                // The named-global registry is threaded through here so the
                // editor analyze path defers resource/asset heads instead of
                // flagging them unresolved; the compile-time guard pass passes
                // the real registry via `guard_readarc_plan`. Empty on the
                // internal compile callers that don't carry one.
                Some(known_globals),
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

/// The shape a CHANNEL edge contributes to its consumer's inbound token.
/// `Some` iff `edge.source_handle` names a declared channel on `producer`
/// (only AutomatedSteps declare channels); `None` means "not a channel edge —
/// merge the producer's whole outbound envelope as before".
///
/// A channel edge's consumer receives the channel PAYLOAD, never the
/// producer's executor envelope (docs/25 §7; `lower/channels.rs`):
///
/// - **control + `each`** (the default join) — `t_{id}_{name}_each` projects
///   `item.payload` as the consumer's input token, so the contribution is the
///   channel's declared ELEMENT shape: `Json{schema}` parsed via
///   [`json_schema_to_token_shape`] against the workflow `definitions`
///   (`#/definitions/*` + `#/$defs/*` resolve), `Any` → [`TokenShape::Any`],
///   `Binary` (shouldn't occur on a control channel) → opaque.
/// - **control + `gather`** — the counted barrier reduces the episode to
///   `#{ output: [<element>…] }` (key hardcoded in `lower/gather.rs`), so the
///   contribution is `Object{ output: Array(<element>) }`.
/// - **data plane** — bulk bytes ride out-of-band and the OPEN descriptor is
///   never value-referenceable (docs/25 §7), so the contribution is an EMPTY
///   object: the consumer keeps a predecessor (no `Any` degrade) but gains no
///   pickable fields, and a guard ref into the channel stays the existing
///   `UnresolvedGuardPath` diagnostic.
///
/// Top-level fields of a control contribution carry channel-stamped
/// [`Provenance`] (`node_id`/`label` = the producer, `channel: Some(name)`) so
/// `reachable_scope` keeps them visible as genuinely token-resident
/// `input.<path>` entries even though the producer is a parked producer (the
/// qualified `<slug>.<field>` form would NOT bind for a channel payload).
pub(crate) fn channel_edge_contribution(
    producer: &WorkflowNode,
    edge: &WorkflowEdge,
    definitions: &BTreeMap<String, serde_json::Value>,
) -> Option<TokenShape> {
    let handle = edge.source_handle.as_deref()?;
    let WorkflowNodeData::AutomatedStep { channels, .. } = &producer.data else {
        return None;
    };
    let ch = channels.iter().find(|c| c.name == handle)?;
    if matches!(ch.plane, ChannelPlane::Data) {
        // Data channels are edge-wired only, never value-referenceable.
        return Some(TokenShape::object());
    }
    let elem = match &ch.element {
        ElementType::Json { schema } => json_schema_to_token_shape(schema, definitions),
        ElementType::Any => TokenShape::Any,
        // Binary is a data-plane concept; on a (mis-declared) control channel
        // the payload is opaque bytes — never drillable.
        ElementType::Binary { content_type } => {
            TokenShape::Opaque(format!("binary channel element ({content_type})"))
        }
    };
    let join = edge.join.unwrap_or_default();
    let prov = |join_label: &str| {
        let mut p = Provenance::new(
            producer,
            format!("control channel '{}' (join: {join_label})", ch.name),
        );
        p.channel = Some(ch.name.clone());
        p
    };
    match join {
        ChannelJoin::Each => {
            // The element IS the consumer's input token. Stamp the channel
            // provenance onto its top-level fields (nested fields keep the
            // structural-shadow provenance). A non-object element has no
            // field slot to carry provenance — the caller models it as the
            // whole inbound shape directly (sole-edge case) or degrades.
            let mut shape = elem;
            if let TokenShape::Object(map) = &mut shape {
                for f in map.values_mut() {
                    f.prov = prov("each");
                }
            }
            Some(shape)
        }
        ChannelJoin::Gather => {
            let mut o = TokenShape::object();
            o.insert("output", TokenShape::Array(Box::new(elem)), prov("gather"));
            Some(o)
        }
    }
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
/// AutomatedStep, Agent, or SubWorkflow (`lower.rs::split_outputs`) **or a
/// Start** (`lower.rs::park_outputs`). Start forks rather than splits — it
/// parks a write-once copy of its declared inputs to `p_{id}_data` while
/// still forwarding the full token — so `start.<field>` is borrow-reachable
/// downstream exactly like `review.<field>`, and the immediately-following
/// task can still interpolate Start fields off the control token.
///
/// SubWorkflow uses the same split_outputs tail as AutomatedStep, so its
/// declared output fields ride the parked `p_{id}_data` place after the
/// join — `<sub_slug>.<field>` is the only addressable form downstream.
///
/// Agent lowering (loop path) also tails into `split_outputs` for its
/// `p_output` and publishes a `data_port`, so `<agent>.response` /
/// `.turn` / `.final_response` borrows resolve via the same hoist path
/// as AutomatedStep. The degenerate single-shot path is already
/// AutomatedStep-shaped (it virtualises into one), so Agent gets full
/// parked-producer semantics regardless of which lowering branch fires.
pub(crate) fn is_parked_producer(graph: &WorkflowGraph, id: &str) -> bool {
    graph.nodes.iter().any(|n| {
        n.id == id
            && matches!(
                n.data,
                WorkflowNodeData::HumanTask { .. }
                    | WorkflowNodeData::AutomatedStep { .. }
                    | WorkflowNodeData::Agent { .. }
                    | WorkflowNodeData::SubWorkflow { .. }
                    | WorkflowNodeData::Start { .. }
                    | WorkflowNodeData::Loop { .. }
                    | WorkflowNodeData::LeaseScope { .. }
                    | WorkflowNodeData::Join { .. }
                    | WorkflowNodeData::Map { .. }
            )
    })
}

/// True if `id` names a `WorkflowNodeData::Map` node. A Map parks a gathered
/// ARRAY at `p_<map>_data`; downstream borrows must iterate it with a `[*]`
/// boundary (`<map_slug>[*].<field>`). A bare `<map_slug>.<field>` is a hard
/// `MapRefMissingStar` error (caught in `resolve_ref` → `guard_readarc_plan`).
pub(crate) fn is_map_node(graph: &WorkflowGraph, id: &str) -> bool {
    graph
        .nodes
        .iter()
        .any(|n| n.id == id && matches!(n.data, WorkflowNodeData::Map { .. }))
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

/// True if `id` names a `WorkflowNodeData::LeaseScope` node. A LeaseScope parks
/// its held grant write-once at `p_<id>_data` under a `lease` key (an opaque
/// `Any` namespace), so `<scope>.lease.<field>` borrows resolve through the same
/// read-arc pipeline as a leased Loop's `<slug>.lease` (see `resolve_ref`'s
/// Qualified branch, which accepts a LeaseScope producer alongside a Loop).
pub(crate) fn is_lease_scope_node(graph: &WorkflowGraph, id: &str) -> bool {
    graph
        .nodes
        .iter()
        .any(|n| n.id == id && matches!(n.data, WorkflowNodeData::LeaseScope { .. }))
}

pub(crate) fn topo_pos(
    order: &[petgraph::graph::NodeIndex],
    wg: &WorkflowDiGraph,
) -> BTreeMap<String, usize> {
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

// ─── Feature B: picker descriptor walker ────────────────────────────────────

/// Walk a [`TokenShape`] (the producer's parked output) into the picker's
/// wire descriptor. `prov_anchor` carries the parent field's
/// [`Provenance::anchor`] — set only for File envelopes — so plain Objects
/// get `selectable: false` and File envelopes get `selectable: true`. Array
/// elements have no parent provenance and therefore are never anchored.
pub fn collect_scope_tree(shape: &TokenShape, prov_anchor: Option<&ScalarTy>) -> TyDescriptor {
    match shape {
        TokenShape::Object(map) => {
            let mut fields = BTreeMap::new();
            for (k, f) in map {
                fields.insert(
                    k.clone(),
                    collect_scope_tree(&f.shape, f.prov.anchor.as_ref()),
                );
            }
            TyDescriptor::Object {
                fields,
                selectable: prov_anchor.is_some(),
            }
        }
        TokenShape::Array(inner) => TyDescriptor::Array {
            element: Box::new(collect_scope_tree(inner, None)),
        },
        TokenShape::Scalar(s) => TyDescriptor::Scalar {
            name: s.label().to_string(),
        },
        TokenShape::Any => TyDescriptor::Any,
        TokenShape::Opaque(n) => TyDescriptor::Opaque { name: n.clone() },
        // A schema-backed leaf is drillable via its structural shadow: recurse
        // into it so the picker renders the real nested shape. Only genuinely
        // unparseable schemas (mapped to `TokenShape::Any`/`Opaque` by
        // `json_schema_to_token_shape`) surface as any/opaque. The shadow's
        // own provenance has no anchor, so we keep this leaf's `prov_anchor`.
        TokenShape::Schema { structural, .. } => collect_scope_tree(structural, prov_anchor),
    }
}

/// Every `(dotted_path, TyDescriptor, provenance)` *root* of a shape — the
/// tree-DTO sibling of [`collect_leaves`]. Mirrors the same "flatten plain
/// Object containers" rule (HumanTask/AutomatedStep runtime envelopes like
/// `data`, `detail` are not part of the addressable surface), but instead
/// of fanning anchored containers and arrays into per-leaf entries it emits
/// **one entry per top-level user-meaningful field**, carrying the entire
/// nested subtree in [`TyDescriptor`]. The picker walks that subtree to
/// offer drill-down without needing additional calls.
///
/// Concretely: a File envelope `document: { url, filename, content_type }`
/// emits **one** root entry `document` whose `ty` is the nested object
/// (with `selectable: true`); an array of objects `tasks: Array<Object>`
/// emits one root entry `tasks` whose `ty` is `Array{ element: Object }`.
pub(crate) fn collect_scope_roots(
    shape: &TokenShape,
    prefix: &str,
    prov: Option<&Provenance>,
    out: &mut Vec<(String, TyDescriptor, Provenance)>,
) {
    match shape {
        TokenShape::Object(map) if !map.is_empty() => {
            // Anchored container (currently: File envelopes) — emit one rich
            // root carrying the full nested tree; do NOT recurse into
            // children at root level. Per-leaf addressability is preserved by
            // the picker walking `ty.fields` instead of by fan-out.
            if prov.and_then(|p| p.anchor.as_ref()).is_some() {
                if let Some(p) = prov {
                    out.push((
                        prefix.to_string(),
                        collect_scope_tree(shape, p.anchor.as_ref()),
                        p.clone(),
                    ));
                }
            } else {
                // Plain Object — runtime envelope. Descend, RESETTING the
                // prefix to the bare child key (matches `collect_leaves`'s
                // long-standing rule: `data.amount` → `amount`).
                for (k, f) in map {
                    collect_scope_roots(&f.shape, k, Some(&f.prov), out);
                }
            }
        }
        // Scalar / Array / Any / Opaque / empty Object — each is a single
        // pickable root entry.
        _ => {
            if let Some(p) = prov {
                out.push((
                    prefix.to_string(),
                    collect_scope_tree(shape, p.anchor.as_ref()),
                    p.clone(),
                ));
            }
        }
    }
}
