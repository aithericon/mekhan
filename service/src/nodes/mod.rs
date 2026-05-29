//! Declarative node-type registry — one `NodeDecl` per `WorkflowNodeData`
//! variant, stored in a static `&[NodeDecl]` slice.
//!
//! Mirrors [`crate::backends`] for `ExecutionBackendType`. The single
//! source of truth for per-variant dispatch — `compiler/lower/mod.rs` and
//! `yjs/doc_ops.rs::write_node_config` both look up the decl and call
//! through its fn pointers; there are no legacy per-variant `match` arms
//! at dispatch sites.
//!
//! Adding a new node type is one entry in [`NODES`] plus the variant-specific
//! module (e.g. `nodes/phase_update.rs`) plus the `WorkflowNodeData::*` arm
//! in [`lookup_by_variant`] that maps the new variant to its wire tag. The
//! registry-coverage tests at the bottom of this file make a forgotten
//! entry a build-time failure.
//!
//! [`WorkflowNodeData`] stays the wire / serde-rename tag (OpenAPI
//! discriminator, Y.Doc-stored string); the registry owns dispatch.

use serde::Serialize;
use utoipa::ToSchema;

use crate::compiler::graph::WorkflowDiGraph;
use crate::compiler::lower::LoweringCtx;
use crate::compiler::token_shape::TokenShape;
use crate::compiler::{CompileError, NodeKind};
use crate::models::template::{Port, WorkflowGraph, WorkflowNode, WorkflowNodeData};

pub mod agent;
pub mod automated_step;
pub mod decision;
pub mod delay;
pub mod end;
pub mod failure;
pub mod human_task;
pub mod join;
pub mod loop_;
pub mod map;
pub mod parallel_split;
pub mod phase_update;
pub mod progress_update;
pub mod scope;
pub mod start;
pub mod sub_workflow;
pub mod timeout;
pub mod trigger;

/// Per-variant declaration. Stored in a `&'static` slice so the registry has
/// zero runtime cost and trivially serializes the metadata subset for
/// `GET /api/v1/node-types`.
///
/// `pub(crate)` because the `lower` fn pointer references the crate-internal
/// [`LoweringCtx`]. The public-facing wire shape is [`NodeDescriptor`] —
/// emitted by [`descriptors`] and served by `GET /api/v1/node-types`.
pub(crate) struct NodeDecl {
    // ── Identity ──────────────────────────────────────────────────────
    /// Snake-case wire tag — equals the variant's `#[serde(rename)]`,
    /// equals `WorkflowNodeData::type_name()`. Lookup key.
    pub wire_name: &'static str,
    /// Palette display label (frontend reads via `GET /api/v1/node-types`).
    pub display_label: &'static str,
    /// Optional static description (palette tooltip + descriptor body).
    pub description: Option<&'static str>,
    /// Runtime kind for projections / borrow planner / hoist_path.
    /// One-to-one with [`WorkflowNodeData`]. For Agent the loop path
    /// publishes `NodeKind::Agent`; the degenerate path delegates via a
    /// virtual `WorkflowNodeData::AutomatedStep` node, so its published
    /// interface kind is read from the *virtual* variant and stays
    /// `AutomatedStep` (keeps the byte-identical contract pinned by
    /// `agent_degenerate_lowers_byte_identical_to_llm_automated_step`).
    pub kind: NodeKind,

    // ── Protocol flags ────────────────────────────────────────────────
    /// `false` only for [`WorkflowNodeData::Trigger`]. Drives the
    /// "must publish_interface" assertion in the lowering dispatcher.
    pub lowers_to_air: bool,
    /// `true` only for [`WorkflowNodeData::Join`]. Join edges bypass the
    /// place-merge optimization in `compiler/wire.rs` (each inbound edge
    /// gets its own named input place).
    pub is_join: bool,
    /// `true` for variants whose lowering parks a borrow-reachable
    /// write-once envelope (Start, HumanTask, AutomatedStep, Agent loop).
    /// The borrow planner read-arc-synthesizes through this place.
    pub parks_data_envelope: bool,

    // ── Variant-specific dispatch (fn pointers, mirror BackendDecl) ───
    /// Per-variant lowering. `None` iff `lowers_to_air == false` (Trigger).
    pub lower: Option<LowerFn>,
    /// Compute typed input ports from this variant's data. Some variants
    /// derive ports from config (Loop has the fixed `body_out` handle, End
    /// reads `terminal`).
    pub input_ports: fn(&WorkflowNodeData) -> Vec<Port>,
    /// Compute typed output ports from this variant's data. Some variants
    /// derive (HumanTask unions step inputs; Decision yields one per branch).
    pub output_ports: fn(&WorkflowNodeData) -> Vec<Port>,
    /// Wiring-time Rhai injected on the inbound edge transition. `Some` only
    /// for HumanTask (which injects step-input bindings). Every other variant
    /// uses the pure pass-through merge path in `compiler/wire.rs`. Takes
    /// `&WorkflowNode` because the injection script names step ids derived
    /// from the node identity.
    pub wiring_logic: Option<fn(&WorkflowNode) -> String>,
    /// Encode this variant's config fields into a Y.Map. Replaces the
    /// per-variant arm in `yjs/doc_ops.rs::write_node_config`. The decoder
    /// path uses unified serde over a flat-merged JSON object, so no
    /// `yjs_decode` is needed.
    pub yjs_encode: YjsEncodeFn,

