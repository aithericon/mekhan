//! Generated **staging net** + trigger (B-staging, Phase 4).
//!
//! Staging pushes one job-template *version* onto one *datacenter* cluster. Per
//! `docs/20` §B6 staging is NOT an imperative endpoint — for Slurm it is
//! genuinely multi-step (build/pull `.sif` → rsync over SSH → warm cache →
//! validate), each step independently failable. So mekhan *generates* a normal
//! Petri net (same AIR as any workflow), deploys it like the pool/lease-adapter
//! nets ([`crate::petri::pool_net`]), and lets the engine drive it. A staging run
//! is therefore an instance you can drill into, inheriting all of Track A's
//! observability for free.
//!
//! ## v1 step-depth (reconciles with the deferred-package scope)
//!
//! The **framework + light steps are live**: the net fires the engine's
//! `stage_template` inline effect, which (Nomad) registers the rendered
//! parameterized job via `PUT /v1/job/{slug}`, or (Slurm) writes the sbatch
//! script and rsyncs it over SSH. Heavy container-build / dependency-cache steps
//! are present-but-basic (the package source is threaded through; industrial
//! Apptainer build/cache is the deferred part). The generator emits a NORMAL net,
//! so "user-authored / overridable staging pipelines" is a later extension with
//! no new abstraction.
//!
//! ## The one-shot shape
//!
//! Unlike the long-lived pool net, a staging net is a **one-shot instance** keyed
//! by the `template_stagings` row id (`staging_id`): `start` is seeded with the
//! stage request, `t_stage` fires the effect once, and the result lands in the
//! `staged` (success) / `failed` (fatal) terminal. The `stage_template` handler
//! records a *cluster* failure as `status: "failed"` DATA on the success port (so
//! the net completes cleanly and the projection records it) — only a truly-fatal
//! config/parse error trips the `_error` → `failed` terminal. The effect_result's
//! echoed `staging_id` correlates straight back to the row the
//! [`crate::projections::template_stagings`] projection updates.

use aithericon_sdk::scenario::ScenarioDefinition;
use aithericon_sdk::{effects, Context, DynamicToken};
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

use crate::compiler::well_known;
use crate::models::job_template::{JobTemplateRow, JobTemplateVersionRow, TemplateStagingRow};
use crate::petri::client::PetriClient;
use crate::petri::pool_net::DatacenterConnection;

/// Build the AIR `ScenarioDefinition` for a one-shot staging run.
///
/// `staging_id` is the `template_stagings` row id — it keys the net id
/// ([`well_known::staging_net_id`]) and is echoed by the `stage_template` effect
/// into its result so the projection can correlate the outcome back to the row.
/// `slug` is the native job NAME registered on the cluster (Nomad parameterized
/// job id / Slurm script name) — the SAME slug a `Scheduled` step's resolved
/// `job_template` dispatches against. `conn.effect_config()` is the per-flavor
/// connection (with `{{secret:…}}` templates) the engine's `ClusterRegistry`
/// builds a client from — IDENTICAL to the lease-adapter net's config.
pub fn build_staging_net(
    staging_id: Uuid,
    slug: &str,
    conn: &DatacenterConnection,
    common_spec: Value,
    escape_hatch: Value,
    package_ref: Option<Value>,
) -> ScenarioDefinition {
    let net_id = well_known::staging_net_id(staging_id);
    let flavor = conn.scheduler_flavor.as_str();
    let mut ctx = Context::new(net_id).description(format!(
        "Staging run {staging_id}: push job-template '{slug}' onto datacenter \
         resource {} (flavor {flavor}) via the stage_template engine effect.",
        conn.resource_id
    ));

    // The full per-flavor connection baked on the effect transition — the SAME
    // shape the lease-adapter net uses, so the engine builds (and caches) the
    // right per-(resource_id, version) ClusterClient on first fire.
    let effect_config = conn.effect_config();

    // start (seeded with the stage request) → t_stage → staged | failed.
    let start: aithericon_sdk::PlaceHandle<DynamicToken> = ctx.state("start", "Stage Request");
    let staged: aithericon_sdk::PlaceHandle<DynamicToken> = ctx.terminal("staged", "Staged");
    let failed: aithericon_sdk::PlaceHandle<DynamicToken> =
        ctx.terminal("failed", "Stage Failed (fatal)");

    // t_stage — the engine's stage_template inline effect. Reads the request on
    // its "request" port (+ the resolved connection from effect_config), renders
    // the native spec, and registers it on the cluster. Its "staged" output
    // carries `{ staging_id, status, remote_ref, slug, error? }` — `status` is
    // "staged" on success OR "failed" on a (non-fatal) cluster error, so the net
    // completes cleanly either way and the projection records the outcome. A
    // truly-fatal config/parse error routes the raw token to `_error` → `failed`.
    ctx.transition("t_stage", "Stage Template")
        .auto_input("request", &start)
        .auto_output("staged", &staged)
        .auto_output("_error", &failed)
        .effect_with_config(effects::STAGE_TEMPLATE.handler_id, effect_config);

    // Seed the one-shot request token.
    ctx.seed_one(
        &start,
        DynamicToken(json!({
            "staging_id": staging_id.to_string(),
            "slug": slug,
            "spec": common_spec,
            "escape_hatch": escape_hatch,
            "package_ref": package_ref,
        })),
    );

    ctx.build()
}

