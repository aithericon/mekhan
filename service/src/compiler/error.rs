//! Compile-time error type and its editor-facing view.

#[derive(Debug, thiserror::Error)]
pub enum CompileError {
    #[error("validation error: {0}")]
    Validation(String),
    #[error("compilation error: {0}")]
    Compilation(String),

    // --- Typed-ports edge errors (Phase 2). Carry the offending edge_id (and
    //     sometimes a node_id / handle) so the editor can highlight inline.
    #[error("edge '{edge_id}' is missing a target_handle (required at publish time)")]
    MissingTargetHandle { edge_id: String },

    #[error(
        "edge '{edge_id}': source handle '{handle}' is not a declared output port on node '{node_id}'"
    )]
    UnknownSourcePort {
        edge_id: String,
        node_id: String,
        handle: String,
    },

    #[error(
        "edge '{edge_id}': target handle '{handle}' is not a declared input port on node '{node_id}'"
    )]
    UnknownTargetPort {
        edge_id: String,
        node_id: String,
        handle: String,
    },

    #[error(
        "edge '{edge_id}': source port fields {expected:?} don't match target port fields {found:?}"
    )]
    EdgeTypeMismatch {
        edge_id: String,
        expected: Vec<String>,
        found: Vec<String>,
    },

    // --- Typed-ports guard errors (Phase 3). Decision/Loop guards are Rhai
    //     expressions; we syntax-check them and resolve each
    //     `<upstream_node>.<field>` reference against the topological scope at
    //     that node. The editor consumes these via `to_view()` and highlights
    //     the offending node.
    #[error("guard on node '{node_id}' has a Rhai syntax error: {message}")]
    GuardSyntax { node_id: String, message: String },

    #[error(
        "guard on node '{node_id}' references unknown identifier '{identifier}' (in-scope upstream identifiers: {available:?})"
    )]
    GuardUnresolved {
        node_id: String,
        identifier: String,
        available: Vec<String>,
    },

    // --- Trigger node errors (Phase 5a). Triggers connect to a target input
    //     port via one outgoing edge and supply a payload_mapping. The
    //     compiler enforces:
    //       - Trigger has exactly one outgoing edge.
    //       - Trigger is never an edge target.
    //       - payload_mapping.target_field exists on the resolved target port.
    //       - payload_mapping.expression parses as Rhai.
    #[error("trigger '{node_id}' must have exactly one outgoing edge (found {found})")]
    TriggerEdgeCardinality { node_id: String, found: usize },

    #[error("trigger '{node_id}' cannot be the target of edge '{edge_id}'")]
    TriggerIsEdgeTarget { node_id: String, edge_id: String },

    #[error(
        "trigger '{node_id}': payload mapping references unknown target field '{field}' (available: {available:?})"
    )]
    TriggerUnknownTargetField {
        node_id: String,
        field: String,
        available: Vec<String>,
    },

    #[error(
        "trigger '{node_id}': payload mapping for field '{field}' has a Rhai syntax error: {message}"
    )]
    TriggerMappingSyntax {
        node_id: String,
        field: String,
        message: String,
    },

    /// Phase 5b: invalid cron schedule (bad cron string or unknown IANA tz).
    #[error("trigger '{node_id}': invalid cron schedule: {message}")]
    TriggerCronInvalid { node_id: String, message: String },

    /// A payload-mapping expression references a `<root>.<field>` whose root
    /// isn't a declared scope identifier for the trigger's source kind (e.g.
    /// referencing `catalogue_entry` from a cron trigger). Mirrors
    /// `GuardUnresolved`; identifier-resolution only (no kind inference).
    #[error(
        "trigger '{node_id}': payload mapping for field '{field}' references unknown identifier '{identifier}' (in-scope for this source: {available:?})"
    )]
    TriggerUnresolvedRef {
        node_id: String,
        field: String,
        identifier: String,
        available: Vec<String>,
    },

    /// The trigger has an empty `payload_mapping` but its resolved target port
    /// declares required field(s). An empty mapping forwards the source payload
    /// verbatim, which can't satisfy a typed port — fail at publish, not at
    /// first fire.
    #[error(
        "trigger '{node_id}': empty payload mapping but the target port requires field(s): {missing:?}"
    )]
    TriggerEmptyMappingRequiredFields {
        node_id: String,
        missing: Vec<String>,
    },
}

impl CompileError {
    /// Stable discriminant for the editor's error map. Keeps the wire format
    /// independent of Rust enum variant names.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Validation(_) => "validation",
            Self::Compilation(_) => "compilation",
            Self::MissingTargetHandle { .. } => "missing_target_handle",
            Self::UnknownSourcePort { .. } => "unknown_source_port",
            Self::UnknownTargetPort { .. } => "unknown_target_port",
            Self::EdgeTypeMismatch { .. } => "edge_type_mismatch",
            Self::GuardSyntax { .. } => "guard_syntax",
            Self::GuardUnresolved { .. } => "guard_unresolved",
            Self::TriggerEdgeCardinality { .. } => "trigger_edge_cardinality",
            Self::TriggerIsEdgeTarget { .. } => "trigger_is_edge_target",
            Self::TriggerUnknownTargetField { .. } => "trigger_unknown_target_field",
            Self::TriggerMappingSyntax { .. } => "trigger_mapping_syntax",
            Self::TriggerCronInvalid { .. } => "trigger_cron_invalid",
            Self::TriggerUnresolvedRef { .. } => "trigger_unresolved_ref",
            Self::TriggerEmptyMappingRequiredFields { .. } => {
                "trigger_empty_mapping_required_fields"
            }
        }
    }

    pub fn edge_id(&self) -> Option<&str> {
        match self {
            Self::MissingTargetHandle { edge_id }
            | Self::UnknownSourcePort { edge_id, .. }
            | Self::UnknownTargetPort { edge_id, .. }
            | Self::EdgeTypeMismatch { edge_id, .. } => Some(edge_id),
            Self::TriggerIsEdgeTarget { edge_id, .. } => Some(edge_id),
            _ => None,
        }
    }

    pub fn node_id(&self) -> Option<&str> {
        match self {
            Self::UnknownSourcePort { node_id, .. } | Self::UnknownTargetPort { node_id, .. } => {
                Some(node_id)
            }
            Self::GuardSyntax { node_id, .. } | Self::GuardUnresolved { node_id, .. } => {
                Some(node_id)
            }
            Self::TriggerEdgeCardinality { node_id, .. }
            | Self::TriggerIsEdgeTarget { node_id, .. }
            | Self::TriggerUnknownTargetField { node_id, .. }
            | Self::TriggerMappingSyntax { node_id, .. }
            | Self::TriggerCronInvalid { node_id, .. }
            | Self::TriggerUnresolvedRef { node_id, .. }
            | Self::TriggerEmptyMappingRequiredFields { node_id, .. } => Some(node_id),
            _ => None,
        }
    }

    pub fn to_view(&self) -> CompileErrorView {
        CompileErrorView {
            kind: self.kind().to_string(),
            message: self.to_string(),
            edge_id: self.edge_id().map(str::to_string),
            node_id: self.node_id().map(str::to_string),
        }
    }
}

/// Structured payload of a compile error for the editor. Returned as part of
/// the publish API response so the frontend can highlight the offending
/// node/edge inline instead of just showing a flat error string.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub struct CompileErrorView {
    pub kind: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edge_id: Option<String>,
}
