//! Seed helpers that make the model-pool placement loop SEE one or more
//! model-serving runners — and a curated autoscale policy — without a real
//! executor or a live presence heartbeat.
//!
//! The placement reconciler
//! ([`mekhan_service::autoscaler::placement::reconcile_placement`]) builds its
//! per-runner zone inventory from `serving_runner_catalogs(db, presence, ws)`,
//! which is the `presence ∩ runner_interfaces` join: a runner is a candidate only
//! when (a) a `runner_interfaces.catalog` JSONB row exists for it in the workspace
//! AND (b) the in-memory presence snapshot marks it `present`. Its policy comes
//! from a `model_states` row with `autoscale_mode IS NOT NULL`. These helpers seed
//! exactly those three things:
//!
//!   * [`seed_model_runner`] — a `runners` row + its `runner_interfaces` catalog
//!     (model ids, residency zone, per-engine `C`, `base_url`, `pulled` set),
//!     mirroring `model_agent_catalog_e2e.rs`'s direct-insert + JSONB catalog shape.
//!   * presence: [`seed_model_runner`] marks the runner PRESENT via
//!     [`RunnerPresence::inject_present_for_test`] — the public test seam added to
//!     the presence map (the `PresenceEntry`/`PresenceMap` types are `pub(crate)`,
//!     so an integration test cannot build a present entry directly; this is how
//!     the placement loop's `present` gate is satisfied offline).
//!   * [`seed_model_policy`] — a curated `model_states` row carrying the folded-in
//!     autoscale policy columns (mode, desired_replicas, residency_zone,
//!     idle_evict, base, cooldown_secs) so `reconcile_placement` picks the model up.

#![allow(dead_code)]

use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

use mekhan_service::models::runner::{mint_token, RUNNER_TOKEN_PREFIX};
use mekhan_service::runners_presence::RunnerPresence;

/// One model the seeded runner advertises in its interface catalog. A base carries
/// its per-engine concurrency `C` (vLLM `--max-num-seqs`); a LoRA omits `C` and
/// back-points at its base.
#[derive(Clone, Debug)]
pub enum SeedModel {
    /// A base engine resident on the runner, with its `C` budget.
    Base { model_id: String, max_num_seqs: u32 },
    /// A LoRA adapter resident on the runner, layered on `base`.
    Lora {
        model_id: String,
        base: String,
        source_uri: Option<String>,
    },
}

impl SeedModel {
    /// A base entry shorthand.
    pub fn base(model_id: impl Into<String>, c: u32) -> Self {
        Self::Base {
            model_id: model_id.into(),
            max_num_seqs: c,
        }
    }

    /// A LoRA entry shorthand (no source_uri).
    pub fn lora(model_id: impl Into<String>, base: impl Into<String>) -> Self {
        Self::Lora {
            model_id: model_id.into(),
            base: base.into(),
            source_uri: None,
        }
    }

    /// Lower to the `ModelEntry` JSON shape stored in the `catalog.models` JSONB
    /// (matches `RunnerInterfaceCatalog`'s serde, see `model_agent_catalog_e2e.rs`).
    fn to_json(&self) -> serde_json::Value {
        match self {
            SeedModel::Base {
                model_id,
                max_num_seqs,
            } => json!({
                "model_id": model_id,
                "kind": "base",
                "max_num_seqs": max_num_seqs,
            }),
            SeedModel::Lora {
                model_id,
                base,
                source_uri,
            } => {
                let mut v = json!({
                    "model_id": model_id,
                    "kind": "lora",
                    "base": base,
                });
                if let Some(uri) = source_uri {
                    v["source_uri"] = json!(uri);
                }
                v
            }
        }
    }
}

