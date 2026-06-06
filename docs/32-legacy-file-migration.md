# Legacy file migration — implementation prep

## Context

The predecessor platform cataloged ~3.96M files (~76 TB across 4 NAS servers) in an
**ArangoDB** (`files` collection — every doc already carries `hash: "SHA256:…"`, `size`,
`path`, `file_server_id`, provenance). We need to ingest/reconcile/migrate that corpus into
this platform: integrity-check (cheap, legacy hashes exist), register by-reference (bytes stay
on NAS), find orphans/duplicates, then selectively copy bytes to the new server and retire old
copies. Design is agreed in `docs/32-legacy-file-migration.md`.

This task **builds and dev-tests the machinery in a dedicated worktree via a Workflow, up to
the point of real operations** — everything is exercised against a *synthetic temp-dir "NAS"* +
a local runner; **nothing touches the real NAS or production**. Scope chosen: through the
migrate/retire scaffold (phases 1–6); deferred: SSH-bastion runner provisioning, live-Arango
incremental refresh, frontend reconcile UI, and the artifact-copy fold-in (docs/32 §4.1).

Three design refinements from planning (reflected in §4 below):
- **Catalog PK** = surrogate `entry_id UUID`; `content_hash` is the unique logical identity,
  enforced by a **UNIQUE CONSTRAINT** (not a partial-unique index) so it can serve as an FK
  target — the nullable column already permits many NULLs (job artifacts) while enforcing
  uniqueness on every non-null hash. `(execution_id, id)` survives only as a secondary
  **partial-unique index** (`WHERE execution_id IS NOT NULL AND execution_id <> ''`), preserving
  the existing artifact lookup. Resolves the compound-PK concern; zero frontend route change.
- **Dump populates catalog + staging only, NOT inventory.** Inventory rows come *only* from
  `crawl` (observed reality) — otherwise `orphan_db` is undetectable.
- **OpenDAL 0.55 `fs` lister returns no size/mtime** → `crawl` must `stat()` each entry.

## §4 Design (what gets built)

### Catalog reshape (`service`, content-addressed)
- New migration `service/migrations/20240152000000_catalog_content_addressed.sql`:
  - `ALTER catalogue_entries`: add `entry_id UUID` (new PK, `DEFAULT gen_random_uuid()`), add
    `content_hash TEXT`; **drop the composite `(execution_id, id)` PK and add the surrogate PK
    FIRST** (Postgres refuses to drop NOT NULL on a PK column), then relax `NOT NULL` on
    `execution_id/id/name/category/filename` (legacy logical rows set `category='legacy'`).
  - `ALTER TABLE catalogue_entries ADD CONSTRAINT uq_cat_content_hash UNIQUE (content_hash);` —
    a UNIQUE **constraint** (FK-targetable), not a partial index. The nullable column permits
    many NULLs (job artifacts) while enforcing uniqueness on every non-null hash.
  - `CREATE UNIQUE INDEX uq_cat_exec_id ON catalogue_entries(execution_id, id) WHERE execution_id IS NOT NULL AND execution_id <> '';`
  - Native provenance columns stay as optional columns (most already exist: `execution_id, id,
    job_id, instance/source_net, source_place, process_id, process_step, storage_path,
    size_bytes, mime_type`). Legacy-stack provenance does NOT get columns.
- DTOs `service/src/catalogue/model.rs`: `CatalogueEntry` gains `entry_id: Option<Uuid>` +
  `content_hash: Option<String>`; `CatalogueRegisterCommand` gains `content_hash: Option<String>`.
  The relaxed display columns stay non-Option `String` on the DTO — the catalogue read path
  projects them with `COALESCE(...,'')` in `queries.rs` (explicit `ENTRY_COLUMNS` projection
  replacing `SELECT *`) so the existing job-net consumers (subscriptions, triggers, responder)
  keep their `String` view; the legacy surface reads through `file_inventory` instead. Filter/sort
  whitelists `queries.rs` gain `content_hash`. Write path `service/src/causality/ingest.rs`
  `register_catalogue_entry()`: INSERT gains `content_hash` (bind NULL on the job path; dedup
  stays on `nats_msg_id`).

