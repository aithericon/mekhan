//! Backend-for-Frontend OIDC: the service runs the entire Authorization-Code
//! plus-PKCE flow and holds the token set in Postgres; the browser sees only
//! an opaque HttpOnly session cookie.
//!
//! - [`oidc`] — hand-rolled Zitadel OIDC client (discovery, authorize URL,
//!   code exchange, refresh), reqwest-based like [`crate::auth::zitadel`].
//! - [`session`] — `SessionStore` port + `PgSessionStore` adapter (token
//!   custody + in-flight PKCE login flows).
//! - [`handlers`] — the unauthenticated `/api/auth/*` endpoints
//!   (`login`, `callback`, `logout`, `session`).
//!
//! The per-request authn seam ([`crate::auth::authenticator`]) consumes a
//! `SessionStore`; the callback path reuses the existing `TokenVerifier` +
//! `PrincipalResolver` so the domain `AuthUser` contract is unchanged.

pub mod handlers;
pub mod oidc;
pub mod session;
