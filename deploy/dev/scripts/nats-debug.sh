#!/usr/bin/env bash
# =============================================================================
# nats-debug.sh — one-command, READ-ONLY NATS introspection for the shared
# aithericon broker (the dev and prod mekhan accounts live on one cluster
# behind `wss://nats.aithericon.eu`).
#
# Why this exists: debugging a wedged runner means asking the cluster "how many
# connections does this identity have / what consumers are bound / how many msgs
# are pending" — answered by the system account's `$SYS.REQ.SERVER.PING.*`
# endpoints. Getting a usable system-account identity used to mean hunting Vault
# paths, hand-extracting `.creds`, writing a tempfile, and remembering the
# account public key + request payloads. This script does all of that once.
#
# It NEVER mints or edits a NATS identity — it only READS the already-provisioned
# resolver (system-account) user from Vault and queries monitoring endpoints.
# See `docs/nats-introspection.md` for the full runbook + the least-privilege
# `nats-debug` user we should migrate to (issue #3 in that doc).
#
# Requires in env (deploy/dev/.envrc exports these on the NetBird mesh):
#   VAULT_ADDR, VAULT_TOKEN   (read on secret/nats/system/resolver
#                              + secret/nats/apps/mekhan/<env>/account)
# Requires local CLIs: nats, vault, jq, python3 (JWT decode).
#
# Usage — run from deploy/dev so direnv has loaded VAULT_*:
#   ./scripts/nats-debug.sh setup            # fetch creds + save a `nats` context
#   ./scripts/nats-debug.sh jsz   [dev|prod] # JetStream streams/consumers for the account
#   ./scripts/nats-debug.sh connz [dev|prod] # connections (+ subs) for the account
#   ./scripts/nats-debug.sh acct  [dev|prod] # print the account public key
#   ./scripts/nats-debug.sh req '<subject>' '<json>'   # raw $SYS.REQ passthrough
#
# Env overrides: NATS_DEBUG_URL, NATS_DEBUG_CONTEXT, NATS_DEBUG_REPLIES,
#                RESOLVER_VAULT_PATH, CLUSTER_VAULT_PATH.
# =============================================================================
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
STATE_DIR="${SCRIPT_DIR}/../.dev/nats-debug"            # under deploy/dev/.dev (gitignored)
CREDS_FILE="${STATE_DIR}/system.creds"

NATS_URL="${NATS_DEBUG_URL:-wss://nats.aithericon.eu}"
CONTEXT="${NATS_DEBUG_CONTEXT:-aithericon-sys}"
# R1 (single-replica) JetStream assets live on exactly ONE of the cluster's
# servers, so a PING fans out to all and only one answers with the detail —
# broadcast to a few and merge. 3 covers the current 3-server cluster.
REPLIES="${NATS_DEBUG_REPLIES:-3}"
RESOLVER_VAULT_PATH="${RESOLVER_VAULT_PATH:-secret/nats/system/resolver}"
CLUSTER_VAULT_PATH="${CLUSTER_VAULT_PATH:-secret/nats/cluster}"

: "${VAULT_ADDR:?set VAULT_ADDR — are you in deploy/dev with direnv loaded? (mesh must be up)}"
: "${VAULT_TOKEN:?set VAULT_TOKEN}"
for c in nats vault jq python3; do
  command -v "$c" >/dev/null 2>&1 || { echo "error: missing required CLI: $c" >&2; exit 1; }
done

# --- creds ------------------------------------------------------------------

fetch_creds() {
  mkdir -p "${STATE_DIR}"
  local json creds
  json="$(vault kv get -format=json "${RESOLVER_VAULT_PATH}" 2>/dev/null || true)"
  creds="$(printf '%s' "${json}" | jq -r '.data.data.creds // empty')"
  if [[ -z "${creds}" ]]; then
    # Older layout: the resolver user lived under the cluster bundle.
    json="$(vault kv get -format=json "${CLUSTER_VAULT_PATH}" 2>/dev/null || true)"
    creds="$(printf '%s' "${json}" | jq -r '.data.data.system_resolver_user.creds // empty')"
  fi
  [[ -n "${creds}" ]] || {
    echo "error: no resolver creds at ${RESOLVER_VAULT_PATH} or ${CLUSTER_VAULT_PATH}" >&2
    echo "       (re-publish via environments/04b_nats_config, or check VAULT_TOKEN scope)" >&2
    exit 1
  }
  ( umask 077; printf '%s\n' "${creds}" > "${CREDS_FILE}" )
  chmod 600 "${CREDS_FILE}"
  echo "wrote ${CREDS_FILE} (chmod 600, gitignored)"
}

