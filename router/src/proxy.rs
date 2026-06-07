//! The OpenAI-compatible chat-completions proxy — the router's hot path.
//!
//! Flow: auth → peek `model`/`stream` → select replica (eligibility +
//! residency hard-filter) → admit (per-replica semaphore) or `429` → register
//! cancellation → forward to `{replica}/v1/chat/completions` → stream the
//! response back (buffered for `stream:false`, SSE passthrough for
//! `stream:true`) → emit one metering record on the terminal.
//!
//! Streaming mechanics (request body buffered for the peek, response streamed)
//! are modeled on `service/src/petri/proxy.rs`. **Inference never crosses the
//! engine net.** The admission permit is held for the entire response lifetime
//! and released on completion, client disconnect (the response future is
//! dropped), or cancellation.

use std::sync::Arc;
use std::sync::OnceLock;

use axum::{
    body::{Body, Bytes},
    extract::State,
    http::{header, HeaderMap, HeaderName, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use chrono::Utc;
use futures::StreamExt;
use inference_core::{PeekChatRequest, Usage};
use reqwest::Client;
use serde_json::json;

use crate::admission;
use crate::auth::AuthConfig;
use crate::cancel::{self, CancellationRegistry, DeregisterGuard};
use crate::metering::{self, MeterContext, MeterStatus};
use crate::metrics::Metrics;
use crate::routing::{ReplicaTable, RouteError};

/// Non-standard status for a request cancelled before its response started.
const CLIENT_CANCELLED: u16 = 499;
const REQUEST_ID_HEADER: &str = "x-inference-request-id";

#[derive(Clone)]
pub struct RouterCtx {
    pub table: Arc<ReplicaTable>,
    pub cancels: CancellationRegistry,
    pub auth: Arc<AuthConfig>,
    pub nats: Option<async_nats::Client>,
    pub metrics: Arc<Metrics>,
}

fn http_client() -> Client {
    static CLIENT: OnceLock<Client> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            Client::builder()
                // SSE streams idle between tokens; don't time them out.
                .pool_idle_timeout(None)
                .build()
                .expect("router reqwest client")
        })
        .clone()
}

