# =============================================================================
# Env-derived names — the isolation boundary on the shared cluster
# =============================================================================
# deploy/dev and deploy/prod run against the SAME HetznerCluster. Every
# cluster-shared identifier below is suffixed with var.environment so the two
# deployments never collide on a Nomad job ID, Consul service, Traefik router,
# Postgres database, NATS account, Vault role, or Zitadel org.
#
# These two directories' *.tf files are kept byte-identical on purpose — the
# ONLY files that differ between dev and prod are backend.tf (state key),
# *.auto.tfvars (values), and the lock file. All env divergence flows from
# var.environment through this one block, so the layers can't silently drift.
# Diff the two dirs to convince yourself: `diff deploy/dev deploy/prod`.
# =============================================================================

locals {
  env = var.environment

  # ── Nomad job IDs ──────────────────────────────────────────────────────────
  # The hard isolation boundary: two distinct job IDs = two independent jobs on
  # the one cluster. These also feed the Vault JWT role bound_claims below.
  service_job_id  = "mekhan-service-${local.env}"
  executor_job_id = "executor-${local.env}"

  # ── Consul services + Traefik routers ─────────────────────────────────────
  # Distinct service names so Consul Catalog lists them separately; distinct
  # Traefik router ids so the two hostnames don't clobber each other's routes.
  service_consul_name = "mekhan-service-${local.env}"
  engine_consul_name  = "engine-${local.env}"
  traefik_router      = "mekhan-${local.env}"
  traefik_router_http = "mekhan-${local.env}-http"
  engine_router       = "engine-${local.env}"

  # Each env's mekhan talks to its OWN env's engine (same alloc, env-suffixed
  # Consul name). Derived so the engine port + service name can never drift
  # apart. An explicit var.petri_lab_url still wins if set (escape hatch).
  petri_lab_url = coalesce(
    var.petri_lab_url,
    "http://${local.engine_consul_name}.service.consul:${var.engine_service_port}",
  )

  # ── Postgres ───────────────────────────────────────────────────────────────
  db_name = "mekhan_${local.env}"

  # ── NATS (shared broker, isolated accounts) ───────────────────────────────
  # Separate account per env ⇒ separate JetStream + subject namespace, so prod
  # workers can never pick up dev work even though the NATS server is shared.
  # Keep in sync with scripts/generate-nats-user.sh (run once per env).
  nats_account_name = "mekhan-${local.env}"
  nats_user_name    = "mekhan-${local.env}-worker"
  nats_user_kv_path = "nats/apps/mekhan/${local.env}/worker"

  # ── Vault ──────────────────────────────────────────────────────────────────
  # Resource-secret prefix stays shared: the service hardcodes it, and per-env
  # workspace UUIDs (separate DBs) already namespace the payloads. Everything
  # else is env-scoped.
  resources_kv_prefix = "aithericon/resources"
  svc_secrets_path    = "services/mekhan/${local.env}"

  vault_policy_nats_read    = "mekhan-${local.env}-nats-read"
  vault_policy_resources_rw = "mekhan-${local.env}-resources-rw"
  vault_policy_wrap         = "mekhan-${local.env}-wrap"
  vault_role_service        = "mekhan-service-${local.env}"
  vault_role_executor       = "executor-${local.env}"

  service_vault_policies = [
    "nomad-workloads",
    local.vault_policy_nats_read,
    local.vault_policy_resources_rw,
    local.vault_policy_wrap,
  ]
  executor_vault_policies = [
    "nomad-workloads",
    local.vault_policy_nats_read,
  ]

  # Worker-pool enrollment token the executor self-enrolls with (env-scoped).
  executor_reg_token_path = "services/mekhan/${local.env}/executor"
}
