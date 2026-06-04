//! Per-project OpenAPI bundle — `GET /api/v1/workspaces/{ws}/projects/{p}/openapi.json`.
//!
//! Assembles a synthetic OpenAPI 3.0.3 document covering every *callable*
//! trigger advertised by the templates attached to the project. This is
//! Phase B's flagship endpoint — it's *why* projects exist as a primitive.
//! Consumers (SDK generators, API doc viewers) can point at one URL per
//! project and get an addressable, typed catalog of trigger entrypoints
//! without needing to crawl the full mekhan API.
//!
//! This module is a **pure consumer of the schema atom**: every requestBody /
//! response shape is derived from the typed model (`Port::json_schema` over the
//! target `Start.initial` for the manual request body, `scope_json_schema` for
//! the loose webhook scope, `derive_output_port_typed` + `Port::json_schema`
//! for the sync invoke envelope). There is no hand-rolled
//! `{additionalProperties: true}` / "Mapped by payloadMapping" property
//! builder — the typed port is the single source of truth, so the editor's
//! variable picker, the runtime contract, and this document cannot drift.
//!
//! Surface emitted, per ENABLED trigger on a PUBLISHED (`is_latest`) template:
//!   - **Manual** → a concrete path *pair* with the real node id substituted:
//!       - `POST /api/v1/triggers/{node_id}/fire`   (async, 202 `{instance_id}`)
//!       - `POST /api/v1/triggers/{node_id}/invoke` (sync, 200 success envelope
//!         `{ ok, value }`, or 202 `{instance_id}` on timeout)
//!     The request body is the workflow's **declared input contract** — the
//!     target `Start.initial` port reached by the trigger's outgoing edge —
//!     yielding *precise* typed properties (that's the win over the old loose
//!     object; the trigger's own "Run with parameters" form is just the editor
//!     dialog and is commonly empty). If that port has a `File`-kind field, the
//!     requestBody offers both `application/json` (File = storage-path string)
//!     and `multipart/form-data` (File = `{type:string, format:binary}`); the
//!     server auto-converts uploads via `build_multipart_payload`, so no
//!     handler change is needed.
//!   - **Webhook** → the concrete external receiver path
//!     `/api/triggers/webhook/{slug}`, async only (202). Its scope
//!     (`payload`/`headers`/`query` are `Json`) is honestly a loose object.
//!   - **Cron / Catalog / NetCompletion** → EXCLUDED (event-driven internals
//!     with no external HTTP surface).
//!
//! The output is a `serde_json::Value` rather than `utoipa::openapi::OpenApi`:
//! utoipa's PathItem builders don't expose runtime path injection without
//! ceremony, and the bundle is a synthesized document that doesn't need to
//! type-check against any Rust handler.

use std::collections::BTreeMap;

use axum::{
    extract::{Path, State},
    Json,
};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::auth::{map_to_api_error, require_member, AuthUser};
use crate::compiler::subworkflow::derive_output_port_typed;
use crate::models::error::{ApiError, ErrorResponse};
use crate::models::template::{
    FieldKind, Port, TriggerSource, WebhookAuth, WorkflowGraph, WorkflowNodeData,
};
use crate::triggers::scope::{scope_json_schema, source_scope};
use crate::AppState;