/// `POST /v1/chat/completions`.
pub async fn chat_completions(
    State(ctx): State<RouterCtx>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    Metrics::inc(&ctx.metrics.requests_total);

    // 1. Auth (Bearer / dev-noop) + identity-header capture.
    let ident = match ctx.auth.authenticate(&headers) {
        Ok(i) => i,
        Err(e) => return error_json(StatusCode::UNAUTHORIZED, &e.to_string()),
    };

    // 2. Peek `model` + `stream` from the (otherwise opaque) body.
    let peek: PeekChatRequest = match serde_json::from_slice(&body) {
        Ok(p) => p,
        Err(e) => {
            return error_json(
                StatusCode::BAD_REQUEST,
                &format!("invalid chat-completions body: {e}"),
            )
        }
    };
    let required_zone = header_str(&headers, "x-residency-zone");

    // 3. Select a replica — eligibility + GDPR residency hard-filter.
    let replica = match ctx
        .table
        .select(&peek.model, required_zone.as_deref())
        .await
    {
        Ok(r) => r,
        Err(RouteError::NoReplica(model)) => {
            // P4-L2 scale-FROM-zero signal: demand for a model with no live
            // replica (e.g. a `scale_to_zero` policy that has scaled to 0).
            ctx.metrics.inc_starved(&model);
            return error_json(
                StatusCode::SERVICE_UNAVAILABLE,
                &format!("no live replica serves model `{model}`"),
            );
        }
        Err(e @ RouteError::ResidencyUnsatisfiable { .. }) => {
            // Fail closed — never cross-zone, never external auto-offload.
            return error_json(StatusCode::UNPROCESSABLE_ENTITY, &e.to_string());
        }
    };

    // 4. Admit against the replica's per-engine slot budget.
    let permit = match admission::try_admit(&replica.sem) {
        Some(p) => p,
        None => {
            Metrics::inc(&ctx.metrics.rejected_429_total);
            // P4-L2 saturation-demand signal: live replicas exist but are all
            // full — the model wants MORE capacity.
            ctx.metrics.inc_starved(&peek.model);
            return too_many_requests(&peek.model);
        }
    };

    // 5. Request id + cancellation registration (RAII deregister).
    let request_id = ident
        .request_id
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let token = ctx.cancels.register(&request_id);
    let guard = DeregisterGuard::new(ctx.cancels.clone(), request_id.clone());

    let meter = MeterContext {
        request_id: request_id.clone(),
        tenant: ident.tenant,
        instance_id: ident.instance_id,
        step_id: ident.step_id,
        model: peek.model.clone(),
        replica_id: replica.id.clone(),
        replica_base_url: replica.base_url.clone(),
        residency_zone: replica.residency_zone.clone(),
        slo_tier: ident.slo_tier,
        started_at: Utc::now(),
    };

    // 6. Forward to the upstream replica, racing the connect against cancel.
    let url = format!("{}/v1/chat/completions", replica.base_url);
    let mut req = http_client()
        .post(&url)
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(key) = &replica.api_key {
        req = req.header(header::AUTHORIZATION, format!("Bearer {key}"));
    }
    let send = req.body(body.clone()).send();

    let upstream = tokio::select! {
        biased;
        _ = token.cancelled() => {
            Metrics::inc(&ctx.metrics.cancelled_total);
            let rec = meter.finish(None, MeterStatus::Cancelled);
            ctx.metrics.observe_record(&rec);
            metering::publish_meter(&ctx.nats, &rec).await;
            if let Some(c) = &ctx.nats {
                cancel::publish_cancelled(c, &request_id).await;
            }
            drop(permit);
            drop(guard);
            return cancelled_response(&request_id);
        }
        res = send => match res {
            Ok(u) => u,
            Err(e) => {
                Metrics::inc(&ctx.metrics.upstream_error_total);
                let rec = meter.finish(None, MeterStatus::UpstreamError);
                ctx.metrics.observe_record(&rec);
                metering::publish_meter(&ctx.nats, &rec).await;
                drop(permit);
                drop(guard);
                return error_json(StatusCode::BAD_GATEWAY, &format!("upstream unreachable: {e}"));
            }
        }
    };

    let status =
        StatusCode::from_u16(upstream.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let upstream_headers = upstream.headers().clone();

    if peek.stream {
        streaming_response(
            ctx,
            upstream,
            status,
            upstream_headers,
            permit,
            guard,
            token,
            meter,
            request_id,
        )
    } else {
        buffered_response(
            ctx,
            upstream,
            status,
            upstream_headers,
            permit,
            guard,
            token,
            meter,
            request_id,
        )
        .await
    }
}

#[allow(clippy::too_many_arguments)]
async fn buffered_response(
    ctx: RouterCtx,
    upstream: reqwest::Response,
    status: StatusCode,
    upstream_headers: reqwest::header::HeaderMap,
    permit: tokio::sync::OwnedSemaphorePermit,
    guard: DeregisterGuard,
    token: tokio_util::sync::CancellationToken,
    meter: MeterContext,
    request_id: String,
) -> Response {
    let bytes = tokio::select! {
        biased;
        _ = token.cancelled() => {
            // Dropping `upstream` closes the connection → upstream aborts.
            Metrics::inc(&ctx.metrics.cancelled_total);
            let rec = meter.finish(None, MeterStatus::Cancelled);
            ctx.metrics.observe_record(&rec);
            metering::publish_meter(&ctx.nats, &rec).await;
            if let Some(c) = &ctx.nats {
                cancel::publish_cancelled(c, &request_id).await;
            }
            drop(permit);
            drop(guard);
            return cancelled_response(&request_id);
        }
        body = upstream.bytes() => match body {
            Ok(b) => b,
            Err(e) => {
                Metrics::inc(&ctx.metrics.upstream_error_total);
                let rec = meter.finish(None, MeterStatus::UpstreamError);
                ctx.metrics.observe_record(&rec);
                metering::publish_meter(&ctx.nats, &rec).await;
                drop(permit);
                drop(guard);
                return error_json(StatusCode::BAD_GATEWAY, &format!("upstream body error: {e}"));
            }
        }
    };

    let usage = Usage::from_response_bytes(&bytes);
    Metrics::inc(&ctx.metrics.completed_total);
    let rec = meter.finish(usage, MeterStatus::Completed);
    ctx.metrics.observe_record(&rec);
    metering::publish_meter(&ctx.nats, &rec).await;
    drop(permit);
    drop(guard);

    build_proxied_response(status, &upstream_headers, Body::from(bytes), &request_id)
}

#[allow(clippy::too_many_arguments)]
fn streaming_response(
    ctx: RouterCtx,
    upstream: reqwest::Response,
    status: StatusCode,
    upstream_headers: reqwest::header::HeaderMap,
    permit: tokio::sync::OwnedSemaphorePermit,
    guard: DeregisterGuard,
    token: tokio_util::sync::CancellationToken,
    meter: MeterContext,
    request_id: String,
) -> Response {
    let nats = ctx.nats.clone();
    let metrics = ctx.metrics.clone();
    let id_for_header = request_id.clone();

    let stream = async_stream::stream! {
        // Hold the permit + deregister guard for the whole stream lifetime;
        // both drop here on completion, cancel, OR client disconnect (the
        // body future is dropped) — which is what releases the slot.
        let _permit = permit;
        let _guard = guard;
        let mut upstream_stream = upstream.bytes_stream();
        let mut last_usage: Option<Usage> = None;
        let mut cancelled = false;

        loop {
            tokio::select! {
                biased;
                _ = token.cancelled() => { cancelled = true; break; }
                chunk = upstream_stream.next() => match chunk {
                    Some(Ok(bytes)) => {
                        if let Some(u) = Usage::scan_sse_chunk(&bytes) {
                            last_usage = Some(u);
                        }
                        yield Ok::<_, std::io::Error>(bytes);
                    }
                    Some(Err(e)) => {
                        Metrics::inc(&metrics.upstream_error_total);
                        yield Err(std::io::Error::other(e));
                        break;
                    }
                    None => break,
                }
            }
        }

        // Terminal disposition. Dropping `upstream_stream` (loop exit) closes
        // the connection so a cancelled upstream aborts server-side.
        if cancelled {
            Metrics::inc(&metrics.cancelled_total);
            let rec = meter.finish(last_usage, MeterStatus::Cancelled);
            metrics.observe_record(&rec);
            metering::publish_meter(&nats, &rec).await;
            if let Some(c) = &nats {
                cancel::publish_cancelled(c, &request_id).await;
            }
        } else {
            Metrics::inc(&metrics.completed_total);
            let rec = meter.finish(last_usage, MeterStatus::Completed);
            metrics.observe_record(&rec);
            metering::publish_meter(&nats, &rec).await;
        }
    };

    build_proxied_response(
        status,
        &upstream_headers,
        Body::from_stream(stream),
        &id_for_header,
    )
}

/// `GET /v1/models` — OpenAI-compatible list of the live routed set.
pub async fn list_models(State(ctx): State<RouterCtx>) -> Json<serde_json::Value> {
    let data: Vec<serde_json::Value> = ctx
        .table
        .live_model_ids()
        .await
        .into_iter()
        .map(|id| json!({"id": id, "object": "model", "owned_by": "aithericon"}))
        .collect();
    Json(json!({"object": "list", "data": data}))
}

/// `GET /metrics` — Prometheus exposition.
pub async fn metrics_handler(State(ctx): State<RouterCtx>) -> Response {
    let replicas = ctx.table.snapshot().await;
    let body = ctx.metrics.render(&replicas);
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/plain; version=0.0.4")],
        body,
    )
        .into_response()
}

