//! Authentication module — hexagonal/ports-and-adapters layout.
//!
//! Domain types (`AuthUser`, `VerifiedClaims`, `AuthError`) and trait ports
//! (`TokenVerifier`, `PrincipalResolver`) live in `model` and `port`. Concrete
//! adapters live in their own files:
//!
//! - [`zitadel`] — `ZitadelTokenVerifier` (JWKS + jsonwebtoken).
//! - [`resolver`] — `StaticPrincipalResolver` (claims → `AuthUser`).
//! - [`dev`] — `NoopTokenVerifier` returning a fixed dev user.
//! - [`extractor`] — HTTP-driving adapter (Axum `FromRequestParts`, layer).
//!
//! The composition root in `main.rs` chooses an adapter from `AuthConfig`.

pub mod dev;
pub mod extractor;
pub mod model;
pub mod port;
pub mod resolver;
pub mod zitadel;

pub use model::{AuthError, AuthUser, VerifiedClaims};
pub use port::{PrincipalResolver, TokenVerifier};