/// Resolve a datacenter resource id to a [`DatacenterConnection`] from its latest
/// version's `public_config`. Workspace-scoped + soft-delete aware. Returns
/// `Ok(None)` when the resource is absent / not a datacenter / missing its
/// flavor's required connection field. Shares the public-config → connection
/// mapping with the resource-create path via
/// [`DatacenterConnection::from_public_config`].
pub async fn resolve_datacenter_connection(
    db: &PgPool,
    workspace_id: Uuid,
    resource_id: Uuid,
) -> Result<Option<DatacenterConnection>, sqlx::Error> {
    let row: Option<(i32, Value)> = sqlx::query_as(
        "SELECT rv.version, rv.public_config \
         FROM resources r \
         JOIN resource_versions rv ON rv.resource_id = r.id AND rv.version = r.latest_version \
         WHERE r.id = $1 AND r.workspace_id = $2 \
           AND r.resource_type = 'datacenter' AND r.deleted_at IS NULL",
    )
    .bind(resource_id)
    .bind(workspace_id)
    .fetch_optional(db)
    .await?;

    let Some((version, public_config)) = row else {
        return Ok(None);
    };
    let Some(public) = public_config.as_object() else {
        return Ok(None);
    };
    let vault_path = crate::handlers::resources::vault_path_for(workspace_id, resource_id, version);
    Ok(DatacenterConnection::from_public_config(
        resource_id,
        version,
        &vault_path,
        public,
    ))
}

/// Outcome of a [`trigger_staging`] call: the (upserted) staging row, plus
/// whether the staging net was successfully deployed. A deploy failure still
/// returns a row (status `failed`, `last_error` populated) — staging failure is
/// recorded, never an error that strands the caller.
pub struct StageTriggerError {
    pub message: String,
}

