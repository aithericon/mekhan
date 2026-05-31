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
//! - `"postgres"`, `"openai"`, `"anthropic"`, `"http_bearer"`,
//!   `"http_basic"`, `"http_api_key"`, `"slack"`, `"smtp"`, `"s3"`,
//!   `"google_oauth"`, `"kv"`.
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

/// OpenAI API credentials + endpoint binding. `base_url` lives on the
/// resource (not on the workflow step) so that self-hosted OpenAI-compatible
/// endpoints — Azure, vLLM, a corp proxy — are paired with the matching key
/// once and reused across every step that points at them.
#[derive(ResourceType, Serialize, Deserialize, schemars::JsonSchema)]
#[resource(name = "openai", display_name = "OpenAI", icon = "lucide-sparkles")]
pub struct OpenAI {
    #[resource(secret)]
    pub api_key: String,
    /// Optional organization id (`org-...`). Some OpenAI customers need this
    /// to route bills correctly.
    #[serde(default)]
    pub organization: Option<String>,
    /// Optional base URL override. Set this for OpenAI-compatible endpoints
    /// — Azure OpenAI deployments, self-hosted vLLM/Ollama-OpenAI shims, or
    /// internal proxies. Absent → the LLM backend uses the vendor default
    /// (`https://api.openai.com/v1`).
    #[serde(default)]
    pub base_url: Option<String>,
}

/// Anthropic API credentials + endpoint binding. Mirrors [`OpenAI`]'s shape
/// minus the org id: `api_key` is the only secret, `base_url` lives on the
/// resource so a corp proxy / Bedrock-Anthropic shim is paired with its key
/// once and reused across every step that points at it.
///
/// The LLM backend's resource overlay is provider-agnostic — it reads
/// `api_key` + `base_url` from the staged `<alias>.json` regardless of
/// resource type (`executor-llm/src/backend.rs::overlay_resource`) — so this
/// kind needs no executor change. Bind it on an LLM step whose `provider` is
/// `anthropic`. See [[project_llm_resource_binding]].
#[derive(ResourceType, Serialize, Deserialize, schemars::JsonSchema)]
#[resource(
    name = "anthropic",
    display_name = "Anthropic",
    icon = "lucide-sparkles"
)]
pub struct Anthropic {
    #[resource(secret)]
    pub api_key: String,
    /// Optional base URL override for Anthropic-compatible endpoints — a
    /// corporate proxy, a Bedrock/Vertex shim, or an internal gateway.
    /// Absent → the LLM backend uses the vendor default
    /// (`https://api.anthropic.com`).
    #[serde(default)]
    pub base_url: Option<String>,
}

/// Slack webhook target — v1 only supports incoming-webhook posting. Bot-
/// token / OAuth Slack flows land in v2.
#[derive(ResourceType, Serialize, Deserialize, schemars::JsonSchema)]
#[resource(
    name = "slack",
    display_name = "Slack (Webhook)",
    icon = "lucide-slack"
)]
pub struct Slack {
    /// `https://hooks.slack.com/services/...` — the whole URL is treated as
    /// a secret because the path component carries the auth material.
    #[resource(secret)]
    pub webhook_url: String,
}

// ─── HTTP auth credentials ───────────────────────────────────────────────────
//
// Three kinds mirroring the HTTP node's `AuthConfig` variants
// (`executor-backend-configs/src/http.rs`): Bearer / Basic / Header. The
// HTTP backend binds one via `auth_resource` (ConfigOverlay channel) and
// fills the *selected* scheme's missing secret from the resource at run
// time — the scheme stays in the step config because the staged
// `<alias>.json` carries no type tag, so the executor can't infer it.
//
// Field names match the `AuthConfig` variant they feed: `http_bearer.token`
// → `Bearer{token}`, `http_basic.{username,password}` → `Basic{..}`,
// `http_api_key.{header_name,value}` → `Header{name,value}`. The frontend
// picker filters resources by the scheme the author selected, so a mismatched
// kind never reaches publish.

/// Bearer-token credential for the HTTP node's `Bearer` auth scheme.
/// Pairs with a step whose `auth.type = "bearer"`.
#[derive(ResourceType, Serialize, Deserialize, schemars::JsonSchema)]
#[resource(
    name = "http_bearer",
    display_name = "Bearer Token",
    icon = "lucide-key-round"
)]
pub struct HttpBearer {
    /// Sent as `Authorization: Bearer <token>`.
    #[resource(secret)]
    pub token: String,
}

/// Username/password credential for the HTTP node's `Basic` auth scheme.
/// Pairs with a step whose `auth.type = "basic"`. `username` is public
/// (it's not a secret); only `password` lands in Vault.
#[derive(ResourceType, Serialize, Deserialize, schemars::JsonSchema)]
#[resource(
    name = "http_basic",
    display_name = "Basic Auth",
    icon = "lucide-key-round"
)]
pub struct HttpBasic {
    pub username: String,
    #[resource(secret)]
    pub password: String,
}

/// API-key-in-header credential for the HTTP node's `Header` auth scheme.
/// Pairs with a step whose `auth.type = "header"`. `header_name` is public
/// (e.g. `X-API-Key`); the `value` is the secret.
#[derive(ResourceType, Serialize, Deserialize, schemars::JsonSchema)]
#[resource(name = "http_api_key", display_name = "API Key", icon = "lucide-key")]
pub struct HttpApiKey {
    /// Header to set, e.g. `X-API-Key`.
    pub header_name: String,
    #[resource(secret)]
    pub value: String,
}

