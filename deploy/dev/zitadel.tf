# =============================================================================
# mekhan-service Zitadel application — provisioned by mekhan, not the cluster
# =============================================================================
# Mekhan registers its own Zitadel ORG + project + OIDC apps against the
# cluster's shared Zitadel instance (id.aithericon.eu). The pattern mirrors
# what mekhan already does for Postgres: the cluster repo runs the
# *infrastructure* (Patroni / Zitadel), and the service repo owns the
# *resources inside it*.
#
# ISOLATION MODEL (early-access testers / demos)
# ----------------------------------------------
# Everything Mekhan-facing now lives in a DEDICATED organization,
# "Mekhan Testers", created here. In Zitadel a user's HOME ORG is the real
# security boundary: a user who lives in this org is NOT a member of the
# cluster's default "ZITADEL" org, so they have no path to the admin-surface
# apps (Vault / Nomad / Grafana / Matrix) that live over there.
#
# This lets us hand out early access without exposing the rest of the
# infrastructure: testers exist only in this org, get only the Mekhan project,
# and nothing else.
#
# `org_id` MUST be set on every Zitadel resource here. Without it the provider
# sees the live `org_id` returned by Zitadel as drift against an implicit
# "null" in the config and flags every resource for replacement on every plan
# — which rotates client_ids on every CI run and invalidates all existing CLI
# tokens + browser sessions. We pin it to the org we create just below.
# =============================================================================

# -----------------------------------------------------------------------------
# Dedicated org for Mekhan early-access. This is the isolation boundary.
# -----------------------------------------------------------------------------
# The cluster's Zitadel IaC service user (iam-admin) is IAM_OWNER at the
# instance level, so it is allowed to create new organizations and manage
# resources inside them.
resource "zitadel_org" "mekhan_testers" {
  name = "Mekhan Testers"
}

# -----------------------------------------------------------------------------
# Mekhan project — lives inside the testers org (no longer the cluster default).
# -----------------------------------------------------------------------------
# project_role_assertion = true  -> the user's Mekhan roles are embedded in the
#   ID/access token under `urn:zitadel:iam:org:project:roles`, which is exactly
#   what service/src/auth/resolver.rs reads to populate AuthUser.roles.
#
# project_role_check / has_project_check = false  -> kept permissive on purpose.
#   Isolation is enforced by the ORG boundary above, not by per-project gating.
#   Turning has_project_check on would also require every machine user minted by
#   the PAT broker (/api/auth/tokens) to carry an explicit grant, which would
#   break that feature. Leave the hard gate off; the org is the fence.
resource "zitadel_project" "mekhan" {
  org_id                   = zitadel_org.mekhan_testers.id
  name                     = "Mekhan"
  project_role_assertion   = true
  project_role_check       = false
  has_project_check        = false
  private_labeling_setting = "PRIVATE_LABELING_SETTING_UNSPECIFIED"
}

# -----------------------------------------------------------------------------
# Project roles — Mekhan's own RBAC vocabulary.
# -----------------------------------------------------------------------------
# Until now no roles were defined, so service/src/auth/resolver.rs fell back to
# auto-assigning every authenticated user the `editor` workspace role. With
# real roles defined and granted, the roles claim carries meaning and the
# resolver can map them deliberately.
#
#   mekhan_user  — ordinary tester / demo account (default for early access)
#   mekhan_admin — elevated account (manage members, etc.)
resource "zitadel_project_role" "mekhan_user" {
  org_id       = zitadel_org.mekhan_testers.id
  project_id   = zitadel_project.mekhan.id
  role_key     = "mekhan_user"
  display_name = "Mekhan User"
}

resource "zitadel_project_role" "mekhan_admin" {
  org_id       = zitadel_org.mekhan_testers.id
  project_id   = zitadel_project.mekhan.id
  role_key     = "mekhan_admin"
  display_name = "Mekhan Admin"
}

