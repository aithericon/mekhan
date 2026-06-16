# =============================================================================
# NATS — shared identifiers (now env-derived)
# =============================================================================
# Companion to scripts/generate-nats-user.sh, which mints the
# `mekhan-<env>-worker` user under the `mekhan-<env>` NATS account and
# publishes its .creds bundle to Vault at
# secret/data/nats/apps/mekhan/<env>/worker.
#
# The canonical NATS-side identifiers (nats_account_name / nats_user_name /
# nats_user_kv_path) live in locals.tf, derived from var.environment, so dev
# and prod get distinct accounts (and therefore distinct JetStream + subject
# namespaces) on the one shared broker. The generate-nats-user.sh script takes
# the env as its argument and must mint the matching account — keep the two in
# sync. They are NOT TF resources/outputs because the script runs first,
# before this layer's state exists.
#
# The Vault side (read policy + JWT-Nomad role) lives in vault.tf.
# =============================================================================
