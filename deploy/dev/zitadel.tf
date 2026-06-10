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
#
# `org_id` MUST be set on every Zitadel resource here. Without it the
# provider sees the live `org_id` returned by Zitadel as drift against an
# implicit "null" in the config, and flags every resource for replacement
# on every plan — which rotates client_ids on every CI run and invalidates
# all existing CLI tokens + browser sessions. See variables.tf for sourcing.
resource "zitadel_project" "mekhan" {
  org_id                   = var.zitadel_org_id
  name                     = "Mekhan"
  project_role_assertion   = true
  project_role_check       = false
  has_project_check        = false
  private_labeling_setting = "PRIVATE_LABELING_SETTING_UNSPECIFIED"
}

# Public SPA client. The redirect URI is the BFF callback path the service
# already exposes at service/src/auth/bff/handlers.rs (GET /api/auth/callback).
resource "zitadel_application_oidc" "spa" {
  org_id     = var.zitadel_org_id
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

  version  = "OIDC_VERSION_1_0"
  dev_mode = false


  access_token_type = "OIDC_TOKEN_TYPE_JWT"

  id_token_role_assertion     = true
  id_token_userinfo_assertion = true
  access_token_role_assertion = true
}

# =============================================================================
# PAT feature — Zitadel side
# =============================================================================
# Mekhan's "Profile → Access tokens" UI mints machine-user PATs that CI clients
# (`mekhan apply`, `MEKHAN_CLI_TOKEN`) present as `Authorization: Bearer …`.
# Zitadel is the sole source of truth — Mekhan stores no token state itself.

# =============================================================================

# Confidential API app — credentials Mekhan uses to authenticate to Zitadel's
# introspection endpoint. BASIC auth = client_id + client_secret as HTTP Basic.
resource "zitadel_application_api" "introspect" {
  org_id           = var.zitadel_org_id
  project_id       = zitadel_project.mekhan.id
  name             = "Mekhan SPA-introspect"
  auth_method_type = "API_AUTH_METHOD_TYPE_BASIC"
}

# Service identity Mekhan uses when brokering per-user PATs through Zitadel's
# Management API. One machine user, one PAT — the bootstrap script's old
# "delete-and-remint on every run" pattern is unnecessary in TF because the
# PAT secret is captured in tfstate at create-time.
resource "zitadel_machine_user" "token_broker" {
  org_id      = var.zitadel_org_id
  user_name   = "mekhan-token-broker"
  name        = "Mekhan Token Broker"
  description = "Brokers per-user automation PATs for the embedded /api/auth/tokens feature"
}

# ORG_OWNER is the minimum role that lets the broker create/delete machine
# users and their PATs in this org. Without it, /api/auth/tokens 502s.
resource "zitadel_org_member" "token_broker" {
  org_id  = var.zitadel_org_id
  user_id = zitadel_machine_user.token_broker.id
  roles   = ["ORG_OWNER"]
}

# The PAT Mekhan presents as the broker. `token` is sensitive and only
# exposed by Zitadel at creation — TF captures it in state.
#
# Far-future expiration: matches the registry example pattern and keeps the
# broker credential from silently expiring. To rotate, `tofu taint` this
# resource (or bump expiration_date) and re-apply.
resource "zitadel_personal_access_token" "token_broker" {
  user_id         = zitadel_machine_user.token_broker.id
  expiration_date = "2099-01-01T00:00:00Z"
}