### `file_inventory` + staging (same migration)
- `file_inventory`: `id UUID PK`, `content_hash TEXT` (LOGICAL link → catalog, index only / **no
  hard FK** — a physical file is observed by `crawl` before its catalog row exists),
  `file_server_id TEXT`, `path TEXT`, `UNIQUE(file_server_id, path)`, `status TEXT`
  (`indexed→verified→registered→copied→deleted` + `mismatch/orphan_disk/orphan_db`),
  `is_canonical BOOL`, `copy_of UUID`, `migration_target TEXT`, `provenance JSONB`,
  `first_seen/last_seen/last_verified/updated_at`. Indexes: `content_hash`, `status`,
  `(file_server_id,status)`. **No size/mime/name columns.**
- `legacy_file_index` (raw 4M baseline): `legacy_key PK, file_server_id, path, hash, size,
  node_id, owner_id, created, modified, raw JSONB`; index on `hash`, `(file_server_id,path)`.
- `legacy_delete_queue` (97k honored deletions): `key PK, hash, size, modified`.

### Bulk-register HTTP API (`service`)
New `service/src/inventory/` module (mirrors `catalogue/`: `mod/model/queries/repository/handlers`)
+ routes in `service/src/lib.rs`, `#[utoipa::path]` + `ToSchema` DTOs, then `just dev::openapi`:
- `POST /api/v1/inventory/register` — batched by-reference upsert. Per item: if it carries
  content metadata + a `content_hash`, UPSERT a logical `catalogue_entries` row (`ON CONFLICT
  (content_hash) DO NOTHING`, `execution_id`/`id` NULL, `category='legacy'`); then UPSERT the
  `file_inventory` row (`ON CONFLICT (file_server_id, path) DO UPDATE SET status/last_seen/
  updated_at/content_hash`). No bytes. Returns `{inventory_upserted, catalogue_inserted}`. For
  online crawl/reconcile output, not the 4M load.
- `GET /api/v1/inventory` (paginated list/filter/sort: `content_hash, file_server_id, path,
  status, is_canonical`) + `GET /api/v1/inventory/stats` (counts by status + by server). Plus
  reconcile-report endpoints (orphans/dupes/mismatches) in a later phase.

### Offline importer (`service`)
New `[[bin]] mekhan-importer` in `service/Cargo.toml`, reusing `mekhan_service::db::create_pool`
(`service/src/db/pool.rs` — embedded migrations guarantee schema). Reads extracted Arango JSON
collections; **`PgCopyIn` (`copy_in_raw("COPY legacy_file_index … FROM STDIN")`)** streaming TSV
(stream-parse JSON, never hold 2.3GB). Then set-based SQL: dedup `legacy_file_index` by `hash`
→ `catalogue_entries` (`ON CONFLICT (content_hash) … DO NOTHING`, strip `"SHA256:"`→bare-hex
lowercase to match probe), COPY `files_to_delete`→`legacy_delete_queue`. **Does NOT write
inventory.** Dev-tested on a sampled subset of the real dump.

### `crawl` op (`executor`, the one new op)
- `executor/crates/executor-backend-configs/src/file_ops.rs`: add `Crawl(CrawlConfig)` to
  `FileOpsConfig`; `CrawlConfig { prefix, storage: StorageConfig, batch_size (default ~5000),
  resume_from: Option<String>, stat: bool=true }`.
- New `executor/crates/executor-file-ops/src/ops/crawl.rs`: stream
  `operator.lister_with(&prefix).recursive(true)` (NOT `list_with` — buffers); skip dir markers;
  **`operator.stat(path)` per entry** for size+mtime (mirrors `ops/list.rs:49`); **no `read`**
  (metadata-only). Emit per `batch_size` via `event_stream.item(channel, episode_uid, idx, batch)`
  + `event_stream.close(...)` (`executor-backend/src/traits.rs:76,83`). Check `CancellationToken`
  between batches (mirror `backend.rs:128`). `resume_from` → `start_after` (best-effort; real
  idempotency = inventory `UNIQUE(file_server_id,path) ON CONFLICT DO NOTHING`).
- Wire arms: `ops/mod.rs` dispatch + validate; `backend.rs` `op_name` and pass the
  currently-unused `event_stream` through to crawl.
- **`ProbeConfig` checksum**: add explicit `checksum_algo` flag forcing bare-hex SHA-256
  (`probe.rs` already computes Sha256 via `aithericon_file_metadata` — make it deterministic +
  unit-test the output format; this is the reconcile-match linchpin).

### Reconcile + targeted-hash + migrate scaffold (`service` + campaign)
- Reconcile: SQL views (new migration) joining `file_inventory ⋈ legacy_file_index ⋈
  catalogue_entries` → classify `verified/mismatch/orphan_disk/orphan_db/duplicate` (pick one
  `is_canonical` per hash); report endpoints. A driver folds crawl batches → inventory via the
  register API.
