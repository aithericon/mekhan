//! Trait ports — the seam between auth domain and external systems.
//!
//! Two ports rather than one because the verifier varies by identity provider
//! (Zitadel vs Keycloak vs Auth0 vs in-process mock) while the resolver
//! encodes Mekhan's own rules (which claim becomes which role, org assignment,
//! etc.). Keeping them separate makes the test double trivial — mock just the
//! verifier and leave the real resolver in place.

use async_trait::async_trait;

use super::model::{AuthError, AuthUser, VerifiedClaims};

/// Validates a raw bearer token and produces verified claims. All I/O
/// (JWKS fetch, key cache, signature check) is hidden behind this trait.
#[async_trait]
pub trait TokenVerifier: Send + Sync {
    async fn verify(&self, raw_token: &str) -> Result<VerifiedClaims, AuthError>;
}

/// Maps verified claims onto Mekhan's domain `AuthUser`. Provider-specific
/// claim names live in implementations of this trait, never in the verifier.
#[async_trait]
pub trait PrincipalResolver: Send + Sync {
    async fn resolve(&self, claims: VerifiedClaims) -> Result<AuthUser, AuthError>;
}
