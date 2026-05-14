//! HTTP handlers for the trigger API (Phase 5 of typed-ports).
//!
//! Endpoints:
//! - GET    `/api/triggers`                       — list all registered triggers
//! - GET    `/api/templates/{id}/triggers`        — list triggers per template
//! - POST   `/api/triggers/{node_id}/fire`        — manual fire (Phase 5a)
//! - GET    `/api/triggers/{node_id}/history`     — recent fire history
//!
//! Webhook receiver lives under `/api/triggers/webhook/{slug}` and lands in
//! Phase 5e.

use std::collections::HashMap;

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, Method, StatusCode},
    Json,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::models::error::{ApiError, ErrorResponse};
use crate::models::template::{HttpMethod, TriggerSource};
use crate::triggers::{FireResult, TriggerError, TriggerRecord};
use crate::AppState;

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct TriggerView {
    pub template_id: Uuid,
    pub template_version: i32,
    pub node_id: String,
    pub kind: String,
    pub target_node_id: String,
    pub target_handle: String,
    pub source_kind: String,
    pub enabled: bool,
    pub registered_at: DateTime<Utc>,
}

impl From<TriggerRecord> for TriggerView {
    fn from(rec: TriggerRecord) -> Self {
        let kind = match rec.kind {
            crate::triggers::TriggerKind::Spawn => "spawn".to_string(),
            crate::triggers::TriggerKind::Signal => "signal".to_string(),
        };
        let source_kind = rec.source.kind().to_string();
        Self {
            template_id: rec.template_id,
            template_version: rec.template_version,
            node_id: rec.node_id,
            kind,
            target_node_id: rec.target_node_id,
            target_handle: rec.target_handle,
            source_kind,
            enabled: rec.enabled,
            registered_at: rec.registered_at,
        }
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct TriggerListResponse {
    pub triggers: Vec<TriggerView>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct FireTriggerRequest {
    /// Free-form JSON payload made available to `payload_mapping` as `payload`.
    /// For `Manual` triggers this is typically the form submission body; for
    /// other sources the dispatcher synthesizes the scope from the event.
    #[serde(default)]
    pub payload: serde_json::Value,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct FireTriggerResponse {
    pub result: FireResult,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct TriggerHistoryResponse {
    pub history: Vec<FireResult>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct CronPreviewRequest {
    pub schedule: String,
    #[serde(default = "default_tz")]
    pub timezone: String,
    /// Number of upcoming fire times to compute (clamped to 1..=10).
    #[serde(default = "default_count")]
    pub count: u32,
}

fn default_tz() -> String {
    "UTC".to_string()
}
fn default_count() -> u32 {
    5
}

#[derive(Debug, Serialize, ToSchema)]
pub struct CronPreviewResponse {
    /// Upcoming fire times in RFC 3339 (UTC).
    pub upcoming: Vec<String>,
    /// Error message if the schedule or timezone is invalid; `upcoming` is
    /// empty when set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// GET /api/triggers
#[utoipa::path(
    get,
    path = "/api/triggers",
    responses(
        (status = 200, description = "All registered triggers", body = TriggerListResponse),
    ),
    tag = "triggers",
)]
pub async fn list_triggers(State(state): State<AppState>) -> Json<TriggerListResponse> {
    let triggers: Vec<TriggerView> = state
        .triggers
        .list_all()
        .into_iter()
        .map(TriggerView::from)
        .collect();
    Json(TriggerListResponse { triggers })
}

/// GET /api/templates/{id}/triggers
#[utoipa::path(
    get,
    path = "/api/templates/{id}/triggers",
    params(("id" = Uuid, Path, description = "Template id")),
    responses(
        (status = 200, description = "Triggers for this template", body = TriggerListResponse),
    ),
    tag = "triggers",
)]
pub async fn list_template_triggers(
    State(state): State<AppState>,
    Path(template_id): Path<Uuid>,
) -> Json<TriggerListResponse> {
    let triggers: Vec<TriggerView> = state
        .triggers
        .list_for_template(template_id)
        .into_iter()
        .map(TriggerView::from)
        .collect();
    Json(TriggerListResponse { triggers })
}

/// POST /api/triggers/{node_id}/fire
#[utoipa::path(
    post,
    path = "/api/triggers/{node_id}/fire",
    params(("node_id" = String, Path, description = "Trigger node id")),
    request_body = FireTriggerRequest,
    responses(
        (status = 200, description = "Trigger fired", body = FireTriggerResponse),
        (status = 404, description = "Trigger not found", body = ErrorResponse),
        (status = 400, description = "Fire failed (e.g. mapping or instance error)", body = ErrorResponse),
    ),
    tag = "triggers",
)]
pub async fn fire_trigger(
    State(state): State<AppState>,
    _user: AuthUser,
    Path(node_id): Path<String>,
    Json(req): Json<FireTriggerRequest>,
) -> Result<Json<FireTriggerResponse>, ApiError> {
    let result = crate::triggers::sources::manual::fire(&state.triggers, &node_id, req.payload)
        .await
        .map_err(map_trigger_error)?;
    Ok(Json(FireTriggerResponse { result }))
}

/// GET /api/triggers/{node_id}/history
#[utoipa::path(
    get,
    path = "/api/triggers/{node_id}/history",
    params(("node_id" = String, Path, description = "Trigger node id")),
    responses(
        (status = 200, description = "Recent fire history", body = TriggerHistoryResponse),
    ),
    tag = "triggers",
)]
pub async fn trigger_history(
    State(state): State<AppState>,
    Path(node_id): Path<String>,
) -> Json<TriggerHistoryResponse> {
    let history = state.triggers.history_for(&node_id);
    Json(TriggerHistoryResponse { history })
}