# Public SPA client. The redirect URI is the BFF callback path the service
# already exposes at service/src/auth/bff/handlers.rs (GET /api/auth/callback).
resource "zitadel_application_oidc" "spa" {
  org_id     = zitadel_org.mekhan_testers.id
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
# Early-access tester accounts
# =============================================================================
# Human users created directly in the testers org. Each gets a random initial
# password (captured in tfstate, surfaced via `tofu output`) that the user must
# change on first login, and a grant of the `mekhan_user` project role.
#
# Driven by var.tester_users so adding/removing a tester is a one-line tfvars
# edit + apply. Membership in THIS org is what scopes them to Mekhan only.

resource "random_password" "tester" {
  for_each = var.tester_users

  length  = 20
  special = true
  # Satisfy Zitadel's default password complexity policy.
  min_special = 2
  min_upper   = 2
  min_lower   = 2
  min_numeric = 2
}

resource "zitadel_human_user" "tester" {
  for_each = var.tester_users

  org_id             = zitadel_org.mekhan_testers.id
  user_name          = each.value.username
  first_name         = each.value.first_name
  last_name          = each.value.last_name
  display_name       = "${each.value.first_name} ${each.value.last_name}"
  preferred_language = "en"
  email              = each.value.email

  # Password flow (not email-invite): a random initial password is set and the
  # email is pre-verified, so the account is usable immediately. The password is
  # published to Vault (below) for retrieval; the user must change it on first
  # login. Used for the bootstrap admin so login never depends on email
  # delivery. (Real testers are onboarded in-app and never reach this resource.)
  is_email_verified = true
  initial_password  = random_password.tester[each.key].result
}

# Grant each tester their project role (default mekhan_user). role_keys
# references the role definitions above so the grant is created after the roles
# exist.
resource "zitadel_user_grant" "tester" {
  for_each = var.tester_users

  org_id     = zitadel_org.mekhan_testers.id
  project_id = zitadel_project.mekhan.id
  user_id    = zitadel_human_user.tester[each.key].id
  role_keys  = [each.value.role]

  depends_on = [
    zitadel_project_role.mekhan_user,
    zitadel_project_role.mekhan_admin,
  ]
}

# Publish each TF-created account's initial password to Vault so it's
# retrievable without the Terraform state / operator env. The pipeline applies
# this; read it back with:
#   vault kv get -field=initial_password secret/services/mekhan/dev/testers/mekhan_admin
# KV v2 at the "secret" mount. Change on first login.
resource "vault_kv_secret_v2" "tester_password" {
  for_each = var.tester_users

  mount = "secret"
  name  = "services/mekhan/dev/testers/${each.key}"

  data_json = jsonencode({
    username         = each.value.username
    email            = each.value.email
    role             = each.value.role
    initial_password = random_password.tester[each.key].result
    note             = "Initial password — change on first login."
  })
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
  org_id           = zitadel_org.mekhan_testers.id
  project_id       = zitadel_project.mekhan.id
  name             = "Mekhan SPA-introspect"
  auth_method_type = "API_AUTH_METHOD_TYPE_BASIC"
}

# Service identity Mekhan uses when brokering per-user PATs through Zitadel's
# Management API. One machine user, one PAT — the bootstrap script's old
# "delete-and-remint on every run" pattern is unnecessary in TF because the
# PAT secret is captured in tfstate at create-time.
resource "zitadel_machine_user" "token_broker" {
  org_id      = zitadel_org.mekhan_testers.id
  user_name   = "mekhan-token-broker"
  name        = "Mekhan Token Broker"
  description = "Brokers per-user automation PATs for the embedded /api/auth/tokens feature"
}

# ORG_OWNER is the minimum role that lets the broker create/delete machine
# users and their PATs in this org. Without it, /api/auth/tokens 502s.
resource "zitadel_org_member" "token_broker" {
  org_id  = zitadel_org.mekhan_testers.id
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
