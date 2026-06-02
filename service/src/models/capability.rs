//! Phase 4 — Typed capability registry model + enroll-time validation.
//!
//! A `capability_type` is an admin-curated, workspace-scoped shape (e.g. `xrd`
//! with fields `max_2theta: number`, `source: text`). Two consumers type
//! against it:
//!
//!   - **enroll** (`handlers/runners.rs`): a runner advertising a
//!     `capabilities` blob `{ "<name>": { "<field>": <value>, … }, … }` is
//!     validated — every key must be a defined capability_type in the
//!     workspace, and each field value must match that type's typed fields
//!     (kind + required). A runner advertising NO caps (`{}`) still enrolls.
//!   - **publish** (`process/publish.rs`): step Requirements naming a
//!     capability/field are validated against the registry.
//!
//! These structs mirror the migration column order (see
//! `service/migrations/20240135000000_capability_types.sql`) so a `SELECT *`
//! reads back via `sqlx::FromRow` without surprises.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::compiler::CompileError;
use crate::models::error::ApiError;
use crate::models::template::{
    Constraint, ConstraintOp, FieldKind, WorkflowGraph, WorkflowNodeData,
};

/// One typed field on a capability. `kind` reuses the platform's unified
/// [`FieldKind`] vocabulary (the SAME enum the compiler/port model use) — do
/// not invent a parallel enum here. `options` carries the enum members when
/// `kind == FieldKind::Select`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CapabilityField {
    pub name: String,
    pub kind: FieldKind,
    #[serde(default)]
    pub required: bool,
    /// Enum members for `Select`-kind fields. Ignored for other kinds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<String>>,
}

/// One row from the `capability_types` table.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct CapabilityTypeRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub name: String,
    /// `[ { name, kind, required, options? } ]`. Deserialized into
    /// `Vec<CapabilityField>` by the handlers + loader.
    pub fields: serde_json::Value,
    pub created_by: Uuid,
    pub created_at: DateTime<Utc>,
    /// `Some(_)` = revoked (soft-deleted). Live queries filter `IS NULL`.
    pub revoked_at: Option<DateTime<Utc>>,
}

impl CapabilityTypeRow {
    /// Parse the JSONB `fields` blob into the typed field list. A malformed
    /// blob (shouldn't happen — only the create handler writes it) yields an
    /// empty list rather than panicking.
    pub fn parse_fields(&self) -> Vec<CapabilityField> {
        serde_json::from_value(self.fields.clone()).unwrap_or_default()
    }
}

// ── Wire DTOs ──────────────────────────────────────────────────────────────

/// Compact list-row shape. Returned by `GET /api/v1/capability-types`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CapabilityTypeSummary {
    pub id: Uuid,
    pub name: String,
    pub fields: Vec<CapabilityField>,
    pub created_at: DateTime<Utc>,
}

impl From<CapabilityTypeRow> for CapabilityTypeSummary {
    fn from(r: CapabilityTypeRow) -> Self {
        let fields = r.parse_fields();
        Self {
            id: r.id,
            name: r.name,
            fields,
            created_at: r.created_at,
        }
    }
}

/// Detail view returned by `GET /api/v1/capability-types/{id}`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CapabilityTypeDetail {
    pub id: Uuid,
    pub name: String,
    pub fields: Vec<CapabilityField>,
    pub created_by: Uuid,
    pub created_at: DateTime<Utc>,
}

impl From<CapabilityTypeRow> for CapabilityTypeDetail {
    fn from(r: CapabilityTypeRow) -> Self {
        let fields = r.parse_fields();
        Self {
            id: r.id,
            name: r.name,
            fields,
            created_by: r.created_by,
            created_at: r.created_at,
        }
    }
}

/// Request body for `POST /api/v1/capability-types`.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct CreateCapabilityTypeRequest {
    /// Capability name, unique within the workspace.
    pub name: String,
    /// Typed field list.
    #[serde(default)]
    pub fields: Vec<CapabilityField>,
}

// ── Registry shape (the loader's return type) ──────────────────────────────

/// The resolved shape of one capability type, keyed by name in
/// [`KnownCapabilities`]. Carries just the typed field list — the validation +
/// matching consumers never need the row's id/audit columns.
#[derive(Debug, Clone)]
pub struct CapabilityType {
    pub id: Uuid,
    pub fields: Vec<CapabilityField>,
}

/// Per-workspace capability map handed to the enroll + publish paths. Keyed by
/// capability name (the `<capability>` a runner advertises and a step
/// Constraint names). `BTreeMap` so iteration / error order is stable.
pub type KnownCapabilities = BTreeMap<String, CapabilityType>;

