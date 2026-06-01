//! Reverse proxy for the petri-lab engine.
//!
//! The SPA hits `/petri/...` on mekhan's origin; this handler strips the
//! `/petri` prefix and forwards the rest to `config.petri_lab_url`. Bodies
//! stream both ways so `/api/nets/{id}/events/stream` (SSE) works without
//! buffering. Authentication is enforced by the same session-cookie middleware
//! that gates every other `/api/v1` route — mounting this inside `protected`
//! is what gives mekhan a single-origin posture in prod.

use axum::{
    body::Body,
    extract::{Request, State},
    http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode},
    response::{IntoResponse, Response},
    routing::any,
    Router,
};
use axum_extra::extract::cookie::CookieJar;
use futures::TryStreamExt;
use reqwest::Client;

use crate::auth::{instance_workspace, member_role, AuthUser, MembershipError};
use crate::AppState;

const STRIP_PREFIX: &str = "/petri";

/// Extract the engine `net_id` from a post-strip proxy path.
///
/// After `STRIP_PREFIX` is removed, engine paths look like
/// `/api/nets/{id}/...`. Anything that doesn't match that shape (engine
/// health probes, root pings) returns `None` — those stay un-gated because
/// they don't enumerate per-instance state.
fn extract_net_id(path: &str) -> Option<&str> {
    let stripped = path.strip_prefix("/api/nets/")?;
    let end = stripped.find(['/', '?']).unwrap_or(stripped.len());
    let id = &stripped[..end];
    if id.is_empty() {
        None
    } else {
        Some(id)
    }
}

/// Hop-by-hop headers that must NOT be forwarded (RFC 7230 §6.1).
fn is_hop_by_hop(name: &HeaderName) -> bool {
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
    HOP.iter().any(|h| name.as_str().eq_ignore_ascii_case(h))
}

fn copy_headers(src: &HeaderMap) -> HeaderMap {
    let mut out = HeaderMap::with_capacity(src.len());
    for (k, v) in src.iter() {
        if !is_hop_by_hop(k) {
            out.insert(k.clone(), v.clone());
        }
    }
    out
}

async fn proxy(State(state): State<AppState>, req: Request) -> Result<Response, ProxyError> {
    let client = reqwest_client();

    let path_and_query = req
        .uri()
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or("/");
    let rest = path_and_query
        .strip_prefix(STRIP_PREFIX)
        .unwrap_or(path_and_query);
    let rest = if rest.is_empty() { "/" } else { rest };

    // Workspace ACL: only paths that scope an engine net (`/api/nets/{id}/...`)
    // are per-instance gated. Engine health / root pings don't enumerate
    // instance state, so they ride through un-gated — the proxy is still
    // only reachable inside the SPA's same-origin posture.
    //
    // Auth is done inline (rather than via `require_auth_middleware`'s
    // injected extension) because `Router::merge` doesn't propagate the
    // middleware from the protected router to merged sub-routers, so the
    // extension isn't actually populated on /petri requests. Inlining the
    // call keeps the proxy correct regardless of how it gets mounted.
    if let Some(net_id) = extract_net_id(rest) {
        let jar = CookieJar::from_headers(req.headers());
        let user = state
            .authenticator
            .authenticate(req.headers(), &jar)
            .await
            .map_err(|_| ProxyError::Unauthenticated)?;
        gate_petri_instance(&state, &user, net_id, req.method()).await?;
    }

    let target = format!(
        "{}{}",
        state.config.petri_lab_url.trim_end_matches('/'),
        rest
    );

    let method = reqwest::Method::from_bytes(req.method().as_str().as_bytes())
        .map_err(|_| ProxyError::BadMethod)?;
    let headers = copy_headers(req.headers());
    let body_stream = req.into_body().into_data_stream();
    let body = reqwest::Body::wrap_stream(body_stream);

    let upstream = client
        .request(method, &target)
        .headers(reqwest_headers(headers))
        .body(body)
        .send()
        .await
        .map_err(ProxyError::Upstream)?;

    let status =
        StatusCode::from_u16(upstream.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let resp_headers = upstream.headers().clone();
    let stream = upstream.bytes_stream().map_err(std::io::Error::other);

    let mut out = Response::new(Body::from_stream(stream));
    *out.status_mut() = status;
    for (k, v) in resp_headers.iter() {
        let name = match HeaderName::from_bytes(k.as_str().as_bytes()) {
            Ok(n) => n,
            Err(_) => continue,
        };
        if is_hop_by_hop(&name) {
            continue;
        }
        if let Ok(val) = HeaderValue::from_bytes(v.as_bytes()) {
            out.headers_mut().append(name, val);
        }
    }
    Ok(out)
}

fn reqwest_headers(h: HeaderMap) -> reqwest::header::HeaderMap {
    let mut out = reqwest::header::HeaderMap::with_capacity(h.len());
    for (k, v) in h.iter() {
        if let (Ok(name), Ok(val)) = (
            reqwest::header::HeaderName::from_bytes(k.as_str().as_bytes()),
            reqwest::header::HeaderValue::from_bytes(v.as_bytes()),
        ) {
            out.append(name, val);
        }
    }
    out
}

fn reqwest_client() -> Client {
    use std::sync::OnceLock;
    static CLIENT: OnceLock<Client> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            Client::builder()
                // SSE streams sit idle between events; don't time them out.
                .pool_idle_timeout(None)
                .build()
                .expect("reqwest client")
        })
        .clone()
}