impl std::fmt::Display for StageTriggerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// Trigger one staging run: upsert the `template_stagings` row to `staging`,
/// generate + deploy the staging net, and return the row. Dual-triggered — the
/// explicit `POST /api/v1/job-templates/{id}/stage` endpoint and the publish-time
/// auto-stage hook both call this.
///
/// Failure modes return `Err(StageTriggerError)`:
/// - the template version row is missing,
/// - the datacenter resource can't be resolved to a connection,
/// - the template's flavor doesn't match the cluster's flavor.
///
/// A *deploy* failure (engine unreachable) is NOT an error: the row is flipped to
/// `failed` with `last_error` and returned, mirroring the pool-net deploy's
/// engine-down tolerance. A clean deploy leaves the row at `staging`; the
/// `template_stagings` projection advances it to `staged`/`failed` from the
/// net's `stage_template` effect result.
pub async fn trigger_staging(
    db: &PgPool,
    petri: &PetriClient,
    workspace_id: Uuid,
    template: &JobTemplateRow,
    version: i32,
    datacenter_resource_id: Uuid,
    package_catalogue_entry_id: Option<Uuid>,
) -> Result<TemplateStagingRow, StageTriggerError> {
    let err = |m: String| StageTriggerError { message: m };

    // (a) Load the version payload (common_spec + escape_hatch).
    let version_row = sqlx::query_as::<_, JobTemplateVersionRow>(
        "SELECT * FROM job_template_versions WHERE template_id = $1 AND version = $2",
    )
    .bind(template.id)
    .bind(version)
    .fetch_optional(db)
    .await
    .map_err(|e| err(format!("load template version: {e}")))?
    .ok_or_else(|| {
        err(format!(
            "template '{}' has no version {version}",
            template.slug
        ))
    })?;

    // (b) Resolve the target cluster connection + flavor-match.
    let conn = resolve_datacenter_connection(db, workspace_id, datacenter_resource_id)
        .await
        .map_err(|e| err(format!("resolve datacenter connection: {e}")))?
        .ok_or_else(|| {
            err(format!(
                "datacenter resource {datacenter_resource_id} not found in workspace, or missing \
                 its flavor's required connection field"
            ))
        })?;
    if conn.scheduler_flavor != template.flavor {
        return Err(err(format!(
            "template '{}' is flavor '{}' but datacenter resource {datacenter_resource_id} is \
             flavor '{}' — a {} template cannot be staged onto a {} cluster",
            template.slug,
            template.flavor,
            conn.scheduler_flavor,
            template.flavor,
            conn.scheduler_flavor
        )));
    }

    // (c) Upsert the staging row → `staging` (fresh attempt clears prior error).
    let staging_row = sqlx::query_as::<_, TemplateStagingRow>(
        "INSERT INTO template_stagings \
            (template_id, template_version, datacenter_resource_id, status) \
         VALUES ($1, $2, $3, 'staging') \
         ON CONFLICT (template_id, template_version, datacenter_resource_id) DO UPDATE SET \
            status = 'staging', last_error = NULL, updated_at = NOW() \
         RETURNING *",
    )
    .bind(template.id)
    .bind(version)
    .bind(datacenter_resource_id)
    .fetch_one(db)
    .await
    .map_err(|e| err(format!("upsert staging row: {e}")))?;

    // (d) Generate + deploy the one-shot staging net.
    let package_ref =
        package_catalogue_entry_id.map(|id| json!({ "catalogue_entry_id": id.to_string() }));
    let air = match serde_json::to_value(build_staging_net(
        staging_row.id,
        &template.slug,
        &conn,
        version_row.common_spec.clone(),
        version_row
            .escape_hatch
            .clone()
            .unwrap_or_else(|| json!({})),
        package_ref,
    )) {
        Ok(v) => v,
        Err(e) => {
            return Err(err(format!("serialize staging net AIR: {e}")));
        }
    };
    let net_id = well_known::staging_net_id(staging_row.id);

    if let Err(e) = crate::petri::instance::deploy_instance(
        petri,
        &net_id,
        &air,
        petri_api_types::DispatchOptions::default(),
        None,
    )
    .await
    {
        // Engine-down: flip the row to failed + return it (don't strand caller).
        tracing::warn!(
            %net_id,
            %e,
            "failed to deploy staging net to the engine; recording staging row as failed"
        );
        let failed = sqlx::query_as::<_, TemplateStagingRow>(
            "UPDATE template_stagings SET status = 'failed', last_error = $2, updated_at = NOW() \
             WHERE id = $1 RETURNING *",
        )
        .bind(staging_row.id)
        .bind(format!("staging net deploy failed: {e}"))
        .fetch_one(db)
        .await
        .map_err(|e| err(format!("record staging deploy failure: {e}")))?;
        return Ok(failed);
    }

    tracing::info!(
        %net_id,
        template = %template.slug,
        version,
        %datacenter_resource_id,
        "deployed staging net"
    );
    Ok(staging_row)
}

