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
    /// Two nodes resolve to the same author-facing `slug` (the
    /// `<slug>.<field>` guard namespace must be unique within a graph). Only
    /// explicit, user-set slugs can conflict — derived defaults are
    /// collision-suffixed deterministically and never reach here.
    #[error(
        "nodes '{node_a}' and '{node_b}' both use slug '{slug}' — slugs must be unique within a graph"
    )]
    SlugConflict {
        slug: String,
        node_a: String,
        node_b: String,
    },

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

    // --- Sub-workflow errors (call/return composition). Resolution + cycle
    //     detection run at the parent's publish time (they need DB access to
    //     resolve the child template version chain); `Unresolved` also fires
    //     in the compiler when a `SubWorkflow` node reaches lowering with no
    //     pre-resolved child AIR. All carry the offending node_id so the
    //     editor canvas can ring it.
    #[error(
        "sub-workflow node '{node_id}' references template '{template_id}' which is not published / not found"
    )]
    SubWorkflowUnresolved {
        node_id: String,
        template_id: String,
    },

    #[error("sub-workflow cycle detected: {chain:?}")]
    SubWorkflowCycle { chain: Vec<String> },

    #[error("sub-workflow nesting too deep (limit {limit}) at node '{node_id}'")]
    SubWorkflowDepthExceeded { node_id: String, limit: usize },

    /// Loop has no body — no child node has `parent_id == loop.id`. An empty
    /// Loop is a config error (an iterating-counter-with-no-work workflow is
    /// not a useful primitive; use a dedicated Delay node if/when needed).
    #[error("loop '{node_id}' has no body — add at least one node inside the loop container")]
    LoopEmpty { node_id: String },

    // --- Python AutomatedStep output-field guards (sibling of the
    //     direct-slug-access input borrows). Declared output.fields[].name is
    //     swept from Python globals after exec() — if the name collides with a
    //     reserved runner global or an upstream slug borrowed by this node,
    //     the runtime would either silently lose the assignment or
    //     accidentally re-emit borrowed input as output. Reject at compile.
    /// Declared output field name matches a reserved runner global (`token`,
    /// `input`, `set_output`, etc — mirror of the runner.rs `_RESERVED_GLOBALS`
    /// set). Rename the field.
    #[error(
        "node '{node_id}': output field '{field_name}' shadows a reserved runner global — rename the field"
    )]
    OutputFieldShadowsReserved {
        node_id: String,
        field_name: String,
    },

    /// Declared output field name matches a slug bound as a Python global on
    /// this node (an upstream producer the user's source borrows as
    /// `<slug>.<attr>`). Without the guard the input global would silently
    /// re-export as this step's output.
    #[error(
        "node '{node_id}': output field '{field_name}' collides with borrowed input '{upstream_slug}' from upstream node '{upstream_node_id}' — rename the output field"
    )]
    OutputFieldShadowsInput {
        node_id: String,
        field_name: String,
        upstream_slug: String,
        upstream_node_id: String,
    },

    // --- LLM / Kreuzberg upstream-producer refs (sibling of Python direct
    //     slug access and HumanTask placeholders). The `{{}}` syntax is
    //     unambiguous so unlike Python's silent-ignore semantics, an
    //     unknown slug or field is a typo — hard-reject at compile.
    /// `{{<slug>.<field>}}` references an unknown slug, or `<field>` is
    /// not declared on the producer's output port. `backend` is `"llm"` or
    /// `"kreuzberg"`; `site` names the offending config field (e.g.
    /// `"prompt"`, `"system_prompt"`, `"file"`, `"images[0].path"`).
    #[error(
        "node '{node_id}' ({backend}): {site} references unknown {kind} '{name}' in `{{{{{slug}.{field}}}}}` (available {kind}s: {available:?})"
    )]
    BackendRefUnresolved {
        node_id: String,
        backend: String,
        site: String,
        slug: String,
        field: String,
        /// `"slug"` when the head doesn't match any graph slug; `"field"`
        /// when the head is known but the attr isn't on its output port.
        kind: String,
        /// The unknown name (== `slug` when kind="slug", == `field` when
        /// kind="field"). Surfaced separately so the editor can highlight
        /// just the failing part of the path.
        name: String,
        /// Candidate names the author might have meant — slugs in the
        /// graph (kind="slug") or fields on the producer (kind="field").
        available: Vec<String>,
    },

    /// `{{<slug>.<field>}}` references a producer that lives downstream of
    /// (or at) the consumer in the graph topology — a borrow cycle. The
    /// `{{}}` syntax pre-binds the field at compile time, so circular
    /// references aren't physically realizable.
    #[error(
        "node '{node_id}' ({backend}): {site} borrows '{{{{{slug}.{field}}}}}' from producer '{producer_node_id}' which is not strictly upstream"
    )]
    BackendRefNotUpstream {
        node_id: String,
        backend: String,
        site: String,
        slug: String,
        field: String,
        producer_node_id: String,
    },

    /// Malformed `{{...}}` placeholder body — not a dotted-identifier path.
    /// Surfaces early from `validate_and_transform` so the author sees a
    /// precise syntax error instead of a downstream "unresolved input".
    #[error(
        "node '{node_id}' ({backend}): {site} contains malformed placeholder '{{{{{body}}}}}' — expected `<slug>.<field>`"
    )]
    BackendPlaceholderSyntax {
        node_id: String,
        backend: String,
        site: String,
        body: String,
    },

    /// LLM `images[i].path` references an upstream producer field whose
    /// declared kind is not `file`. Unlike Kreuzberg (which can stage text
    /// as a temp file), LLM vision needs actual image bytes.
    #[error(
        "node '{node_id}' (llm): {site} requires a file-kind upstream field; '{{{{{slug}.{field}}}}}' resolves to kind '{actual_kind}'"
    )]
    LlmImageRefNotFileKind {
        node_id: String,
        site: String,
        slug: String,
        field: String,
        actual_kind: String,
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
            Self::SlugConflict { .. } => "slug_conflict",
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
            Self::SubWorkflowUnresolved { .. } => "subworkflow_unresolved",
            Self::SubWorkflowCycle { .. } => "subworkflow_cycle",
            Self::SubWorkflowDepthExceeded { .. } => "subworkflow_depth_exceeded",
            Self::LoopEmpty { .. } => "loop_empty",
            Self::OutputFieldShadowsReserved { .. } => "output_field_shadows_reserved",
            Self::OutputFieldShadowsInput { .. } => "output_field_shadows_input",
            Self::BackendRefUnresolved { .. } => "backend_ref_unresolved",
            Self::BackendRefNotUpstream { .. } => "backend_ref_not_upstream",
            Self::BackendPlaceholderSyntax { .. } => "backend_placeholder_syntax",
            Self::LlmImageRefNotFileKind { .. } => "llm_image_ref_not_file_kind",
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
            Self::SlugConflict { node_a, .. } => Some(node_a),
            Self::TriggerEdgeCardinality { node_id, .. }
            | Self::TriggerIsEdgeTarget { node_id, .. }
            | Self::TriggerUnknownTargetField { node_id, .. }
            | Self::TriggerMappingSyntax { node_id, .. }
            | Self::TriggerCronInvalid { node_id, .. }
            | Self::TriggerUnresolvedRef { node_id, .. }
            | Self::TriggerEmptyMappingRequiredFields { node_id, .. }
            | Self::SubWorkflowUnresolved { node_id, .. }
            | Self::SubWorkflowDepthExceeded { node_id, .. }
            | Self::LoopEmpty { node_id }
            | Self::OutputFieldShadowsReserved { node_id, .. }
            | Self::OutputFieldShadowsInput { node_id, .. }
            | Self::BackendRefUnresolved { node_id, .. }
            | Self::BackendRefNotUpstream { node_id, .. }
            | Self::BackendPlaceholderSyntax { node_id, .. }
            | Self::LlmImageRefNotFileKind { node_id, .. } => Some(node_id),
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
