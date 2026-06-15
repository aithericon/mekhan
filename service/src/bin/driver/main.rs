//! `legacy-migration-driver` — runs the legacy-migration pipeline end-to-end
//! against a SYNTHETIC NAS (docs/32 Phase 5).
//!
//! ```text
//! crawl (real op, in-process)
//!   └─▶ fold each batch → reconcile_batch  (verified/mismatch/orphan_disk)
//!         └─▶ hash-pending: real probe op on orphan_disk/mismatch rows
//!               └─▶ set content_hash + UPSERT catalogue + advance status
//! ```
//!
//! ## Transport (scope note)
//!
//! This bin invokes the `executor-file-ops` crawl/probe ops **IN-PROCESS**
//! against a `Local` `StorageConfig` (a local root path standing in for an NFS
//! mount) as the dev/scaffold harness. In production these SAME ops run inside a
//! co-located runner pulling jobs over NATS (already supported by the file-ops
//! backend); the NATS-dispatch + SSH-deployed-runner layer is the deferred
//! "real operations" step. The pipeline logic itself
//! (`mekhan_service::migration_driver`) is transport-agnostic.
//!
//! The whole bin is gated behind the `migration-driver` cargo feature
//! (`required-features`), so the default service build pulls in none of the
//! file-ops / OpenDAL deps.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use mekhan_service::migration_driver::{self, synthetic};

