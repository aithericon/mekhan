//! Declarative node-type registry — one `NodeDecl` per `WorkflowNodeData`
//! variant, stored in a static `&[NodeDecl]` slice.
//!
//! Mirrors [`crate::backends`] for `ExecutionBackendType`. Replaces the
//! per-variant `match` arms scattered through `models/template.rs`,
//! `compiler/lower/mod.rs`, `compiler/wire.rs`, `compiler/token_shape/analyze.rs`,
//! `projections/step_executions/consumer.rs`, and `yjs/doc_ops.rs`.
//!
//! Adding a new node type is one entry in [`NODES`] plus the variant-specific
//! module (e.g. `nodes/phase_update.rs`). Dispatch sites do
//! [`lookup_by_variant`] and call into the decl's fn pointers.
//!
//! [`WorkflowNodeData`] stays the wire / serde-rename tag (OpenAPI
//! discriminator, Y.Doc-stored string); the registry replaces the variant's
//! role as a dispatch source-of-truth.
//!
//! ## PR1 status
//!
//! PR1 lands the registry skeleton + endpoint + two migrated variants
//! ([`phase_update`], [`trigger`]) as proof. The dispatcher in
//! `compiler/lower/mod.rs::NodeLowering` and `yjs/doc_ops.rs::write_node_config`
//! consult the registry first and fall back to legacy `match` arms for
//! un-migrated variants. PR2 migrates the remaining 13.

use serde::Serialize;
use utoipa::ToSchema;

use crate::compiler::lower::LoweringCtx;
use crate::compiler::{CompileError, NodeKind};
use crate::models::template::{Port, WorkflowNode, WorkflowNodeData};

pub mod agent;
pub mod automated_step;
pub mod decision;
pub mod end;
pub mod failure;
pub mod human_task;
pub mod join;
pub mod loop_;
pub mod parallel_split;
pub mod phase_update;
pub mod progress_update;
pub mod scope;
pub mod start;
pub mod sub_workflow;
pub mod trigger;

/// Per-variant declaration. Stored in a `&'static` slice so the registry has
/// zero runtime cost and trivially serializes the metadata subset for
/// `GET /api/v1/node-types`.
///
/// Only the fields the duplication sites actually need. Future fields land
/// when a duplication site is collapsed in PR2; see `humble-inventing-rose.md`.
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
    /// One-to-one with [`WorkflowNodeData`] for every variant **except
    /// Agent**, which maps to `NodeKind::AutomatedStep` (declared, not
    /// derived — preserves the byte-identical contract with
    /// `AutomatedStep(Llm)` pinned by `agent_degenerate_lowers_byte_identical_to_llm_automated_step`).
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
    /// Compute the per-node typed input ports. Argument is `&WorkflowNode`
    /// because some variants derive ports from config (Loop has the fixed
    /// `body_out` handle, End reads `terminal`).
    pub input_ports: fn(&WorkflowNode) -> Vec<Port>,
    /// Per-node typed output ports. Some variants derive (HumanTask unions
    /// step inputs; Decision yields one per branch).
    pub output_ports: fn(&WorkflowNode) -> Vec<Port>,
    /// Encode this variant's config fields into a Y.Map. Replaces the
    /// per-variant arm in `yjs/doc_ops.rs::write_node_config`. The decoder
    /// path uses unified serde over a flat-merged JSON object, so no
    /// `yjs_decode` is needed.
    pub yjs_encode: YjsEncodeFn,
}

/// Per-variant lowering function pointer. Same signature as the existing
/// `NodeLowering::lower` impl bodies in `compiler/lower/<variant>.rs`.
pub(crate) type LowerFn = fn(&mut LoweringCtx) -> Result<(), CompileError>;

/// YJS-side config encoder. Writes the variant's per-config fields into
/// the supplied `Y.Map` under the surrounding transaction. Mirrors one
/// arm of `yjs/doc_ops.rs::write_node_config` exactly.
pub(crate) type YjsEncodeFn = fn(
    txn: &mut yrs::TransactionMut<'_>,
    config: &yrs::MapRef,
    data: &WorkflowNodeData,
);

// ─── Registry ───────────────────────────────────────────────────────────────

