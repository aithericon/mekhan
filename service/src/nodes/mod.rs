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

pub mod phase_update;
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
    &phase_update::PHASE_UPDATE_DECL,
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
        ConcurrencyPolicy, ManualTrigger, PhaseUpdateStatus, TriggerSource,
    };

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
    fn lookup_by_wire_finds_registered() {
        assert!(lookup_by_wire("phase_update").is_some());
        assert!(lookup_by_wire("trigger").is_some());
        // PR1 has not registered automated_step yet.
        assert!(lookup_by_wire("automated_step").is_none());
        assert!(lookup_by_wire("nonexistent").is_none());
    }

    #[test]
    fn descriptors_emit_camel_case_fields() {
        let all = descriptors();
        assert_eq!(all.len(), 2);
        let pu = all.iter().find(|d| d.wire_name == "phase_update").unwrap();
        assert_eq!(pu.kind, "phase_update");
        assert!(pu.lowers_to_air);
        let tr = all.iter().find(|d| d.wire_name == "trigger").unwrap();
        assert!(!tr.lowers_to_air);
    }
}