#[derive(Parser)]
#[command(
    name = "legacy-migration-driver",
    about = "Drive the legacy-migration pipeline (crawl → reconcile → hash → \
             register) against a synthetic NAS, using the REAL file-ops ops \
             in-process."
)]
struct Cli {
    /// Postgres URL. Defaults to `$MEKHAN__DATABASE_URL` (the service's
    /// canonical var) then `$MEKHAN_DATABASE_URL`.
    #[arg(
        long,
        value_name = "URL",
        env = "MEKHAN__DATABASE_URL",
        default_value = ""
    )]
    database_url: String,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Crawl `--root` (real op) and fold every batch through reconcile, writing
    /// `file_inventory` rows classified verified/mismatch/orphan_disk.
    IndexReconcile {
        /// File-server identifier (matches `legacy_file_index.file_server_id`).
        #[arg(long)]
        file_server_id: String,
        /// Absolute root directory of the synthetic NAS / NFS mount.
        #[arg(long)]
        root: String,
        /// Files per emitted crawl batch.
        #[arg(long, default_value_t = 5000)]
        batch_size: usize,
    },

    /// Targeted-hash the pending rows (orphan_disk/mismatch) via the real probe
    /// op, set content_hash, UPSERT catalogue, advance status.
    HashPending {
        #[arg(long)]
        file_server_id: String,
        #[arg(long)]
        root: String,
        /// Also re-probe this percentage (0-100) of `verified` rows as an audit
        /// sample (provenance-only, no status change).
        #[arg(long, default_value_t = 0)]
        sample_verified: u8,
    },

    /// Dev-only: write a synthetic-NAS tree under `--root` (a tempdir if
    /// omitted) + seed matching `legacy_file_index` baseline rows so a run
    /// yields verified + mismatch + orphan_disk + orphan_db classes.
    SeedSynthetic {
        #[arg(long)]
        file_server_id: String,
    },

    /// Copy bytes from a source server to a target server (REAL copy op),
    /// verify each copy by re-probing the destination (REAL probe op), and
    /// record a `copied` inventory row per verified copy. NEVER deletes.
    ///
    /// Selector: `--hash <h>` migrates that one content hash; `--all-canonical`
    /// migrates every `is_canonical` source row (add `--respect-target` to take
    /// only rows whose `migration_target` is the target server).
    Migrate {
        /// Source file-server identifier.
        #[arg(long)]
        source_server: String,
        /// Absolute root directory of the SOURCE synthetic NAS.
        #[arg(long)]
        source_root: String,
        /// Target file-server identifier (recorded on the copied rows).
        #[arg(long)]
        target_server: String,
        /// Absolute root directory of the TARGET synthetic NAS.
        #[arg(long)]
        target_root: String,
        /// Migrate only this content hash.
        #[arg(long, conflicts_with = "all_canonical")]
        hash: Option<String>,
        /// Migrate every `is_canonical` source row.
        #[arg(long, conflicts_with = "hash")]
        all_canonical: bool,
        /// With `--all-canonical`, restrict to rows whose `migration_target`
        /// equals `--target-server`.
        #[arg(long, requires = "all_canonical")]
        respect_target: bool,
    },

    /// Delete source copies on a server (REAL delete op) — but ONLY for rows
    /// where a verified copy survives on a DIFFERENT server
    /// (`status IN ('copied','verified')`, same `content_hash`). Rows without a
    /// surviving verified copy are SKIPPED, never deleted.
    Retire {
        /// File-server identifier to retire copies from.
        #[arg(long)]
        server: String,
        /// Absolute root directory of the synthetic NAS to delete from.
        #[arg(long)]
        root: String,
        /// Restrict candidates to rows whose `content_hash` is in
        /// `legacy_delete_queue` (the surviving-copy gate STILL applies).
        #[arg(long)]
        honor_delete_queue: bool,
        /// List eligible rows but delete NOTHING on disk and change no status.
        #[arg(long)]
        dry_run: bool,
    },
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

    match cli.command {
        Command::IndexReconcile {
            file_server_id,
            root,
            batch_size,
        } => {
            let counts =
                migration_driver::index_reconcile(&pool, &file_server_id, &root, batch_size)
                    .await
                    .context("index-reconcile")?;
            println!(
                "index-reconcile: verified={} mismatch={} orphan_disk={}",
                counts.verified, counts.mismatch, counts.orphan_disk
            );
        }
        Command::HashPending {
            file_server_id,
            root,
            sample_verified,
        } => {
            let counts =
                migration_driver::hash_pending(&pool, &file_server_id, &root, sample_verified)
                    .await
                    .context("hash-pending")?;
            println!(
                "hash-pending: orphan_disk_registered={} mismatch_rehashed={} \
                 verified_sampled={} probe_failed={}",
                counts.orphan_disk_registered,
                counts.mismatch_rehashed,
                counts.verified_sampled,
                counts.probe_failed
            );
        }
        Command::SeedSynthetic { file_server_id } => {
            let nas = synthetic::build(&pool, &file_server_id)
                .await
                .context("seed synthetic NAS")?;
            // The tempdir is dropped at exit, so the seeded tree is ephemeral —
            // this subcommand is for inspecting the BASELINE rows + verifying the
            // generator, not for a persistent NAS. For an e2e run, prefer the
            // integration test which keeps the tempdir alive across both phases.
            println!(
                "seed-synthetic: root={} verified={} mismatch={} orphan_disk={} orphan_db={}",
                nas.root_str(),
                nas.verified.path,
                nas.mismatch.path,
                nas.orphan_disk.path,
                nas.orphan_db_path
            );
        }
        Command::Migrate {
            source_server,
            source_root,
            target_server,
            target_root,
            hash,
            all_canonical,
            respect_target,
        } => {
            let selector = if let Some(h) = hash {
                migration_driver::MigrateSelector::Hash(h)
            } else if all_canonical {
                migration_driver::MigrateSelector::AllCanonical { respect_target }
            } else {
                anyhow::bail!("migrate: pass either --hash <h> or --all-canonical");
            };
            let counts = migration_driver::migrate(
                &pool,
                &source_server,
                &source_root,
                &target_server,
                &target_root,
                selector,
            )
            .await
            .context("migrate")?;
            println!(
                "migrate: copied={} verified={} failed={}",
                counts.copied, counts.verified, counts.failed
            );
        }
        Command::Retire {
            server,
            root,
            honor_delete_queue,
            dry_run,
        } => {
            let counts =
                migration_driver::retire(&pool, &server, &root, honor_delete_queue, dry_run)
                    .await
                    .context("retire")?;
            println!(
                "retire: deleted={} skipped_no_verified_copy={} deleted_from_queue={} dry_run={}",
                counts.deleted, counts.skipped_no_verified_copy, counts.deleted_from_queue, dry_run
            );
        }
    }

    pool.close().await;
    Ok(())
}

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
