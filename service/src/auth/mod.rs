//! Authentication module — hexagonal/ports-and-adapters layout.
//!
//! Domain types (`AuthUser`, `VerifiedClaims`, `AuthError`) and trait ports
//! (`Authenticator`, `TokenVerifier`, `PrincipalResolver`) live in `model`,
//! `authenticator`, and `port`. Concrete adapters live in their own files:
//!
//! - [`authenticator`] — the per-request authn seam: `BffAuthenticator`
//!   (opaque session cookie → Postgres token custody) and
//!   `NoopAuthenticator` (fixed dev user for `dev_noop`).
//! - [`bff`] — server-side OIDC flow: OIDC client, session store, the
//!   `/api/auth/*` endpoints.
//! - [`zitadel`] — `ZitadelTokenVerifier` (JWKS + jsonwebtoken). Reused
//!   internally by the BFF callback to verify the IdP's token.
//! - [`resolver`] — `StaticPrincipalResolver` (claims → `AuthUser`).
//! - [`dev`] — `NoopTokenVerifier` returning a fixed dev user.
//! - [`extractor`] — HTTP-driving adapter (Axum `FromRequestParts`, layer).
//!
//! The composition root in `main.rs` chooses adapters from `AuthConfig`.

pub mod authenticator;
pub mod bff;
pub mod dev;
pub mod extractor;
pub mod introspection;
pub mod mgmt;
pub mod model;
pub mod port;
pub mod resolver;
pub mod zitadel;

pub use authenticator::{Authenticator, SESSION_COOKIE};
pub use introspection::IntrospectionVerifier;
pub use mgmt::ZitadelMgmt;
pub use model::{AuthError, AuthUser, VerifiedClaims};
pub use port::{PrincipalResolver, TokenVerifier};