/// `GET /healthz`.
pub async fn healthz() -> Response {
    (StatusCode::OK, "ok").into_response()
}

// ---------------------------------------------------------------------------
// Response helpers
// ---------------------------------------------------------------------------

fn build_proxied_response(
    status: StatusCode,
    upstream_headers: &reqwest::header::HeaderMap,
    body: Body,
    request_id: &str,
) -> Response {
    let mut resp = Response::new(body);
    *resp.status_mut() = status;
    for (k, v) in upstream_headers.iter() {
        // Skip hop-by-hop + content-length (we re-frame the body / stream).
        if is_hop_by_hop(k.as_str()) || k.as_str().eq_ignore_ascii_case("content-length") {
            continue;
        }
        if let (Ok(name), Ok(val)) = (
            HeaderName::from_bytes(k.as_str().as_bytes()),
            HeaderValue::from_bytes(v.as_bytes()),
        ) {
            resp.headers_mut().append(name, val);
        }
    }
    if let Ok(val) = HeaderValue::from_str(request_id) {
        resp.headers_mut().insert(REQUEST_ID_HEADER, val);
    }
    resp
}

fn is_hop_by_hop(name: &str) -> bool {
    const HOP: &[&str] = &[
        "connection",
        "keep-alive",
        "proxy-authenticate",
        "proxy-authorization",
        "te",
        "trailers",
        "transfer-encoding",
        "upgrade",
        "host",
    ];
    HOP.iter().any(|h| name.eq_ignore_ascii_case(h))
}

fn header_str(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)?
        .to_str()
        .ok()
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
}

fn error_json(status: StatusCode, message: &str) -> Response {
    let body = json!({
        "error": {
            "message": message,
            "type": "router_error",
            "code": status.as_u16(),
        }
    });
    (status, Json(body)).into_response()
}

fn too_many_requests(model: &str) -> Response {
    let body = json!({
        "error": {
            "message": format!("all eligible replicas for model `{model}` are saturated"),
            "type": "rate_limit_exceeded",
            "code": 429,
        }
    });
    (
        StatusCode::TOO_MANY_REQUESTS,
        [(header::RETRY_AFTER, "1")],
        Json(body),
    )
        .into_response()
}

fn cancelled_response(request_id: &str) -> Response {
    let status = StatusCode::from_u16(CLIENT_CANCELLED).unwrap_or(StatusCode::REQUEST_TIMEOUT);
    let body = json!({
        "error": {
            "message": "request cancelled",
            "type": "cancelled",
            "code": CLIENT_CANCELLED,
        }
    });
    let mut resp = (status, Json(body)).into_response();
    if let Ok(val) = HeaderValue::from_str(request_id) {
        resp.headers_mut().insert(REQUEST_ID_HEADER, val);
    }
    resp
}
