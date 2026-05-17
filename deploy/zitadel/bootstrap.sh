#!/usr/bin/env bash
# Bootstrap the local Zitadel container for Mekhan.
#
# What this does (idempotent):
#   1. Waits for Zitadel and reads the auto-generated admin PAT.
#   2. Creates a project "Mekhan" (skipped if it already exists).
#   3. Creates an SPA OIDC application bound to it (public USER_AGENT client
#      + PKCE — the BFF runs the code+PKCE flow server-side; no client secret).
#   4. Writes the resulting client_id/audience to the backend config only:
#        - mekhan.local.toml / service/mekhan.local.toml  (mekhan-service)
#      The BFF model means the SPA holds NO auth config — it only calls
#      GET /api/auth/session — so no frontend env file is written.
#
# Re-runnable: existing project / app are detected and reused.
#
# Requires: curl, jq.

set -euo pipefail

ZITADEL_HOST="${ZITADEL_HOST:-http://localhost:8080}"
PAT_FILE="${PAT_FILE:-deploy/zitadel/pat/zitadel-admin-sa.pat}"

PROJECT_NAME="${PROJECT_NAME:-Mekhan}"
APP_NAME="${APP_NAME:-Mekhan SPA}"
# BFF: the OIDC callback is handled by the Rust service, not a SPA route.
REDIRECT_URI="${REDIRECT_URI:-http://localhost:5173/api/auth/callback}"
POST_LOGOUT_URI="${POST_LOGOUT_URI:-http://localhost:5173/}"

# cargo's CWD depends on how you invoke it — from workspace root it picks up
# ./mekhan.local.toml; from inside service/ it picks up ./mekhan.local.toml
# there. Write both so either workflow works without re-running bootstrap.
BACKEND_CONFIG_FILES=(
  "mekhan.local.toml"
  "service/mekhan.local.toml"
)

# ── prerequisites ────────────────────────────────────────────────────────────
for cmd in curl jq; do
  if ! command -v "$cmd" >/dev/null; then
    echo "error: '$cmd' is required" >&2
    exit 1
  fi
done

# ── wait for Zitadel + PAT ───────────────────────────────────────────────────
echo "Waiting for Zitadel at $ZITADEL_HOST..."
until curl -fsS "$ZITADEL_HOST/debug/ready" >/dev/null 2>&1; do
  sleep 2
done

echo "Waiting for admin PAT at $PAT_FILE..."
for _ in $(seq 1 60); do
  if [ -s "$PAT_FILE" ]; then break; fi
  sleep 1
done
if [ ! -s "$PAT_FILE" ]; then
  echo "error: PAT not written to $PAT_FILE — is the zitadel container running?" >&2
  exit 1
fi
PAT="$(cat "$PAT_FILE")"

api() {
  local method="$1" path="$2"
  if [ "$#" -ge 3 ]; then
    curl -fsS -X "$method" \
      -H "Authorization: Bearer $PAT" \
      -H "Content-Type: application/json" \
      -d "$3" "$ZITADEL_HOST$path"
  else
    curl -fsS -X "$method" \
      -H "Authorization: Bearer $PAT" \
      "$ZITADEL_HOST$path"
  fi
}

# Variant that tolerates 4xx/5xx and returns the body so we can inspect errors.
api_soft() {
  local method="$1" path="$2"
  if [ "$#" -ge 3 ]; then
    curl -sS -X "$method" \
      -H "Authorization: Bearer $PAT" \
      -H "Content-Type: application/json" \
      -d "$3" "$ZITADEL_HOST$path"
  else
    curl -sS -X "$method" \
      -H "Authorization: Bearer $PAT" \
      "$ZITADEL_HOST$path"
  fi
}