    // ── Structural validation + token-shape (registry-driven coverage) ───
    /// Per-variant structural validation run by `compiler/validate.rs::validate`.
    /// `Some` only for variants carrying a per-node structural rule (Loop's
    /// `max_iterations`/condition, Delay/Timeout duration + body, Decision's
    /// `defaultBranch`, ParallelSplit fan-out, the AutomatedStep/HumanTask
    /// unmerged-fan-in warning). Pure pass-through / control-flow variants keep
    /// `None`. Takes the full graph + digraph because some checks count incident
    /// edges (`ParallelSplit`, `Timeout` body). Pushing this into the registry
    /// means a future variant with a structural rule can't be silently skipped
    /// — the dispatcher walks every node through `decl.validate`.
    pub validate: Option<ValidateFn>,
    /// Per-variant outbound token-shape derivation — the single arm of
    /// `compiler/token_shape/analyze.rs::out_shape`. Returns the shape this
    /// variant emits downstream given the inbound shape. EVERY variant declares
    /// this (the previous `out_shape` match was exhaustive); a missing hook on a
    /// new variant now fails the `token_shape_hook_declared_for_every_variant`
    /// conformance test instead of silently defaulting to a pass-through.
    pub token_shape: Option<TokenShapeFn>,
}

/// Per-variant lowering function pointer. Same signature as the existing
/// `NodeLowering::lower` impl bodies in `compiler/lower/<variant>.rs`.
pub(crate) type LowerFn = fn(&mut LoweringCtx) -> Result<(), CompileError>;

/// Per-variant structural validation function pointer. Same shape as one arm of
/// the per-node loops in `compiler/validate.rs::validate`. Receives the node,
/// the owning graph (for edge inspection — `Timeout` body handles) and the
/// pre-built [`WorkflowDiGraph`] (for incident-edge counts — `ParallelSplit`).
pub(crate) type ValidateFn =
    fn(&WorkflowNode, &WorkflowGraph, &WorkflowDiGraph<'_>) -> Result<(), CompileError>;

/// Per-variant outbound token-shape function pointer. Identical signature to the
/// pre-refactor `out_shape(node, in_shape)` free fn in
/// `compiler/token_shape/analyze.rs`.
pub(crate) type TokenShapeFn = fn(&WorkflowNode, &TokenShape) -> TokenShape;

/// YJS-side config encoder. Writes the variant's per-config fields into
/// the supplied `Y.Map` under the surrounding transaction. Mirrors one
/// arm of `yjs/doc_ops.rs::write_node_config` exactly.
pub(crate) type YjsEncodeFn = fn(
    txn: &mut yrs::TransactionMut<'_>,
    config: &yrs::MapRef,
    data: &WorkflowNodeData,
);

// ─── Registry ───────────────────────────────────────────────────────────────

/// Static slice of every registered node type. Covers all
/// `WorkflowNodeData` variants; the registry-coverage tests below
/// (one `lookup_by_variant_finds_<variant>` per variant +
/// `lookup_by_wire_finds_registered` + `descriptors_emit_camel_case_fields`)
/// make a forgotten entry a build-time failure.
pub(crate) static NODES: &[&NodeDecl] = &[
    &agent::AGENT_DECL,
    &automated_step::AUTOMATED_STEP_DECL,
    &decision::DECISION_DECL,
    &delay::DELAY_DECL,
    &end::END_DECL,
    &failure::FAILURE_DECL,
    &human_task::HUMAN_TASK_DECL,
    &join::JOIN_DECL,
    &loop_::LOOP_DECL,
    &map::MAP_DECL,
    &parallel_split::PARALLEL_SPLIT_DECL,
    &phase_update::PHASE_UPDATE_DECL,
    &progress_update::PROGRESS_UPDATE_DECL,
    &scope::SCOPE_DECL,
    &start::START_DECL,
    &sub_workflow::SUB_WORKFLOW_DECL,
    &timeout::TIMEOUT_DECL,
    &trigger::TRIGGER_DECL,
];

/// Look up the decl for a variant's data. Returns `Some` for every variant —
/// the conformance test `descriptors_emit_camel_case_fields` pins that
/// invariant. Returns `Option` rather than panicking so callers can decide
/// whether a missing entry is a bug or a downstream-recoverable miss.
///
/// Matches `data` directly (not via `data.type_name()`) because `type_name`
/// itself routes through this lookup — calling it here would recurse.
pub(crate) fn lookup_by_variant(data: &WorkflowNodeData) -> Option<&'static NodeDecl> {
    let tag: &'static str = match data {
        WorkflowNodeData::Start { .. } => "start",
        WorkflowNodeData::End { .. } => "end",
        WorkflowNodeData::HumanTask { .. } => "human_task",
        WorkflowNodeData::AutomatedStep { .. } => "automated_step",
        WorkflowNodeData::Agent { .. } => "agent",
        WorkflowNodeData::Decision { .. } => "decision",
        WorkflowNodeData::ParallelSplit { .. } => "parallel_split",
        WorkflowNodeData::Join { .. } => "join",
        WorkflowNodeData::Loop { .. } => "loop",
        WorkflowNodeData::Scope { .. } => "scope",
        WorkflowNodeData::Map { .. } => "map",
        WorkflowNodeData::PhaseUpdate { .. } => "phase_update",
        WorkflowNodeData::ProgressUpdate { .. } => "progress_update",
        WorkflowNodeData::Failure { .. } => "failure",
        WorkflowNodeData::Delay { .. } => "delay",
        WorkflowNodeData::Timeout { .. } => "timeout",
        WorkflowNodeData::Trigger { .. } => "trigger",
        WorkflowNodeData::SubWorkflow { .. } => "sub_workflow",
    };
    NODES.iter().copied().find(|d| d.wire_name == tag)
}