- Targeted-hash driver: issue `probe` jobs only for `orphan_disk`/`mismatch` (+ audit sample);
  populate `content_hash`, link inventory→catalog.
- Migrate/retire campaign scaffold: gated `copy → probe-verify(hash match) → delete`, reusing
  existing file-ops `copy/move/delete`. Honors `legacy_delete_queue`; deletes source only after a
  verified copy. **Dev-tested on synthetic 2-server temp dirs only.**

## Build: worktree + Workflow

**Setup (serial, once):** `just dev::worktree-add legacy-migration` (auto-slot, ports, .envrc) on
branch `feat/legacy-migration`; worktree at `.claude/worktrees/legacy-migration`.

**Workflow guardrails (known footguns):** every edit agent gets the worktree path prefix and
operates ONLY under `.claude/worktrees/legacy-migration/...` (absolute paths otherwise land in
the primary checkout); **no destructive git** in any agent (`checkout/reset/restore/stash/clean`
forbidden — one agent's `git checkout -- dir/` has wiped siblings' work); re-Read before Edit;
end each agent with read-only `git status --short` confined-diff check.

**Phases & gates** (parallelism limited by *shared files*: `service/Cargo.toml`, `lib.rs`,
`catalogue/handlers.rs`, generated openapi — only one agent edits each at a time):
1. **Catalog reshape + inventory + staging migration + DTOs/queries/ingest + register & inventory
   API + openapi regen + docs/32 refinements.** SERIAL (foundational). GATE.
2. **Importer bin** (after Gate 1; touches `service/Cargo.toml` + new files).
3. **`crawl` op + `ProbeConfig` checksum + tests** (after Gate 1; `executor` crates — disjoint
   from P2, can run parallel to P2).
4. **Reconcile views + report endpoints + fold driver** (after P1; shares `handlers.rs`/`lib.rs`
   — serialize those edits after P2's Cargo.toml lands).
5. **Targeted-hash driver** (after P3 + P4).
6. **Migrate/retire campaign scaffold** (after P5).

**Gate (each phase that compiles/changes API):** `just ci::quality-rust` + `just ci::test-rust`
(against a real Postgres — there is **no `.sqlx` offline dir**, queries are runtime-checked, so
tests MUST hit a live DB) + `just dev::openapi` + `just ci::openapi-drift` + `(cd app && pnpm run check)`.

## Critical files
- `service/migrations/20240152000000_*.sql` (new; after `20240151000000_node_replicas.sql`), `service/src/db/pool.rs`
- `service/src/catalogue/{model,queries,handlers,responder}.rs`, `service/src/lib.rs`, `service/src/causality/ingest.rs`
- `service/src/inventory/{mod,model,queries,repository,handlers}.rs` (new), `service/src/openapi.rs`
- `service/Cargo.toml` + new `service/src/bin/importer/`
- `executor/crates/executor-backend-configs/src/file_ops.rs`, `executor/crates/executor-file-ops/src/{backend.rs,ops/mod.rs,ops/probe.rs}` + new `ops/crawl.rs`
- `app/src/lib/api/*` (regenerated only) + `docs/32-legacy-file-migration.md` (refinements)

## Verification (no real NAS)
- **Importer**: load a sampled subset of the real dump into dev Postgres; assert COPY succeeds,
  `"SHA256:"` stripping correct, dedup GROUP BY yields one catalog row per unique hash, inventory
  untouched. (Full 4M run available but not required this pass.)
- **crawl/probe/copy**: run `executor-service` with `file-ops` feature against local NATS, pointed
  at synthetic temp-dir trees via `StorageConfig{ backend: Local, endpoint: <tempdir> }` (the real
  `fs://` code path). Assert streamed batches land in inventory; resume is idempotent.
- **Reconcile (core test)**: crafted fixtures provoking each class — `verified` (on disk + staging,
  sizes match), `mismatch` (size differs), `orphan_disk` (on disk, not in staging), `orphan_db`
  (in staging, not crawled), `duplicate` (same hash, two paths → exactly one `is_canonical`).
  Assert the views return exactly these. (This suite also guards the "dump doesn't write
  inventory" refinement.)
- **Migrate scaffold**: two synthetic "servers"; run gated `copy → verify → delete` on a few files;
  assert source deleted only after verified copy and that `legacy_delete_queue` is honored.
- Final: full `just ci::quality-rust` + `just ci::test-rust` + `just ci::openapi-drift` + svelte-check green.