# ── grant IAM_LOGIN_CLIENT to the machine admin ──────────────────────────────
# Zitadel v4's split login UI calls OIDCService/CreateCallback under the
# service user's identity. That endpoint requires IAM_LOGIN_CLIENT in addition
# to IAM_OWNER — adding it explicitly here is idempotent.
echo "Ensuring machine admin has IAM_LOGIN_CLIENT…"
sa_search=$(jq -nc --arg name "zitadel-admin-sa" \
  '{queries: [{userNameQuery: {userName: $name, method: "TEXT_QUERY_METHOD_EQUALS"}}]}')
sa_user_id=$(api_soft POST /v2/users "$sa_search" | jq -r '.result[0].userId // empty')
if [ -z "$sa_user_id" ]; then
  echo "warning: machine admin 'zitadel-admin-sa' not found — skipping role grant"
else
  member_payload=$(jq -nc --arg uid "$sa_user_id" \
    '{userId: $uid, roles: ["IAM_OWNER", "IAM_LOGIN_CLIENT"]}')
  # PUT idempotently sets roles; POST fails if member already exists.
  api_soft PUT "/admin/v1/members/$sa_user_id" "$member_payload" >/dev/null || \
    api_soft POST /admin/v1/members "$member_payload" >/dev/null
  echo "Machine admin $sa_user_id has IAM_OWNER + IAM_LOGIN_CLIENT"
fi

# ── reset human admin password ───────────────────────────────────────────────
# FirstInstance YAML set this, but the value can be silently dropped on some
# Zitadel versions (e.g. when complexity rules reject it). Re-set via API so
# the documented password actually works.
echo "Resetting human admin password to documented value…"
human_search=$(jq -nc --arg name "zitadel-admin" \
  '{queries: [{userNameQuery: {userName: $name, method: "TEXT_QUERY_METHOD_EQUALS"}}]}')
human_user_id=$(api_soft POST /v2/users "$human_search" | jq -r '.result[0].userId // empty')
if [ -n "$human_user_id" ]; then
  pw_payload=$(jq -nc '{newPassword: {password: "Password1!", changeRequired: false}}')
  api_soft POST "/v2/users/$human_user_id/password" "$pw_payload" >/dev/null
  echo "Password reset for $human_user_id"
fi

# ── ensure project ───────────────────────────────────────────────────────────
echo "Looking up project '$PROJECT_NAME'..."
search_payload=$(jq -nc --arg name "$PROJECT_NAME" \
  '{queries: [{nameQuery: {name: $name, method: "TEXT_QUERY_METHOD_EQUALS"}}]}')
project_id=$(api POST /management/v1/projects/_search "$search_payload" \
  | jq -r '.result[0].id // empty')

if [ -z "$project_id" ]; then
  echo "Creating project '$PROJECT_NAME'..."
  create_payload=$(jq -nc --arg name "$PROJECT_NAME" \
    '{name: $name, projectRoleAssertion: true, projectRoleCheck: false, hasProjectCheck: false}')
  project_id=$(api POST /management/v1/projects "$create_payload" | jq -r '.id')
fi
echo "Project id: $project_id"

# ── ensure OIDC application ──────────────────────────────────────────────────
echo "Looking up OIDC app '$APP_NAME'..."
app_search=$(jq -nc --arg name "$APP_NAME" \
  '{queries: [{nameQuery: {name: $name, method: "TEXT_QUERY_METHOD_EQUALS"}}]}')
app_id=$(api POST "/management/v1/projects/$project_id/apps/_search" "$app_search" \
  | jq -r '.result[0].id // empty')

if [ -z "$app_id" ]; then
  echo "Creating OIDC application '$APP_NAME'..."
  app_payload=$(jq -nc \
    --arg name "$APP_NAME" \
    --arg redirect "$REDIRECT_URI" \
    --arg logout "$POST_LOGOUT_URI" \
    '{
      name: $name,
      redirectUris: [$redirect],
      responseTypes: ["OIDC_RESPONSE_TYPE_CODE"],
      grantTypes: ["OIDC_GRANT_TYPE_AUTHORIZATION_CODE", "OIDC_GRANT_TYPE_REFRESH_TOKEN"],
      appType: "OIDC_APP_TYPE_USER_AGENT",
      authMethodType: "OIDC_AUTH_METHOD_TYPE_NONE",
      postLogoutRedirectUris: [$logout],
      version: "OIDC_VERSION_1_0",
      devMode: true,
      accessTokenType: "OIDC_TOKEN_TYPE_JWT",
      accessTokenRoleAssertion: true,
      idTokenRoleAssertion: true,
      idTokenUserinfoAssertion: true
    }')
  create_resp=$(api POST "/management/v1/projects/$project_id/apps/oidc" "$app_payload")
  app_id=$(echo "$create_resp" | jq -r '.appId')
  client_id=$(echo "$create_resp" | jq -r '.clientId')
