# =============================================================================
# NATS access for mekhan-service — Vault policy + Nomad workload-identity role
# =============================================================================
# Companion to deploy/dev/scripts/generate-nats-user.sh:
#
#   - The script (run out-of-band: CI step + manual rotation) mints the
#     `mekhan-dev-worker` user under the `mekhan-dev` NATS account and
#     publishes its .creds bundle to Vault at
#     secret/nats/apps/mekhan/dev/worker.
#
#   - This file declares the Vault side: a read-only policy on that path,
#     plus a JWT auth role on the cluster's `jwt-nomad` backend that binds
#     mekhan-service's Nomad workload identity to that policy.
#
# Mekhan's jobspec (deploy/dev/mekhan.nomad.hcl.tpl) consumes both via:
#     vault { policies = ["nomad-workloads", "mekhan-dev"]; role = "mekhan-dev" }
#     template { data = "{{ with secret \"secret/data/nats/apps/mekhan/dev/worker\" }}..." }
#
# Same pattern web-platform/deploy/dev/main.tf uses for its 6 telemetry users
# (vault_policy.telemetry_nats_user + the iot-platform JWT role in
# deploy/modules/telemetry/main.tf:459).
#
# Identifiers (account/user names, Vault paths) are duplicated between the
# script and this file. Keep them in sync — they are NOT TF outputs because
# the script runs first, before TF state exists.
# =============================================================================

locals {
  nats_account_name = "mekhan-dev"
  nats_user_name    = "mekhan-dev-worker"
  nats_user_kv_path = "nats/apps/mekhan/dev/worker"
  nats_vault_policy = "mekhan-dev"
  nats_vault_role   = "mekhan-dev"

  nats_nomad_job_ids = ["mekhan-service", "engine", "executor"]
}


resource "vault_policy" "mekhan_dev_nats" {
  name = local.nats_vault_policy

  policy = <<-EOT
  path "secret/data/${local.nats_user_kv_path}" {
    capabilities = ["read"]
  }

  path "secret/metadata/${local.nats_user_kv_path}" {
    capabilities = ["read"]
  }
  EOT
}

# Nomad workload-identity → Vault token exchange. When a Nomad alloc for any
# of the `mekhan-service` / `engine` / `executor` jobs presents its JWT to
# Vault's `jwt-nomad` backend, Vault checks the bound_claims; if they match, it
# issues a service token carrying the listed policies. `nomad-workloads`
# is the cluster's default (granted by 03d_nomad_acl); `mekhan_dev_nats`
# is our least-privilege add-on. Token TTL of 30 min with auto-renew is
# what 08a_educational_nats_lab and web-platform's iot-platform role
# both use.
resource "vault_jwt_auth_backend_role" "mekhan_dev_nats" {
  backend   = "jwt-nomad"
  role_name = local.nats_vault_role
  role_type = "jwt"

  bound_audiences = ["vault.io"]
  # `bound_claims` is map(string) in the hashicorp/vault provider; multiple
  # acceptable values per claim are expressed as a comma-separated string
  # which Vault splits server-side into an OR set. List-typed values are
  # rejected at plan time with "string required".
  bound_claims = {
    nomad_namespace = var.nomad_namespace
    nomad_job_id    = join(",", local.nats_nomad_job_ids)
  }

  user_claim              = "nomad_job_id"
  user_claim_json_pointer = false

  token_type     = "service"
  token_period   = 1800
  token_policies = ["nomad-workloads", vault_policy.mekhan_dev_nats.name]
}