/// The author-written Rhai expressions a variant carries that the compiler
/// must syntax-check and resolve `input.<path>` refs against. The **single**
/// source of truth for "which fields on this variant are Rhai-bearing" —
/// both `validate::validate_guards` (publish-time syntax check) and
/// `token_shape::analyze` (editor-time shape re-validation) dispatch through
/// here so they can't drift.
///
/// Empties (after trimming) are filtered so callers get only the live
/// expressions. The `match` is exhaustive: a new variant with a Rhai-bearing
/// field that forgets an arm is a build failure (no `_ =>` wildcard that
/// silently returns nothing — every variant is named).
pub(crate) fn guard_rhai_sources(data: &WorkflowNodeData) -> Vec<&str> {
    let raw: Vec<&str> = match data {
        WorkflowNodeData::Decision { conditions, .. } => {
            conditions.iter().map(|c| c.guard.as_str()).collect()
        }
        WorkflowNodeData::Loop { loop_condition, .. } => vec![loop_condition.as_str()],
        WorkflowNodeData::End { result_mapping, .. } => {
            result_mapping.iter().map(|m| m.expression.as_str()).collect()
        }
        WorkflowNodeData::Failure {
            error_result_mapping,
            ..
        } => error_result_mapping
            .iter()
            .map(|m| m.expression.as_str())
            .collect(),
        WorkflowNodeData::Delay {
            duration_ms_expr, ..
        }
        | WorkflowNodeData::Timeout {
            duration_ms_expr, ..
        } => vec![duration_ms_expr.as_str()],
        // Non-Rhai-bearing variants. Named exhaustively (no wildcard) so a
        // future variant that grows a guard/expression field can't slip
        // through with an empty list.
        WorkflowNodeData::Start { .. }
        | WorkflowNodeData::HumanTask { .. }
        | WorkflowNodeData::AutomatedStep { .. }
        | WorkflowNodeData::Agent { .. }
        | WorkflowNodeData::ParallelSplit { .. }
        | WorkflowNodeData::Join { .. }
        | WorkflowNodeData::Scope { .. }
        | WorkflowNodeData::Map { .. }
        | WorkflowNodeData::PhaseUpdate { .. }
        | WorkflowNodeData::ProgressUpdate { .. }
        | WorkflowNodeData::Trigger { .. }
        | WorkflowNodeData::SubWorkflow { .. } => vec![],
    };
    raw.into_iter().filter(|s| !s.trim().is_empty()).collect()
}

/// Look up by wire name (snake_case tag). Symmetric to `backends::lookup` —
/// reserved for future endpoints (e.g. a `POST /api/v1/node-types/{name}/derive-ports`
/// mirror of the backends `derive-output` pattern, if a node ever needs
/// server-side port derivation from its config).
#[allow(dead_code)]
pub(crate) fn lookup_by_wire(name: &str) -> Option<&'static NodeDecl> {
    NODES.iter().copied().find(|d| d.wire_name == name)
}

// ─── Wire descriptor (frontend metadata via `GET /api/v1/node-types`) ──────

/// Frontend-visible metadata for one node type. Returned by
/// `GET /api/v1/node-types`.
///
/// Mirrors [`crate::backends::BackendDescriptor`]'s role for backends. The
/// Svelte component map, Lucide icon imports, and per-section property
/// panels stay frontend-only (they reference Svelte components that can't
/// be serialized through JSON); everything else flows from here.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NodeDescriptor {
    /// Snake-case wire tag — matches the variant's serde rename.
    pub wire_name: String,
    pub display_label: String,
    pub description: Option<String>,
    /// Runtime kind as snake_case string. Differs from `wire_name` only for
    /// `agent` (kind = `automated_step`).
    pub kind: String,
    pub lowers_to_air: bool,
    pub is_join: bool,
    pub parks_data_envelope: bool,
}

