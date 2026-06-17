# =============================================================================
# Headless platform provisioning credentials.
#
# Post platform-tier, the shared `default` (worker) and `model_serving` (runner)
# groups live in the global PLATFORM scope, and minting their registration
# tokens is platform-admin-gated — awkward for automated provisioning. These
# generated secrets remove the manual step entirely:
#
#   * platform_root_token  — a `plat_` bearer that mekhan resolves to a headless
#     platform admin (auth.platform_root_token), for CI/Terraform platform ops.
#   * bootstrap_*_reg_token — full PLATFORM registration tokens. mekhan's startup
#     seeder (MEKHAN__BOOTSTRAP__*) upserts a reusable platform-scoped token whose
#     hash matches each, so the executor / model-pool runners self-enroll using
#     the SAME value with no interactive mint.
#
# All three are stored in the service-only `runtime` Vault secret (rendered onto
# mekhan). The worker bootstrap token is ALSO written to the executor's own Vault
# path (the value the executor enrolls with) so the two always agree. Rotate by
# tainting the random_* resources + `tofu apply` (re-seed revokes the prior
# bootstrap token; mekhan + executor restart on the changed Vault data).
# =============================================================================

# ── platform root token (plat_<secret>) ─────────────────────────────────────
resource "random_password" "platform_root_token" {
  length  = 48
  special = false
}

# ── worker bootstrap registration token (wt_<uuid>.<secret>) ─────────────────
resource "random_uuid" "bootstrap_worker" {}
resource "random_password" "bootstrap_worker" {
  length  = 48
  special = false
}

# ── runner bootstrap registration token (rt_<uuid>.<secret>) ─────────────────
resource "random_uuid" "bootstrap_runner" {}
resource "random_password" "bootstrap_runner" {
  length  = 48
  special = false
}

locals {
  platform_root_token        = "plat_${random_password.platform_root_token.result}"
  bootstrap_worker_reg_token = "wt_${random_uuid.bootstrap_worker.result}.${random_password.bootstrap_worker.result}"
  bootstrap_runner_reg_token = "rt_${random_uuid.bootstrap_runner.result}.${random_password.bootstrap_runner.result}"
}

# The executor reads `worker_reg_token` from this path at alloc start and
# self-enrolls with it — it MUST equal the bootstrap token mekhan seeds. Owning
# it here (instead of a manual `vault kv put`) keeps them in lockstep.
resource "vault_kv_secret_v2" "executor_reg_token" {
  mount = "secret"
  name  = local.executor_reg_token_path

  data_json = jsonencode({
    worker_reg_token = local.bootstrap_worker_reg_token
  })
}