/// Load every LIVE capability type in a workspace into a [`KnownCapabilities`]
/// map. Single-query, mirroring `discover_known_resources`' style so the
/// enroll path (`handlers/runners.rs`) and the publish path
/// (`process/publish.rs`) both call ONE loader and can't drift.
pub async fn load_known_capabilities(
    db: &sqlx::PgPool,
    workspace_id: Uuid,
) -> Result<KnownCapabilities, ApiError> {
    let rows = sqlx::query_as::<_, CapabilityTypeRow>(
        "SELECT * FROM capability_types \
         WHERE workspace_id = $1 AND revoked_at IS NULL",
    )
    .bind(workspace_id)
    .fetch_all(db)
    .await
    .map_err(|e| ApiError::internal(format!("workspace capability lookup: {e}")))?;

    let mut known = KnownCapabilities::new();
    for row in rows {
        let fields = row.parse_fields();
        known.insert(row.name, CapabilityType { id: row.id, fields });
    }
    Ok(known)
}

// ── Enroll-time validation ─────────────────────────────────────────────────

/// Validate a runner's advertised `capabilities` blob against the workspace's
/// defined capability types (the enroll-time check, contract §"runner caps").
///
/// `caps` is the `runners.capabilities` JSONB:
///   `{ "<capability_name>": { "<field>": <value>, … }, … }`
///
/// Rules:
///   - Empty caps (`{}` or a non-object — defensively) => `Ok(())`.
///   - Every advertised capability key MUST be a defined `capability_type` in
///     the workspace; an unknown key is an error.
///   - Each advertised capability value MUST be a JSON object.
///   - Each REQUIRED field of the type MUST be present.
///   - Each present field value MUST match its declared [`FieldKind`] (via
///     `FieldKind::accepts`). Unknown fields (not on the type) are an error.
///   - For `Select`-kind fields with `options`, the value (a string) must be
///     one of the declared members.
///
/// Returns the FIRST violation as a human-readable string so the enroll
/// handler can surface a 400/422.
pub fn validate_caps_against_types(
    caps: &serde_json::Value,
    types: &KnownCapabilities,
) -> Result<(), String> {
    // A runner advertising no caps still enrolls. Treat a non-object blob
    // (e.g. `null`) as "no caps" too — the column defaults to `{}`.
    let Some(map) = caps.as_object() else {
        return Ok(());
    };
    if map.is_empty() {
        return Ok(());
    }

    for (cap_name, cap_value) in map {
        let Some(cap_type) = types.get(cap_name) else {
            return Err(format!(
                "unknown capability '{cap_name}' — not a defined capability type in this workspace"
            ));
        };
        let Some(field_map) = cap_value.as_object() else {
            return Err(format!(
                "capability '{cap_name}' must be a JSON object of field -> value"
            ));
        };

        // Required-field gate.
        for field in &cap_type.fields {
            if field.required && !field_map.contains_key(&field.name) {
                return Err(format!(
                    "capability '{cap_name}' is missing required field '{}'",
                    field.name
                ));
            }
        }

        // Per-advertised-field kind + membership check; unknown fields rejected.
        for (field_name, field_value) in field_map {
            let Some(spec) = cap_type.fields.iter().find(|f| &f.name == field_name) else {
                return Err(format!(
                    "capability '{cap_name}' has unknown field '{field_name}' \
                     (not declared on capability type '{cap_name}')"
                ));
            };
            if !spec.kind.accepts(field_value) {
                return Err(format!(
                    "capability '{cap_name}' field '{field_name}' has the wrong type \
                     (expected {:?})",
                    spec.kind
                ));
            }
            // Select membership: a string value must be one of the declared
            // options when options are present.
            if matches!(spec.kind, FieldKind::Select) {
                if let (Some(options), Some(v)) = (&spec.options, field_value.as_str()) {
                    if !options.iter().any(|o| o == v) {
                        return Err(format!(
                            "capability '{cap_name}' field '{field_name}' value '{v}' \
                             is not one of the declared options {options:?}"
                        ));
                    }
                }
            }
        }
    }

    Ok(())
}

// ── Publish-time validation: step Requirements vs. the capability registry ───