/// SMTP relay credentials. Covers the common transactional-mail surface:
/// host/port + auth + an optional default `from` address. TLS mode is
/// communicated by `port` convention (`587` = STARTTLS, `465` = implicit
/// TLS, `25` = plain) rather than a flag — keeps the credential surface
/// minimal and aligns with how most SMTP libraries pick a mode.
#[derive(ResourceType, Serialize, Deserialize, schemars::JsonSchema)]
#[resource(name = "smtp", display_name = "SMTP", icon = "lucide-mail")]
pub struct Smtp {
    /// Relay hostname, e.g. `smtp.gmail.com` or `smtp.sendgrid.net`.
    pub host: String,
    /// `587` STARTTLS, `465` implicit TLS, `25` plain. No default — picking
    /// a port is a security decision the workflow author should make.
    pub port: u16,
    pub username: String,
    #[resource(secret)]
    pub password: String,
    /// Optional default `From:` address. Workflows that send from multiple
    /// senders set this per-message instead.
    #[serde(default)]
    pub from_address: Option<String>,
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

// ─── Resource-pool kinds ─────────────────────────────────────────────────────
//
// Two kinds that describe *contended capacity* rather than a single credential.
// A workflow step claims against one of these by alias (`resourcePool: { alias
// }`) and holds a typed lease for its duration. The claim/lease *schemas* are
// pool semantics — they live in a focused side-registry (`crate::pool`), not on
// `ResourceTypeDescriptor` (see that module's doc comment for the rationale).
// Here we declare only the resource's own config surface, exactly like any
// other kind.

/// Platform-owned in-net capacity pool. A `TokenPool` of capacity N is modelled
/// (in R3) as a deployed pool net holding N identical capacity tokens; the
/// engine's firing rule then provides admission control + mutual exclusion for
/// free. No secret — the pool is owned by the platform, not an external system,
/// so there is no credential to store. See `docs/14`.
#[derive(ResourceType, Serialize, Deserialize, schemars::JsonSchema)]
#[resource(
    name = "token_pool",
    display_name = "Token Pool",
    icon = "lucide-layers"
)]
pub struct TokenPool {
    /// Number of concurrent holders the pool admits. Surfaces N capacity tokens
    /// in the backing net.
    pub capacity: u32,
    /// Optional human label for one unit (e.g. `"GPU"`, `"license seat"`).
    /// Cosmetic — drives dashboard / picker copy, never the firing rule.
    #[serde(default)]
    pub unit_label: Option<String>,
}

/// External-allocator connection: a datacenter / scheduler that owns placement.
/// The net holds a *lease* against it (not a mirror of its state) — the external
/// allocator stays the source of truth. `token` authenticates to the allocator's
/// HTTP API. See `docs/13` (datacenter-as-resource) and `docs/14` (the lease
/// lifecycle). The scheduler backend (R4) builds its client from the resolved
/// secret per the docs/13 "engine is the consumer" design.
///
/// **Discriminated resource.** `scheduler_flavor` is the serde tag: it selects
/// the engine leg (R4) AND the connection variant. As an internally-tagged enum
/// it serializes to the SAME flat JSON the engine consumes
/// (`{ "scheduler_flavor": "slurm", "ssh_host": …, "ssh_key": … }`), and makes
/// schemars emit a discriminated `oneOf` so the resource editor renders ONLY the
/// chosen flavor's fields (and the schema enforces per-flavor required-ness
/// instead of a flat "everything optional" struct). The `#[resource(secret)]`
/// fields (`ssh_key` / `nomad_token` / `token`) are unioned across variants for
/// the Vault split — `split_config` keys off the field name, not the variant.
#[derive(ResourceType, Serialize, Deserialize, schemars::JsonSchema)]
#[resource(
    name = "datacenter",
    display_name = "Datacenter",
    icon = "lucide-server",
    tag = "scheduler_flavor"
)]
#[serde(tag = "scheduler_flavor", rename_all = "lowercase")]
pub enum Datacenter {
    /// Slurm cluster reached over SSH (salloc / srun / scancel + squeue/sacct).
    Slurm {
        /// SSH host of the Slurm login node.
        ssh_host: String,
        /// SSH port. Engine defaults to `22` if absent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ssh_port: Option<u16>,
        /// SSH user.
        ssh_user: String,
        /// Known-hosts policy: `"strict"` | `"add"` | `"accept"`. Engine
        /// defaults to `"accept"` if absent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ssh_known_hosts: Option<String>,
        /// Job-script dir on the login node (the lease executor script lives
        /// here).
        template_dir: String,
        /// Inline PEM private key (NOT a path). The engine writes a 0600 temp
        /// file at use. Vault-stored.
        #[resource(secret)]
        ssh_key: String,
    },
    /// Nomad cluster (HTTP dispatch + allocation event stream).
    Nomad {
        /// Nomad HTTP API address.
        nomad_addr: String,
        /// Nomad region. Engine defaults to `"global"` if absent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        nomad_region: Option<String>,
        /// Nomad ACL token. Vault-stored. Optional (Nomad without ACLs needs
        /// none).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[resource(secret)]
        nomad_token: Option<String>,
    },
    /// Generic HTTP allocator — the mock-allocator slice / a custom lease API.
    Http {
        /// Base URL of the HTTP allocator's lease API (claim → POST, release →
        /// DELETE).
        allocator_url: String,
        /// Bearer/API token presented to the HTTP allocator. Vault-stored.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[resource(secret)]
        token: Option<String>,
    },
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
