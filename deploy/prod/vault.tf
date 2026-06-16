# =============================================================================
# Vault — secret store for mekhan-service, engine, and executor
# =============================================================================
# Three secret-store concerns share one Vault server (10.20.0.20:8200) via
# Nomad workload-identity → JWT-Nomad auth:
#
#   1. NATS user credentials  (existing, dev/04c)
#      Path:  secret/data/nats/apps/mekhan/dev/worker
#      Bootstrap: deploy/dev/scripts/generate-nats-user.sh (out-of-band)
#      Consumers: mekhan-service, engine, executor — all render via
#                 `template { ... }` at alloc start, change_mode=restart.
#
#   2. Resource version secrets  (Phase B.9; service/src/main.rs:207)
#      Path:  secret/data/aithericon/resources/{workspace_id}/{resource_id}/v{n}
#      Writer:    mekhan-service `VaultResourceStore::put_kv` on resource
#                 create / new version. Sees plaintext at write time only.
#      Reader:    engine (response-wrap path in core-engine's executor client)
#                 — fetches values, wraps into a single-use `hvs.xxx` token,
#                 publishes the token (NOT plaintext) onto NATS.
#      Unwrapper: executor — calls `vault_unwrap_secrets()` with the
#                 wrapping token itself as auth. NO Vault policy needed,
#                 just network reach + VAULT_ADDR.
#      See engine/docs/adr/10-secret-management.md for the full flow.
#
#   3. Response wrapping  (sys/wrapping/wrap)
#      Engine needs `update` capability to mint single-use wrapping tokens
#      that carry resolved secrets onto NATS without persisting plaintext.
#
# Split rationale: mekhan-service + engine run as separate tasks in the SAME
# Nomad alloc (the `mekhan-service` job), so they share a workload-identity
# JWT and therefore one policy bundle. The `executor` job runs as a separate
# alloc with its own JWT — it gets a minimum-privilege policy (NATS read
# only). Executor's unwrap call doesn't authenticate against a policy at
# all; the wrapping token itself is single-use and TTL-bound.
# =============================================================================

# Policy / role names + the resource-store prefix are env-derived in locals.tf
# (vault_policy_nats_read / vault_policy_resources_rw / vault_policy_wrap /
# vault_role_service / vault_role_executor / resources_kv_prefix /
# svc_secrets_path / executor_reg_token_path). resources_kv_prefix stays SHARED
# across envs on purpose — the service hardcodes it and per-env workspace UUIDs
# already namespace the payloads. Keep that prefix in sync with:
#   - shared/resources/src/store.rs (the format string)
#   - service/src/handlers/resources.rs:219 (where the path is built)
#   - service/migrations/20240120000000_create_resources.sql header docs

# ── Policy 1 — NATS user creds (read-only) ───────────────────────────────────
# Replaces the old `mekhan-dev` policy from nats.tf. Granted to BOTH roles;
# all three workloads need to render the same .creds bundle at alloc start.
resource "vault_policy" "mekhan_nats_read" {
  name = local.vault_policy_nats_read

  policy = <<-EOT
  path "secret/data/${local.nats_user_kv_path}" {
    capabilities = ["read"]
  }

  path "secret/metadata/${local.nats_user_kv_path}" {
    capabilities = ["read"]
  }

  # Worker-pool enrollment token (executor self-enroll). Reusable `wt_` token,
  # rendered into EXECUTOR_WORKER_REG_TOKEN by executor.nomad.hcl.tpl.
  path "secret/data/${local.executor_reg_token_path}" {
    capabilities = ["read"]
  }

  # S3 artifact-store keys — rendered into the storage.env template by BOTH the
  # service and the executor (both need bucket access), so the read lives in
  # this shared policy. The secret VALUES are written by vault.tf, never baked
  # into the jobspec.
  path "secret/data/${local.svc_secrets_path}/storage" {
    capabilities = ["read"]
  }
  path "secret/metadata/${local.svc_secrets_path}/storage" {
    capabilities = ["read"]
  }
  EOT
}

