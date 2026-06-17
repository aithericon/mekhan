//! Trigger-node sources (Phase 5) and the per-trigger concurrency policy.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use super::human_task::TaskFieldConfig;

// --- Trigger nodes (Phase 5) ---

/// What event source fires a `Trigger` node. Tagged enum on the wire
/// (`{"kind": "cron", ...}`). Phase 5a only wires `Manual` into the dispatcher
/// end-to-end; the other variants are stored as data and surfaced through the
/// API for the editor to round-trip, but firing logic for each lands in 5b–5e.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TriggerSource {
    Cron(CronTrigger),
    Catalog(CatalogTrigger),
    NetCompletion(NetCompletionTrigger),
    Webhook(WebhookTrigger),
    Manual(ManualTrigger),
}

impl TriggerSource {
    /// Discriminant string used for routing in the dispatcher and metrics.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Cron(_) => "cron",
            Self::Catalog(_) => "catalog",
            Self::NetCompletion(_) => "net_completion",
            Self::Webhook(_) => "webhook",
            Self::Manual(_) => "manual",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CronTrigger {
    /// Standard cron expression (5- or 6-field). Validated at compile time.
    pub schedule: String,
    /// IANA timezone (e.g. `"Europe/Berlin"`). Defaults to `"UTC"` if absent.
    #[serde(default = "default_timezone")]
    pub timezone: String,
    /// Optional jitter window in seconds; the dispatcher fires within
    /// `[scheduled, scheduled + jitter_secs]` to spread load.
    #[serde(default)]
    pub jitter_secs: u32,
    /// What to do after a service restart with missed fire windows.
    #[serde(default)]
    pub catchup: CronCatchup,
}

fn default_timezone() -> String {
    "UTC".to_string()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum CronCatchup {
    /// Fire every missed window from the last-fire timestamp.
    FireMissed,
    /// Discard missed windows; only fire the next one.
    #[default]
    SkipMissed,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CatalogTrigger {
    /// Catalogue query DSL (the SAME text the data browser submits, e.g.
    /// `category:model filename~report meta.format:fasta created_at>-7d`). The
    /// trigger fires when a newly ingested artifact would appear in this query.
    /// Compiled server-side at eval time so relative dates re-resolve per fire.
    #[serde(default)]
    pub query: String,
    /// If true, the dispatcher walks existing catalogue entries matching the
    /// query when the trigger is first registered.
    #[serde(default)]
    pub backfill: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NetCompletionTrigger {
    /// Source template whose instance completion fires this trigger.
    pub source_template_id: Uuid,
    /// Specific version, or `None` for any published version.
    #[serde(default)]
    pub source_version: Option<i32>,
    /// Which terminal status counts as a fire.
    pub on: CompletionStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CompletionStatus {
    Success,
    Failure,
    Cancelled,
    Any,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WebhookTrigger {
    /// Stable slug appended to `/api/triggers/webhook/{slug}`. Must be unique
    /// across published templates — the editor reserves it at publish.
    pub slug: String,
    pub auth: WebhookAuth,
    #[serde(default)]
    pub require_method: Option<HttpMethod>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WebhookAuth {
    /// No auth — endpoint is publicly fireable. Sane only for staging or
    /// trusted networks; the editor surfaces a warning.
    None,
    /// Compare a header (typically `Authorization` or `X-Webhook-Token`) to a
    /// static shared secret. Secret is stored encrypted at rest.
    SharedSecret { header: String, secret_ref: String },
    /// HMAC-SHA256 signature over the request body, with the signing key
    /// stored encrypted at rest and the signature read from `header`.
    SignedHmac { header: String, secret_ref: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "UPPERCASE")]
pub enum HttpMethod {
    Get,
    Post,
    Put,
    Patch,
    Delete,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ManualTrigger {
    /// Form schema for the "Run with parameters" dialog. Reuses the same
    /// `TaskFieldConfig` shape as human-task forms.
    #[serde(default)]
    pub form: Vec<TaskFieldConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum ConcurrencyPolicy {
    /// Every fire produces an event (default).
    #[default]
    Allow,
    /// At most one fire in flight; subsequent fires are dropped while running.
    Skip,
    /// Queue fires behind the active one; drained when it completes.
    Queue,
    /// Dedup by hashing the result of a Rhai `expression` over the event scope;
    /// fires whose key has been seen within `window_secs` are dropped.
    DedupKey {
        expression: String,
        window_secs: u32,
    },
}
