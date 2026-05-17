use std::collections::{HashMap, HashSet};

use chrono::Utc;
use serde_json::{json, Map, Value};
use uuid::Uuid;

use crate::models::instance::StartToken;
use crate::models::template::{
    FieldKind, Port, PortValidationError, WorkflowGraph, WorkflowNodeData,
};
use crate::petri::client::{PetriClient, PetriError};

/// Errors returned by `parameterize_air` when the supplied `start_tokens`
/// don't match the template's declared Start ports. The instance API surfaces
/// these as HTTP 400 responses so callers can correct their payload.
#[derive(Debug, thiserror::Error)]
pub enum ParameterizeError {
    /// One or more Start blocks declare a non-empty `initial` port (with
    /// required fields or any fields at all) but no matching `start_tokens`
    /// entry was supplied. The vec lists the offending block ids.
    #[error("missing start_tokens for start block(s): {0:?}")]
    MissingStartTokens(Vec<String>),

    /// A `start_tokens` entry refers to a `start_block_id` that doesn't exist
    /// in the template (or isn't a Start block).
    #[error("start_tokens references unknown start block '{0}'")]
    UnknownStartBlock(String),

    /// Two `start_tokens` entries target the same Start block.
    #[error("duplicate start_tokens entry for start block '{0}'")]
    DuplicateStartToken(String),

    /// The supplied token isn't a JSON object — Start tokens must be field-keyed.
    #[error("token for start block '{0}' must be a JSON object")]
    TokenNotObject(String),

    /// A required field declared on the Start's `initial` port is absent from
    /// the supplied token.
    #[error("token for start block '{block}': field '{field}' is required but missing")]
    MissingRequiredField { block: String, field: String },

    /// A field is present but its JSON kind doesn't match the declared
    /// `FieldKind` (e.g. a string supplied for a `Number` field). `Json` and
    /// `File` are permissive escape hatches that don't trip this.
    #[error("token for start block '{block}': field '{field}' has wrong type for kind {kind:?}")]
    FieldKindMismatch {
        block: String,
        field: String,
        kind: FieldKind,
    },
}