else
  echo "Reusing existing app $app_id"
  client_id=$(api GET "/management/v1/projects/$project_id/apps/$app_id" \
    | jq -r '.app.oidcConfig.clientId')
fi

echo "OIDC app id:   $app_id"
echo "OIDC clientId: $client_id"

# ── ensure API application (introspection credential) ────────────────────────
# Mekhan authenticates to Zitadel's RFC 7662 introspection endpoint as this
# confidential API app (HTTP Basic) to validate machine PATs presented by CI
# `mekhan apply`. The client secret is only returned at creation, so an
# existing app's secret is regenerated to keep the written config valid.
API_APP_NAME="${APP_NAME}-introspect"
echo "Looking up API app '$API_APP_NAME'..."
api_app_search=$(jq -nc --arg name "$API_APP_NAME" \
  '{queries: [{nameQuery: {name: $name, method: "TEXT_QUERY_METHOD_EQUALS"}}]}')
api_app_id=$(api POST "/management/v1/projects/$project_id/apps/_search" "$api_app_search" \
  | jq -r '.result[0].id // empty')

if [ -z "$api_app_id" ]; then
  echo "Creating API application '$API_APP_NAME'..."
  api_create=$(api POST "/management/v1/projects/$project_id/apps/api" \
    "$(jq -nc --arg name "$API_APP_NAME" \
        '{name: $name, authMethodType: "API_AUTH_METHOD_TYPE_BASIC"}')")
  api_app_id=$(echo "$api_create" | jq -r '.appId')
  introspect_client_id=$(echo "$api_create" | jq -r '.clientId')
  introspect_client_secret=$(echo "$api_create" | jq -r '.clientSecret')
else
  echo "Reusing API app $api_app_id — regenerating client secret"
  api_regen=$(api POST \
    "/management/v1/projects/$project_id/apps/$api_app_id/api_config/_secret" '{}')
  introspect_client_id=$(echo "$api_regen" | jq -r '.clientId')
  introspect_client_secret=$(echo "$api_regen" | jq -r '.clientSecret')
fi
echo "API app id:    $api_app_id"

# ── ensure token-broker service user ─────────────────────────────────────────
# Backs the embedded /api/auth/tokens feature: Mekhan, authenticated as this
# machine user's PAT, lazily creates one machine user + PAT per token a
# logged-in human mints from /profile. ORG_OWNER lets it manage those users
# within the org. The PAT secret is only returned at creation, so (like the
# API-app secret above) any existing broker PATs are deleted and a fresh one
# minted on every run to keep the written config valid.
BROKER_USERNAME="mekhan-token-broker"
echo "Looking up token-broker service user '$BROKER_USERNAME'..."
broker_search=$(jq -nc --arg name "$BROKER_USERNAME" \
  '{queries: [{userNameQuery: {userName: $name, method: "TEXT_QUERY_METHOD_EQUALS"}}]}')
broker_user_id=$(api_soft POST /v2/users "$broker_search" | jq -r '.result[0].userId // empty')

if [ -z "$broker_user_id" ]; then
  echo "Creating token-broker service user..."
  broker_create=$(api POST /management/v1/users/machine \
    "$(jq -nc --arg name "$BROKER_USERNAME" \
        '{userName: $name, name: "Mekhan Token Broker", description: "Brokers per-user automation PATs for the embedded /api/auth/tokens feature"}')")
  broker_user_id=$(echo "$broker_create" | jq -r '.userId')
