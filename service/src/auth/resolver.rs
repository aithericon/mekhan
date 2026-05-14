//! `StaticPrincipalResolver` — maps verified JWT claims onto Mekhan's
//! `AuthUser`. Reads the Zitadel-specific roles claim layout, so this is the
//! one place provider-specific identifiers live outside the adapter itself.

use async_trait::async_trait;
use serde_json::Value;

use super::model::{AuthError, AuthUser, VerifiedClaims};
use super::port::PrincipalResolver;

/// Zitadel emits roles under this claim. Value is a nested object:
/// `{ "<role>": { "<org_id>": "<org_domain>" } }`. We flatten to the set of
/// role names and adopt the first org_id we encounter as the user's org.
const ZITADEL_ROLES_CLAIM: &str = "urn:zitadel:iam:org:project:roles";

#[derive(Debug, Clone, Default)]
pub struct StaticPrincipalResolver;

#[async_trait]
impl PrincipalResolver for StaticPrincipalResolver {
    async fn resolve(&self, claims: VerifiedClaims) -> Result<AuthUser, AuthError> {
        let email = string_claim(&claims, "email");
        let display_name = string_claim(&claims, "name").or_else(|| string_claim(&claims, "preferred_username"));

        let (roles, org_id) = match claims.extra.get(ZITADEL_ROLES_CLAIM) {
            Some(Value::Object(roles_obj)) => {
                let roles: Vec<String> = roles_obj.keys().cloned().collect();
                let org_id = roles_obj
                    .values()
                    .filter_map(|orgs| orgs.as_object())
                    .flat_map(|m| m.keys().cloned())
                    .next();
                (roles, org_id)
            }
            _ => (Vec::new(), None),
        };

        Ok(AuthUser {
            subject: claims.subject,
            email,
            display_name,
            roles,
            org_id,
        })
    }
}

fn string_claim(claims: &VerifiedClaims, key: &str) -> Option<String> {
    claims.extra.get(key).and_then(|v| v.as_str()).map(str::to_string)
}
