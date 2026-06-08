//! Humans as a Capacity — the roster (docs/33 §7).
//!
//! A "human capacity" is a `capacity` resource (`presence · offer · …`) backed
//! by a `pool-<resource_id>` net. The ROSTER is the set of `workspace_members`
//! enrolled in it — the human counterpart to the `runners` fleet behind the
//! runner pool. These structs mirror the migration column order (see
//! `service/migrations/20240156000000_roster_members.sql`) so a `SELECT *` reads
//! back via `sqlx::FromRow` without surprises.
//!
//! Caps are ADMIN-ASSIGNED and live on the trusted row (`caps` JSONB), validated
//! against the workspace's `CapabilityType`s exactly like a runner's enrollment
//! caps — the client never asserts its own. The same trusted caps feed both the
//! injected pool unit (the engine `t_claim` matcher's authority) and the inbox
//! eligibility filter, so the two can only ever disagree on "offer already
//! taken", never on "you weren't eligible".

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

// ── Availability config ────────────────────────────────────────────────────

/// What renews a roster member's presence (docs/33 §7.1). A person has no daemon
/// heartbeat, so availability is one parameterised controller, not three code
/// paths — this picks the renewal signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum LivenessSource {
    /// No liveness signal — presence is the durable `available` toggle alone
    /// (`ttl=∞`); the member stays online until explicitly toggled off.
    None,
    /// The already-open task-SSE connection acts as the heartbeat; a closed tab
    /// grace-expires out of the pool.
    #[default]
    Session,
    /// An external signal renews presence — a shift / HR / calendar webhook.
    External,
}

/// The availability knobs stored in the `availability` JSONB column. Container-level
/// `#[serde(default)]` so an empty `{}` JSONB — or any missing field — falls back to
/// [`AvailabilityConfig::default`] (the interactive defaults: `session` liveness, 45 s
/// TTL, 15 s grace), NOT each field's type-default (which would zero `ttl_secs`).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct AvailabilityConfig {
    /// What renews presence for this member.
    pub liveness_source: LivenessSource,
    /// Expiry window: seconds since the last renewal before presence lapses.
    pub ttl_secs: u64,
    /// Additional grace before a lapsed presence is reaped.
    pub grace_secs: u64,
}

impl Default for AvailabilityConfig {
    fn default() -> Self {
        Self {
            liveness_source: LivenessSource::Session,
            ttl_secs: 45,
            grace_secs: 15,
        }
    }
}

// ── DB row ─────────────────────────────────────────────────────────────────

/// One row from the `roster_members` table. Column order matches the migration.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct RosterMemberRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    /// The human-capacity `resources.id` — the pool net is `pool-<capacity_id>`.
    pub capacity_id: Uuid,
    /// The enrolled member's `workspace_members.user_id`.
    pub member_user_id: Uuid,
    /// Admin-assigned capability blob, validated against the workspace's
    /// `CapabilityType`s. The trusted source for both the injected pool unit's
    /// caps and the inbox eligibility filter.
    pub caps: serde_json::Value,
    /// Per-person `C` — the presence controller's slot count.
    pub concurrency: i32,
    /// `{liveness_source, ttl_secs, grace_secs}`. `{}` → interactive defaults.
    pub availability: serde_json::Value,
    /// Durable intent toggle.
    pub available: bool,
    pub available_since: Option<DateTime<Utc>>,
    pub enrolled_by: Uuid,
    pub enrolled_at: DateTime<Utc>,
    /// Soft-delete tombstone. NULL = live.
    pub revoked_at: Option<DateTime<Utc>>,
}

impl RosterMemberRow {
    /// Deserialize the `availability` JSONB into a typed [`AvailabilityConfig`],
    /// falling back to [`AvailabilityConfig::default`] on any error (a stored
    /// `{}` already lands on the defaults via `#[serde(default)]`).
    pub fn availability_config(&self) -> AvailabilityConfig {
        serde_json::from_value(self.availability.clone()).unwrap_or_default()
    }
}

// ── Wire DTOs ──────────────────────────────────────────────────────────────

/// Compact list-row shape. Returned by the roster list endpoint.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct RosterMemberSummary {
    pub id: Uuid,
    pub capacity_id: Uuid,
    pub member_user_id: Uuid,
    pub concurrency: i32,
    pub available: bool,
    pub enrolled_at: DateTime<Utc>,
}

impl From<RosterMemberRow> for RosterMemberSummary {
    fn from(r: RosterMemberRow) -> Self {
        Self {
            id: r.id,
            capacity_id: r.capacity_id,
            member_user_id: r.member_user_id,
            concurrency: r.concurrency,
            available: r.available,
            enrolled_at: r.enrolled_at,
        }
    }
}

/// Admin view for a single roster member — carries the trusted caps and the
/// typed availability config.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct RosterMemberDetail {
    pub id: Uuid,
    pub capacity_id: Uuid,
    pub member_user_id: Uuid,
    pub caps: serde_json::Value,
    pub concurrency: i32,
    pub availability: AvailabilityConfig,
    pub available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub available_since: Option<DateTime<Utc>>,
    pub enrolled_by: Uuid,
    pub enrolled_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revoked_at: Option<DateTime<Utc>>,
}