/// Parameterize the compiled AIR JSON for a specific instance.
///
/// Replaces template placeholders, then for each `Start` block in the
/// template's `WorkflowGraph` seeds the matching AIR place
/// (`p_{start_block_id}_ready`) with a single initial token built from the
/// supplied `start_tokens` (validated against the Start's `initial` port).
/// Starts that aren't in `start_tokens` get a default `{}` token, but only if
/// their declared `initial` port has no fields — otherwise `MissingStartTokens`
/// is returned.
///
/// Template-string substitutions (kept from the previous parameterize):
/// - `__INSTANCE_ID__` -> instance UUID
/// - `__TIMESTAMP__`   -> current ISO 8601 timestamp
/// - `__TEMPLATE_ID__` -> template UUID
///
/// System fields injected into every seeded Start token:
/// - `_instance_id`, `_template_id`, `_template_version`, `_created_at`, `_created_by`
///
/// Unlike pre-typed-ports behavior, the caller's `metadata` blob is **not**
/// merged into tokens — it's audit-only at the instance row level.
pub fn parameterize_air(
    air_json: &Value,
    instance_id: Uuid,
    template_id: Uuid,
    template_version: i32,
    created_by: Uuid,
    graph: &WorkflowGraph,
    start_tokens: &[StartToken],
) -> Result<Value, ParameterizeError> {
    let now = Utc::now().to_rfc3339();

    // String-substitute template placeholders against the serialized AIR.
    let mut air_str = serde_json::to_string(air_json).unwrap_or_default();
    air_str = air_str.replace("__INSTANCE_ID__", &instance_id.to_string());
    air_str = air_str.replace("__TIMESTAMP__", &now);
    air_str = air_str.replace("__TEMPLATE_ID__", &template_id.to_string());
    let mut air: Value = serde_json::from_str(&air_str).unwrap_or(json!({}));

    // 1. Index Start blocks by id.
    let starts: HashMap<&str, &Port> = graph
        .nodes
        .iter()
        .filter_map(|n| match &n.data {
            WorkflowNodeData::Start { initial, .. } => Some((n.id.as_str(), initial)),
            _ => None,
        })
        .collect();

    // 2. Validate the supplied start_tokens against the Start set.
    let mut seen: HashSet<&str> = HashSet::new();
    for st in start_tokens {
        if !starts.contains_key(st.start_block_id.as_str()) {
            return Err(ParameterizeError::UnknownStartBlock(
                st.start_block_id.clone(),
            ));
        }
        if !seen.insert(st.start_block_id.as_str()) {
            return Err(ParameterizeError::DuplicateStartToken(
                st.start_block_id.clone(),
            ));
        }
    }

    // 3. For each Start block, resolve its token (provided or default `{}`),
    //    validate, and inject system fields.
    let supplied: HashMap<&str, &Value> = start_tokens
        .iter()
        .map(|st| (st.start_block_id.as_str(), &st.token))
        .collect();

    let empty_token = Value::Object(Map::new());
    let mut missing_for_required: Vec<String> = Vec::new();
    let mut seeded: HashMap<String, Value> = HashMap::new();

    for (start_id, port) in &starts {
        let raw = supplied.get(start_id).copied().unwrap_or(&empty_token);

        // Reject non-object tokens up front — every Start port is field-keyed.
        let obj = match raw.as_object() {
            Some(o) => o,
            None => return Err(ParameterizeError::TokenNotObject((*start_id).to_string())),
        };

        // A Start with declared fields and no supplied entry is rejected as
        // a batch ("missing start_tokens for start block(s): [...]"). We collect
        // all such cases before returning so the caller sees all of them at once.
        if !port.fields.is_empty() && !supplied.contains_key(start_id) {
            missing_for_required.push((*start_id).to_string());
            continue;
        }

        // Per-field validation when an entry was supplied. Delegates to the
        // shared `Port::validate_token` so spawn (here) and in-flight signal
        // (trigger dispatcher) enforce byte-identical rules; we only re-attach
        // the offending block id to the error.
        port.validate_token(raw).map_err(|e| match e {
            PortValidationError::NotObject => {
                ParameterizeError::TokenNotObject((*start_id).to_string())
            }
            PortValidationError::MissingRequiredField { field } => {
                ParameterizeError::MissingRequiredField {
                    block: (*start_id).to_string(),
                    field,
                }
            }
            PortValidationError::FieldKindMismatch { field, kind } => {
                ParameterizeError::FieldKindMismatch {
                    block: (*start_id).to_string(),
                    field,
                    kind,
                }
            }
        })?;

        // Clone token + inject system fields.
        let mut token = obj.clone();
        token.insert("_instance_id".to_string(), json!(instance_id.to_string()));
        token.insert("_template_id".to_string(), json!(template_id.to_string()));
        token.insert("_template_version".to_string(), json!(template_version));
        token.insert("_created_at".to_string(), json!(now));
        token.insert("_created_by".to_string(), json!(created_by.to_string()));

        seeded.insert((*start_id).to_string(), Value::Object(token));
    }

    if !missing_for_required.is_empty() {
        return Err(ParameterizeError::MissingStartTokens(missing_for_required));
    }

    // 4. Walk AIR places; for each `p_{start_id}_ready`, replace its
    //    `initial_tokens` with the seeded token. Other places untouched —
    //    the compiler emits empty `initial_tokens` for non-Start places, and
    //    parameterize doesn't touch those.
    if let Some(places) = air.get_mut("places").and_then(|p| p.as_array_mut()) {
        for place in places {
            let place_id = place.get("id").and_then(|v| v.as_str()).map(str::to_string);
            let Some(place_id) = place_id else { continue };
            let Some(start_id) = place_id
                .strip_prefix("p_")
                .and_then(|s| s.strip_suffix("_ready"))
            else {
                continue;
            };
            if let Some(token) = seeded.remove(start_id) {
                if let Some(obj) = place.as_object_mut() {
                    obj.insert("initial_tokens".to_string(), Value::Array(vec![token]));
                }
            }
        }
    }

    Ok(air)
}