// ─── Image materialization (docs/22 container staging) ───────────────────────
//
// Symmetric with staging, one layer down: a one-shot `materialize-<row>` net
// fires the engine's `materialize_image` inline effect, which pulls an OCI image
// to an Apptainer `.sif` on the datacenter's login node. The effect_result's
// echoed `materialize_id` correlates back to the `image_materializations` row the
// projection updates. v1 supports PUBLIC images; private-registry credentials are
// a documented later refinement (the engine resolves `{{secret:…}}` only in
// effect_config, and detecting cred presence per resource is deferred).

use crate::models::image_materialization::ImageMaterializationRow;

/// Build the AIR for a one-shot image-materialization run. `effect_config` is the
/// datacenter connection (where to SSH/pull) MERGED with the non-secret
/// `image_ref` — the `materialize_image` handler reads both from the resolved
/// config. The seeded request token carries only the correlation id.
pub fn build_materialize_image_net(
    materialize_id: Uuid,
    image_ref: &str,
    conn: &DatacenterConnection,
) -> ScenarioDefinition {
    let net_id = well_known::materialize_net_id(materialize_id);
    let flavor = conn.scheduler_flavor.as_str();
    let mut ctx = Context::new(net_id).description(format!(
        "Materialize run {materialize_id}: pull image '{image_ref}' to a .sif on \
         datacenter resource {} (flavor {flavor}) via the materialize_image engine effect.",
        conn.resource_id
    ));

    // datacenter connection + image_ref on the effect transition. (Registry
    // credentials would be merged here as `{{secret:…}}` refs — deferred in v1.)
    let mut effect_config = conn.effect_config();
    if let Some(obj) = effect_config.as_object_mut() {
        obj.insert("image_ref".to_string(), json!(image_ref));
    }

    let start: aithericon_sdk::PlaceHandle<DynamicToken> =
        ctx.state("start", "Materialize Request");
    let materialized: aithericon_sdk::PlaceHandle<DynamicToken> =
        ctx.terminal("materialized", "Materialized");
    let failed: aithericon_sdk::PlaceHandle<DynamicToken> =
        ctx.terminal("failed", "Materialize Failed (fatal)");

    ctx.transition("t_materialize", "Materialize Image")
        .auto_input("request", &start)
        .auto_output("materialized", &materialized)
        .auto_output("_error", &failed)
        .effect_with_config(effects::MATERIALIZE_IMAGE.handler_id, effect_config);

    ctx.seed_one(
        &start,
        DynamicToken(json!({ "materialize_id": materialize_id.to_string() })),
    );

    ctx.build()
}

/// Resolve a `container_image` resource id to `(version, image_ref)` from its
/// latest version's `public_config`. Workspace-scoped + soft-delete aware.
/// `Ok(None)` when absent / not a container_image / missing `image_ref`.
pub async fn resolve_container_image(
    db: &PgPool,
    workspace_id: Uuid,
    resource_id: Uuid,
) -> Result<Option<(i32, String)>, sqlx::Error> {
    let row: Option<(i32, Value)> = sqlx::query_as(
        "SELECT rv.version, rv.public_config \
         FROM resources r \
         JOIN resource_versions rv ON rv.resource_id = r.id AND rv.version = r.latest_version \
         WHERE r.id = $1 AND r.workspace_id = $2 \
           AND r.resource_type = 'container_image' AND r.deleted_at IS NULL",
    )
    .bind(resource_id)
    .bind(workspace_id)
    .fetch_optional(db)
    .await?;

    let Some((version, public_config)) = row else {
        return Ok(None);
    };
    let image_ref = public_config
        .get("image_ref")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());
    Ok(image_ref.map(|r| (version, r.to_string())))
}

