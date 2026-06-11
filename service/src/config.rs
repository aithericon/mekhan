use config::{Config, Environment, File};
use serde::Deserialize;

#[derive(Debug, Deserialize, Clone)]
pub struct AppConfig {
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    pub database_url: String,
    #[serde(default = "default_petri_lab_url")]
    pub petri_lab_url: String,
    #[serde(default = "default_nats_url")]
    pub nats_url: String,
    /// Path to NATS credentials file (.creds) for authenticated connections.
    #[serde(default)]
    pub nats_creds: Option<String>,
    #[serde(default)]
    pub cleanup: CleanupConfig,
    /// File-analytics growth snapshots (docs/32 Cut 2) — the periodic
    /// `inventory_snapshots` capture job.
    #[serde(default)]
    pub analytics: AnalyticsConfig,
    /// Upper bound (seconds) a `?reply=wait` fire holds the HTTP connection
    /// before degrading to `202 { instance_id }`. Bounds connection/pool
    /// pressure; SSE is the path for genuinely long workflows.
    #[serde(default = "default_wait_timeout_secs")]
    pub wait_timeout_secs: u64,
    #[serde(default)]
    pub s3: S3Config,
    #[serde(default)]
    pub artifact_s3: Option<S3Config>,
    /// Path to a built static SPA (adapter-static output). When set, the service
    /// serves files from this directory and falls back to `index.html` for SPA
    /// routing. Unset in dev — the Vite dev server fronts the SPA directly.
    #[serde(default)]
    pub frontend_dir: Option<String>,
    #[serde(default)]
    pub auth: AuthConfig,
    #[serde(default)]
    pub demos: DemosConfig,
    /// LiveKit server credentials. When set, mekhan mints subscribe-only viewer
    /// tokens (`/api/v1/executions/{id}/channels/{channel}/livekit`) so a browser
    /// can join the room the executor publishes frames to. Unset ⇒ the endpoint
    /// returns 503. Parsed from `MEKHAN__LIVEKIT__*`.
    #[serde(default)]
    pub livekit: Option<LiveKitConfig>,
    /// File-server serve bridge (docs/32 — multi-endpoint file-servers, Phase 3b).
    /// Controls how `GET /api/v1/data/entries/{content_hash}/content` serves an
    /// `s3` endpoint: `false` (default) → mint a presigned GET URL and 302 the
    /// browser straight to the object store (mekhan never touches the bytes);
    /// `true` → proxy the bytes through mekhan (single-origin, no presign leak,
    /// at the cost of bandwidth through the control plane). Parsed from
    /// `MEKHAN__PROXY_S3_READS`.
    #[serde(default)]
    pub proxy_s3_reads: bool,
    /// Invite email + accept-link configuration (Phase 4). Default `mode=log`
    /// (the dev/offline path — the accept URL is `tracing::info!`d, no SMTP).
    #[serde(default)]
    pub email: EmailConfig,
}

/// Invite-email delivery + accept-link construction (Phase 4).
#[derive(Debug, Deserialize, Clone)]
pub struct EmailConfig {
    /// `log` (default — emit the accept URL to the tracing log; offline-friendly)
    /// or `smtp` (send via the configured relay).
    #[serde(default)]
    pub mode: EmailMode,
    /// From-address on outgoing invite mail.
    #[serde(default = "default_email_from")]
    pub from_address: String,
    /// Public origin the accept link is built against:
    /// `{public_base_url}/invite/accept?token=...`. Defaults to the dev SPA.
    #[serde(default = "default_public_base_url")]
    pub public_base_url: String,
    /// Invite lifetime in seconds (default 7 days).
    #[serde(default = "default_invite_ttl_secs")]
    pub invite_ttl_secs: i64,
    /// SMTP relay host (only read when `mode = smtp`).
    #[serde(default)]
    pub smtp_host: Option<String>,
    #[serde(default)]
    pub smtp_port: Option<u16>,
    #[serde(default)]
    pub smtp_username: Option<String>,
    #[serde(default)]
    pub smtp_password: Option<String>,
}

impl Default for EmailConfig {
    fn default() -> Self {
        Self {
            mode: EmailMode::default(),
            from_address: default_email_from(),
            public_base_url: default_public_base_url(),
            invite_ttl_secs: default_invite_ttl_secs(),
            smtp_host: None,
            smtp_port: None,
            smtp_username: None,
            smtp_password: None,
        }
    }
}

#[derive(Debug, Deserialize, Clone, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EmailMode {
    /// Log the accept URL instead of sending (default; offline dev).
    #[default]
    Log,
    /// Send via SMTP relay.
    Smtp,
}

fn default_email_from() -> String {
    "no-reply@aithericon.local".to_string()
}
fn default_public_base_url() -> String {
    "http://localhost:15173".to_string()
}
fn default_invite_ttl_secs() -> i64 {
    7 * 24 * 60 * 60
}

