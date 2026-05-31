//! Per-project OpenAPI bundle — `GET /api/v1/workspaces/{ws}/projects/{p}/openapi.json`.
//!
//! Assembles a synthetic OpenAPI 3.0.3 document covering every webhook
//! trigger advertised by the templates attached to the project. This is
//! Phase B's flagship endpoint — it's *why* projects exist as a primitive.
//! Consumers (SDK generators, API doc viewers) can point at one URL per
//! project and get an addressable, typed catalog of webhook entrypoints
//! without needing to crawl the full mekhan API.
//!
//! Build process:
//!   1. Resolve the project, gate on workspace membership.
//!   2. For each attached `base_template_id`, fetch the latest published
//!      version (the live chain head). Templates that have never published
//!      contribute nothing — their graphs may be drafts.
//!   3. Walk the graph JSONB for `Trigger` nodes whose `source.kind ==
//!      "webhook"`. Each such node becomes one PathItem at
//!      `/api/triggers/webhook/{slug}` keyed by its declared HTTP method
//!      (defaulting to POST when `requireMethod` is unset — the receiver
//!      accepts any verb, but POST is the convention).
//!   4. Emit `components.schemas` entries when the trigger's target fields
//!      are derivable from `interface_json`. When we can't infer a shape,
//!      we still emit the path with a free-form `application/json` body so
//!      the entry is at minimum *callable*.
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
use crate::models::error::{ApiError, ErrorResponse};
use crate::AppState;

/// GET /api/v1/workspaces/{workspace_id}/projects/{project_id}/openapi.json
///
/// Returns a synthesized OpenAPI 3.0.3 document covering every webhook
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
    let template_rows: Vec<(
        Uuid,
        String,
        serde_json::Value,
        Option<serde_json::Value>,
        i32,
    )> = sqlx::query_as(
        "SELECT t.id, t.name, t.graph, t.interface_json, t.version \
               FROM project_templates pt \
               JOIN workflow_templates t \
                 ON COALESCE(t.base_template_id, t.id) = pt.base_template_id \
              WHERE pt.project_id = $1 AND t.is_latest = TRUE \
              ORDER BY t.name",
    )
    .bind(project_id)
    .fetch_all(&state.db)
    .await?;

    // BTreeMap so the emitted `paths` object is stably ordered (matters for
    // diffing the spec across versions).
    let mut paths: BTreeMap<String, Value> = BTreeMap::new();
    let mut schemas: BTreeMap<String, Value> = BTreeMap::new();

    for (template_id, template_name, graph, interface_json, version) in &template_rows {
        let webhooks = extract_webhooks(graph);
        for hook in webhooks {
            let operation = build_webhook_operation(
                &hook,
                template_id,
                template_name,
                *version,
                interface_json.as_ref(),
                &mut schemas,
            );
            let path = format!("/api/triggers/webhook/{}", hook.slug);
            let entry = paths.entry(path).or_insert_with(|| json!({}));
            if let Some(obj) = entry.as_object_mut() {
                obj.insert(hook.method.to_lowercase(), operation);
            }
        }
    }

    let mut doc = json!({
        "openapi": "3.0.3",
        "info": {
            "title": format!("Project: {project_display}"),
            "version": "1.0.0",
            "description": format!(
                "Webhook trigger surface for project `{project_slug}`.{}",
                if project_desc.is_empty() { String::new() } else { format!("\n\n{project_desc}") }
            ),
        },
        "servers": [
            { "url": "/", "description": "Same-origin BFF" },
        ],
        "paths": paths,
    });

    if !schemas.is_empty() {
        doc["components"] = json!({ "schemas": schemas });
    }

    Ok(Json(doc))
}

/// Minimal projection of a `WebhookTrigger` node from the graph JSONB.
#[derive(Debug, Clone)]
struct WebhookSpec {
    /// Node id (used for cross-references + as a fallback schema name).
    node_id: String,
    /// Human-visible label from the trigger node.
    label: String,
    description: Option<String>,
    /// `slug` from `source.slug` — the path tail for the webhook URL.
    slug: String,
    /// HTTP method (UPPERCASE). Defaults to `POST` when `requireMethod` is
    /// absent. Receivers actually accept every method, but for OpenAPI we
    /// need to pick one to bind the operation under.
    method: String,
    /// `source.auth.kind` — `none`, `shared_secret`, `signed_hmac`.
    auth_kind: String,
    /// Header name to send the credential in, when applicable.
    auth_header: Option<String>,
    /// `payloadMapping[*].targetField` — the *named* fields the trigger
    /// expects to find on the inbound payload (after `expression` projection).
    /// These are the named keys that will be most useful to API consumers.
    target_fields: Vec<String>,
    /// Owning template name for OpenAPI tags / operationId.
    enabled: bool,
}