impl From<RosterMemberRow> for RosterMemberDetail {
    fn from(r: RosterMemberRow) -> Self {
        let availability = r.availability_config();
        Self {
            id: r.id,
            capacity_id: r.capacity_id,
            member_user_id: r.member_user_id,
            caps: r.caps,
            concurrency: r.concurrency,
            availability,
            available: r.available,
            available_since: r.available_since,
            enrolled_by: r.enrolled_by,
            enrolled_at: r.enrolled_at,
            revoked_at: r.revoked_at,
        }
    }
}

// ── Request DTOs ───────────────────────────────────────────────────────────

/// Request body for enrolling a `workspace_member` into a human capacity.
/// Caps are admin-assigned here — the trusted row, never the wire claim.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct EnrollMemberRequest {
    /// The human-capacity `resources.id` to enroll into.
    pub capacity_id: Uuid,
    /// The `workspace_members.user_id` being enrolled.
    pub member_user_id: Uuid,
    /// Admin-assigned capability blob, validated against `CapabilityType`s.
    /// Defaults to `{}`.
    #[serde(default = "empty_object")]
    pub caps: serde_json::Value,
    /// Per-person `C`. Defaults to `1` when omitted.
    #[serde(default)]
    pub concurrency: Option<u32>,
    /// Optional availability config; defaults to the interactive preset.
    #[serde(default)]
    pub availability: Option<AvailabilityConfig>,
}

fn empty_object() -> serde_json::Value {
    serde_json::Value::Object(serde_json::Map::new())
}

/// Request body for an admin update of a roster member. Every field optional —
/// only the supplied ones are written.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct UpdateRosterMemberRequest {
    #[serde(default)]
    pub caps: Option<serde_json::Value>,
    #[serde(default)]
    pub concurrency: Option<u32>,
    #[serde(default)]
    pub availability: Option<AvailabilityConfig>,
}

/// Request body for a member's durable availability toggle. The member flips
/// their own intent on a specific human capacity.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct AvailabilityRequest {
    /// The human-capacity `resources.id`.
    pub capacity_id: Uuid,
    /// `true` → online (available for offers); `false` → offline.
    pub available: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn liveness_source_serializes_snake_case() {
        assert_eq!(
            serde_json::to_value(LivenessSource::Session).unwrap(),
            serde_json::json!("session")
        );
        assert_eq!(
            serde_json::to_value(LivenessSource::External).unwrap(),
            serde_json::json!("external")
        );
        let parsed: LivenessSource = serde_json::from_value(serde_json::json!("none")).unwrap();
        assert_eq!(parsed, LivenessSource::None);
    }

    #[test]
    fn liveness_source_defaults_to_session() {
        assert_eq!(LivenessSource::default(), LivenessSource::Session);
    }

    #[test]
    fn availability_config_defaults() {
        let cfg = AvailabilityConfig::default();
        assert_eq!(cfg.liveness_source, LivenessSource::Session);
        assert_eq!(cfg.ttl_secs, 45);
        assert_eq!(cfg.grace_secs, 15);
    }

    #[test]
    fn empty_jsonb_deserializes_to_defaults() {
        // A `{}` availability column must land on the interactive defaults via
        // the per-field `#[serde(default)]`.
        let cfg: AvailabilityConfig = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(cfg.liveness_source, LivenessSource::Session);
        assert_eq!(cfg.ttl_secs, 45);
        assert_eq!(cfg.grace_secs, 15);
    }

    #[test]
    fn partial_jsonb_fills_missing_with_defaults() {
        let cfg: AvailabilityConfig =
            serde_json::from_value(serde_json::json!({ "liveness_source": "none" })).unwrap();
        assert_eq!(cfg.liveness_source, LivenessSource::None);
        assert_eq!(cfg.ttl_secs, 45);
        assert_eq!(cfg.grace_secs, 15);
    }

    #[test]
    fn row_availability_config_falls_back_on_garbage() {
        let row = sample_row(serde_json::json!({ "liveness_source": 12345 }));
        // Garbage JSONB → default, never a panic.
        let cfg = row.availability_config();
        assert_eq!(cfg.liveness_source, LivenessSource::Session);
        assert_eq!(cfg.ttl_secs, 45);
    }

    #[test]
    fn detail_carries_typed_availability() {
        let row = sample_row(serde_json::json!({
            "liveness_source": "external",
            "ttl_secs": 600,
            "grace_secs": 60
        }));
        let detail = RosterMemberDetail::from(row);
        assert_eq!(
            detail.availability.liveness_source,
            LivenessSource::External
        );
        assert_eq!(detail.availability.ttl_secs, 600);
        assert_eq!(detail.availability.grace_secs, 60);
    }

    fn sample_row(availability: serde_json::Value) -> RosterMemberRow {
        RosterMemberRow {
            id: Uuid::new_v4(),
            workspace_id: Uuid::new_v4(),
            capacity_id: Uuid::new_v4(),
            member_user_id: Uuid::new_v4(),
            caps: serde_json::json!({}),
            concurrency: 1,
            availability,
            available: false,
            available_since: None,
            enrolled_by: Uuid::new_v4(),
            enrolled_at: Utc::now(),
            revoked_at: None,
        }
    }
}
