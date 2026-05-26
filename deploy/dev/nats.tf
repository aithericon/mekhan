# =============================================================================
# NATS — shared identifiers
# =============================================================================
# Companion to deploy/dev/scripts/generate-nats-user.sh, which mints the
# `mekhan-dev-worker` user under the `mekhan-dev` NATS account and publishes
# its .creds bundle to Vault at secret/data/nats/apps/mekhan/dev/worker.
#
# The Vault side (read policy + JWT-Nomad role) moved to deploy/dev/vault.tf
# once Vault became a multi-purpose secret store (NATS creds + Resource KVs
# + response wrapping). The locals below are kept here so the script and the
# template stanzas in mekhan/executor jobspecs continue to reference one
# canonical source for the NATS-side identifiers — they are NOT TF outputs
# because the script runs first, before TF state exists.
# =============================================================================

locals {
  nats_account_name = "mekhan-dev"
  nats_user_name    = "mekhan-dev-worker"
  nats_user_kv_path = "nats/apps/mekhan/dev/worker"
}
