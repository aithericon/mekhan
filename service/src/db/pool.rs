use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;

pub async fn create_pool(database_url: &str) -> Result<PgPool, sqlx::Error> {
    let pool = PgPoolOptions::new()
        .max_connections(10)
        .connect(database_url)
        .await?;

    // NOTE: `sqlx::migrate!` embeds the ./migrations directory at COMPILE time.
    // Touch this line whenever a migration is added/renamed so the macro re-embeds
    // it — sccache content-hashes this file but NOT the external migrations dir, so
    // a real edit here is required to bust the cache (a bare `touch` won't).
    // (latest: 20240173000000_catalogue_data_types.sql — registered data types
    //  (schema-digest promotion): catalogue_data_types +
    //  catalogue_data_type_digests. 20240171–172 are reserved by the in-flight
    //  IAM worktree — renumber-check `ls migrations | tail` before commit.)
    // (latest: 20240168000000_catalogue_query.sql — catalogue query layer:
    //  fmeta expression indexes (format/num_rows/schema digest) +
    //  catalogue_saved_queries table.)
    // (latest: 20240167000000_inventory_snapshots.sql — file-analytics growth
    //  snapshots (Cut 2); 20240166 promotes size/mtime/uid/gid/extension onto
    //  file_inventory. Renumber-check `ls migrations | tail` before commit —
    //  concurrent unpushed branches may also claim 20240166+.)
    // (latest: 20240157000000_hpi_tasks_workspace_offer.sql — P3 humans-as-capacity:
    //  hpi_tasks.workspace_id + claimed_at, for the offered→claimed lifecycle.)
    // (latest: 20240156000000_roster_members.sql — P2 humans-as-capacity: the roster.)
    // (latest: 20240155000000_model_idle_evict.sql — model-pool idle-eviction
    //  (sleep/wake): model_states.idle_evict col + model_replicas 'sleeping' status.)
    // (latest: 20240154000000_reconcile_views.sql — legacy-migration reconcile
    //  views. Merge de-collided a duplicate 20240152000000: node_replicas → 151,
    //  model_states_policy → 152, catalog_content_addressed → 153, reconcile → 154.)
    // (latest: 20240135000000_capability_types.sql — Phase 4 capability registry)
    // (latest: 20240134000000_runners.sql — Lab Runner Fleet).
    sqlx::migrate!("./migrations").run(&pool).await?;

    Ok(pool)
}