/// LiveKit server connection + API credentials. `url` is the WebSocket signalling
/// URL the browser connects to; `api_key`/`api_secret` sign the viewer JWT.
#[derive(Debug, Deserialize, Clone)]
pub struct LiveKitConfig {
    pub url: String,
    pub api_key: String,
    pub api_secret: String,
}

/// Built-in-demo seeder controls. The seeder runs at service startup,
/// idempotent by stable template id (see `service/src/demos.rs`).
#[derive(Debug, Deserialize, Clone, Default)]
pub struct DemosConfig {
    /// Master switch. Default off — production deployments must opt in;
    /// `just dev::up` flips it on via `MEKHAN__DEMOS__SEED=true`.
    #[serde(default)]
    pub seed: bool,
    /// Where to look for `<name>/demo.json` directories. Default
    /// `./demos` — relative to the service binary's cwd, which `just dev`
    /// sets to the repo root.
    #[serde(default = "default_demos_dir")]
    pub dir: String,
}

fn default_demos_dir() -> String {
    "demos".to_string()
}

/// Identity-provider configuration. The hexagonal seam lets `mode` pick
/// between adapters at boot without rewiring callers.
///
/// In `bff` mode the Rust service runs the entire OIDC Authorization-Code +
/// PKCE flow itself and hands the browser only an opaque HttpOnly session
/// cookie — no token ever reaches client JS. The same `issuer_url`/`audience`
/// also feed the existing `ZitadelTokenVerifier`, used to verify the token the
/// IdP returns to the callback before caching the resolved `AuthUser`.
#[derive(Debug, Deserialize, Clone)]
pub struct AuthConfig {
    #[serde(default)]
    pub mode: AuthMode,
    /// Required when `mode = "bff"`. The Zitadel issuer URL
    /// (e.g. `https://your-instance.zitadel.cloud`). OIDC discovery is done
    /// against `{issuer_url}/.well-known/openid-configuration`.
    #[serde(default)]
    pub issuer_url: Option<String>,
    /// Required when `mode = "bff"`. The OIDC `aud` claim value Mekhan was
    /// registered as in Zitadel. For the bootstrapped public client this is
    /// the client_id.
    #[serde(default)]
    pub audience: Option<String>,
    /// Required when `mode = "bff"`. The OIDC client_id of the registered
    /// application (public USER_AGENT client + PKCE — no secret needed).
    #[serde(default)]
    pub client_id: Option<String>,
    /// Optional client secret. Left unset for the public PKCE client; present
    /// only if a future confidential WEB app replaces it.
    #[serde(default)]
    pub client_secret: Option<String>,
    /// OIDC scopes requested at the authorize endpoint. `offline_access` is
    /// required to obtain a refresh token for transparent renewal.
    #[serde(default = "default_scopes")]
    pub scopes: String,
    /// Default path the browser lands on after a successful login when the
    /// caller didn't specify a (sanitized) `return_to`.
    #[serde(default = "default_post_login_redirect")]
    pub post_login_redirect: String,
    /// Lifetime of a server-side session row before the TTL sweep deletes it,
    /// in seconds. Independent of the access-token lifetime (which is
    /// refreshed transparently).
    #[serde(default = "default_session_ttl_secs")]
    pub session_ttl_secs: i64,
    /// Sets the `Secure` attribute on the session cookie. `false` for local
    /// http development, `true` in production (served over https).
    #[serde(default)]
    pub cookie_secure: bool,
    /// Optional explicit cookie `Domain`. Unset → host-only cookie (the common
    /// same-origin case). Set for a shared apex domain across subdomains.
    #[serde(default)]
    pub cookie_domain: Option<String>,
    /// Comma-separated list of permitted CORS origins. The SPA is now
    /// same-origin so this is unused for it, but kept for non-browser API
    /// clients that still send a Bearer (future).
    #[serde(default)]
    pub cors_origins: Vec<String>,
    /// OIDC client_id of the confidential **API application** Mekhan uses to
    /// authenticate itself when calling Zitadel's token-introspection
    /// endpoint (RFC 7662) to validate machine PATs (CI `mekhan apply`).
    /// Distinct from the public SPA `client_id`. Unset ⇒ the Bearer
    /// introspection path is disabled (cookie auth only).
    #[serde(default)]
    pub introspection_client_id: Option<String>,
    /// Client secret of that API application (HTTP Basic on the introspect
    /// call). Provisioned by `deploy/zitadel/bootstrap.sh`.
    #[serde(default)]
    pub introspection_client_secret: Option<String>,
    /// Personal Access Token of the dedicated `mekhan-token-broker` Zitadel
    /// service user. Mekhan presents this as a Bearer when brokering the
    /// embedded `/api/v1/auth/tokens` feature (creating the per-token machine
    /// users + their PATs via the Management API). Provisioned by
    /// `deploy/zitadel/bootstrap.sh`. Unset ⇒ token management is disabled
    /// (the endpoints 503 and the UI hides the section).
    #[serde(default)]
    pub broker_pat: Option<String>,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            mode: AuthMode::default(),
            issuer_url: None,
            audience: None,
            client_id: None,
            client_secret: None,
            scopes: default_scopes(),
            post_login_redirect: default_post_login_redirect(),
            session_ttl_secs: default_session_ttl_secs(),
            cookie_secure: false,
            cookie_domain: None,
            cors_origins: Vec::new(),
            introspection_client_id: None,
            introspection_client_secret: None,
            broker_pat: None,
        }
    }
}