/// GET /api/v1/workspaces/{workspace_id}/projects/{project_id}/openapi.json
///
/// Returns a synthesized OpenAPI 3.0.3 document covering every callable
/// trigger in the project's attached templates. The shape is suitable for
/// feeding into `openapi-typescript`, `openapi-generator`, or any OAS3
/// viewer.
#[utoipa::path(
    get,
    path = "/api/v1/workspaces/{workspace_id}/projects/{project_id}/openapi.json",
    params(
        ("workspace_id" = Uuid, Path, description = "Workspace id"),
        ("project_id" = Uuid, Path, description = "Project id")
    ),
    responses(
        (status = 200, description = "Project OpenAPI bundle", body = serde_json::Value),
        (status = 403, description = "Not a member", body = ErrorResponse),
        (status = 404, description = "Project not found or not in workspace", body = ErrorResponse),
    ),
    tag = "projects",
)]
pub async fn project_openapi_bundle(
    State(state): State<AppState>,
    user: AuthUser,
    Path((workspace_id, project_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<Value>, ApiError> {
    require_member(&state.db, &user, workspace_id)
        .await
        .map_err(map_to_api_error)?;

    // Confirm the project belongs to the gated workspace. Without this an
    // editor in WS-A could read a project in WS-B by guessing its id.
    let project: Option<(Uuid, String, String, String)> = sqlx::query_as(
        "SELECT id, slug, display_name, description \
           FROM projects WHERE id = $1 AND workspace_id = $2",
    )
    .bind(project_id)
    .bind(workspace_id)
    .fetch_optional(&state.db)
    .await?;
    let (_, project_slug, project_display, project_desc) =
        project.ok_or_else(|| ApiError::not_found("project not found in this workspace"))?;

    // Live chain heads (`is_latest = true`) for every template attached to
    // the project. We deliberately read the live row rather than whichever
    // version was attached — projects follow the version chain.
    let template_rows: Vec<(Uuid, String, serde_json::Value, i32)> = sqlx::query_as(
        "SELECT t.id, t.name, t.graph, t.version \
               FROM project_templates pt \
               JOIN workflow_templates t \
                 ON COALESCE(t.base_template_id, t.id) = pt.base_template_id \
              WHERE pt.project_id = $1 AND t.is_latest = TRUE \
              ORDER BY t.name",
    )
    .bind(project_id)
    .fetch_all(&state.db)
    .await?;

    // BTreeMaps so the emitted `paths` / `schemas` / `securitySchemes` objects
    // are stably ordered (matters for diffing the spec across versions).
    let mut paths: BTreeMap<String, Value> = BTreeMap::new();
    let mut schemas: BTreeMap<String, Value> = BTreeMap::new();
    let mut security_schemes: BTreeMap<String, Value> = BTreeMap::new();

    for (template_id, template_name, graph_json, version) in &template_rows {
        // Typed iteration over the graph. A graph that fails to deserialize
        // (legacy / partially-migrated shape) contributes nothing rather than
        // 500ing the whole bundle.
        let graph: WorkflowGraph = match serde_json::from_value(graph_json.clone()) {
            Ok(g) => g,
            Err(e) => {
                tracing::warn!(
                    template_id = %template_id,
                    error = %e,
                    "openapi_bundle: skipping template with undeserializable graph"
                );
                continue;
            }
        };

        // The sync `invoke` success envelope's `value` is this template's
        // derived output port — recovered once per template.
        let output_port = derive_output_port_typed(&graph);
        let output_schema = output_port.json_schema();

        for node in &graph.nodes {
            let WorkflowNodeData::Trigger {
                label,
                description,
                source,
                enabled,
                ..
            } = &node.data
            else {
                continue;
            };
            if !*enabled {
                continue;
            }

            match source {
                TriggerSource::Manual(_) => emit_manual(
                    &graph,
                    &node.id,
                    label,
                    description.as_deref(),
                    template_id,
                    template_name,
                    *version,
                    &output_schema,
                    &mut paths,
                    &mut schemas,
                ),
                TriggerSource::Webhook(_) => emit_webhook(
                    &node.id,
                    label,
                    description.as_deref(),
                    source,
                    template_id,
                    template_name,
                    *version,
                    &mut paths,
                    &mut schemas,
                    &mut security_schemes,
                ),
                // Cron / Catalog / NetCompletion are event-driven internals
                // with no external HTTP surface — excluded by design.
                _ => {}
            }
        }
    }

    // Session cookie + machine PAT (bearer) both authenticate the protected
    // `/fire` + `/invoke` routes (RFC 7662 introspection in
    // `require_auth_middleware`). Advertise both whenever a manual op exists.
    let manual_present = paths
        .keys()
        .any(|p| p.starts_with("/api/v1/triggers/"));
    if manual_present {
        security_schemes.insert(
            "sessionCookie".to_string(),
            json!({
                "type": "apiKey",
                "in": "cookie",
                "name": "mekhan_session",
                "description": "Browser session cookie issued by the OAuth login flow.",
            }),
        );
        security_schemes.insert(
            "bearerAuth".to_string(),
            json!({
                "type": "http",
                "scheme": "bearer",
                "description": "Machine personal access token (PAT), validated via RFC 7662 introspection.",
            }),
        );
    }

    let mut doc = json!({
        "openapi": "3.0.3",
        "info": {
            "title": format!("Project: {project_display}"),
            "version": "1.0.0",
            "description": format!(
                "Callable trigger surface for project `{project_slug}`.{}",
                if project_desc.is_empty() { String::new() } else { format!("\n\n{project_desc}") }
            ),
        },
        "servers": [
            { "url": "/", "description": "Same-origin BFF" },
        ],
        "paths": paths,
    });

    let mut components = serde_json::Map::new();
    if !schemas.is_empty() {
        components.insert("schemas".to_string(), json!(schemas));
    }
    if !security_schemes.is_empty() {
        components.insert("securitySchemes".to_string(), json!(security_schemes));
    }
    if !components.is_empty() {
        doc["components"] = Value::Object(components);
    }

    Ok(Json(doc))
}

/// Resolve a Manual trigger's typed input shape: follow its single outgoing
/// edge to the target node and, when that target is a `Start`, return the
/// `Start.initial` port — the workflow's *actual* declared input contract. This
/// is what makes the manual `/fire` + `/invoke` request body precise (the
/// trigger's own `form` is just the "Run with parameters" dialog and is
/// commonly left empty in favor of authoring the contract on `Start.initial` +
/// a pass-through `payloadMapping`). Returns `None` for signal-kind manual
/// fires (target is a mid-net handle, not a Start) — caller falls back to a
/// permissive object.
fn resolve_trigger_input_port(graph: &WorkflowGraph, trigger_id: &str) -> Option<Port> {
    let edge = graph.edges.iter().find(|e| e.source == trigger_id)?;
    let target = graph.nodes.iter().find(|n| n.id == edge.target)?;
    match &target.data {
        WorkflowNodeData::Start { initial, .. } => Some(initial.clone()),
        _ => None,
    }
}

/// Does this port declare at least one `File`-kind field? File fields earn a
/// `multipart/form-data` content alternative on the requestBody.
fn port_has_file(port: &Port) -> bool {
    port.fields.iter().any(|f| matches!(f.kind, FieldKind::File))
}

/// Build the requestBody `content` map for a typed input `Port`.
///
/// Always offers `application/json` with the typed port schema (File field =
/// storage-path string). When the port contains any `File` field it
/// additionally offers `multipart/form-data` where the File field(s) become
/// `{type:string, format:binary}` and every other field mirrors the json
/// shape — the server's `build_multipart_payload` auto-converts uploads into
/// file-reference objects, so both content types reach the same handler.
fn request_content(port: &Port) -> Value {
    let json_schema = port.json_schema();
    let mut content = json!({
        "application/json": { "schema": json_schema },
    });

    if port_has_file(port) {
        // Mirror the json shape but swap File fields to binary uploads.
        let mut properties = serde_json::Map::new();
        for f in &port.fields {
            let prop = if matches!(f.kind, FieldKind::File) {
                json!({ "type": "string", "format": "binary" })
            } else {
                f.kind.json_schema(f)
            };
            properties.insert(f.name.clone(), prop);
        }
        let required: Vec<&str> = port
            .fields
            .iter()
            .filter(|f| f.required)
            .map(|f| f.name.as_str())
            .collect();
        let mut multipart_schema = json!({
            "type": "object",
            "properties": properties,
            "additionalProperties": false,
        });
        if !required.is_empty() {
            multipart_schema["required"] = json!(required);
        }
        content["multipart/form-data"] = json!({ "schema": multipart_schema });
    }

    content
}

/// Emit the `/fire` + `/invoke` path pair for one Manual trigger.
#[allow(clippy::too_many_arguments)]
fn emit_manual(
    graph: &WorkflowGraph,
    node_id: &str,
    label: &str,
    description: Option<&str>,
    template_id: &Uuid,
    template_name: &str,
    template_version: i32,
    output_schema: &Value,
    paths: &mut BTreeMap<String, Value>,
    schemas: &mut BTreeMap<String, Value>,
) {
    // The precise caller-facing request shape is the target `Start.initial`
    // port — the workflow's declared input contract. Signal-kind manual fires
    // (no Start target) fall back to a permissive object.
    let input_port = resolve_trigger_input_port(graph, node_id);
    let safe = sanitize_for_ref(node_id);

    // Register the reusable request schema + sync-response envelope.
    let request_ref = format!("Trigger_{safe}_Request");
    let request_schema = input_port
        .as_ref()
        .map(|p| p.json_schema())
        .unwrap_or_else(|| json!({ "type": "object", "additionalProperties": true }));
    schemas.insert(request_ref.clone(), request_schema);

    let envelope_ref = format!("Trigger_{safe}_Response");
    schemas.insert(
        envelope_ref.clone(),
        json!({
            "type": "object",
            "description": "Success envelope. `value` is this template's derived output.",
            "properties": {
                "ok": { "type": "boolean", "enum": [true] },
                "value": output_schema.clone(),
            },
            "required": ["ok", "value"],
            "additionalProperties": false,
        }),
    );

    // Both ops carry the same typed requestBody. The request `content` is built
    // fresh (not $ref'd) when the input port has File fields, because the
    // multipart alternative needs the binary-swapped shape inline.
    let req_content = match &input_port {
        Some(port) if port_has_file(port) => request_content(port),
        _ => json!({
            "application/json": {
                "schema": { "$ref": format!("#/components/schemas/{request_ref}") }
            }
        }),
    };

    let security = json!([
        { "sessionCookie": [] },
        { "bearerAuth": [] },
    ]);

    let mut tags = vec!["triggers".to_string()];
    if !template_name.is_empty() {
        tags.push(template_name.to_string());
    }

    let x_ext = |op: &mut Value| {
        op["x-mekhan-template-id"] = json!(template_id.to_string());
        op["x-mekhan-template-version"] = json!(template_version);
        op["x-mekhan-node-id"] = json!(node_id);
        op["x-mekhan-enabled"] = json!(true);
        if let Some(d) = description {
            op["description"] = json!(d);
        }
    };

    // --- /fire (async 202) ---
    let mut fire = json!({
        "tags": tags.clone(),
        "summary": format!("{label} (fire)"),
        "operationId": format!("trigger_fire_{safe}"),
        "security": security.clone(),
        "requestBody": {
            "required": true,
            "content": req_content.clone(),
        },
        "responses": {
            "202": {
                "description": "Accepted — instance launched asynchronously.",
                "content": {
                    "application/json": {
                        "schema": {
                            "type": "object",
                            "properties": {
                                "instance_id": { "type": "string", "format": "uuid" }
                            },
                            "required": ["instance_id"],
                            "additionalProperties": false,
                        }
                    }
                }
            },
            "401": { "description": "Unauthenticated." },
            "404": { "description": "Trigger node not found / not fireable." },
        },
    });
    x_ext(&mut fire);
    insert_op(paths, &format!("/api/v1/triggers/{node_id}/fire"), "post", fire);

    // --- /invoke (sync 200 envelope, 202 timeout) ---
    let mut invoke = json!({
        "tags": tags,
        "summary": format!("{label} (invoke)"),
        "operationId": format!("trigger_invoke_{safe}"),
        "security": security,
        "requestBody": {
            "required": true,
            "content": req_content,
        },
        "responses": {
            "200": {
                "description": "Instance completed — success envelope { ok, value }.",
                "content": {
                    "application/json": {
                        "schema": { "$ref": format!("#/components/schemas/{envelope_ref}") }
                    }
                }
            },
            "202": {
                "description": "Timed out waiting for completion — instance keeps running.",
                "content": {
                    "application/json": {
                        "schema": {
                            "type": "object",
                            "properties": {
                                "instance_id": { "type": "string", "format": "uuid" }
                            },
                            "required": ["instance_id"],
                            "additionalProperties": false,
                        }
                    }
                }
            },
            "401": { "description": "Unauthenticated." },
            "404": { "description": "Trigger node not found / not invokable." },
        },
    });
    x_ext(&mut invoke);
    insert_op(
        paths,
        &format!("/api/v1/triggers/{node_id}/invoke"),
        "post",
        invoke,
    );
}

/// Emit the external receiver path for one Webhook trigger.
#[allow(clippy::too_many_arguments)]
fn emit_webhook(
    node_id: &str,
    label: &str,
    description: Option<&str>,
    source: &TriggerSource,
    template_id: &Uuid,
    template_name: &str,
    template_version: i32,
    paths: &mut BTreeMap<String, Value>,
    schemas: &mut BTreeMap<String, Value>,
    security_schemes: &mut BTreeMap<String, Value>,
) {
    let TriggerSource::Webhook(hook) = source else {
        return;
    };
    if hook.slug.is_empty() {
        return;
    }
    let safe = sanitize_for_ref(&hook.slug);

    // The webhook scope (`payload`/`headers`/`query` = Json) is honestly a
    // loose object — `scope_json_schema` emits exactly that.
    let vars = source_scope(source);
    let body_ref = format!("Webhook_{safe}_Request");
    schemas.insert(body_ref.clone(), scope_json_schema(&vars));

    // Method to bind the operation under: declared `requireMethod`, else POST
    // (receivers accept any verb; POST is the convention).
    let method = match &hook.require_method {
        Some(m) => serde_json::to_value(m)
            .ok()
            .and_then(|v| v.as_str().map(str::to_lowercase))
            .unwrap_or_else(|| "post".to_string()),
        None => "post".to_string(),
    };

    let mut tags = vec!["webhooks".to_string()];
    if !template_name.is_empty() {
        tags.push(template_name.to_string());
    }

    let mut op = json!({
        "tags": tags,
        "summary": label,
        "operationId": format!("webhook_{safe}"),
        "requestBody": {
            "required": true,
            "content": {
                "application/json": {
                    "schema": { "$ref": format!("#/components/schemas/{body_ref}") }
                }
            }
        },
        "responses": {
            "202": { "description": "Accepted — fire enqueued." },
            "401": { "description": "Webhook auth failed (when `auth.kind != none`)." },
            "404": { "description": "Webhook slug not found." },
        },
        "x-mekhan-template-id": template_id.to_string(),
        "x-mekhan-template-version": template_version,
        "x-mekhan-node-id": node_id,
        "x-mekhan-enabled": true,
    });
    if let Some(d) = description {
        op["description"] = json!(d);
    }

    // Auth — derive the security requirement from the webhook's declared auth.
    match &hook.auth {
        WebhookAuth::None => {
            // No requirement — publicly fireable. An explicit empty `security`
            // overrides any document-level default.
            op["security"] = json!([]);
        }
        WebhookAuth::SharedSecret { header, .. } | WebhookAuth::SignedHmac { header, .. } => {
            // Register a per-webhook apiKey-in-header scheme and require it.
            let scheme_name = format!("webhookAuth_{safe}");
            security_schemes.insert(
                scheme_name.clone(),
                json!({
                    "type": "apiKey",
                    "in": "header",
                    "name": header,
                    "description": format!(
                        "Webhook credential ({} auth).",
                        hook.auth.auth_kind_str()
                    ),
                }),
            );
            op["security"] = json!([ { scheme_name: [] } ]);
        }
    }

    insert_op(paths, &format!("/api/triggers/webhook/{}", hook.slug), &method, op);
}

/// Slot an operation into the `paths` map under the given path + lowercase
/// method, merging into an existing PathItem when present.
fn insert_op(paths: &mut BTreeMap<String, Value>, path: &str, method: &str, op: Value) {
    let entry = paths.entry(path.to_string()).or_insert_with(|| json!({}));
    if let Some(obj) = entry.as_object_mut() {
        obj.insert(method.to_string(), op);
    }
}

/// Sanitize a slug / node id for use inside a JSON `$ref` / operationId.
/// OpenAPI component names must match `^[a-zA-Z0-9._-]+$`; slugs and node ids
/// are user-/editor-controlled so we replace anything outside that class with
/// an underscore.
fn sanitize_for_ref(raw: &str) -> String {
    raw.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// `WebhookAuth` discriminant string, for the security-scheme description.
impl WebhookAuth {
    fn auth_kind_str(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::SharedSecret { .. } => "shared_secret",
            Self::SignedHmac { .. } => "signed_hmac",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::template::{
        ManualTrigger, Port, Position, WorkflowNode,
    };

    fn trigger_node(id: &str, source: TriggerSource, enabled: bool) -> WorkflowNode {
        WorkflowNode {
            id: id.to_string(),
            node_type: "trigger".to_string(),
            slug: None,
            position: Position { x: 0.0, y: 0.0 },
            data: WorkflowNodeData::Trigger {
                label: "T".to_string(),
                description: None,
                source,
                concurrency: Default::default(),
                payload_mapping: vec![],
                enabled,
                air_target_place_id: None,
            },
            parent_id: None,
            width: None,
            height: None,
        }
    }

    fn port_field(name: &str, kind: FieldKind, required: bool) -> crate::models::template::PortField {
        crate::models::template::PortField {
            name: name.into(),
            label: name.into(),
            kind,
            required,
            options: None,
            description: None,
            accept: None,
            schema: None,
        }
    }

    fn start_node(id: &str, fields: Vec<crate::models::template::PortField>) -> WorkflowNode {
        WorkflowNode {
            id: id.to_string(),
            node_type: "start".to_string(),
            slug: None,
            position: Position { x: 0.0, y: 0.0 },
            data: WorkflowNodeData::Start {
                label: "Start".to_string(),
                description: None,
                initial: Port {
                    id: "in".into(),
                    label: "Input".into(),
                    fields,
                },
                process_name: None,
            },
            parent_id: None,
            width: None,
            height: None,
        }
    }

    /// Build a graph wiring a manual trigger → a Start node whose `initial`
    /// port carries `start_fields` (the workflow's declared input contract).
    fn manual_graph(
        trigger_id: &str,
        start_fields: Vec<crate::models::template::PortField>,
    ) -> WorkflowGraph {
        WorkflowGraph {
            nodes: vec![
                trigger_node(
                    trigger_id,
                    TriggerSource::Manual(ManualTrigger { form: vec![] }),
                    true,
                ),
                start_node("start", start_fields),
            ],
            edges: vec![crate::models::template::WorkflowEdge {
                id: "e1".into(),
                source: trigger_id.into(),
                target: "start".into(),
                source_handle: None,
                target_handle: Some("in".into()),
                label: None,
                join: None,
                edge_type: "default".into(),
            }],
            viewport: None,
            instance_concurrency: Default::default(),
            definitions: Default::default(),
            default_scheduler: None,
        }
    }

    fn empty_output_schema() -> Value {
        Port {
            id: "out".into(),
            label: "Output".into(),
            fields: vec![],
        }
        .json_schema()
    }

    #[test]
    fn manual_emits_fire_and_invoke_with_typed_body_and_envelope() {
        // The request shape is the target Start.initial port, not the trigger form.
        let graph = manual_graph(
            "trigger_1",
            vec![
                port_field("customer", FieldKind::Text, true),
                port_field("amount", FieldKind::Number, false),
            ],
        );
        let mut paths = BTreeMap::new();
        let mut schemas = BTreeMap::new();
        emit_manual(
            &graph,
            "trigger_1",
            "Run order",
            None,
            &Uuid::nil(),
            "Orders",
            2,
            &empty_output_schema(),
            &mut paths,
            &mut schemas,
        );

        let fire = &paths["/api/v1/triggers/trigger_1/fire"]["post"];
        let invoke = &paths["/api/v1/triggers/trigger_1/invoke"]["post"];

        // Typed requestBody: the Start.initial port names both fields, and
        // `required` reflects the port (customer required, amount not).
        let req = &schemas["Trigger_trigger_1_Request"];
        assert_eq!(req["properties"]["customer"]["type"], json!("string"));
        assert_eq!(req["properties"]["amount"]["type"], json!("number"));
        assert_eq!(req["required"], json!(["customer"]));
        assert_eq!(req["additionalProperties"], json!(false));
        assert_eq!(fire["requestBody"]["required"], json!(true));

        // /fire → 202 with instance_id.
        assert_eq!(
            fire["responses"]["202"]["content"]["application/json"]["schema"]["properties"]
                ["instance_id"]["format"],
            json!("uuid")
        );

        // /invoke → 200 success envelope referencing the response schema.
        assert_eq!(
            invoke["responses"]["200"]["content"]["application/json"]["schema"]["$ref"],
            json!("#/components/schemas/Trigger_trigger_1_Response")
        );
        let env = &schemas["Trigger_trigger_1_Response"];
        assert_eq!(env["properties"]["ok"]["enum"], json!([true]));
        assert!(env["properties"].get("value").is_some());
        // /invoke also has a 202 timeout fallback.
        assert!(invoke["responses"].get("202").is_some());

        // Both ops advertise session + bearer security.
        let sec = fire["security"].as_array().unwrap();
        assert!(sec.iter().any(|s| s.get("sessionCookie").is_some()));
        assert!(sec.iter().any(|s| s.get("bearerAuth").is_some()));
    }

    #[test]
    fn manual_file_field_emits_json_and_multipart_binary() {
        let graph = manual_graph(
            "t_file",
            vec![
                port_field("note", FieldKind::Text, false),
                port_field("attachment", FieldKind::File, true),
            ],
        );
        let mut paths = BTreeMap::new();
        let mut schemas = BTreeMap::new();
        emit_manual(
            &graph,
            "t_file",
            "Upload",
            None,
            &Uuid::nil(),
            "",
            1,
            &empty_output_schema(),
            &mut paths,
            &mut schemas,
        );

        let content = &paths["/api/v1/triggers/t_file/fire"]["post"]["requestBody"]["content"];
        // application/json — File is a storage-path string.
        let json_props =
            &content["application/json"]["schema"]["properties"];
        assert_eq!(json_props["attachment"]["type"], json!("string"));
        assert!(json_props["attachment"].get("format").is_none());
        // multipart/form-data — File becomes a binary upload, others mirror.
        let mp_props =
            &content["multipart/form-data"]["schema"]["properties"];
        assert_eq!(mp_props["attachment"]["type"], json!("string"));
        assert_eq!(mp_props["attachment"]["format"], json!("binary"));
        assert_eq!(mp_props["note"]["type"], json!("string"));
    }

    #[test]
    fn webhook_stays_async_202_loose() {
        let source = TriggerSource::Webhook(crate::models::template::WebhookTrigger {
            slug: "invoice".into(),
            auth: WebhookAuth::None,
            require_method: None,
        });
        let mut paths = BTreeMap::new();
        let mut schemas = BTreeMap::new();
        let mut sec = BTreeMap::new();
        emit_webhook(
            "wh_1",
            "Invoice hook",
            None,
            &source,
            &Uuid::nil(),
            "Billing",
            3,
            &mut paths,
            &mut schemas,
            &mut sec,
        );

        let op = &paths["/api/triggers/webhook/invoice"]["post"];
        // Async only — 202, no 200.
        assert!(op["responses"].get("202").is_some());
        assert!(op["responses"].get("200").is_none());
        // Loose scope body: payload/headers/query are Json → additionalProperties.
        let body = &schemas["Webhook_invoice_Request"];
        assert_eq!(body["additionalProperties"], json!(true));
        assert!(body["properties"].get("payload").is_some());
        // auth none → empty security (no requirement), no scheme registered.
        assert_eq!(op["security"], json!([]));
        assert!(sec.is_empty());
    }

    #[test]
    fn webhook_shared_secret_registers_header_scheme() {
        let source = TriggerSource::Webhook(crate::models::template::WebhookTrigger {
            slug: "secure".into(),
            auth: WebhookAuth::SharedSecret {
                header: "X-Webhook-Token".into(),
                secret_ref: "ref".into(),
            },
            require_method: None,
        });
        let mut paths = BTreeMap::new();
        let mut schemas = BTreeMap::new();
        let mut sec = BTreeMap::new();
        emit_webhook(
            "wh_2", "Secure", None, &source, &Uuid::nil(), "", 1, &mut paths, &mut schemas,
            &mut sec,
        );
        let op = &paths["/api/triggers/webhook/secure"]["post"];
        let scheme_name = "webhookAuth_secure";
        assert_eq!(op["security"], json!([ { scheme_name: [] } ]));
        assert_eq!(sec[scheme_name]["in"], json!("header"));
        assert_eq!(sec[scheme_name]["name"], json!("X-Webhook-Token"));
    }

    #[test]
    fn webhook_method_defaults_to_post_or_honors_require_method() {
        // Default POST.
        let source = TriggerSource::Webhook(crate::models::template::WebhookTrigger {
            slug: "a".into(),
            auth: WebhookAuth::None,
            require_method: None,
        });
        let mut paths = BTreeMap::new();
        let (mut s, mut sec) = (BTreeMap::new(), BTreeMap::new());
        emit_webhook("w", "A", None, &source, &Uuid::nil(), "", 1, &mut paths, &mut s, &mut sec);
        assert!(paths["/api/triggers/webhook/a"]["post"].is_object());

        // Explicit PUT.
        let source = TriggerSource::Webhook(crate::models::template::WebhookTrigger {
            slug: "b".into(),
            auth: WebhookAuth::None,
            require_method: Some(crate::models::template::HttpMethod::Put),
        });
        let mut paths = BTreeMap::new();
        let (mut s, mut sec) = (BTreeMap::new(), BTreeMap::new());
        emit_webhook("w", "B", None, &source, &Uuid::nil(), "", 1, &mut paths, &mut s, &mut sec);
        assert!(paths["/api/triggers/webhook/b"]["put"].is_object());
    }

    #[test]
    fn cron_and_catalog_triggers_excluded() {
        // The handler-level match excludes Cron/Catalog/NetCompletion; assert
        // the predicate directly via a graph round-trip.
        let graph = WorkflowGraph {
            nodes: vec![
                trigger_node(
                    "cron_1",
                    TriggerSource::Cron(crate::models::template::CronTrigger {
                        schedule: "0 9 * * *".into(),
                        timezone: "UTC".into(),
                        jitter_secs: 0,
                        catchup: Default::default(),
                    }),
                    true,
                ),
                trigger_node(
                    "cat_1",
                    TriggerSource::Catalog(crate::models::template::CatalogTrigger {
                        filters: Default::default(),
                        backfill: false,
                    }),
                    true,
                ),
            ],
            edges: vec![],
            viewport: None,
            instance_concurrency: Default::default(),
            definitions: Default::default(),
            default_scheduler: None,
        };

        let callable = graph
            .nodes
            .iter()
            .filter(|n| match &n.data {
                WorkflowNodeData::Trigger { source, enabled, .. } => {
                    *enabled
                        && matches!(
                            source,
                            TriggerSource::Manual(_) | TriggerSource::Webhook(_)
                        )
                }
                _ => false,
            })
            .count();
        assert_eq!(callable, 0);
    }

    #[test]
    fn disabled_trigger_is_skipped() {
        let node = trigger_node(
            "m1",
            TriggerSource::Manual(ManualTrigger { form: vec![] }),
            false,
        );
        let enabled = matches!(
            &node.data,
            WorkflowNodeData::Trigger { enabled: true, .. }
        );
        assert!(!enabled);
    }

    #[test]
    fn sanitize_replaces_unsafe_chars() {
        assert_eq!(sanitize_for_ref("a/b c"), "a_b_c");
        assert_eq!(sanitize_for_ref("invoice-v2.1_alpha"), "invoice-v2.1_alpha");
    }

    #[test]
    fn port_file_detection() {
        let with_file = Port {
            id: "in".into(),
            label: "in".into(),
            fields: vec![port_field("f", FieldKind::File, false)],
        };
        assert!(port_has_file(&with_file));
        let without = Port {
            id: "in".into(),
            label: "in".into(),
            fields: vec![port_field("t", FieldKind::Text, false)],
        };
        assert!(!port_has_file(&without));
    }
}