/// How to seed one model-serving runner. `Default` gives an empty, zoneless,
/// base_url-less runner in the nil workspace named `model-runner`.
#[derive(Clone, Debug, Default)]
pub struct SeedRunnerSpec {
    /// Workspace the runner + its catalog land in. The placement loop scans per
    /// workspace, so this MUST match the policy's workspace ([`seed_model_policy`]).
    /// Defaults to `Uuid::nil()` (the dev-noop workspace).
    pub workspace_id: Uuid,
    /// Operator-facing runner name.
    pub name: Option<String>,
    /// Models RESIDENT on the runner (its `catalog.models`).
    pub models: Vec<SeedModel>,
    /// Model ids PROVISIONED TO DISK (the `catalog.pulled` superset — loadable
    /// without a download even when not currently resident).
    pub pulled: Vec<String>,
    /// The runner's GDPR residency zone (`catalog.residency_zone`). `None` ⇒
    /// zone-agnostic (placeable by any zoned-or-zoneless model).
    pub residency_zone: Option<String>,
    /// The runner's OpenAI-compatible inference endpoint (`catalog.base_url`) — set
    /// this to a [`super::fake_upstream::FakeUpstream::base_url`] to make it
    /// routable by the inference router. `None` ⇒ not routable (still a placement
    /// candidate).
    pub base_url: Option<String>,
}

impl SeedRunnerSpec {
    /// A zoned base-serving runner: one base model with budget `c` in `zone`,
    /// pointing its inference endpoint at `base_url`.
    pub fn base_serving(
        workspace_id: Uuid,
        model_id: impl Into<String>,
        c: u32,
        zone: impl Into<String>,
        base_url: impl Into<String>,
    ) -> Self {
        Self {
            workspace_id,
            models: vec![SeedModel::base(model_id, c)],
            residency_zone: Some(zone.into()),
            base_url: Some(base_url.into()),
            ..Default::default()
        }
    }
}

/// A seeded runner's identity + credential, returned by [`seed_model_runner`].
#[derive(Clone, Debug)]
pub struct SeededRunner {
    pub runner_id: Uuid,
    pub workspace_id: Uuid,
    /// The full `rnr_{id}.{secret}` bearer (e.g. for a runner-token catalog re-push).
    pub runner_token: String,
}

