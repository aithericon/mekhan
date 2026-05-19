# =============================================================================
# mekhan-service Zitadel application — provisioned by mekhan, not the cluster
# =============================================================================
# Mekhan registers its own Zitadel project + OIDC SPA application against the
# cluster's shared Zitadel instance (id.aithericon.eu). The pattern mirrors
# what mekhan already does for Postgres: the cluster repo runs the
# *infrastructure* (Patroni / Zitadel), and the service repo owns the
# *resources inside it* (the mekhan_dev DB role / the "Mekhan" OIDC app).
#
# This keeps every Zitadel-related artifact in one repo, one TF state, one
# apply. The cluster's 06e_zitadel_config layer is untouched.
#
# Single SPA application: public USER_AGENT client + PKCE + Authorization
# Code, no client secret. The BFF in service/src/auth/bff/ runs the full
# code flow server-side and hands the browser only an opaque session
# cookie — matches the local-dev pattern baked into deploy/zitadel/bootstrap.sh.
# =============================================================================

# Mekhan's own org-level project. Distinct from the cluster's "Aithericon"
# project (which is for admin surfaces — Vault, Nomad, Grafana). Keeps blast
# radius scoped: deleting this layer's state never affects cluster-admin apps.
resource "zitadel_project" "mekhan" {
  name                     = "Mekhan"
  project_role_assertion   = true
  project_role_check       = false
  has_project_check        = false
  private_labeling_setting = "PRIVATE_LABELING_SETTING_UNSPECIFIED"
}

# Public SPA client. The redirect URI is the BFF callback path the service
# already exposes at service/src/auth/bff/handlers.rs (GET /api/auth/callback).
resource "zitadel_application_oidc" "spa" {
  project_id = zitadel_project.mekhan.id

  name = "Mekhan SPA"

  redirect_uris = [
    "https://${var.hostname}/api/auth/callback",
  ]
  post_logout_redirect_uris = [
    "https://${var.hostname}/",
  ]

  response_types = ["OIDC_RESPONSE_TYPE_CODE"]
  grant_types    = ["OIDC_GRANT_TYPE_AUTHORIZATION_CODE", "OIDC_GRANT_TYPE_REFRESH_TOKEN"]
  app_type       = "OIDC_APP_TYPE_USER_AGENT"

  # NONE = public PKCE client. No client_secret issued, no HTTP Basic on the
  # token endpoint. The Rust BFF holds the verifier; the browser only holds
  # the opaque session cookie.
  auth_method_type = "OIDC_AUTH_METHOD_TYPE_NONE"

  version              = "OIDC_VERSION_1_0"
  dev_mode             = false
  access_token_type    = "OIDC_TOKEN_TYPE_BEARER"
  id_token_role_assertion       = true
  id_token_userinfo_assertion   = true
  access_token_role_assertion   = true
}
