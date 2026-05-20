#!/usr/bin/env bash
# Helper that creates a NATS account user and publishes the resulting creds
# and account JWTs into Vault.
# Requires: nsc, vault CLI, jq, and an authenticated Vault session.
#
# ── VENDORED COPY ────────────────────────────────────────────────────────────
# Canonical source:
#   HetznerCluster/layers/08a_educational_nats_lab/scripts/generate-lab-user.sh
# Vendored here so mekhan's deploy doesn't need the cluster repo checked out
# next to it. Same pattern web-platform uses
# (web-platform/deploy/08_educational_nats_lab/scripts/generate-lab-user.sh).
# If you fix a bug here, port it back to HetznerCluster too — otherwise the
# next sync overwrites it.
# ─────────────────────────────────────────────────────────────────────────────

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LAB_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

OPERATOR_NAME="${OPERATOR_NAME:-aithericon}"
SYSTEM_NAME="${SYSTEM_NAME:-}"
ACCOUNT_NAME="${ACCOUNT_NAME:-orders-dev}"
USER_NAME="${USER_NAME:-orders-worker}"
VAULT_PATH="${VAULT_PATH:-secret/nats/apps/orders/dev/worker}"
VAULT_OPERATOR_FIELD="${VAULT_OPERATOR_FIELD:-secret/nats/cluster}"
NSC_HOME="${NSC_HOME:-$(mktemp -d)}"
KEEP_NSC_HOME="${KEEP_NSC_HOME:-false}"
NATS_URL="${NATS_URL:-nats.service.consul:4222}"
ACCOUNT_VAULT_PATH="${ACCOUNT_VAULT_PATH:-secret/nats/apps/orders/dev/account}"
ACCOUNT_FORCE_CREATE="${ACCOUNT_FORCE_CREATE:-true}"
ACCOUNT_ENABLE_JS="${ACCOUNT_ENABLE_JS:-true}"
ACCOUNT_JS_MEM="${ACCOUNT_JS_MEM:-512Mi}"
ACCOUNT_JS_STORAGE="${ACCOUNT_JS_STORAGE:-5Gi}"
ACCOUNT_JS_STREAMS="${ACCOUNT_JS_STREAMS:-4}"
ACCOUNT_JS_CONSUMERS="${ACCOUNT_JS_CONSUMERS:-16}"
RESOLVER_VAULT_PATH="${RESOLVER_VAULT_PATH:-secret/nats/system/resolver}"
PRIVATE_KEY_FLAG=()
RESOLVER_USER_NAME=""

cleanup() {
  if [[ "${KEEP_NSC_HOME}" != "true" ]]; then
    rm -rf "${NSC_HOME}"
  fi
}
trap cleanup EXIT

log() {
  echo "[generate-lab-user] $*"
}

require_cmd() {
  command -v "$1" >/dev/null 2>&1 || { echo "Missing required command: $1" >&2; exit 1; }
}

require_cmd nsc
require_cmd vault
require_cmd jq


log "Using NSC_HOME=${NSC_HOME}"
mkdir -p "${NSC_HOME}"

log "Fetching operator and system account material from Vault (${VAULT_OPERATOR_FIELD})"
OPERATOR_SECRET_JSON="$(vault kv get -format=json "${VAULT_OPERATOR_FIELD}")"

echo "${OPERATOR_SECRET_JSON}" | jq -r '.data.data.operator.jwt' >"${NSC_HOME}/operator.jwt"
echo "${OPERATOR_SECRET_JSON}" | jq -r '.data.data.operator.seed // empty' >"${NSC_HOME}/operator.seed"
echo "${OPERATOR_SECRET_JSON}" | jq -r '.data.data.system_account.jwt' >"${NSC_HOME}/system.jwt"

if [[ -z "${SYSTEM_NAME}" ]]; then
  SYSTEM_NAME="$(echo "${OPERATOR_SECRET_JSON}" | jq -r '.data.data.system_account.name // empty')"
  if [[ -z "${SYSTEM_NAME}" || "${SYSTEM_NAME}" == "null" ]]; then
    SYSTEM_NAME="${OPERATOR_NAME}-system"
  fi
fi


ACCOUNT_JWT_FILE="${NSC_HOME}/account.jwt"
ACCOUNT_JWT_SOURCE=$(vault kv get -format=json "${ACCOUNT_VAULT_PATH}" 2>/dev/null || true)
if [[ -n "${ACCOUNT_JWT_SOURCE}" ]]; then
  echo "${ACCOUNT_JWT_SOURCE}" | jq -r '.data.data.jwt // empty' >"${ACCOUNT_JWT_FILE}"
fi