/// Validate every `AutomatedStep`'s placement [`Requirements`] in `graph`
/// against the workspace's defined capability types (the publish-time check,
/// Phase 4). Returns ALL hard violations as [`CompileError`]s (so the publish
/// path can surface them together via each error's `to_view()` with the
/// offending `node_id`, letting the editor highlight every bad step).
///
/// This is the publish-side TWIN of [`validate_caps_against_types`] (the
/// enroll-side check on a runner's advertised caps) — both type a JSON shape
/// against the SAME [`KnownCapabilities`] registry, so the producer (runner
/// caps) and the consumer (step requirements) can't drift.
///
/// It lives HERE (called from `process/publish.rs`) rather than in a compiler
/// `validate` hook because the pure `compile_to_air` has no DB handle and the
/// `KnownCapabilities` map is only loadable at publish.
///
/// Hard-error classes (per [`Requirements`] §"Step Requirements"):
///   - constraint names a capability NOT in the registry ⇒
///     [`CompileError::UndefinedRequirementCapability`];
///   - constraint names a field NOT on that capability's typed schema ⇒
///     [`CompileError::UnknownRequirementField`];
///   - constraint's op/value is incompatible with the field's [`FieldKind`]
///     (numeric op on a non-numeric kind; literal `value` whose JSON type the
///     kind rejects; `in` value not an array of acceptable members) ⇒
///     [`CompileError::RequirementTypeMismatch`].
///
/// `op == Exists` ignores `value` and only requires the capability+field to be
/// defined. An empty / `None` requirements set yields nothing.
pub fn validate_requirements_against_registry(
    graph: &WorkflowGraph,
    registry: &KnownCapabilities,
) -> Vec<CompileError> {
    let mut errors = Vec::new();
    for node in &graph.nodes {
        let WorkflowNodeData::AutomatedStep {
            requirements: Some(reqs),
            ..
        } = &node.data
        else {
            continue;
        };
        for c in &reqs.constraints {
            validate_one_constraint(&node.id, c, registry, &mut errors);
        }
    }
    errors
}

/// Validate a single [`Constraint`] against the registry, pushing the FIRST
/// applicable hard error for it onto `errors`.
fn validate_one_constraint(
    node_id: &str,
    c: &Constraint,
    registry: &KnownCapabilities,
    errors: &mut Vec<CompileError>,
) {
    let Some(cap_type) = registry.get(&c.capability) else {
        errors.push(CompileError::UndefinedRequirementCapability {
            node_id: node_id.to_string(),
            capability: c.capability.clone(),
        });
        return;
    };
    let Some(spec) = cap_type.fields.iter().find(|f| f.name == c.field) else {
        errors.push(CompileError::UnknownRequirementField {
            node_id: node_id.to_string(),
            capability: c.capability.clone(),
            field: c.field.clone(),
        });
        return;
    };

    // `exists` only needs the capability+field to be defined — value/op-type
    // compatibility is irrelevant.
    if matches!(c.op, ConstraintOp::Exists) {
        return;
    }

    if let Some(message) = op_value_incompatibility(spec.kind, c.op, &c.value) {
        errors.push(CompileError::RequirementTypeMismatch {
            node_id: node_id.to_string(),
            capability: c.capability.clone(),
            field: c.field.clone(),
            message,
        });
    }
}

/// Returns `Some(reason)` when `op`+`value` is type-incompatible with a field of
/// the given [`FieldKind`], else `None`. Pure, total, never panics — mirrors the
/// engine `satisfies` matcher's coercion model so compile-time rejection and
/// runtime evaluation agree:
///   - numeric ops (`gt`/`gte`/`lt`/`lte`) require a `Number`-kind field AND a
///     numeric `value`;
///   - `in` requires `value` to be a JSON array whose members the field's kind
///     `accepts`;
///   - `eq`/`neq` require the literal `value` to be a JSON type the field's kind
///     `accepts`.
fn op_value_incompatibility(
    kind: FieldKind,
    op: ConstraintOp,
    value: &serde_json::Value,
) -> Option<String> {
    match op {
        ConstraintOp::Exists => None,
        ConstraintOp::Gt | ConstraintOp::Gte | ConstraintOp::Lt | ConstraintOp::Lte => {
            if !matches!(kind, FieldKind::Number) {
                return Some(format!(
                    "numeric comparison '{}' requires a number-kind field, but the field is {kind:?}",
                    op_wire(op)
                ));
            }
            if !value.is_number() {
                return Some(format!(
                    "numeric comparison '{}' requires a numeric value, got {value}",
                    op_wire(op)
                ));
            }
            None
        }
        ConstraintOp::In => {
            let Some(arr) = value.as_array() else {
                return Some("operator 'in' requires the value to be an array".to_string());
            };
            for member in arr {
                if !kind.accepts(member) {
                    return Some(format!(
                        "operator 'in' member {member} is not acceptable for a {kind:?} field"
                    ));
                }
            }
            None
        }
        ConstraintOp::Eq | ConstraintOp::Neq => {
            if !kind.accepts(value) {
                return Some(format!(
                    "value {value} is not acceptable for a {kind:?} field"
                ));
            }
            None
        }
    }
}