/// Static slice of every registered node type. PR1 covers `PhaseUpdate` +
/// `Trigger`; PR2 fills in the remaining 13 variants. The conformance test
/// (`lookup_by_variant_finds_registered`) is the gate.
pub(crate) static NODES: &[&NodeDecl] = &[
    &agent::AGENT_DECL,
    &automated_step::AUTOMATED_STEP_DECL,
    &decision::DECISION_DECL,
    &end::END_DECL,
    &failure::FAILURE_DECL,
    &human_task::HUMAN_TASK_DECL,
    &join::JOIN_DECL,
    &loop_::LOOP_DECL,
    &parallel_split::PARALLEL_SPLIT_DECL,
    &phase_update::PHASE_UPDATE_DECL,
    &progress_update::PROGRESS_UPDATE_DECL,
    &scope::SCOPE_DECL,
    &start::START_DECL,
    &sub_workflow::SUB_WORKFLOW_DECL,
    &trigger::TRIGGER_DECL,
];

/// Look up the decl for a variant's data. Returns `None` if the variant is
/// not yet registered (PR1 returns `None` for 13 of 15 variants — the
/// dispatcher falls back to its legacy match for those).
pub(crate) fn lookup_by_variant(data: &WorkflowNodeData) -> Option<&'static NodeDecl> {
    let tag = data.type_name();
    NODES.iter().copied().find(|d| d.wire_name == tag)
}

/// Look up by wire name (snake_case tag). Symmetric to `backends::lookup` —
/// reserved for future endpoints (PR2 may add `POST /api/v1/node-types/{name}/derive-ports`
/// mirroring the backends `derive-output` pattern).
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
            kind: node_kind_wire_str(self.kind).to_string(),
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

/// Snake-case wire string for a [`NodeKind`]. Duplicates the table in
/// `projections/step_executions/consumer.rs::node_kind_to_str` — PR2
/// centralizes it on `impl NodeKind` and rewires both callers.
fn node_kind_wire_str(k: NodeKind) -> &'static str {
    match k {
        NodeKind::Start => "start",
        NodeKind::End => "end",
        NodeKind::HumanTask => "human_task",
        NodeKind::AutomatedStep => "automated_step",
        NodeKind::Decision => "decision",
        NodeKind::Loop => "loop",
        NodeKind::ParallelSplit => "parallel_split",
        NodeKind::Join => "join",
        NodeKind::Scope => "scope",
        NodeKind::SubWorkflow => "sub_workflow",
        NodeKind::PhaseUpdate => "phase_update",
        NodeKind::ProgressUpdate => "progress_update",
        NodeKind::Failure => "failure",
        NodeKind::Trigger => "trigger",
    }
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
        // Declared Agent→AutomatedStep mapping (preserves the byte-identical
        // contract pinned by `agent_degenerate_lowers_byte_identical_to_llm_automated_step`;
        // replaces the `node_kind_of` hack at lower/mod.rs:412).
        assert_eq!(decl.kind, NodeKind::AutomatedStep);
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
        let node = WorkflowNode {
            id: "n".to_string(),
            node_type: "decision".to_string(),
            slug: None,
            position: crate::models::template::Position { x: 0.0, y: 0.0 },
            data,
            parent_id: None,
            width: None,
            height: None,
            tool_meta: None,
        };
        let outs = (decl.output_ports)(&node);
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

    #[test]
    fn lookup_by_wire_finds_registered() {
        // All 15 variants registered after PR2.
        assert!(lookup_by_wire("phase_update").is_some());
        assert!(lookup_by_wire("trigger").is_some());
        assert!(lookup_by_wire("start").is_some());
        assert!(lookup_by_wire("end").is_some());
        assert!(lookup_by_wire("progress_update").is_some());
        assert!(lookup_by_wire("failure").is_some());
        assert!(lookup_by_wire("agent").is_some());
        assert!(lookup_by_wire("parallel_split").is_some());
        assert!(lookup_by_wire("join").is_some());
        assert!(lookup_by_wire("loop").is_some());
        assert!(lookup_by_wire("sub_workflow").is_some());
        assert!(lookup_by_wire("human_task").is_some());
        assert!(lookup_by_wire("automated_step").is_some());
        assert!(lookup_by_wire("decision").is_some());
        assert!(lookup_by_wire("scope").is_some());
        assert!(lookup_by_wire("nonexistent").is_none());
    }

    #[test]
    fn descriptors_emit_camel_case_fields() {
        let all = descriptors();
        // All 15 WorkflowNodeData variants registered after PR2.
        assert_eq!(all.len(), 15);
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
        // Agent's serialized kind is "automated_step" (declared mapping;
        // see `agent::AGENT_DECL` doc comment).
        let ag = all.iter().find(|d| d.wire_name == "agent").unwrap();
        assert_eq!(ag.kind, "automated_step");
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