RESOLVER_SECRET_SOURCE=$(vault kv get -format=json "${RESOLVER_VAULT_PATH}" 2>/dev/null || true)
if [[ -z "${RESOLVER_SECRET_SOURCE}" ]]; then
  log "ERROR: resolver credentials not present at ${RESOLVER_VAULT_PATH}. Re-run environments/04b_nats_config to publish the resolver sync user."
  exit 1
fi

RESOLVER_USER_NAME=$(echo "${RESOLVER_SECRET_SOURCE}" | jq -r '.data.data.name // ""')
echo "${RESOLVER_SECRET_SOURCE}" | jq -r '.data.data.jwt // empty' >"${NSC_HOME}/resolver-user.jwt"
echo "${RESOLVER_SECRET_SOURCE}" | jq -r '.data.data.creds // empty' >"${NSC_HOME}/resolver-user.creds"

if [[ -z "${RESOLVER_USER_NAME}" || ! -s "${NSC_HOME}/resolver-user.creds" ]]; then
  log "ERROR: resolver credentials incomplete in ${RESOLVER_VAULT_PATH}; aborting."
  exit 1
fi

# Initialise operator/account inside the workspace.
# `nsc env -o` returns non-zero when the operator isn't known yet (fresh
# workspace, or stores that nsc 2.12+ resolves outside NSC_HOME), so we
# tolerate that here — the import step right after will populate it, and
# we re-run `nsc env -o` afterwards to switch context.
log "Seeding NSC workspace with existing operator/account"
nsc env -o "${OPERATOR_NAME}" >/dev/null 2>&1 || true
# nsc 2.x renamed `import operator` → `add operator --url`. The `--url`
# flag takes a plain local path (no file:// prefix — nsc treats that as
# a relative path and fails). --force makes it idempotent on re-runs.
nsc describe operator --name "${OPERATOR_NAME}" --raw >/dev/null 2>&1 || nsc add operator --url "${NSC_HOME}/operator.jwt" --force >/dev/null
nsc env -o "${OPERATOR_NAME}" >/dev/null 2>&1 || true
if [[ -s "${NSC_HOME}/operator.seed" ]]; then
SEED_VALUE="$(tr -d '\r\n ' < "${NSC_HOME}/operator.seed")"
if [[ -n "${SEED_VALUE}" && "${SEED_VALUE}" != "null" ]]; then
  echo "${SEED_VALUE}" > "${NSC_HOME}/operator.seed.cleaned"
PRIVATE_KEY_FLAG=(--private-key "${NSC_HOME}/operator.seed.cleaned")
else
  log "ERROR: operator seed present but empty in ${VAULT_OPERATOR_FIELD}; rerun environments/04b_nats_config."
  exit 1
fi
else
  log "ERROR: operator seed missing in ${VAULT_OPERATOR_FIELD}; rerun environments/04b_nats_config."
  exit 1
fi
nsc describe account --name "${SYSTEM_NAME}" --raw >/dev/null 2>&1 || nsc import account --file "${NSC_HOME}/system.jwt" >/dev/null
nsc env -a "${SYSTEM_NAME}" >/dev/null 2>&1 || true

if ! nsc import user --file "${NSC_HOME}/resolver-user.creds" --overwrite >/dev/null 2>&1; then
  log "ERROR: failed to import resolver user credentials from ${RESOLVER_VAULT_PATH}; rerun environments/04b_nats_config."
  exit 1
fi
nsc describe account --name "${ACCOUNT_NAME}" --raw >/dev/null 2>&1 || {
  if [[ -s "${ACCOUNT_JWT_FILE}" ]]; then
    log "Importing existing account JWT for ${ACCOUNT_NAME} from Vault."
    nsc import account --file "${ACCOUNT_JWT_FILE}" >/dev/null
  elif [[ "${ACCOUNT_FORCE_CREATE}" == "true" ]] || [[ "${ACCOUNT_NAME}" != "aithericon-system" ]]; then
    log "Account ${ACCOUNT_NAME} not found; creating."
    # PRIVATE_KEY_FLAG points at the operator MAIN seed (from Vault). Without
    # it, nsc auto-generates an operator signing key for itself and signs the
    # account JWT with it — but that signing key isn't declared in the
    # operator JWT's `signing_keys` list, so NATS will later reject the
    # account JWT as untrusted. Always sign with the main key.
    nsc add account --name "${ACCOUNT_NAME}" "${PRIVATE_KEY_FLAG[@]}" >/dev/null
  else
    log "Account ${ACCOUNT_NAME} not found and ACCOUNT_FORCE_CREATE=false; importing system account from Vault."
    nsc import account --file "${NSC_HOME}/system.jwt" >/dev/null
  fi
}

nsc env -a "${ACCOUNT_NAME}" >/dev/null 2>&1