# ── Policy 2 — Resource version secrets (CRUD) ───────────────────────────────
# mekhan-service writes here on resource create / new-version; the engine
# reads here when wrapping secrets before NATS dispatch. Granted only to the
# `mekhan-service` role (mekhan + engine tasks share that alloc's JWT). The
# executor role does NOT get this — it never touches secret/data/... by path.
resource "vault_policy" "mekhan_resources_rw" {
  name = local.vault_policy_resources_rw

  policy = <<-EOT
  # KV v2 data path — actual secret payloads (versioned by Vault).
  path "secret/data/${local.resources_kv_prefix}/*" {
    capabilities = ["create", "read", "update", "delete"]
  }

  # KV v2 metadata path — required for delete-version + soft-delete recovery.
  # `list` lets ops inspect which resources exist via `vault kv list`; remove
  # if that turns out to be more surface than we want.
  path "secret/metadata/${local.resources_kv_prefix}/*" {
    capabilities = ["read", "list", "delete"]
  }

  # Service-only runtime secrets (DB URL, Zitadel introspection secret + broker
  # PAT, SMTP creds) rendered into the runtime.env template at alloc start. Only
  # the mekhan-service role gets this — the executor never reads them.
  path "secret/data/${local.svc_secrets_path}/runtime" {
    capabilities = ["read"]
  }
  path "secret/metadata/${local.svc_secrets_path}/runtime" {
    capabilities = ["read"]
  }
  EOT
}

# ── Policy 3 — Cubbyhole response wrapping ───────────────────────────────────
# Engine needs `update` on sys/wrapping/wrap to mint single-use tokens that
# carry resolved secrets onto NATS. Companion `sys/wrapping/unwrap` is NOT
# granted: the executor unwraps using the wrapping token itself (no Vault
# token at all on the executor side — that's the whole point of cubbyhole).
resource "vault_policy" "mekhan_wrap" {
  name = local.vault_policy_wrap

  policy = <<-EOT
  path "sys/wrapping/wrap" {
    capabilities = ["update"]
  }
  EOT
}

# ── Role 1 — mekhan-service (mekhan + engine tasks) ──────────────────────────
# Binds the `mekhan-service` Nomad job identity to the three policies it
# needs: NATS creds + resource store + response-wrapping.
resource "vault_jwt_auth_backend_role" "mekhan_service" {
  backend   = "jwt-nomad"
  role_name = local.vault_role_service
  role_type = "jwt"

  bound_audiences = ["vault.io"]
  bound_claims = {
    nomad_namespace = var.nomad_namespace
    nomad_job_id    = local.service_job_id
  }

  user_claim              = "nomad_job_id"
  user_claim_json_pointer = false

  token_type   = "service"
  token_period = 1800
  token_policies = [
    "nomad-workloads",
    vault_policy.mekhan_nats_read.name,
    vault_policy.mekhan_resources_rw.name,
    vault_policy.mekhan_wrap.name,
  ]
}

# ── Role 2 — executor (minimum-privilege) ────────────────────────────────────
# Executor only needs to render the NATS .creds bundle. Its unwrap path uses
# the wrapping token itself as auth, NOT a Vault token; so this role is
# deliberately narrower than the mekhan-service role.
resource "vault_jwt_auth_backend_role" "mekhan_executor" {
  backend   = "jwt-nomad"
  role_name = local.vault_role_executor
  role_type = "jwt"

  bound_audiences = ["vault.io"]
  bound_claims = {
    nomad_namespace = var.nomad_namespace
    nomad_job_id    = local.executor_job_id
  }

  user_claim              = "nomad_job_id"
  user_claim_json_pointer = false

  token_type   = "service"
  token_period = 1800
  token_policies = [
    "nomad-workloads",
    vault_policy.mekhan_nats_read.name,
  ]
}

# =============================================================================
# Runtime secret KVs — the values the jobspec used to bake into `env {}`
# =============================================================================
# Written here by Terraform so the secret VALUES live only in Vault (+ tfstate)
# and the rendered Nomad job carries just the read PATHS. The mekhan/executor
# tasks render them via `template { env = true; ... }` at alloc start. This is
# the web-platform pattern (deploy/modules/database-credentials/main.tf).
#
# Split by audience: `runtime` is service-only; `storage` is read by both the
# service and the executor (see the policy grants above).

resource "vault_kv_secret_v2" "mekhan_runtime" {
  mount = "secret"
  name  = "${local.svc_secrets_path}/runtime"

  data_json = jsonencode({
    database_url                = local.database_url
    introspection_client_secret = zitadel_application_api.introspect.client_secret
    broker_pat                  = zitadel_personal_access_token.token_broker.token
    smtp_username               = var.email_smtp_username
    smtp_password               = var.email_smtp_password
  })
}

resource "vault_kv_secret_v2" "mekhan_storage" {
  mount = "secret"
  name  = "${local.svc_secrets_path}/storage"

  data_json = jsonencode({
    s3_access_key = var.s3_access_key
    s3_secret_key = var.s3_secret_key
  })
}
