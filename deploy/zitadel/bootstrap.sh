#!/usr/bin/env bash
# Bootstrap the local Zitadel container for Mekhan.
#
# What this does (idempotent):
#   1. Waits for Zitadel and reads the auto-generated admin PAT.
#   2. Creates a project "Mekhan" (skipped if it already exists).
#   3. Creates an SPA OIDC application bound to it.
#   4. Writes the resulting client_id and audience to:
#        - app/.env.local                (frontend SPA)
#        - service/mekhan.local.toml     (backend mekhan-service)
#
# Re-runnable: existing project / app are detected and reused.
#
# Requires: curl, jq.

set -euo pipefail

ZITADEL_HOST="${ZITADEL_HOST:-http://localhost:8080}"
PAT_FILE="${PAT_FILE:-deploy/zitadel/pat/zitadel-admin-sa.pat}"

PROJECT_NAME="${PROJECT_NAME:-Mekhan}"
APP_NAME="${APP_NAME:-Mekhan SPA}"
REDIRECT_URI="${REDIRECT_URI:-http://localhost:5173/auth/callback}"
POST_LOGOUT_URI="${POST_LOGOUT_URI:-http://localhost:5173}"

FRONTEND_ENV_FILE="${FRONTEND_ENV_FILE:-app/.env.local}"
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

ISSUER="$ZITADEL_HOST"
AUDIENCE="$client_id"

# ── write frontend env ───────────────────────────────────────────────────────
mkdir -p "$(dirname "$FRONTEND_ENV_FILE")"
cat > "$FRONTEND_ENV_FILE" <<EOF
# Generated by deploy/zitadel/bootstrap.sh — re-run to refresh.
VITE_AUTH_MODE=zitadel
VITE_AUTH_ISSUER_URL=$ISSUER
VITE_AUTH_CLIENT_ID=$client_id
VITE_AUTH_REDIRECT_URI=$REDIRECT_URI
VITE_AUTH_POST_LOGOUT_URI=$POST_LOGOUT_URI
VITE_AUTH_SCOPE=openid profile email offline_access
EOF
echo "Wrote $FRONTEND_ENV_FILE"

# ── write backend config ─────────────────────────────────────────────────────
for cfg in "${BACKEND_CONFIG_FILES[@]}"; do
  mkdir -p "$(dirname "$cfg")"
  cat > "$cfg" <<EOF
# Generated by deploy/zitadel/bootstrap.sh — re-run to refresh.
# Backend reads this in addition to mekhan.toml when running locally.

[auth]
mode = "zitadel"
issuer_url = "$ISSUER"
audience = "$AUDIENCE"
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
  cd service && cargo run            # picks up service/mekhan.local.toml
  cd app && pnpm dev                  # picks up app/.env.local
────────────────────────────────────────────────────────────
EOF