# Apply JetStream limits idempotently — runs whether the account was just
# created or re-imported from Vault. The previous version of this script
# put a JS-enable block inside the `nsc add account` branch only, which
# meant re-applies (where the account already exists in Vault) silently
# skipped JS provisioning. mekhan-service then crashed with
# `JetStream not enabled for account` despite the layer applying clean.
#
# nsc 2.x notes:
#   - Setting any --js-* limit implicitly enables JetStream. No separate
#     `--js-enable` flag (that was nsc 0.x).
#   - Flag renames vs older nsc:
#       --js-file-storage  →  --js-disk-storage
#       --js-consumers     →  --js-consumer  (singular)
#   - Must pass PRIVATE_KEY_FLAG to re-sign with the operator MAIN key;
#     otherwise nsc auto-creates a signing key that the operator JWT
#     doesn't trust and the account becomes unauth-able.
if [[ "${ACCOUNT_ENABLE_JS}" == "true" ]]; then
  log "Ensuring JetStream limits on ${ACCOUNT_NAME} (mem=${ACCOUNT_JS_MEM}, disk=${ACCOUNT_JS_STORAGE}, streams=${ACCOUNT_JS_STREAMS}, consumer=${ACCOUNT_JS_CONSUMERS})"
  nsc edit account --name "${ACCOUNT_NAME}" \
    --js-mem-storage  "${ACCOUNT_JS_MEM}" \
    --js-disk-storage "${ACCOUNT_JS_STORAGE}" \
    --js-streams      "${ACCOUNT_JS_STREAMS}" \
    --js-consumer     "${ACCOUNT_JS_CONSUMERS}" \
    "${PRIVATE_KEY_FLAG[@]}" >/dev/null
fi

# Create or rotate the user
if nsc describe user --account "${ACCOUNT_NAME}" --name "${USER_NAME}" >/dev/null 2>&1; then
  log "User ${USER_NAME} already exists; rotating credentials."
  nsc delete user --account "${ACCOUNT_NAME}" --name "${USER_NAME}" --rm-creds --rm-nkey >/dev/null
fi

nsc add user \
  --account "${ACCOUNT_NAME}" \
  --name "${USER_NAME}" \
  --allow-pub ">" \
  --allow-sub ">" \
  >/dev/null

# Export creds
CREDS_FILE="$(mktemp)"
nsc generate creds \
  --account "${ACCOUNT_NAME}" \
  --name "${USER_NAME}" \
  --output-file "${CREDS_FILE}" >/dev/null

# Capture account metadata
ACCOUNT_JWT=$(nsc describe account --name "${ACCOUNT_NAME}" --raw)
ACCOUNT_INFO_JSON=$(nsc describe account --name "${ACCOUNT_NAME}" --json 2>/dev/null || echo "{}")
ACCOUNT_PUBLIC_KEY=$(echo "${ACCOUNT_INFO_JSON}" | jq -r '.sub // .iss // empty')
USER_INFO_JSON=$(nsc describe user --account "${ACCOUNT_NAME}" --name "${USER_NAME}" --json 2>/dev/null || echo "{}")
USER_PUBLIC_KEY=$(echo "${USER_INFO_JSON}" | jq -r '.sub // empty')

# Publish to Vault
log "Storing account JWT at vault:${ACCOUNT_VAULT_PATH}"
vault kv put "${ACCOUNT_VAULT_PATH}" \
  name="${ACCOUNT_NAME}" \
  public_key="${ACCOUNT_PUBLIC_KEY}" \
  jwt="${ACCOUNT_JWT}" >/dev/null
log "Account JWT ready: vault kv get ${ACCOUNT_VAULT_PATH}"

log "Storing user credentials at vault:${VAULT_PATH}"
vault kv put "${VAULT_PATH}" \
  account="${ACCOUNT_NAME}" \
  name="${USER_NAME}" \
  public_key="${USER_PUBLIC_KEY}" \
  creds=@"${CREDS_FILE}" >/dev/null
log "User creds ready: vault kv get ${VAULT_PATH}"

log "Done. Creds written to ${CREDS_FILE} (not removed). Set KEEP_NSC_HOME=true to retain nsc artifacts."

PUSH_TARGET="${NATS_URL}"
if [[ "${PUSH_TARGET}" != nats://* ]]; then
  PUSH_TARGET="nats://${PUSH_TARGET}"
fi
log "Pushing account ${ACCOUNT_NAME} to NATS resolver at ${PUSH_TARGET}"
push_cmd=(nsc push --account "${ACCOUNT_NAME}" --account-jwt-server-url "${PUSH_TARGET}" --system-account "${SYSTEM_NAME}" --system-user "${RESOLVER_USER_NAME}")
push_cmd+=("${PRIVATE_KEY_FLAG[@]}")
if ! push_output=$("${push_cmd[@]}" 2>&1); then
  log "ERROR: account push failed"
  printf '%s\n' "${push_output}" >&2
  exit 1
fi
[[ -n "${push_output}" ]] && printf '%s\n' "${push_output}"
log "Resolver push complete."