/// Seed a model-serving runner the placement loop can place onto: insert the
/// `runners` row + its `runner_interfaces` catalog, then mark it PRESENT in the
/// in-memory presence map so `serving_runner_catalogs`'s `present ∩ catalog` gate
/// admits it.
///
/// Mirrors `model_agent_catalog_e2e.rs`'s direct-insert pattern (bypassing the
/// registration-token enroll gate, orthogonal here) + its JSONB `catalog` shape.
/// The catalog is written DIRECTLY into `runner_interfaces` (not via the HTTP
/// upsert) so the seed needs no live router/app.
pub async fn seed_model_runner(
    db: &PgPool,
    presence: &RunnerPresence,
    spec: SeedRunnerSpec,
) -> SeededRunner {
    let runner_id = Uuid::new_v4();
    let minted = mint_token(RUNNER_TOKEN_PREFIX, runner_id);
    let name = spec
        .name
        .clone()
        .unwrap_or_else(|| format!("model-runner-{}", &runner_id.simple().to_string()[..8]));

    // (1) the runners row.
    sqlx::query(
        "INSERT INTO runners (id, workspace_id, name, token_hash, enrolled_by) \
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(runner_id)
    .bind(spec.workspace_id)
    .bind(&name)
    .bind(&minted.token_hash)
    .bind(Uuid::nil())
    .execute(db)
    .await
    .expect("insert seeded runner row");

    // (2) its interface catalog (the JSONB the placement loop parses into a
    //     RunnerInterfaceCatalog). Topics/services/actions empty; the model-pool
    //     fields are what matter here.
    let mut catalog = json!({
        "topics": [],
        "services": [],
        "actions": [],
        "models": spec.models.iter().map(SeedModel::to_json).collect::<Vec<_>>(),
        "pulled": spec.pulled,
    });
    if let Some(zone) = &spec.residency_zone {
        catalog["residency_zone"] = json!(zone);
    }
    if let Some(url) = &spec.base_url {
        catalog["base_url"] = json!(url);
    }

    sqlx::query(
        "INSERT INTO runner_interfaces (runner_id, workspace_id, catalog, catalog_version) \
         VALUES ($1, $2, $3, $4) \
         ON CONFLICT (runner_id) DO UPDATE \
            SET catalog = EXCLUDED.catalog, \
                catalog_version = EXCLUDED.catalog_version, \
                workspace_id = EXCLUDED.workspace_id",
    )
    .bind(runner_id)
    .bind(spec.workspace_id)
    .bind(&catalog)
    .bind("v1")
    .execute(db)
    .await
    .expect("insert seeded runner_interfaces row");

    // (3) mark it present so the placement loop's presence gate admits it.
    presence.inject_present_for_test(runner_id, true).await;

    SeededRunner {
        runner_id,
        workspace_id: spec.workspace_id,
        runner_token: minted.full_token,
    }
}

/// The folded-in autoscale policy to seed onto a `model_states` row. `Default`
/// gives a zoneless `manual`-mode policy with `desired_replicas = Some(1)` and no
/// idle-evict — the simplest "place this model onto 1 runner" policy.
#[derive(Clone, Debug)]
pub struct SeedPolicySpec {
    pub workspace_id: Uuid,
    pub model_id: String,
    /// `manual` | `scale_to_zero` | `keep_warm`.
    pub mode: String,
    pub desired_replicas: Option<i32>,
    pub residency_zone: Option<String>,
    pub idle_evict: bool,
    pub cooldown_secs: Option<i64>,
    /// LoRA base back-pointer — `Some(base)` makes this policy an adapter that packs
    /// onto `base`'s shared `C`; `None` ⇒ the policy IS a base engine.
    pub base: Option<String>,
}

impl Default for SeedPolicySpec {
    fn default() -> Self {
        Self {
            workspace_id: Uuid::nil(),
            model_id: String::new(),
            mode: "manual".to_string(),
            desired_replicas: Some(1),
            residency_zone: None,
            idle_evict: false,
            cooldown_secs: None,
            base: None,
        }
    }
}

impl SeedPolicySpec {
    /// A base-model policy: `mode` over `model_id` in `zone`, spreading to `n`
    /// runners.
    pub fn base(
        workspace_id: Uuid,
        model_id: impl Into<String>,
        mode: impl Into<String>,
        zone: impl Into<String>,
        n: i32,
    ) -> Self {
        Self {
            workspace_id,
            model_id: model_id.into(),
            mode: mode.into(),
            desired_replicas: Some(n),
            residency_zone: Some(zone.into()),
            ..Default::default()
        }
    }
}

/// Seed a curated `model_states` row carrying the autoscale policy columns so the
/// reconciler's `WHERE autoscale_mode IS NOT NULL` scan picks it up.
///
/// UPSERTs on the `(workspace_id, model_id)` PK: the row lands in lifecycle state
/// `loaded` (so the model-pool read's AND-gate would surface it) with the policy
/// columns set. Re-seeding the same model overwrites the policy.
pub async fn seed_model_policy(db: &PgPool, spec: SeedPolicySpec) {
    sqlx::query(
        "INSERT INTO model_states \
            (workspace_id, model_id, state, base, replicas, \
             autoscale_mode, desired_replicas, residency_zone, idle_evict, cooldown_secs) \
         VALUES ($1, $2, 'loaded', $3, 0, $4, $5, $6, $7, $8) \
         ON CONFLICT (workspace_id, model_id) DO UPDATE SET \
            state = 'loaded', \
            base = EXCLUDED.base, \
            autoscale_mode = EXCLUDED.autoscale_mode, \
            desired_replicas = EXCLUDED.desired_replicas, \
            residency_zone = EXCLUDED.residency_zone, \
            idle_evict = EXCLUDED.idle_evict, \
            cooldown_secs = EXCLUDED.cooldown_secs",
    )
    .bind(spec.workspace_id)
    .bind(&spec.model_id)
    .bind(&spec.base)
    .bind(&spec.mode)
    .bind(spec.desired_replicas)
    .bind(&spec.residency_zone)
    .bind(spec.idle_evict)
    .bind(spec.cooldown_secs)
    .execute(db)
    .await
    .expect("seed model_states policy row");
}

/// Read back a `model_replicas` row's `(status, desired_count, observed_count)` for
/// an assertion after a reconcile tick. `None` when the reconciler hasn't written a
/// row for this model yet.
pub async fn read_replica_status(
    db: &PgPool,
    workspace_id: Uuid,
    model_id: &str,
) -> Option<(String, i32, i32)> {
    sqlx::query_as::<_, (String, i32, i32)>(
        "SELECT status, desired_count, observed_count \
         FROM model_replicas WHERE workspace_id = $1 AND model_id = $2",
    )
    .bind(workspace_id)
    .bind(model_id)
    .fetch_optional(db)
    .await
    .expect("read model_replicas row")
}