/// Trigger one image-materialization run: upsert the `image_materializations` row
/// to `materializing`, generate + deploy the one-shot materialize net, return the
/// row. Dual-triggered (explicit endpoint + publish-time auto hook), idempotent
/// on the row (re-materializing reuses the row id → replaces the net).
///
/// A *deploy* failure flips the row to `failed` and returns it (never strands the
/// caller); a clean deploy leaves it at `materializing` for the projection to
/// advance to `ready`/`failed` from the effect result.
pub async fn trigger_materialize_image(
    db: &PgPool,
    petri: &PetriClient,
    workspace_id: Uuid,
    container_resource_id: Uuid,
    datacenter_resource_id: Uuid,
) -> Result<ImageMaterializationRow, StageTriggerError> {
    let err = |m: String| StageTriggerError { message: m };

    // (a) Resolve the container image (version + image_ref).
    let (container_version, image_ref) =
        resolve_container_image(db, workspace_id, container_resource_id)
            .await
            .map_err(|e| err(format!("resolve container_image: {e}")))?
            .ok_or_else(|| {
                err(format!(
                    "container_image resource {container_resource_id} not found in workspace, \
                     or missing image_ref"
                ))
            })?;

    // (b) Resolve the target cluster connection.
    let conn = resolve_datacenter_connection(db, workspace_id, datacenter_resource_id)
        .await
        .map_err(|e| err(format!("resolve datacenter connection: {e}")))?
        .ok_or_else(|| {
            err(format!(
                "datacenter resource {datacenter_resource_id} not found in workspace, or missing \
                 its flavor's required connection field"
            ))
        })?;

    // (c) Upsert the materialization row → `materializing`.
    let row = sqlx::query_as::<_, ImageMaterializationRow>(
        "INSERT INTO image_materializations \
            (container_resource_id, container_version, datacenter_resource_id, status) \
         VALUES ($1, $2, $3, 'materializing') \
         ON CONFLICT (container_resource_id, container_version, datacenter_resource_id) DO UPDATE SET \
            status = 'materializing', last_error = NULL, updated_at = NOW() \
         RETURNING *",
    )
    .bind(container_resource_id)
    .bind(container_version)
    .bind(datacenter_resource_id)
    .fetch_one(db)
    .await
    .map_err(|e| err(format!("upsert materialization row: {e}")))?;

    // (d) Generate + deploy the one-shot materialize net.
    let air = serde_json::to_value(build_materialize_image_net(row.id, &image_ref, &conn))
        .map_err(|e| err(format!("serialize materialize net AIR: {e}")))?;
    let net_id = well_known::materialize_net_id(row.id);

    if let Err(e) = crate::petri::instance::deploy_instance(
        petri,
        &net_id,
        &air,
        petri_api_types::DispatchOptions::default(),
        None,
    )
    .await
    {
        tracing::warn!(
            %net_id, %e,
            "failed to deploy materialize net; recording row as failed"
        );
        let failed = sqlx::query_as::<_, ImageMaterializationRow>(
            "UPDATE image_materializations SET status = 'failed', last_error = $2, updated_at = NOW() \
             WHERE id = $1 RETURNING *",
        )
        .bind(row.id)
        .bind(format!("materialize net deploy failed: {e}"))
        .fetch_one(db)
        .await
        .map_err(|e| err(format!("record materialize deploy failure: {e}")))?;
        return Ok(failed);
    }

    tracing::info!(
        %net_id,
        %container_resource_id,
        image_ref = %image_ref,
        %datacenter_resource_id,
        "deployed materialize net"
    );
    Ok(row)
}
