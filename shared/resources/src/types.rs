//! Built-in resource types — v1.
//!
//! Every struct here is a typed credential surface that workflow authors can
//! reference by alias. Each one:
//!
//! - derives `Serialize` + `Deserialize` so it can travel through the public
//!   config blob (`resource_versions.public_config`),
//! - derives `JsonSchema` so the OpenAPI types endpoint and frontend picker
//!   get a schema-driven form for free,
//! - derives `ResourceType` (this crate's macro) so the registry sees it at
//!   link time.
//!
//! **Wire names** (stored in the DB and emitted on the API surface) are
//! locked in here:
//! - `"postgres"`, `"openai"`, `"slack"`, `"s3"`, `"google_oauth"`.
//!
//! These names are hard to change once shipped (DB rows reference them; the
//! workflow YAML embeds them; the frontend filter dropdowns key off them).
//! Renames require a data migration.

use serde::{Deserialize, Serialize};

use crate::ResourceType;

// The `Kv` registration at the bottom of this file submits the descriptor
// manually rather than via the derive (the type has no struct fields).
// It uses `inventory::submit!` directly + builds the schema with
// `serde_json::json!`.
use crate::__macro_support::{inventory, serde_json};

/// Postgres connection credentials. `password` is the only Vault-stored
/// field. `sslmode` defaults to `None` so workflows that don't care about
/// TLS verification don't have to set it.
#[derive(ResourceType, Serialize, Deserialize, schemars::JsonSchema)]
#[resource(name = "postgres", display_name = "Postgres", icon = "lucide-database")]
pub struct Postgres {
    /// Hostname or IP. No URI parsing — keep components separate so the
    /// picker UI can show them individually.
    pub host: String,
    /// Default Postgres port is `5432`; we don't apply that default here so
    /// authors stay deliberate about which port they're talking to.
    pub port: u16,
    pub database: String,
    pub username: String,
    #[resource(secret)]
    pub password: String,
    /// Optional `sslmode` (`require`, `verify-full`, etc.).
    #[serde(default)]
    pub sslmode: Option<String>,
}

/// OpenAI API credentials. Just the key — `base_url` overrides for self-
/// hosted OpenAI-compatible endpoints belong on the workflow side, not in
/// the resource (those are operational policy, not credentials).
#[derive(ResourceType, Serialize, Deserialize, schemars::JsonSchema)]
#[resource(name = "openai", display_name = "OpenAI", icon = "lucide-sparkles")]
pub struct OpenAI {
    #[resource(secret)]
    pub api_key: String,
    /// Optional organization id (`org-...`). Some OpenAI customers need this
    /// to route bills correctly.
    #[serde(default)]
    pub organization: Option<String>,
}

/// Slack webhook target — v1 only supports incoming-webhook posting. Bot-
/// token / OAuth Slack flows land in v2.
#[derive(ResourceType, Serialize, Deserialize, schemars::JsonSchema)]
#[resource(name = "slack", display_name = "Slack (Webhook)", icon = "lucide-slack")]
pub struct Slack {
    /// `https://hooks.slack.com/services/...` — the whole URL is treated as
    /// a secret because the path component carries the auth material.
    #[resource(secret)]
    pub webhook_url: String,
}

/// S3-compatible object storage credentials. Named `S3Resource` to avoid
/// colliding with the SDK's `aws_sdk_s3` types; `name = "s3"` keeps the wire
/// identifier short.
#[derive(ResourceType, Serialize, Deserialize, schemars::JsonSchema)]
#[resource(name = "s3", display_name = "S3", icon = "lucide-archive")]
pub struct S3Resource {
    pub endpoint: String,
    pub region: String,
    pub bucket: String,
    #[resource(secret)]
    pub access_key_id: String,
    #[resource(secret)]
    pub secret_access_key: String,
    /// Path-style addressing (`endpoint/bucket/key`) vs. virtual-hosted
    /// (`bucket.endpoint/key`). MinIO needs `true`; AWS S3 prefers `false`.
    #[serde(default)]
    pub force_path_style: Option<bool>,
}

/// Google OAuth token bundle. Created and refreshed by the OAuth handler
/// (B.11), not by the standard CRUD flow. The presence of an
/// `oauth_provider` attribute steers the picker to render a "Connect Google"
/// button instead of a JSON form.
#[derive(ResourceType, Serialize, Deserialize, schemars::JsonSchema)]
#[resource(
    name = "google_oauth",
    display_name = "Google OAuth",
    icon = "lucide-key-round",
    oauth_provider = "google"
)]
pub struct GoogleOauth {
    /// Bearer token used in `Authorization: Bearer …`.
    #[resource(secret)]
    pub access_token: String,
    /// Refresh token; absent for short-lived flows without `offline_access`.
    #[resource(secret)]
    pub refresh_token: String,
    /// Absolute expiry as a Unix timestamp (seconds). Public so the refresh
    /// background task can poll without unwrapping secrets.
    pub expires_at: i64,
    /// Space-separated OAuth scopes granted at consent time.
    pub scopes: String,
}

// ─── Kv — the dynamic-fields escape hatch ────────────────────────────────────
//
// The 5 typed resources above cover the common credential surfaces. `kv`
// fills the gap for everything else: opaque API keys, tokens, webhook URLs,
// vendor-specific bundles where shipping a typed struct isn't worth a
// service rebuild.
//
// All values are treated as secrets. User-supplied key names are stored in
// `resource_versions.public_config.__kv_keys` so the picker + resolver can
// iterate them at runtime — the descriptor's `dynamic_fields: true` flag
// tells the rest of the stack to drive off that list rather than the
// (empty) static `secret_fields` / `public_fields`. Registered manually
// rather than via `#[derive(ResourceType)]` because the derive walks struct
// fields, and Kv deliberately has none.
inventory::submit! {
    crate::registry::ResourceTypeDescriptor {
        name: "kv",
        display_name: "Key/Value",
        icon: "lucide-key",
        oauth_provider: None,
        secret_fields: &[],
        public_fields: &[],
        schema_json: || {
            // Open-ended string map — picks up `additionalProperties` and
            // renders as a key/value editor in the modal. The constraint
            // that key names match the workflow code's `<path>.<key>`
            // grammar lives in the handler, not in the schema (validation
            // happens at create time, not at form-input time).
            serde_json::json!({
                "type": "object",
                "additionalProperties": { "type": "string" }
            })
        },
        dynamic_fields: true,
    }
}