/// Best-effort Rust mirror of the engine `satisfies(requirements, caps)`
/// matcher — used ONLY for the publish-time empty-fleet WARNING (never to
/// hard-error; the engine guard is authoritative at runtime). Returns `true`
/// iff EVERY constraint is satisfied by `caps` (the `runners.capabilities`
/// JSONB `{ "<cap>": { "<field>": <value> } }`). Empty constraints ⇒ `true`.
/// Total + never panics: any missing/malformed data ⇒ that constraint is not
/// satisfied (`false`), matching the engine matcher's fail-safe semantics.
pub fn caps_satisfy_constraints(constraints: &[Constraint], caps: &serde_json::Value) -> bool {
    if constraints.is_empty() {
        return true;
    }
    let Some(cap_obj) = caps.as_object() else {
        return false;
    };
    constraints.iter().all(|c| {
        let Some(field_value) = cap_obj
            .get(&c.capability)
            .and_then(|m| m.as_object())
            .and_then(|m| m.get(&c.field))
        else {
            // `exists` and every other op require the field to be present.
            return false;
        };
        match c.op {
            ConstraintOp::Exists => true,
            ConstraintOp::Eq => field_value == &c.value,
            ConstraintOp::Neq => field_value != &c.value,
            ConstraintOp::Gt | ConstraintOp::Gte | ConstraintOp::Lt | ConstraintOp::Lte => {
                match (field_value.as_f64(), c.value.as_f64()) {
                    (Some(a), Some(b)) => match c.op {
                        ConstraintOp::Gt => a > b,
                        ConstraintOp::Gte => a >= b,
                        ConstraintOp::Lt => a < b,
                        ConstraintOp::Lte => a <= b,
                        _ => false,
                    },
                    _ => false,
                }
            }
            ConstraintOp::In => c
                .value
                .as_array()
                .map(|arr| arr.iter().any(|m| m == field_value))
                .unwrap_or(false),
        }
    })
}

