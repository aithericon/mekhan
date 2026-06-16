//! `mekhan-importer` — offline legacy ArangoDB dump → Postgres importer
//! (docs/32 Phase 2).
//!
//! Bulk-loads the dumped `files` and `files_to_delete` collections (NDJSON,
//! gz-aware) into the legacy staging tables via Postgres `COPY`, then
//! set-dedups `legacy_file_index` into the content-addressed
//! `catalogue_entries`. It does **NOT** write `file_inventory` — inventory is
//! observed reality from the `crawl` op alone, and seeding it from the dump
//! would make `orphan_db` undetectable.
//!
//! Streaming throughout: the real `files` collection is 3.96M lines / 2.3 GB,
//! so nothing is materialized in memory.
//!
//! ## Idempotency
//! The staging load uses **TRUNCATE + COPY**: each run starts from an empty
//! `legacy_file_index` / `legacy_delete_queue` and rebuilds the one-shot
//! baseline. (TRUNCATE is the right call for a full-dump baseline; an
//! incremental refresh would instead COPY into a temp table and UPSERT.) The
//! catalogue dedup is `ON CONFLICT (content_hash) DO NOTHING`, so re-runs never
//! create duplicate catalogue rows.
//!
//! ## Connection
//! Reuses `mekhan_service::db::create_pool`, which runs the embedded migrations
//! on connect — so the staging tables are guaranteed present. The database URL
//! comes from `--database-url` or, by default, the same `MEKHAN__DATABASE_URL`
//! / `MEKHAN_DATABASE_URL` env vars the service uses.

mod copy;
mod dedup;
mod source;
mod tsv;

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;

#[derive(Parser)]
#[command(
    name = "mekhan-importer",
    about = "Offline legacy ArangoDB dump importer: COPY-load the `files` / \
             `files_to_delete` collections into Postgres staging, then dedup \
             into the content-addressed catalogue. Does not write inventory."
)]
struct Cli {
    /// Path to the `files` collection (NDJSON, one doc per line; `.gz` is
    /// decompressed transparently). → `legacy_file_index` (+ catalogue dedup).
    #[arg(long, value_name = "PATH")]
    files: PathBuf,

    /// Path to the `files_to_delete` collection (NDJSON; `.gz` ok).
    /// → `legacy_delete_queue`. Payload is nested under `fingerprint`.
    #[arg(long = "delete-queue", value_name = "PATH")]
    delete_queue: PathBuf,

    /// Postgres URL. Defaults to `$MEKHAN__DATABASE_URL` (figment, double
    /// underscore — the service's canonical var) then `$MEKHAN_DATABASE_URL`.
    #[arg(
        long,
        value_name = "URL",
        env = "MEKHAN__DATABASE_URL",
        default_value = ""
    )]
    database_url: String,

    /// Skip the catalogue dedup step (load staging only).
    #[arg(long)]
    skip_dedup: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let cli = Cli::parse();

    let database_url = resolve_database_url(&cli.database_url)?;

    tracing::info!("connecting to Postgres (runs embedded migrations)…");
    let pool = mekhan_service::db::create_pool(&database_url)
        .await
        .context("create pool / run migrations")?;

    // --- staging: TRUNCATE + COPY (re-runnable one-shot baseline) ----------
    tracing::info!("truncating legacy staging tables");
    sqlx::query("TRUNCATE legacy_file_index, legacy_delete_queue")
        .execute(&pool)
        .await
        .context("truncate legacy staging tables")?;

    tracing::info!(path = %cli.files.display(), "loading `files` → legacy_file_index");
    let files_src = source::open(&cli.files)?;
    let files_stats = copy::load_files(&pool, files_src).await?;
    tracing::info!(
        rows = files_stats.rows,
        skipped = files_stats.skipped,
        "loaded legacy_file_index"
    );

    tracing::info!(
        path = %cli.delete_queue.display(),
        "loading `files_to_delete` → legacy_delete_queue"
    );
    let del_src = source::open(&cli.delete_queue)?;
    let del_stats = copy::load_delete_queue(&pool, del_src).await?;
    tracing::info!(
        rows = del_stats.rows,
        skipped = del_stats.skipped,
        "loaded legacy_delete_queue"
    );

    // --- dedup → catalogue (content-addressed) -----------------------------
    if cli.skip_dedup {
        tracing::info!("--skip-dedup set; leaving catalogue untouched");
    } else {
        tracing::info!("deduping legacy_file_index → catalogue_entries (category='file')");
        let inserted = dedup::run(&pool).await?;
        tracing::info!(catalogue_inserted = inserted, "dedup complete");
    }

    pool.close().await;

    tracing::info!(
        legacy_file_index = files_stats.rows,
        legacy_delete_queue = del_stats.rows,
        "import finished"
    );
    Ok(())
}

/// Resolve the database URL: the `--database-url` flag (which also honors
/// `$MEKHAN__DATABASE_URL` via clap's `env`) first, then fall back to the
/// single-underscore `$MEKHAN_DATABASE_URL`.
fn resolve_database_url(flag: &str) -> Result<String> {
    if !flag.is_empty() {
        return Ok(flag.to_string());
    }
    if let Ok(url) = std::env::var("MEKHAN_DATABASE_URL") {
        if !url.is_empty() {
            return Ok(url);
        }
    }
    anyhow::bail!(
        "no database URL: pass --database-url or set MEKHAN__DATABASE_URL \
         (double underscore) / MEKHAN_DATABASE_URL"
    )
}