fn default_scopes() -> String {
    "openid profile email offline_access".to_string()
}

fn default_post_login_redirect() -> String {
    "/".to_string()
}

fn default_session_ttl_secs() -> i64 {
    // 30 days — the session row outlives individual access tokens, which are
    // refreshed in place. The IdP refresh-token lifetime is the real cap.
    60 * 60 * 24 * 30
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum AuthMode {
    /// Backend-for-Frontend: the service runs the OIDC flow server-side and
    /// authenticates requests via an opaque HttpOnly session cookie.
    Bff,
    /// Accept any (or no) credential and inject a fixed dev user. **Never**
    /// boot production with this — `main.rs` refuses the combination.
    #[default]
    DevNoop,
}

#[derive(Debug, Deserialize, Clone)]
pub struct S3Config {
    #[serde(default = "default_s3_endpoint")]
    pub endpoint: String,
    #[serde(default = "default_s3_bucket")]
    pub bucket: String,
    #[serde(default)]
    pub access_key: String,
    #[serde(default)]
    pub secret_key: String,
    #[serde(default = "default_s3_region")]
    pub region: String,
}

impl Default for S3Config {
    fn default() -> Self {
        Self {
            endpoint: default_s3_endpoint(),
            bucket: default_s3_bucket(),
            access_key: String::new(),
            secret_key: String::new(),
            region: default_s3_region(),
        }
    }
}

fn default_s3_endpoint() -> String {
    "http://localhost:9000".to_string()
}

fn default_s3_bucket() -> String {
    "mekhan-artifacts".to_string()
}

fn default_s3_region() -> String {
    "us-east-1".to_string()
}

/// File-analytics snapshot job controls (`MEKHAN__ANALYTICS__*`).
#[derive(Debug, Deserialize, Clone)]
pub struct AnalyticsConfig {
    /// Master switch for the background snapshot job. The manual
    /// `POST /api/v1/data/analytics/snapshot` trigger works regardless.
    #[serde(default = "default_snapshot_enabled")]
    pub snapshot_enabled: bool,
    /// Minutes between captures (clamped to ≥1 at spawn).
    #[serde(default = "default_snapshot_interval_minutes")]
    pub snapshot_interval_minutes: u64,
}

impl Default for AnalyticsConfig {
    fn default() -> Self {
        Self {
            snapshot_enabled: default_snapshot_enabled(),
            snapshot_interval_minutes: default_snapshot_interval_minutes(),
        }
    }
}

fn default_snapshot_enabled() -> bool {
    true
}

fn default_snapshot_interval_minutes() -> u64 {
    60
}

#[derive(Debug, Deserialize, Clone)]
pub struct CleanupConfig {
    #[serde(default = "default_retention_hours")]
    pub retention_hours: u64,
    #[serde(default = "default_sweep_interval_minutes")]
    pub sweep_interval_minutes: u64,
    #[serde(default = "default_purge_events")]
    pub purge_events: bool,
}

impl Default for CleanupConfig {
    fn default() -> Self {
        Self {
            retention_hours: default_retention_hours(),
            sweep_interval_minutes: default_sweep_interval_minutes(),
            purge_events: default_purge_events(),
        }
    }
}

fn default_wait_timeout_secs() -> u64 {
    30
}

fn default_host() -> String {
    "0.0.0.0".to_string()
}

fn default_port() -> u16 {
    3100
}

fn default_petri_lab_url() -> String {
    "http://localhost:3030".to_string()
}

fn default_nats_url() -> String {
    "nats://localhost:4333".to_string()
}

fn default_retention_hours() -> u64 {
    72
}

fn default_sweep_interval_minutes() -> u64 {
    60
}

fn default_purge_events() -> bool {
    true
}

impl AppConfig {
    pub fn load() -> Result<Self, config::ConfigError> {
        let config = Config::builder()
            .set_default("host", default_host())?
            .set_default("port", default_port() as i64)?
            .set_default("petri_lab_url", default_petri_lab_url())?
            .set_default("nats_url", default_nats_url())?
            .add_source(File::with_name("mekhan").required(false))
            // Optional local-only overlay. Generated by
            // `deploy/zitadel/bootstrap.sh` so the dev loop can pick up
            // Zitadel issuer/audience without exporting env vars.
            .add_source(File::with_name("mekhan.local").required(false))
            .add_source(
                Environment::with_prefix("MEKHAN")
                    .separator("__")
                    .try_parsing(true),
            )
            .build()?;

        config.try_deserialize()
    }
}