fi
echo "Token-broker user id: $broker_user_id"

# Grant ORG_OWNER (manage users/PATs within the org). PUT is idempotent;
# POST is the create path when no membership row exists yet.
echo "Ensuring token-broker has ORG_OWNER…"
broker_member=$(jq -nc --arg uid "$broker_user_id" '{userId: $uid, roles: ["ORG_OWNER"]}')
api_soft PUT "/management/v1/orgs/me/members/$broker_user_id" "$broker_member" >/dev/null || \
  api_soft POST /management/v1/orgs/me/members "$broker_member" >/dev/null

# Re-mint the broker PAT (secret returned once). Delete any existing PATs so
# re-runs converge on exactly one valid token written to config.
echo "Re-minting token-broker PAT…"
existing_pats=$(api_soft POST "/management/v1/users/$broker_user_id/pats/_search" '{}' \
  | jq -r '.result[]?.id // empty')
for tid in $existing_pats; do
  api_soft DELETE "/management/v1/users/$broker_user_id/pats/$tid" >/dev/null || true
done
broker_pat=$(api POST "/management/v1/users/$broker_user_id/pats" '{}' | jq -r '.token')
if [ -z "$broker_pat" ] || [ "$broker_pat" = "null" ]; then
  echo "error: failed to mint token-broker PAT" >&2
  exit 1
fi

ISSUER="$ZITADEL_HOST"
AUDIENCE="$client_id"

# ── write backend config (BFF) ───────────────────────────────────────────────
# No frontend env file: in the BFF model the SPA holds no auth config — the
# Rust service runs the OIDC flow and hands the browser only a session cookie.
for cfg in "${BACKEND_CONFIG_FILES[@]}"; do
  mkdir -p "$(dirname "$cfg")"
  cat > "$cfg" <<EOF
# Generated by deploy/zitadel/bootstrap.sh — re-run to refresh.
# Backend reads this in addition to mekhan.toml when running locally.

[auth]
mode = "bff"
issuer_url = "$ISSUER"
audience = "$AUDIENCE"
client_id = "$client_id"
# Confidential API app Mekhan uses to call Zitadel token introspection
# (RFC 7662) so CI \`mekhan apply\` can authenticate with a service-user PAT.
introspection_client_id = "$introspect_client_id"
introspection_client_secret = "$introspect_client_secret"
# Service-user PAT Mekhan presents to the Zitadel Management API to broker the
# embedded /api/auth/tokens feature (Profile → Access tokens): one machine
# user + PAT per token a logged-in human mints. Re-minted every bootstrap run.
broker_pat = "$broker_pat"
# Local http dev: do not set the Secure cookie attribute (browsers drop
# Secure cookies on plain http). Set true behind https in production.
cookie_secure = false
cors_origins = ["http://localhost:5173"]
EOF
  echo "Wrote $cfg"
done

cat <<EOF

────────────────────────────────────────────────────────────
✓ Zitadel local bootstrap complete.

  Console:        http://localhost:8080/ui/console
  Admin login:    zitadel-admin@mekhan.localhost
  Admin password: Password1!

  Issuer URL:     $ISSUER
  Client ID:      $client_id
  Redirect URI:   $REDIRECT_URI

Next:
  cd service && cargo run            # picks up service/mekhan.local.toml (bff)
  cd app && pnpm dev                 # SPA needs no auth env in the BFF model

  Then open http://localhost:5173 — you'll be redirected to Zitadel to log
  in, and land back signed in with only an HttpOnly session cookie.

GitOps machine token (CI \`mekhan apply\`):
  No Console steps — mint it from inside Mekhan:
    Profile → Access tokens → Create token → copy the secret once.
  Put that token in MEKHAN_CLI_TOKEN locally / the \`mekhan_cli_token\`
  Woodpecker secret. Revoke it from the same page; Zitadel introspection
  reflects the revocation within ~60 s.
────────────────────────────────────────────────────────────
EOF
