#!/usr/bin/env bash
# Shared secret loader for mekhan deploy environments — provides the cluster
# bootstrap Vault token via rbw (Vaultwarden). Sourced by deploy/{dev,prod}/.envrc.
# Not a template: the file IS the loader.
#
# mekhan's dev + prod share ONE HetznerCluster (and one Vault), so almost every
# secret is read FROM Vault by the .envrc (secret/postgres/patroni,
# secret/docker/registry, secret/services/mekhan/${ENV_NAME}, secret/zitadel/iac-jwt).
# The ONLY thing you need before Vault works is the Vault token itself — that is
# what this loader provides. Mirrors deploy/secrets.sh from web-platform.
#
# The caller MAY export ENV_NAME ("dev" or "prod") before sourcing (the .envrc
# does). It's not needed for the token lookup — dev + prod share ONE cluster, so
# the bootstrap token is a SINGLE rbw item, not per-env.
#
# Workflow:
#   1. The cluster Vault token lives in rbw at
#        clusters/aithericon-prod / vault-root-token
#      (one item for the shared cluster), OR drop it into
#      deploy/${ENV_NAME}/.secrets (gitignored) as `export VAULT_TOKEN=…`.
#   2. `rbw unlock` once per shell session (only if you use rbw).
#   3. `direnv allow` in deploy/dev or deploy/prod.
#
# Local overrides:
#   deploy/${ENV_NAME}/.secrets (gitignored) is sourced by the .envrc AFTER this
#   file, so a plaintext `export VAULT_TOKEN=…` there always wins over rbw.

# Single rbw location for the shared-cluster Vault token (overridable). dev +
# prod are one HetznerCluster + one Vault, so there is only one token value.
RBW_VAULT_FOLDER="${RBW_VAULT_FOLDER:-clusters/aithericon-prod}"
RBW_VAULT_ITEM="${RBW_VAULT_ITEM:-vault-root-token}"

# =============================================================================
# Cluster Vault token (the single bootstrap secret; everything else lives in
# Vault and is resolved by the .envrc). Only fetch it when not already exported
# so .secrets / the ambient shell env win. Graceful — if rbw is locked / missing
# / the item is absent, the lookup returns empty and .secrets fills the gap.
# =============================================================================
if [[ -z "${VAULT_TOKEN:-}" ]] && command -v rbw >/dev/null 2>&1; then
  export VAULT_TOKEN="$(rbw get --folder "$RBW_VAULT_FOLDER" "$RBW_VAULT_ITEM" 2>/dev/null || true)"
fi