/// Walk a graph JSONB and extract every webhook trigger spec.
fn extract_webhooks(graph: &Value) -> Vec<WebhookSpec> {
    let mut out = Vec::new();
    let Some(nodes) = graph.get("nodes").and_then(|n| n.as_array()) else {
        return out;
    };
    for node in nodes {
        if node.get("node_type").and_then(|v| v.as_str()) != Some("trigger") {
            continue;
        }
        let data = match node.get("data") {
            Some(d) => d,
            None => continue,
        };
        let source = match data.get("source") {
            Some(s) => s,
            None => continue,
        };
        if source.get("kind").and_then(|v| v.as_str()) != Some("webhook") {
            continue;
        }
        let slug = match source.get("slug").and_then(|v| v.as_str()) {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => continue,
        };
        let label = data
            .get("label")
            .and_then(|v| v.as_str())
            .unwrap_or("Webhook trigger")
            .to_string();
        let description = data
            .get("description")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let method = source
            .get("requireMethod")
            .and_then(|v| v.as_str())
            .map(str::to_uppercase)
            .unwrap_or_else(|| "POST".to_string());
        let auth = source.get("auth");
        let auth_kind = auth
            .and_then(|a| a.get("kind"))
            .and_then(|v| v.as_str())
            .unwrap_or("none")
            .to_string();
        let auth_header = auth
            .and_then(|a| a.get("header"))
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let target_fields = data
            .get("payloadMapping")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|m| m.get("targetField").and_then(|f| f.as_str()))
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        let enabled = data
            .get("enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let node_id = node
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        out.push(WebhookSpec {
            node_id,
            label,
            description,
            slug,
            method,
            auth_kind,
            auth_header,
            target_fields,
            enabled,
        });
    }
    out
}

/// Build the OpenAPI operation object for one webhook + register any
/// schemas it needs in `schemas`. Returns the operation Value to be slotted
/// into the parent paths map.
fn build_webhook_operation(
    hook: &WebhookSpec,
    template_id: &Uuid,
    template_name: &str,
    template_version: i32,
    interface_json: Option<&Value>,
    schemas: &mut BTreeMap<String, Value>,
) -> Value {
    // Derive the request body schema. If the trigger declares
    // `payloadMapping` target fields we can at least name them as required
    // properties; otherwise we fall back to a free-form object.
    let body_schema_name = format!("Webhook_{}", sanitize_for_ref(&hook.slug));
    let body_schema = if hook.target_fields.is_empty() {
        json!({
            "type": "object",
            "description": "Free-form JSON payload — the trigger's `payloadMapping` projects fields via Rhai expressions.",
            "additionalProperties": true,
        })
    } else {
        let mut props = serde_json::Map::new();
        for field in &hook.target_fields {
            props.insert(
                field.clone(),
                json!({ "description": format!("Mapped by payloadMapping → {field}") }),
            );
        }
        json!({
            "type": "object",
            "properties": props,
            "additionalProperties": true,
            "description": "Hinted fields are those referenced by the trigger's payloadMapping; additional properties are forwarded to Rhai expressions.",
        })
    };
    schemas.insert(body_schema_name.clone(), body_schema);

    let mut tags = vec!["webhooks".to_string()];
    if !template_name.is_empty() {
        tags.push(template_name.to_string());
    }

    let mut op = json!({
        "tags": tags,
        "summary": hook.label.clone(),
        "operationId": format!("webhook_{}", sanitize_for_ref(&hook.slug)),
        "requestBody": {
            "required": true,
            "content": {
                "application/json": {
                    "schema": { "$ref": format!("#/components/schemas/{}", body_schema_name) }
                }
            }
        },
        "responses": {
            "202": {
                "description": "Accepted — instance launched / fire enqueued.",
            },
            "401": { "description": "Webhook auth failed (when `auth.kind != none`)." },
            "404": { "description": "Webhook slug not found." },
        },
        "x-mekhan-template-id": template_id.to_string(),
        "x-mekhan-template-version": template_version,
        "x-mekhan-node-id": hook.node_id,
        "x-mekhan-enabled": hook.enabled,
    });

    if let Some(desc) = &hook.description {
        op["description"] = json!(desc);
    }

    // Auth — emit a security requirement when the trigger demands one.
    // We don't add `securitySchemes` to the bundle because the credential
    // is keyed by `secret_ref` — the consumer registers it out-of-band.
    if hook.auth_kind != "none" {
        if let Some(header) = &hook.auth_header {
            op["parameters"] = json!([
                {
                    "in": "header",
                    "name": header,
                    "required": true,
                    "schema": { "type": "string" },
                    "description": format!("Webhook credential ({} auth).", hook.auth_kind),
                }
            ]);
        }
    }

    // If interface_json gives us per-node target-port schema entries we can
    // wire a more accurate schema. This is best-effort — the registry's
    // shape is published-format-versioned and we'd rather degrade than
    // crash on a shape we don't recognize.
    if let Some(iface) = interface_json {
        if let Some(node_iface) = iface.as_object().and_then(|m| m.get(&hook.node_id)) {
            op["x-mekhan-node-interface"] = node_iface.clone();
        }
    }

    op
}

