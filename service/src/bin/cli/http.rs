//! Shared HTTP auth for the CLI.
//!
//! The CLI is a non-interactive client. Against an auth-enabled server every
//! request needs the Zitadel service-user PAT, supplied as `MEKHAN_CLI_TOKEN`
//! and validated server-side via RFC 7662 introspection (the dual-use
//! `AuthUser` extractor accepts it exactly like a browser session cookie).
//! No-op when the env var is unset — local `dev_noop` servers need no token.

use reqwest::RequestBuilder;

/// Attach `Authorization: Bearer $MEKHAN_CLI_TOKEN` when it is set.
pub fn auth(rb: RequestBuilder) -> RequestBuilder {
    match std::env::var("MEKHAN_CLI_TOKEN") {
        Ok(t) if !t.is_empty() => rb.bearer_auth(t),
        _ => rb,
    }
}
