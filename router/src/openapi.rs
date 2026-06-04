//! The router's OWN OpenAPI document, served at `GET /openapi.json`.
//!
//! Deliberately SEPARATE from `openapi-mekhan.json` — the router is its own
//! deployable with its own contract, so it does not participate in mekhan's
//! `ci::openapi-drift` gate. Hand-written (the surface is tiny) to avoid
//! pulling utoipa into the router crate.

use axum::Json;
use serde_json::{json, Value};

pub async fn openapi_doc() -> Json<Value> {
    Json(json!({
        "openapi": "3.1.0",
        "info": {
            "title": "Aithericon Inference Router",
            "version": env!("CARGO_PKG_VERSION"),
            "description": "OpenAI-compatible inference router (model-pool data plane). \
                Bypasses the engine Petri net; residency is a hard placement filter that \
                fails closed; there is no automatic external offload."
        },
        "paths": {
            "/v1/chat/completions": {
                "post": {
                    "summary": "OpenAI-compatible chat completions (buffered or SSE).",
                    "description": "Routes to a live replica serving `model`. Honors an \
                        optional `X-Residency-Zone` header as a hard placement filter. \
                        Returns 429 on replica saturation, 422 when residency is \
                        unsatisfiable, 503 when no replica serves the model. The chosen \
                        request id is echoed in the `X-Inference-Request-Id` response header.",
                    "responses": {
                        "200": {"description": "Completion (JSON or text/event-stream)."},
                        "401": {"description": "Missing bearer token (bearer auth mode)."},
                        "422": {"description": "Residency zone unsatisfiable."},
                        "429": {"description": "All eligible replicas saturated."},
                        "502": {"description": "Upstream replica unreachable."},
                        "503": {"description": "No replica serves the requested model."}
                    }
                }
            },
            "/v1/models": {
                "get": {
                    "summary": "OpenAI-compatible model list (the live routed set).",
                    "responses": {"200": {"description": "List of served model ids."}}
                }
            },
            "/healthz": {
                "get": {
                    "summary": "Liveness probe.",
                    "responses": {"200": {"description": "Router is up."}}
                }
            },
            "/metrics": {
                "get": {
                    "summary": "Prometheus exposition (autoscale signal source).",
                    "responses": {"200": {"description": "Prometheus text exposition."}}
                }
            }
        }
    }))
}