/// Deploy a workflow instance to petri-lab.
///
/// 1. Parameterize AIR JSON
/// 2. Deploy scenario to petri-lab
/// 3. Set run mode to "running"
pub async fn deploy_instance(
    client: &PetriClient,
    net_id: &str,
    air_json: &Value,
) -> Result<(), PetriError> {
    // Deploy the scenario
    client.deploy_scenario(net_id, air_json).await?;

    // Start execution
    client
        .set_run_mode(net_id, petri_api_types::RunMode::Running)
        .await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::template::{
        FieldKind, Port, PortField, Position, WorkflowEdge, WorkflowNode, WorkflowNodeData,
    };

    fn graph_with_start(initial: Port) -> WorkflowGraph {
        WorkflowGraph {
            nodes: vec![
                WorkflowNode {
                    id: "n_start".to_string(),
                    node_type: "start".to_string(),
                    position: Position { x: 0.0, y: 0.0 },
                    data: WorkflowNodeData::Start {
                        label: "Start".to_string(),
                        description: None,
                        initial,
                        process_name: None,
                    },
                    parent_id: None,
                    width: None,
                    height: None,
                },
                WorkflowNode {
                    id: "n_end".to_string(),
                    node_type: "end".to_string(),
                    position: Position { x: 100.0, y: 0.0 },
                    data: WorkflowNodeData::End {
                        label: "End".to_string(),
                        description: None,
                        terminal: crate::models::template::default_terminal_port(),
                        result_mapping: Vec::new(),
                    },
                    parent_id: None,
                    width: None,
                    height: None,
                },
            ],
            edges: vec![WorkflowEdge {
                id: "e1".to_string(),
                source: "n_start".to_string(),
                target: "n_end".to_string(),
                source_handle: None,
                target_handle: Some("in".to_string()),
                label: None,
                edge_type: "sequence".to_string(),
            }],
            viewport: None,
        }
    }

    fn air_with_start_place() -> Value {
        json!({
            "places": [
                { "id": "p_n_start_ready", "name": "Start", "initial_tokens": [] },
                { "id": "p_n_end_done", "name": "End", "initial_tokens": [] }
            ]
        })
    }

    #[test]
    fn seeds_empty_start_with_default_token() {
        let graph = graph_with_start(Port::empty_input());
        let air = air_with_start_place();
        let result = parameterize_air(&air, Uuid::nil(), Uuid::nil(), 1, Uuid::nil(), &graph, &[])
            .expect("simple Start with empty initial port should seed default token");

        let tokens = &result["places"][0]["initial_tokens"];
        assert_eq!(tokens.as_array().expect("array").len(), 1);
        let tok = &tokens[0];
        assert!(tok["_instance_id"].is_string(), "system field injected");
        assert!(tok["_template_id"].is_string());
    }

    #[test]
    fn rejects_missing_start_tokens_when_port_has_fields() {
        let port = Port {
            id: "in".to_string(),
            label: "Input".to_string(),
            fields: vec![PortField {
                name: "customer_id".to_string(),
                label: "Customer ID".to_string(),
                kind: FieldKind::Text,
                required: true,
                options: None,
                description: None,
                accept: None,
            }],
        };
        let graph = graph_with_start(port);
        let air = air_with_start_place();
        let err = parameterize_air(&air, Uuid::nil(), Uuid::nil(), 1, Uuid::nil(), &graph, &[])
            .expect_err("should reject missing tokens for a port with fields");
        match err {
            ParameterizeError::MissingStartTokens(ids) => {
                assert_eq!(ids, vec!["n_start".to_string()]);
            }
            e => panic!("unexpected error: {e}"),
        }
    }

    #[test]
    fn rejects_missing_required_field() {
        let port = Port {
            id: "in".to_string(),
            label: "Input".to_string(),
            fields: vec![PortField {
                name: "customer_id".to_string(),
                label: "Customer ID".to_string(),
                kind: FieldKind::Text,
                required: true,
                options: None,
                description: None,
                accept: None,
            }],
        };
        let graph = graph_with_start(port);
        let air = air_with_start_place();
        let st = StartToken {
            start_block_id: "n_start".to_string(),
            token: json!({ "other": "x" }),
        };
        let err = parameterize_air(
            &air,
            Uuid::nil(),
            Uuid::nil(),
            1,
            Uuid::nil(),
            &graph,
            &[st],
        )
        .expect_err("should reject token missing required field");
        match err {
            ParameterizeError::MissingRequiredField { block, field } => {
                assert_eq!(block, "n_start");
                assert_eq!(field, "customer_id");
            }
            e => panic!("unexpected error: {e}"),
        }
    }

    #[test]
    fn rejects_kind_mismatch() {
        let port = Port {
            id: "in".to_string(),
            label: "Input".to_string(),
            fields: vec![PortField {
                name: "amount".to_string(),
                label: "Amount".to_string(),
                kind: FieldKind::Number,
                required: true,
                options: None,
                description: None,
                accept: None,
            }],
        };
        let graph = graph_with_start(port);
        let air = air_with_start_place();
        let st = StartToken {
            start_block_id: "n_start".to_string(),
            token: json!({ "amount": "not-a-number" }),
        };
        let err = parameterize_air(
            &air,
            Uuid::nil(),
            Uuid::nil(),
            1,
            Uuid::nil(),
            &graph,
            &[st],
        )
        .expect_err("should reject kind mismatch");
        assert!(matches!(err, ParameterizeError::FieldKindMismatch { .. }));
    }

    #[test]
    fn seeds_validated_token_with_system_fields() {
        let port = Port {
            id: "in".to_string(),
            label: "Input".to_string(),
            fields: vec![PortField {
                name: "customer_id".to_string(),
                label: "Customer ID".to_string(),
                kind: FieldKind::Text,
                required: true,
                options: None,
                description: None,
                accept: None,
            }],
        };
        let graph = graph_with_start(port);
        let air = air_with_start_place();
        let st = StartToken {
            start_block_id: "n_start".to_string(),
            token: json!({ "customer_id": "c-42" }),
        };
        let result = parameterize_air(
            &air,
            Uuid::nil(),
            Uuid::nil(),
            7,
            Uuid::nil(),
            &graph,
            &[st],
        )
        .expect("valid token should seed");

        let tok = &result["places"][0]["initial_tokens"][0];
        assert_eq!(tok["customer_id"], "c-42");
        assert_eq!(tok["_template_version"], 7);
    }

    #[test]
    fn rejects_unknown_start_block_id() {
        let graph = graph_with_start(Port::empty_input());
        let air = air_with_start_place();
        let st = StartToken {
            start_block_id: "nope".to_string(),
            token: json!({}),
        };
        let err = parameterize_air(
            &air,
            Uuid::nil(),
            Uuid::nil(),
            1,
            Uuid::nil(),
            &graph,
            &[st],
        )
        .expect_err("unknown start_block_id should fail");
        assert!(matches!(err, ParameterizeError::UnknownStartBlock(_)));
    }

    #[test]
    fn rejects_duplicate_start_block_entries() {
        let graph = graph_with_start(Port::empty_input());
        let air = air_with_start_place();
        let dup = vec![
            StartToken {
                start_block_id: "n_start".to_string(),
                token: json!({}),
            },
            StartToken {
                start_block_id: "n_start".to_string(),
                token: json!({}),
            },
        ];
        let err = parameterize_air(&air, Uuid::nil(), Uuid::nil(), 1, Uuid::nil(), &graph, &dup)
            .expect_err("duplicate start_block_id should fail");
        assert!(matches!(err, ParameterizeError::DuplicateStartToken(_)));
    }
}
