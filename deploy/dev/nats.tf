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
# =============================================================================

locals {
  nats_account_name = "mekhan-dev"
  nats_user_name    = "mekhan-dev-worker"
  nats_user_kv_path = "nats/apps/mekhan/dev/worker"
  nats_vault_policy = "mekhan-dev"
  nats_vault_role   = "mekhan-dev"

  nats_nomad_job_ids = ["mekhan-service", "executor"]
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


resource "vault_jwt_auth_backend_role" "mekhan_dev_nats" {
  backend   = "jwt-nomad"
  role_name = local.nats_vault_role
  role_type = "jwt"

  bound_audiences = ["vault.io"]

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
