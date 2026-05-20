#!/usr/bin/env bash
# =============================================================================
# Bootstrap or rotate the mekhan-dev NATS account + user on the shared
# HetznerCluster NATS broker.
# =============================================================================
# Mints a single user (`mekhan-dev-worker`) under the `mekhan-dev` account and
# publishes its .creds bundle to Vault at secret/nats/apps/mekhan/dev/worker.
# Mekhan's Nomad jobspec (deploy/dev/mekhan.nomad.hcl.tpl) reads that path via
# a `template` stanza at alloc start. The matching Vault policy + JWT auth
# role are owned by deploy/dev/nats.tf — `tofu apply` creates them, this
# script populates the secret they grant access to.
#
# When to run:
#   - Once, before the first `tofu apply` (the Nomad template renders the
#     creds bundle from Vault; without it the alloc never starts).
#   - Whenever you want to rotate the user's creds (the helper deletes and
#     recreates the user, then rewrites the Vault entry).
#
# Required env (CI sets both at the workflow level in 40-deploy.yml):
#   VAULT_ADDR   — e.g. http://10.20.0.20:8200
#   VAULT_TOKEN  — read+write on secret/nats/apps/mekhan/dev/* and read on
#                  secret/nats/cluster + secret/nats/system/resolver
#
# Required local CLIs: nsc, vault, jq.
#
# Mirrors web-platform/deploy/dev/scripts/generate-nats-users.sh; kept thin so
# the canonical logic stays in the vendored generate-lab-user.sh.
# =============================================================================

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
HELPER_SCRIPT="${SCRIPT_DIR}/generate-lab-user.sh"

if [[ ! -x "${HELPER_SCRIPT}" ]]; then
  echo "error: helper script not found or not executable at ${HELPER_SCRIPT}" >&2
  exit 1
fi

: "${VAULT_ADDR:?VAULT_ADDR must be set}"
: "${VAULT_TOKEN:?VAULT_TOKEN must be set}"

# Account + user identifiers. Keep in sync with deploy/dev/nats.tf (the JWT
# role's bound_claims) and deploy/dev/mekhan.nomad.hcl.tpl (the template
# stanza's Vault path). Changing any of these is a coordinated edit across
# all three.
ACCOUNT_NAME="mekhan-dev"
USER_NAME="mekhan-dev-worker"
ACCOUNT_VAULT_PATH="secret/nats/apps/mekhan/dev/account"
USER_VAULT_PATH="secret/nats/apps/mekhan/dev/worker"

# Direct IP rather than `nats.service.consul:4222`: NATS rejects the auth
# handshake when `nsc push` runs against the Consul DNS name from outside the
# cluster mesh (something about the cluster gossip URLs returned in INFO
# confuses the client's reconnect path). Direct IP bypasses that. Any of the
# 3 NATS nodes' static IPs works — 10.20.2.10 is the first one.
NATS_URL="${NATS_URL:-10.20.2.10:4222}"

# JetStream provisioning — mekhan-service needs JS KV buckets for catalogue
# subscriptions, petri-net lifecycle, timers, and the global event stream
# (~4 streams minimum, more after engine integration). The 08a helper
# script's defaults (4 streams / 16 consumers / 512Mi mem / 5Gi disk) were
# sized for the lab example, too tight for mekhan. The helper re-applies
# these limits on every invocation, so re-runs don't silently drop JS.
ACCOUNT_ENABLE_JS="true"
ACCOUNT_JS_MEM="256M"
ACCOUNT_JS_STORAGE="2G"
ACCOUNT_JS_STREAMS="20"
ACCOUNT_JS_CONSUMERS="200"

echo "==> Generating credentials for ${USER_NAME} (${USER_VAULT_PATH})"

ACCOUNT_NAME="${ACCOUNT_NAME}" \
ACCOUNT_VAULT_PATH="${ACCOUNT_VAULT_PATH}" \
USER_NAME="${USER_NAME}" \
VAULT_PATH="${USER_VAULT_PATH}" \
NATS_URL="${NATS_URL}" \
ACCOUNT_ENABLE_JS="${ACCOUNT_ENABLE_JS}" \
ACCOUNT_JS_MEM="${ACCOUNT_JS_MEM}" \
ACCOUNT_JS_STORAGE="${ACCOUNT_JS_STORAGE}" \
ACCOUNT_JS_STREAMS="${ACCOUNT_JS_STREAMS}" \
ACCOUNT_JS_CONSUMERS="${ACCOUNT_JS_CONSUMERS}" \
"${HELPER_SCRIPT}"

echo "==> NATS user ${USER_NAME} ready. Vault paths:"
echo "       account JWT: ${ACCOUNT_VAULT_PATH}"
echo "       user  creds: ${USER_VAULT_PATH}"
