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

pub mod active_workspace;
pub mod authenticator;
pub mod bff;
pub mod dev;
pub mod extractor;
pub mod grants;
pub mod membership;
pub mod model;
pub mod platform_root;
pub mod port;
pub mod resolver;
pub mod runner_token;
pub mod user_pat;
pub mod worker_token;
pub mod zitadel;

pub use authenticator::{Authenticator, SESSION_COOKIE};
pub use grants::{
    annotate_roles_keep_all, apply_grant, effective_object_role, effective_object_roles,
    filter_and_annotate_visible, grant_context, require_object_role, AclAnnotated, GrantContext,
    ObjectKind, ObjectRef,
};
pub use membership::{
    can_read_template, instance_ref_by_net_id, instance_workspace, map_to_api_error, member_role,
    require_member, require_role, require_workspace_read, resolve_fork_target, template_workspace,
    MembershipError, Role,
};
pub use model::{AuthError, AuthUser, VerifiedClaims};
pub use port::{PrincipalResolver, TokenVerifier};