/// The lowercase wire string for an op — for diagnostic messages (matches the
/// `#[serde(rename_all = "lowercase")]` on [`ConstraintOp`] + the engine
/// matcher's op strings).
fn op_wire(op: ConstraintOp) -> &'static str {
    match op {
        ConstraintOp::Eq => "eq",
        ConstraintOp::Neq => "neq",
        ConstraintOp::Gt => "gt",
        ConstraintOp::Gte => "gte",
        ConstraintOp::Lt => "lt",
        ConstraintOp::Lte => "lte",
        ConstraintOp::In => "in",
        ConstraintOp::Exists => "exists",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::template::{
        DeploymentModel, ExecutionBackendType, ExecutionSpecConfig, Port, Position, Requirements,
        RetryPolicy, WorkflowGraph, WorkflowNode,
    };

    fn registry() -> KnownCapabilities {
        let mut m = KnownCapabilities::new();
        m.insert(
            "xrd".to_string(),
            CapabilityType {
                id: Uuid::nil(),
                fields: vec![
                    CapabilityField {
                        name: "max_2theta".to_string(),
                        kind: FieldKind::Number,
                        required: false,
                        options: None,
                    },
                    CapabilityField {
                        name: "source".to_string(),
                        kind: FieldKind::Text,
                        required: false,
                        options: None,
                    },
                ],
            },
        );
        m
    }

    fn step_with(requirements: Option<Requirements>) -> WorkflowGraph {
        WorkflowGraph {
            nodes: vec![WorkflowNode {
                id: "step".to_string(),
                node_type: "automated_step".to_string(),
                slug: None,
                position: Position { x: 0.0, y: 0.0 },
                data: WorkflowNodeData::AutomatedStep {
                    label: "Step".to_string(),
                    description: None,
                    execution_spec: ExecutionSpecConfig {
                        backend_type: ExecutionBackendType::Python,
                        entrypoint: Some("main.py".to_string()),
                        config: serde_json::json!({}),
                    },
                    input: Port::empty_input(),
                    output: Port::empty_input(),
                    retry_policy: RetryPolicy::default(),
                    deployment_model: DeploymentModel::default(),
                    stream_output: false,
                    stream_input: false,
                    requirements,
                    asset_bindings: Vec::new(),
                },
                parent_id: None,
                width: None,
                height: None,
            }],
            edges: Vec::new(),
            viewport: None,
            instance_concurrency: Default::default(),
            definitions: Default::default(),
            default_scheduler: None,
        }
    }

    fn c(capability: &str, field: &str, op: ConstraintOp, value: serde_json::Value) -> Constraint {
        Constraint {
            capability: capability.to_string(),
            field: field.to_string(),
            op,
            value,
        }
    }

    #[test]
    fn no_requirements_no_errors() {
        let errs = validate_requirements_against_registry(&step_with(None), &registry());
        assert!(errs.is_empty());
    }

    #[test]
    fn valid_requirements_compile_clean() {
        let reqs = Requirements {
            constraints: vec![
                c("xrd", "max_2theta", ConstraintOp::Gte, serde_json::json!(160.0)),
                c("xrd", "source", ConstraintOp::Exists, serde_json::Value::Null),
                c(
                    "xrd",
                    "source",
                    ConstraintOp::Eq,
                    serde_json::json!("synchrotron"),
                ),
            ],
        };
        let errs = validate_requirements_against_registry(&step_with(Some(reqs)), &registry());
        assert!(errs.is_empty(), "expected clean, got {errs:?}");
    }

    #[test]
    fn undefined_capability_is_compile_error() {
        let reqs = Requirements {
            constraints: vec![c(
                "spectrometer",
                "range",
                ConstraintOp::Gt,
                serde_json::json!(1),
            )],
        };
        let errs = validate_requirements_against_registry(&step_with(Some(reqs)), &registry());
        assert_eq!(errs.len(), 1);
        assert!(matches!(
            &errs[0],
            CompileError::UndefinedRequirementCapability { capability, .. } if capability == "spectrometer"
        ));
    }

    #[test]
    fn unknown_field_is_compile_error() {
        let reqs = Requirements {
            constraints: vec![c("xrd", "nonexistent", ConstraintOp::Exists, serde_json::Value::Null)],
        };
        let errs = validate_requirements_against_registry(&step_with(Some(reqs)), &registry());
        assert_eq!(errs.len(), 1);
        assert!(matches!(
            &errs[0],
            CompileError::UnknownRequirementField { field, .. } if field == "nonexistent"
        ));
    }

    #[test]
    fn numeric_op_on_text_field_is_type_mismatch() {
        let reqs = Requirements {
            constraints: vec![c("xrd", "source", ConstraintOp::Gt, serde_json::json!(5))],
        };
        let errs = validate_requirements_against_registry(&step_with(Some(reqs)), &registry());
        assert_eq!(errs.len(), 1);
        assert!(matches!(
            &errs[0],
            CompileError::RequirementTypeMismatch { field, .. } if field == "source"
        ));
    }

    #[test]
    fn eq_value_wrong_json_type_is_type_mismatch() {
        // max_2theta is Number; an `eq` against a string is incompatible.
        let reqs = Requirements {
            constraints: vec![c(
                "xrd",
                "max_2theta",
                ConstraintOp::Eq,
                serde_json::json!("nope"),
            )],
        };
        let errs = validate_requirements_against_registry(&step_with(Some(reqs)), &registry());
        assert_eq!(errs.len(), 1);
        assert!(matches!(
            &errs[0],
            CompileError::RequirementTypeMismatch { .. }
        ));
    }

    #[test]
    fn caps_satisfy_mirror_matches_engine_semantics() {
        let constraints = vec![
            c("xrd", "max_2theta", ConstraintOp::Gte, serde_json::json!(160.0)),
            c("xrd", "source", ConstraintOp::Exists, serde_json::Value::Null),
        ];
        // A runner with max_2theta=180 + source present satisfies.
        let good = serde_json::json!({ "xrd": { "max_2theta": 180.0, "source": "synchrotron" } });
        assert!(caps_satisfy_constraints(&constraints, &good));

        // max_2theta below the floor fails.
        let low = serde_json::json!({ "xrd": { "max_2theta": 90.0, "source": "lab" } });
        assert!(!caps_satisfy_constraints(&constraints, &low));

        // Missing capability fails.
        let none = serde_json::json!({ "other": {} });
        assert!(!caps_satisfy_constraints(&constraints, &none));

        // Empty constraints match anything.
        assert!(caps_satisfy_constraints(&[], &none));
    }
}