/// Sanitize a slug for use inside a JSON `$ref` / operationId. OpenAPI
/// component names must match `^[a-zA-Z0-9._-]+$`; webhook slugs are
/// already user-controlled so we replace anything outside that class with
/// an underscore.
fn sanitize_for_ref(slug: &str) -> String {
    slug.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn graph_with_webhook(slug: &str, method: Option<&str>) -> Value {
        let mut source = json!({
            "kind": "webhook",
            "slug": slug,
            "auth": { "kind": "none" },
        });
        if let Some(m) = method {
            source["requireMethod"] = json!(m);
        }
        json!({
            "nodes": [{
                "id": "trigger_1",
                "node_type": "trigger",
                "data": {
                    "type": "trigger",
                    "label": "Test webhook",
                    "source": source,
                    "payloadMapping": [
                        { "targetField": "invoice_id", "expression": "payload.id" },
                        { "targetField": "amount", "expression": "payload.amount" }
                    ],
                    "enabled": true
                }
            }]
        })
    }

    #[test]
    fn extracts_webhook_with_explicit_method() {
        let g = graph_with_webhook("invoice", Some("PUT"));
        let webhooks = extract_webhooks(&g);
        assert_eq!(webhooks.len(), 1);
        assert_eq!(webhooks[0].slug, "invoice");
        assert_eq!(webhooks[0].method, "PUT");
        assert_eq!(webhooks[0].target_fields.len(), 2);
        assert!(webhooks[0].enabled);
    }

    #[test]
    fn webhook_method_defaults_to_post() {
        let g = graph_with_webhook("ping", None);
        let webhooks = extract_webhooks(&g);
        assert_eq!(webhooks[0].method, "POST");
    }

    #[test]
    fn non_webhook_triggers_filtered() {
        let g = json!({
            "nodes": [
                {
                    "id": "t1",
                    "node_type": "trigger",
                    "data": {
                        "source": { "kind": "cron", "schedule": "0 * * * *" }
                    }
                },
                {
                    "id": "t2",
                    "node_type": "automated",
                    "data": {}
                },
            ]
        });
        assert!(extract_webhooks(&g).is_empty());
    }

    #[test]
    fn empty_graph_returns_no_webhooks() {
        assert!(extract_webhooks(&json!({})).is_empty());
        assert!(extract_webhooks(&json!({ "nodes": [] })).is_empty());
    }

    #[test]
    fn sanitize_replaces_unsafe_chars() {
        assert_eq!(sanitize_for_ref("a/b c"), "a_b_c");
        assert_eq!(sanitize_for_ref("invoice-v2.1_alpha"), "invoice-v2.1_alpha");
    }

    #[test]
    fn build_operation_emits_required_body_and_auth_header() {
        let hook = WebhookSpec {
            node_id: "trigger_1".into(),
            label: "Webhook".into(),
            description: Some("Receives invoice events".into()),
            slug: "invoice".into(),
            method: "POST".into(),
            auth_kind: "shared_secret".into(),
            auth_header: Some("X-Webhook-Token".into()),
            target_fields: vec!["invoice_id".into()],
            enabled: true,
        };
        let mut schemas = BTreeMap::new();
        let op = build_webhook_operation(&hook, &Uuid::nil(), "Invoices", 3, None, &mut schemas);
        assert_eq!(op["requestBody"]["required"], true);
        assert_eq!(op["x-mekhan-template-version"], 3);
        let params = op["parameters"].as_array().expect("auth param emitted");
        assert_eq!(params[0]["name"], "X-Webhook-Token");
        assert!(schemas.contains_key("Webhook_invoice"));
    }

    #[test]
    fn build_operation_no_auth_param_for_none() {
        let hook = WebhookSpec {
            node_id: "trigger_1".into(),
            label: "Webhook".into(),
            description: None,
            slug: "ping".into(),
            method: "POST".into(),
            auth_kind: "none".into(),
            auth_header: None,
            target_fields: vec![],
            enabled: true,
        };
        let mut schemas = BTreeMap::new();
        let op = build_webhook_operation(&hook, &Uuid::nil(), "", 1, None, &mut schemas);
        assert!(op.get("parameters").is_none());
        // Free-form body when no target fields are declared.
        assert_eq!(schemas["Webhook_ping"]["additionalProperties"], true);
    }
}