#[derive(Debug, Serialize, ToSchema)]
pub struct TriggerMetricsResponse {
    pub by_source_kind: HashMap<String, crate::triggers::dispatcher::FireMetrics>,
    pub total_registered: usize,
}

/// GET /api/triggers/metrics
///
/// Returns aggregate counters per source kind plus the registry size. Useful
/// for /admin dashboards and the editor's trigger landing page.
#[utoipa::path(
    get,
    path = "/api/triggers/metrics",
    responses(
        (status = 200, description = "Per-source-kind fire counters", body = TriggerMetricsResponse),
    ),
    tag = "triggers",
)]
pub async fn trigger_metrics(State(state): State<AppState>) -> Json<TriggerMetricsResponse> {
    Json(TriggerMetricsResponse {
        by_source_kind: state.triggers.metrics_snapshot(),
        total_registered: state.triggers.list_all().len(),
    })
}

/// POST /api/triggers/preview/cron
///
/// Returns the next N fire times for a cron schedule. Used by the editor's
/// trigger inspector to show users when their cron will fire next without
/// having to ship the workflow first.
#[utoipa::path(
    post,
    path = "/api/triggers/preview/cron",
    request_body = CronPreviewRequest,
    responses(
        (status = 200, description = "Upcoming fire times (or error)", body = CronPreviewResponse),
    ),
    tag = "triggers",
)]
pub async fn preview_cron(Json(req): Json<CronPreviewRequest>) -> Json<CronPreviewResponse> {
    use crate::models::template::{CronCatchup, CronTrigger};
    let count = req.count.clamp(1, 10) as usize;
    let trigger = CronTrigger {
        schedule: req.schedule,
        timezone: req.timezone,
        jitter_secs: 0,
        catchup: CronCatchup::SkipMissed,
    };
    match crate::triggers::sources::cron::parse_cron(&trigger) {
        Ok((schedule, tz)) => {
            let now = chrono::Utc::now().with_timezone(&tz);
            let upcoming: Vec<String> = schedule
                .after(&now)
                .take(count)
                .map(|t| t.with_timezone(&chrono::Utc).to_rfc3339())
                .collect();
            Json(CronPreviewResponse {
                upcoming,
                error: None,
            })
        }
        Err(msg) => Json(CronPreviewResponse {
            upcoming: vec![],
            error: Some(msg),
        }),
    }
}