/// Per-instance ACL: read = `safe method on public template OR member`,
/// write = `member`. `safe` follows RFC 7231 (GET/HEAD/OPTIONS/TRACE); the
/// engine treats anything else as state-changing (run-mode flips, command
/// fires, scenario loads), so public-write is never allowed even for a
/// publicly visible template.
async fn gate_petri_instance(
    state: &AppState,
    user: &AuthUser,
    net_id: &str,
    method: &Method,
) -> Result<(), ProxyError> {
    let (workspace_id, visibility) = match instance_workspace(&state.db, net_id).await {
        Ok(v) => v,
        // The net_id isn't a mekhan-managed workflow instance. That means it's
        // either a shared INFRA net deployed straight to the engine
        // (`resource-pool-net`, `executor-net`) or a
        // non-net engine listing path the extractor optimistically treated as
        // an id (e.g. `/api/nets/metadata`). None of these enumerate
        // per-principal instance data, and every genuine user instance lives
        // in mekhan's DB — so a not-found id is necessarily non-sensitive.
        // Allow read-only (safe) methods through (this is what powers the
        // Engine Nets browser and the resource-pool dashboard); deny anything
        // state-changing.
        Err(MembershipError::TemplateNotFound(_)) => {
            return if method.is_safe() {
                Ok(())
            } else {
                Err(ProxyError::NotFound)
            };
        }
        Err(MembershipError::Db(e)) => return Err(ProxyError::Db(e.to_string())),
        // `instance_workspace` never returns NotMember / InsufficientRole;
        // collapse to Db for completeness.
        Err(other) => return Err(ProxyError::Db(other.to_string())),
    };

    let is_safe = method.is_safe();
    if is_safe && visibility == "public" {
        return Ok(());
    }

    match member_role(&state.db, user, workspace_id).await {
        Ok(_) => Ok(()),
        Err(MembershipError::NotMember(_)) => Err(ProxyError::Forbidden),
        Err(MembershipError::Db(e)) => Err(ProxyError::Db(e.to_string())),
        Err(other) => Err(ProxyError::Db(other.to_string())),
    }
}

#[derive(Debug, thiserror::Error)]
enum ProxyError {
    #[error("bad method")]
    BadMethod,
    #[error("engine upstream: {0}")]
    Upstream(reqwest::Error),
    #[error("instance not found")]
    NotFound,
    #[error("forbidden")]
    Forbidden,
    #[error("unauthenticated")]
    Unauthenticated,
    #[error("db error: {0}")]
    Db(String),
}

impl IntoResponse for ProxyError {
    fn into_response(self) -> Response {
        let (status, msg) = match self {
            ProxyError::BadMethod => (StatusCode::BAD_REQUEST, "bad method"),
            ProxyError::Upstream(_) => (StatusCode::BAD_GATEWAY, "engine unreachable"),
            ProxyError::NotFound => (StatusCode::NOT_FOUND, "instance not found"),
            ProxyError::Forbidden => (
                StatusCode::FORBIDDEN,
                "not a member of this instance's workspace",
            ),
            ProxyError::Unauthenticated => (StatusCode::UNAUTHORIZED, "unauthenticated"),
            ProxyError::Db(_) => (StatusCode::INTERNAL_SERVER_ERROR, "internal error"),
        };
        (status, msg).into_response()
    }
}

/// Build the `/petri/*` reverse-proxy router. Mount inside the auth layer so
/// the engine inherits the same session-cookie gate as the rest of `/api/v1`.
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/petri", any(proxy))
        .route("/petri/{*rest}", any(proxy))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::extract_net_id;

    #[test]
    fn extracts_net_id_from_well_formed_engine_path() {
        assert_eq!(
            extract_net_id("/api/nets/mekhan-abc/state"),
            Some("mekhan-abc")
        );
        assert_eq!(
            extract_net_id("/api/nets/abc-123/events?from_sequence=10"),
            Some("abc-123")
        );
        assert_eq!(extract_net_id("/api/nets/only-id"), Some("only-id"));
    }

    #[test]
    fn no_net_id_for_engine_root_or_health() {
        assert_eq!(extract_net_id("/"), None);
        assert_eq!(extract_net_id("/healthz"), None);
        assert_eq!(extract_net_id("/api/nets/"), None);
        assert_eq!(extract_net_id("/api/something-else"), None);
    }
}