impl NodeDecl {
    pub fn to_descriptor(&self) -> NodeDescriptor {
        NodeDescriptor {
            wire_name: self.wire_name.to_string(),
            display_label: self.display_label.to_string(),
            description: self.description.map(str::to_string),
            kind: self.kind.wire_str().to_string(),
            lowers_to_air: self.lowers_to_air,
            is_join: self.is_join,
            parks_data_envelope: self.parks_data_envelope,
        }
    }
}

/// Serialize every registered node type for `GET /api/v1/node-types`.
pub fn descriptors() -> Vec<NodeDescriptor> {
    NODES.iter().map(|d| d.to_descriptor()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::template::{
        default_automated_input_port, default_automated_output_port, default_initial_port,
        default_join_output_port, default_subworkflow_output_port, default_terminal_port,
        BranchCondition, ConcurrencyPolicy, ContextStrategy, ExecutionBackendType,
        ExecutionSpecConfig, JoinMode, ManualTrigger, ModelRef, PhaseUpdateStatus, RetryPolicy,
        ToolErrorPolicy, TriggerSource, VersionPin,
    };
    use uuid::Uuid;

    #[test]
    fn lookup_by_variant_finds_phase_update() {
        let data = WorkflowNodeData::PhaseUpdate {
            label: "p".to_string(),
            description: None,
            phase_name: "x".to_string(),
            status: PhaseUpdateStatus::Running,
            message: None,
        };
        let decl = lookup_by_variant(&data).expect("phase_update registered");
        assert_eq!(decl.wire_name, "phase_update");
        assert_eq!(decl.kind, NodeKind::PhaseUpdate);
        assert!(decl.lowers_to_air);
    }

    #[test]
    fn lookup_by_variant_finds_trigger() {
        let data = WorkflowNodeData::Trigger {
            label: "t".to_string(),
            description: None,
            source: TriggerSource::Manual(ManualTrigger { form: vec![] }),
            concurrency: ConcurrencyPolicy::default(),
            payload_mapping: vec![],
            reply_default: None,
            enabled: false,
        };
        let decl = lookup_by_variant(&data).expect("trigger registered");
        assert_eq!(decl.wire_name, "trigger");
        assert!(!decl.lowers_to_air);
        assert!(decl.lower.is_none());
    }

    #[test]
    fn lookup_by_variant_finds_start() {
        let data = WorkflowNodeData::Start {
            label: "s".to_string(),
            description: None,
            initial: default_initial_port(),
            process_name: None,
        };
        let decl = lookup_by_variant(&data).expect("start registered");
        assert_eq!(decl.wire_name, "start");
        assert_eq!(decl.kind, NodeKind::Start);
        assert!(decl.lowers_to_air);
        assert!(decl.parks_data_envelope);
        assert!(decl.lower.is_some());
    }

    #[test]
    fn lookup_by_variant_finds_end() {
        let data = WorkflowNodeData::End {
            label: "e".to_string(),
            description: None,
            terminal: default_terminal_port(),
            result_mapping: vec![],
        };
        let decl = lookup_by_variant(&data).expect("end registered");
        assert_eq!(decl.wire_name, "end");
        assert_eq!(decl.kind, NodeKind::End);
        assert!(decl.lowers_to_air);
        assert!(!decl.parks_data_envelope);
        assert!(decl.lower.is_some());
    }

    #[test]
    fn lookup_by_variant_finds_progress_update() {
        let data = WorkflowNodeData::ProgressUpdate {
            label: "p".to_string(),
            description: None,
            fraction: 0.5,
            message: None,
            current_step: None,
            total_steps: None,
        };
        let decl = lookup_by_variant(&data).expect("progress_update registered");
        assert_eq!(decl.wire_name, "progress_update");
        assert_eq!(decl.kind, NodeKind::ProgressUpdate);
        assert!(decl.lowers_to_air);
        assert!(decl.lower.is_some());
    }

    #[test]
    fn lookup_by_variant_finds_failure() {
        let data = WorkflowNodeData::Failure {
            label: "f".to_string(),
            description: None,
            failure_message: None,
            error_result_mapping: vec![],
        };
        let decl = lookup_by_variant(&data).expect("failure registered");
        assert_eq!(decl.wire_name, "failure");
        assert_eq!(decl.kind, NodeKind::Failure);
        assert!(decl.lowers_to_air);
        assert!(decl.lower.is_some());
    }

    #[test]
    fn lookup_by_variant_finds_agent() {
        let data = WorkflowNodeData::Agent {
            label: "a".to_string(),
            description: None,
            model: ModelRef {
                provider: "openai".to_string(),
                model: "gpt-4o".to_string(),
                api_key: None,
                base_url: None,
                resource_alias: None,
                temperature: None,
                max_tokens: None,
            },
            system_prompt: None,
            user_prompt: "hello".to_string(),
            response_format: None,
            max_turns: 1,
            stop_when: None,
            context_strategy: ContextStrategy::None,
            on_tool_error: ToolErrorPolicy::Feedback,
        };
        let decl = lookup_by_variant(&data).expect("agent registered");
        assert_eq!(decl.wire_name, "agent");
        // Loop-path kind. The degenerate path's published kind is read
        // through a virtual `AutomatedStep` node, so the byte-identical
        // contract (`agent_degenerate_lowers_byte_identical_to_llm_automated_step`)
        // is unaffected by this declaration.
        assert_eq!(decl.kind, NodeKind::Agent);
        assert!(decl.lowers_to_air);
        assert!(decl.parks_data_envelope);
        assert!(decl.lower.is_some());
        assert!(!decl.is_join);
    }

    #[test]
    fn lookup_by_variant_finds_parallel_split() {
        let data = WorkflowNodeData::ParallelSplit {
            label: "ps".to_string(),
            description: None,
        };
        let decl = lookup_by_variant(&data).expect("parallel_split registered");
        assert_eq!(decl.wire_name, "parallel_split");
        assert_eq!(decl.kind, NodeKind::ParallelSplit);
        assert!(decl.lowers_to_air);
        assert!(!decl.parks_data_envelope);
        assert!(!decl.is_join);
        assert!(decl.lower.is_some());
    }

    #[test]
    fn lookup_by_variant_finds_join() {
        let data = WorkflowNodeData::Join {
            label: "j".to_string(),
            description: None,
            mode: JoinMode::All,
            merge_strategy: None,
            output: default_join_output_port(),
        };
        let decl = lookup_by_variant(&data).expect("join registered");
        assert_eq!(decl.wire_name, "join");
        assert_eq!(decl.kind, NodeKind::Join);
        assert!(decl.lowers_to_air);
        assert!(decl.lower.is_some());
        // ── The single load-bearing assertion for the Join migration: the
        // only variant where `is_join` is true. `wire.rs::edge_target_place`
        // + the can-merge gate at `wire.rs:95` rely on this fact.
        assert!(decl.is_join);
    }

    #[test]
    fn join_is_the_only_variant_with_is_join_true() {
        // Pins the invariant that `is_join` is set on exactly one decl.
        // If a future migration accidentally sets it elsewhere (e.g. on
        // ParallelSplit because of its converging cousin Join), this test
        // catches it before it reaches `wire.rs`'s special-case branch.
        let count_is_join = NODES.iter().filter(|d| d.is_join).count();
        assert_eq!(count_is_join, 1, "exactly one variant should have is_join: true (Join)");
        let only = NODES.iter().find(|d| d.is_join).unwrap();
        assert_eq!(only.wire_name, "join");
    }

    #[test]
    fn lookup_by_variant_finds_loop() {
        let data = WorkflowNodeData::Loop {
            label: "l".to_string(),
            description: None,
            max_iterations: 10,
            loop_condition: "false".to_string(),
            accumulators: vec![],
            lease: None,
        };
        let decl = lookup_by_variant(&data).expect("loop registered");
        assert_eq!(decl.wire_name, "loop");
        assert_eq!(decl.kind, NodeKind::Loop);
        assert!(decl.lowers_to_air);
        // Loop's lowering parks data in p_<id>_data so downstream borrows
        // resolve via the standard read-arc pipeline.
        assert!(decl.parks_data_envelope);
        assert!(!decl.is_join);
        assert!(decl.lower.is_some());
    }

    #[test]
    fn lookup_by_variant_finds_map() {
        let data = WorkflowNodeData::Map {
            label: "m".to_string(),
            description: None,
            items_ref: "extract.tasks".to_string(),
            item_var: "item".to_string(),
            result_var: "result".to_string(),
            output: None,
        };
        let decl = lookup_by_variant(&data).expect("map registered");
        assert_eq!(decl.wire_name, "map");
        assert_eq!(decl.kind, NodeKind::Map);
        assert!(decl.lowers_to_air);
        // Map parks the gathered collection at p_<id>_data so downstream
        // `<slug>[*].<field>` borrows resolve via the standard read-arc pipeline.
        assert!(decl.parks_data_envelope);
        assert!(!decl.is_join);
        assert!(decl.lower.is_some());
        assert!(decl.validate.is_some());
        // Derived ports mirror Loop: outer in/out + body_in/body_out handles.
        let ins = (decl.input_ports)(&data);
        assert!(ins.iter().any(|p| p.id == "body_out"));
        let outs = (decl.output_ports)(&data);
        assert!(outs.iter().any(|p| p.id == "body_in"));
    }

    #[test]
    fn lookup_by_variant_finds_sub_workflow() {
        let data = WorkflowNodeData::SubWorkflow {
            label: "sw".to_string(),
            description: None,
            template_id: Uuid::nil(),
            version_pin: VersionPin::Latest,
            input_mapping: vec![],
            output: default_subworkflow_output_port(),
        };
        let decl = lookup_by_variant(&data).expect("sub_workflow registered");
        assert_eq!(decl.wire_name, "sub_workflow");
        assert_eq!(decl.kind, NodeKind::SubWorkflow);
        assert!(decl.lowers_to_air);
        assert!(!decl.is_join);
        assert!(decl.lower.is_some());
    }

    #[test]
    fn lookup_by_variant_finds_human_task() {
        let data = WorkflowNodeData::HumanTask {
            label: "h".to_string(),
            description: None,
            task_title: "task".to_string(),
            instructions_mdsvex: None,
            steps: vec![],
            steps_ref: None,
        };
        let decl = lookup_by_variant(&data).expect("human_task registered");
        assert_eq!(decl.wire_name, "human_task");
        assert_eq!(decl.kind, NodeKind::HumanTask);
        assert!(decl.lowers_to_air);
        assert!(decl.parks_data_envelope);
        assert!(decl.lower.is_some());
    }

    #[test]
    fn lookup_by_variant_finds_automated_step() {
        let data = WorkflowNodeData::AutomatedStep {
            label: "a".to_string(),
            description: None,
            execution_spec: ExecutionSpecConfig {
                backend_type: ExecutionBackendType::Python,
                entrypoint: Some("main.py".to_string()),
                config: serde_json::json!({}),
            },
            input: default_automated_input_port(),
            output: default_automated_output_port(),
            retry_policy: RetryPolicy::default(),
            deployment_model: Default::default(),
        };
        let decl = lookup_by_variant(&data).expect("automated_step registered");
        assert_eq!(decl.wire_name, "automated_step");
        assert_eq!(decl.kind, NodeKind::AutomatedStep);
        assert!(decl.lowers_to_air);
        assert!(decl.parks_data_envelope);
        assert!(decl.lower.is_some());
    }

    #[test]
    fn lookup_by_variant_finds_decision() {
        let data = WorkflowNodeData::Decision {
            label: "d".to_string(),
            description: None,
            conditions: vec![BranchCondition {
                edge_id: "branch_1".to_string(),
                label: "Yes".to_string(),
                guard: "true".to_string(),
            }],
            default_branch: Some("default".to_string()),
        };
        let decl = lookup_by_variant(&data).expect("decision registered");
        assert_eq!(decl.wire_name, "decision");
        assert_eq!(decl.kind, NodeKind::Decision);
        assert!(decl.lowers_to_air);
        // Decision is control flow only — no parked envelope.
        assert!(!decl.parks_data_envelope);
        // Derived output ports: one per condition + the default catch-all.
        let outs = (decl.output_ports)(&data);
        assert_eq!(outs.len(), 2);
        assert_eq!(outs[0].id, "branch_1");
        assert_eq!(outs[0].label, "Yes");
        assert_eq!(outs[1].id, "default");
    }

    #[test]
    fn lookup_by_variant_finds_scope() {
        let data = WorkflowNodeData::Scope {
            label: "s".to_string(),
            description: None,
        };
        let decl = lookup_by_variant(&data).expect("scope registered");
        assert_eq!(decl.wire_name, "scope");
        assert_eq!(decl.kind, NodeKind::Scope);
        assert!(decl.lowers_to_air);
        // Scope is a structural boundary, not a data producer.
        assert!(!decl.parks_data_envelope);
        assert!(decl.lower.is_some());
    }

    // ── Registry-coverage conformance (Workstream C) ────────────────────────
    // These pin that the per-node-kind logic moved into the registry stays
    // exhaustive: a future variant that forgets its `token_shape` /
    // structural-rule / guard-source wiring fails HERE (a test failure) instead
    // of silently defaulting at runtime (the original bug class — token shape
    // defaulting to a pass-through, a structural rule getting skipped).

    /// EVERY variant must declare a `token_shape` hook. The pre-refactor
    /// `out_shape` match was exhaustive (every variant had an arm, even if it
    /// was the shared pass-through); the registry must preserve that — a `None`
    /// here would make `analyze::out_shape` panic on that variant at runtime.
    #[test]
    fn token_shape_hook_declared_for_every_variant() {
        for decl in NODES {
            assert!(
                decl.token_shape.is_some(),
                "node `{}` has no token_shape hook — `analyze::out_shape` would \
                 panic on it; every variant must declare one (use \
                 `out_shape_passthrough` for pure routing/marker nodes)",
                decl.wire_name
            );
        }
    }

    /// Every kind whose data carries guard / Rhai-bearing fields must surface
    /// those expressions through `guard_rhai_sources` — the single source of
    /// truth shared by `validate_guards` (publish-time syntax+ref check) and
    /// `token_shape::analyze` (editor shape re-validation). Built from a
    /// representative instance of each Rhai-bearing variant so a future variant
    /// that grows a guard field but forgets its `guard_rhai_sources` arm trips
    /// here (and the exhaustive match in `guard_rhai_sources` itself fails the
    /// build first).
    #[test]
    fn guard_rhai_sources_surfaces_every_rhai_bearing_variant() {
        use crate::models::template::{BranchCondition, FieldMapping};

        // Decision — conditions[].guard
        let decision = WorkflowNodeData::Decision {
            label: "d".to_string(),
            description: None,
            conditions: vec![BranchCondition {
                edge_id: "b1".to_string(),
                label: "Yes".to_string(),
                guard: "x > 1".to_string(),
            }],
            default_branch: None,
        };
        assert_eq!(guard_rhai_sources(&decision), vec!["x > 1"]);

        // Loop — loop_condition
        let loop_ = WorkflowNodeData::Loop {
            label: "l".to_string(),
            description: None,
            max_iterations: 3,
            loop_condition: "i < 3".to_string(),
            accumulators: vec![],
            lease: None,
        };
        assert_eq!(guard_rhai_sources(&loop_), vec!["i < 3"]);

        // End — result_mapping[].expression
        let end = WorkflowNodeData::End {
            label: "e".to_string(),
            description: None,
            terminal: crate::models::template::default_terminal_port(),
            result_mapping: vec![FieldMapping {
                target_field: "ok".to_string(),
                expression: "input.amount".to_string(),
            }],
        };
        assert_eq!(guard_rhai_sources(&end), vec!["input.amount"]);

        // Failure — error_result_mapping[].expression
        let failure = WorkflowNodeData::Failure {
            label: "f".to_string(),
            description: None,
            failure_message: None,
            error_result_mapping: vec![FieldMapping {
                target_field: "err".to_string(),
                expression: "input.reason".to_string(),
            }],
        };
        assert_eq!(guard_rhai_sources(&failure), vec!["input.reason"]);

        // Delay — duration_ms_expr
        let delay = WorkflowNodeData::Delay {
            label: "dl".to_string(),
            description: None,
            duration_ms_expr: "1000".to_string(),
        };
        assert_eq!(guard_rhai_sources(&delay), vec!["1000"]);

        // Timeout — duration_ms_expr
        let timeout = WorkflowNodeData::Timeout {
            label: "to".to_string(),
            description: None,
            duration_ms_expr: "5000".to_string(),
        };
        assert_eq!(guard_rhai_sources(&timeout), vec!["5000"]);

        // Empties are filtered (blank guard surfaces nothing).
        let blank = WorkflowNodeData::Loop {
            label: "l".to_string(),
            description: None,
            max_iterations: 1,
            loop_condition: "   ".to_string(),
            accumulators: vec![],
            lease: None,
        };
        assert!(guard_rhai_sources(&blank).is_empty());
    }

    /// Every kind that carries a guard/Rhai-bearing field or a per-node
    /// structural rule must declare *at least one* registry coverage hook:
    /// a `validate` hook (structural rule) OR a non-empty `guard_rhai_sources`
    /// (Rhai surface the guard passes check). This is the load-bearing
    /// "forgotten kind fails the build" assertion — it ties the per-variant
    /// data shape to the registry so a new rule-bearing variant can't ship
    /// without wiring.
    #[test]
    fn rule_bearing_kinds_declare_a_coverage_hook() {
        use crate::models::template::{
            default_automated_input_port, default_automated_output_port, default_terminal_port,
            BranchCondition, ExecutionBackendType, ExecutionSpecConfig, FieldMapping, RetryPolicy,
        };

        // One representative of each variant carrying a guard/Rhai field or a
        // per-node structural rule. (Variants with neither — Start, Scope,
        // Join, PhaseUpdate, ProgressUpdate, SubWorkflow, Trigger — are
        // legitimately `validate: None` + empty sources and are excluded.)
        let rule_bearing: Vec<WorkflowNodeData> = vec![
            // Structural rule (validate hook):
            WorkflowNodeData::Loop {
                label: "l".to_string(),
                description: None,
                max_iterations: 1,
                loop_condition: "true".to_string(),
                accumulators: vec![],
                lease: None,
            },
            WorkflowNodeData::Delay {
                label: "d".to_string(),
                description: None,
                duration_ms_expr: "1".to_string(),
            },
            WorkflowNodeData::Timeout {
                label: "t".to_string(),
                description: None,
                duration_ms_expr: "1".to_string(),
            },
            WorkflowNodeData::Decision {
                label: "dc".to_string(),
                description: None,
                conditions: vec![BranchCondition {
                    edge_id: "b".to_string(),
                    label: "Y".to_string(),
                    guard: "true".to_string(),
                }],
                default_branch: None,
            },
            WorkflowNodeData::ParallelSplit {
                label: "ps".to_string(),
                description: None,
            },
            WorkflowNodeData::AutomatedStep {
                label: "a".to_string(),
                description: None,
                execution_spec: ExecutionSpecConfig {
                    backend_type: ExecutionBackendType::Python,
                    entrypoint: Some("m.py".to_string()),
                    config: serde_json::json!({}),
                },
                input: default_automated_input_port(),
                output: default_automated_output_port(),
                retry_policy: RetryPolicy::default(),
                deployment_model: Default::default(),
            },
            // Rhai-bearing only (no structural validate hook — covered by
            // guard_rhai_sources / validate_guards):
            WorkflowNodeData::End {
                label: "e".to_string(),
                description: None,
                terminal: default_terminal_port(),
                result_mapping: vec![FieldMapping {
                    target_field: "ok".to_string(),
                    expression: "input.x".to_string(),
                }],
            },
            WorkflowNodeData::Failure {
                label: "f".to_string(),
                description: None,
                failure_message: None,
                error_result_mapping: vec![FieldMapping {
                    target_field: "e".to_string(),
                    expression: "input.r".to_string(),
                }],
            },
        ];

        for data in &rule_bearing {
            let decl = lookup_by_variant(data).expect("registered");
            let has_validate = decl.validate.is_some();
            let has_rhai = !guard_rhai_sources(data).is_empty();
            assert!(
                has_validate || has_rhai,
                "node `{}` carries a guard/Rhai field or structural rule but \
                 declares neither a `validate` hook nor a `guard_rhai_sources` \
                 arm — a forgotten coverage hook",
                decl.wire_name
            );
        }
    }

    #[test]
    fn lookup_by_wire_finds_registered() {
        // Every wire tag in `NODES` must be reachable via wire-name lookup.
        // Keep this list in sync with `WorkflowNodeData` variants.
        for wire in [
            "agent",
            "automated_step",
            "decision",
            "delay",
            "end",
            "failure",
            "human_task",
            "join",
            "loop",
            "map",
            "parallel_split",
            "phase_update",
            "progress_update",
            "scope",
            "start",
            "sub_workflow",
            "timeout",
            "trigger",
        ] {
            assert!(lookup_by_wire(wire).is_some(), "missing decl for {wire}");
        }
        assert!(lookup_by_wire("nonexistent").is_none());
    }

    #[test]
    fn descriptors_emit_camel_case_fields() {
        let all = descriptors();
        // One descriptor per registered variant.
        assert_eq!(all.len(), NODES.len());
        let pu = all.iter().find(|d| d.wire_name == "phase_update").unwrap();
        assert_eq!(pu.kind, "phase_update");
        assert!(pu.lowers_to_air);
        let tr = all.iter().find(|d| d.wire_name == "trigger").unwrap();
        assert!(!tr.lowers_to_air);
        let st = all.iter().find(|d| d.wire_name == "start").unwrap();
        assert_eq!(st.kind, "start");
        assert!(st.parks_data_envelope);
        let en = all.iter().find(|d| d.wire_name == "end").unwrap();
        assert_eq!(en.kind, "end");
        assert!(en.lowers_to_air);
        let pgu = all.iter().find(|d| d.wire_name == "progress_update").unwrap();
        assert_eq!(pgu.kind, "progress_update");
        let fl = all.iter().find(|d| d.wire_name == "failure").unwrap();
        assert_eq!(fl.kind, "failure");
        // Agent's serialized kind is "agent" (loop-path kind declaration;
        // see `agent::AGENT_DECL` doc comment). Same hoist_path as
        // AutomatedStep — the runtime envelope shape is shared.
        let ag = all.iter().find(|d| d.wire_name == "agent").unwrap();
        assert_eq!(ag.kind, "agent");
        let ps = all.iter().find(|d| d.wire_name == "parallel_split").unwrap();
        assert_eq!(ps.kind, "parallel_split");
        let jn = all.iter().find(|d| d.wire_name == "join").unwrap();
        assert_eq!(jn.kind, "join");
        assert!(jn.is_join);
        let lp = all.iter().find(|d| d.wire_name == "loop").unwrap();
        assert_eq!(lp.kind, "loop");
        let sw = all.iter().find(|d| d.wire_name == "sub_workflow").unwrap();
        assert_eq!(sw.kind, "sub_workflow");
        let ht = all.iter().find(|d| d.wire_name == "human_task").unwrap();
        assert_eq!(ht.kind, "human_task");
        assert!(ht.parks_data_envelope);
        let asd = all.iter().find(|d| d.wire_name == "automated_step").unwrap();
        assert_eq!(asd.kind, "automated_step");
        assert!(asd.parks_data_envelope);
        let dn = all.iter().find(|d| d.wire_name == "decision").unwrap();
        assert_eq!(dn.kind, "decision");
        assert!(!dn.parks_data_envelope);
        let sc = all.iter().find(|d| d.wire_name == "scope").unwrap();
        assert_eq!(sc.kind, "scope");
    }
}