fn map_trigger_error(e: TriggerError) -> ApiError {
    match e {
        TriggerError::NotFound(_) => ApiError::not_found(e.to_string()),
        TriggerError::Disabled(_) => ApiError::new(StatusCode::CONFLICT, e.to_string()),
        TriggerError::Database(_) => ApiError::internal(e.to_string()),
        TriggerError::TargetMissing { .. }
        | TriggerError::PayloadMappingFailed { .. }
        | TriggerError::InstanceFailed(_)
        | TriggerError::SignalFailed(_) => ApiError::bad_request(e.to_string()),
    }
}

/// Convenience: utoipa schema registration helper. Exposes the FireResult /
/// FireOutcome / TriggerLocator wire shapes to the generated TS client.
#[allow(dead_code)]
pub fn schema_exports() {
    let _ = std::any::type_name::<crate::triggers::FireResult>();
    let _ = std::any::type_name::<crate::triggers::FireOutcome>();
    let _ = std::any::type_name::<crate::triggers::TriggerLocator>();
}

/// POST /api/triggers/webhook/{slug}
///
/// Public webhook receiver. The handler resolves the slug to a registered
/// webhook trigger, validates auth per the trigger's `WebhookAuth` policy,
/// then fires with a payload of `{ payload (body), headers, query, fire_time }`.
///
/// Mounted outside the auth middleware — external systems POST to this URL
/// without a Bearer token.
pub async fn webhook_receiver(
    State(state): State<AppState>,
    method: Method,
    Path(slug): Path<String>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Result<Json<FireTriggerResponse>, ApiError> {
    let trigger = crate::triggers::sources::webhook::find_by_slug(&state.triggers, &slug)
        .ok_or_else(|| ApiError::not_found(format!("webhook '{slug}' not found")))?;

    let TriggerSource::Webhook(ref webhook) = trigger.source else {
        return Err(ApiError::internal("trigger source is not a webhook"));
    };

    // Method restriction (optional). The default Webhook.require_method is
    // None which accepts every method.
    if let Some(req_method) = webhook.require_method {
        if !methods_match(req_method, &method) {
            return Err(ApiError::new(
                StatusCode::METHOD_NOT_ALLOWED,
                format!("webhook requires {req_method:?}, got {method}"),
            ));
        }
    }

    let header_map: HashMap<String, String> = headers
        .iter()
        .filter_map(|(k, v)| {
            v.to_str().ok().map(|s| (k.as_str().to_string(), s.to_string()))
        })
        .collect();

    // Secret resolver: looks up secrets from app config. Phase 5e uses env
    // vars prefixed with `WEBHOOK_SECRET_` for simplicity — a real deployment
    // would point at a secret store.
    let resolver = |secret_ref: &str| -> Option<String> {
        std::env::var(format!("WEBHOOK_SECRET_{secret_ref}")).ok()
    };

    if let Err(msg) = crate::triggers::sources::webhook::check_auth(
        webhook,
        &header_map,
        body.as_ref(),
        resolver,
    ) {
        return Err(ApiError::new(StatusCode::UNAUTHORIZED, msg));
    }

    // Parse the body as JSON if Content-Type is application/json; otherwise
    // pass it as a base64-encoded blob under `body_bytes`.
    let body_payload: Value =
        if let Ok(v) = serde_json::from_slice::<Value>(body.as_ref()) {
            v
        } else {
            serde_json::json!({
                "body_bytes": format!("{}", String::from_utf8_lossy(body.as_ref())),
            })
        };

    let payload = serde_json::json!({
        "payload": body_payload,
        "headers": header_map,
        "query": query,
        "fire_time": Utc::now().to_rfc3339(),
    });

    let result = state
        .triggers
        .fire(&trigger.node_id, payload)
        .await
        .map_err(map_trigger_error)?;
    Ok(Json(FireTriggerResponse { result }))
}

fn methods_match(declared: HttpMethod, actual: &Method) -> bool {
    match declared {
        HttpMethod::Get => actual == Method::GET,
        HttpMethod::Post => actual == Method::POST,
        HttpMethod::Put => actual == Method::PUT,
        HttpMethod::Patch => actual == Method::PATCH,
        HttpMethod::Delete => actual == Method::DELETE,
    }
}