ensure_creds() { [[ -s "${CREDS_FILE}" ]] || fetch_creds; }

# --- account public key (the `sub` of the env's account JWT) -----------------

account_pubkey() {
  local env="${1}" jwt
  jwt="$(vault kv get -format=json "secret/nats/apps/mekhan/${env}/account" 2>/dev/null \
        | jq -r '.data.data.jwt // empty')"
  [[ -n "${jwt}" ]] || { echo "error: no account JWT at secret/nats/apps/mekhan/${env}/account" >&2; return 1; }
  printf '%s' "${jwt}" | python3 -c '
import sys, base64, json
p = sys.stdin.read().strip().split(".")[1]
p += "=" * (-len(p) % 4)
print(json.loads(base64.urlsafe_b64decode(p))["sub"])'
}

# --- request helpers --------------------------------------------------------

raw_req() {
  ensure_creds
  nats --server "${NATS_URL}" --creds "${CREDS_FILE}" req "$1" "$2" \
       --replies "${REPLIES}" --raw 2>/dev/null
}

setup() {
  fetch_creds
  nats context save "${CONTEXT}" --server "${NATS_URL}" --creds "${CREDS_FILE}" >/dev/null
  echo "saved nats context '${CONTEXT}' -> ${NATS_URL}"
  echo "account public keys:"
  for e in dev prod; do
    printf '  %-4s %s\n' "$e" "$(account_pubkey "$e" 2>/dev/null || echo '(unavailable)')"
  done
  echo
  echo "try:  $0 jsz dev   |   $0 connz dev"
}

jsz() {
  local env="${1:-dev}" acc; acc="$(account_pubkey "${env}")"
  raw_req '$SYS.REQ.SERVER.PING.JSZ' \
    "$(jq -nc --arg a "$acc" '{account:$a,streams:true,consumer:true,config:true}')" \
    | jq -c 'select(.data.account_details)
             | .data.account_details[].stream_detail[]?
             | {stream:.name, msgs:.state.messages, bytes:.state.bytes,
                consumers:[.consumer_detail[]? | {name, pending:.num_pending, waiting:.num_waiting, ack_pending:.num_ack_pending}]}' \
    || { echo "(no JSZ detail — dumping raw; check the jq filter against this shape)" >&2; \
         raw_req '$SYS.REQ.SERVER.PING.JSZ' "$(jq -nc --arg a "$acc" '{account:$a,streams:true,consumer:true}')"; }
}

connz() {
  local env="${1:-dev}" acc; acc="$(account_pubkey "${env}")"
  raw_req '$SYS.REQ.SERVER.PING.CONNZ' \
    "$(jq -nc --arg a "$acc" '{acc:$a,subscriptions:true}')" \
    | jq -c 'select(.data.connections)
             | .data.connections[]
             | {cid, ip, name, start, last_activity,
                in_msgs, out_msgs, subs:.subscriptions_list}' \
    || { echo "(no CONNZ detail — dumping raw)" >&2; \
         raw_req '$SYS.REQ.SERVER.PING.CONNZ' "$(jq -nc --arg a "$acc" '{acc:$a}')"; }
}

cmd="${1:-setup}"; shift || true
case "${cmd}" in
  setup) setup ;;
  fetch) fetch_creds ;;
  acct|account) account_pubkey "${1:-dev}" ;;
  jsz)   jsz "${1:-dev}" ;;
  connz) connz "${1:-dev}" ;;
  req)   raw_req "${1:?usage: req <subject> <json>}" "${2:-{\}}" ;;
  *) echo "usage: $0 {setup|fetch|acct|jsz|connz|req} [env|args]" >&2; exit 2 ;;
esac
