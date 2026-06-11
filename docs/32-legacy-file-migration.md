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

### 4.1 File servers as first-class entities (`file_servers`)
> **Evolved → multi-endpoint (§4.3).** §4.1 below describes the original
> single-transport shape (transport `kind` + `base_path` + `resource_ref` ON the
> entity). That model has since split: the **entity is identity-only** and the
> *ways to reach it* are N typed child **endpoints** (`file_server_endpoints`).
> §4.3 is the authoritative shipped description.

Until this landed, `file_inventory.file_server_id` was a bare `TEXT` string the
control plane only knew by the name echoed back in inventory rows. `file_servers`
(`20240159000000_file_servers.sql`) makes each storage backend a real, manageable
entity — a **hybrid** model:
- The **entity** owns *identity + topology + lifecycle*: `key` (== the inventory
  `file_server_id`, **soft join, no FK** — preserves crawl-before-register and lets
  an unknown server still render with an "adopt" affordance), `display_name`, a
  transport `kind` (`object_store | s3 | sftp`; `nfs/local` reserved), optional
  `base_path`, `status`, `config`. Workspace-scoped (`UNIQUE(workspace_id, key)`).
- **Secrets never live on the entity.** `resource_ref` points at a workspace
  `resource` (by its `path`) holding the connection + credentials in Vault — an
  `s3` resource for `kind=s3`, the new `sftp` resource (username + inline-PEM
  `private_key` + `known_hosts`) for `kind=sftp`. The built-in `object_store`
  (the platform S3 bucket) auto-seeds at startup with no `resource_ref` (uses
  platform config), so every `log_artifact` copy lands on a tracked server.
- **Rollups are derived, never stored**: file count / summed size (via
  `catalogue_entries.size_bytes` joined on `content_hash`) / per-status breakdown
  are computed from `file_inventory` by `key` at read time.

HTTP: `GET/POST/PUT/DELETE /api/v1/file-servers`, `GET …/{key}`, and
`POST …/adopt` (promote an inventory key into an entity). The compiler is
unchanged — a node's file-server selection is resolved in the editor into the
existing `StorageConfig` (`backend` from `kind`, `resource_alias = resource_ref`,
`base_path` → prefix), so crawl/migrate connect via the existing
`resource_overlay → opendal` path. SFTP is wired end-to-end (`StorageBackend::Sftp`
+ opendal `services-sftp`; the inline PEM is materialized to a 0600 temp file at
operator-build).

### 4.2 Unified Data browser (`app` + `GET /api/v1/data/entries`)
The catalogue (logical) and inventory (physical) were split-world pages bridged
only by an un-navigable `content_hash`. `GET /api/v1/data/entries` joins them —
paginated catalogued entries (reusing the catalogue filter/sort DSL), each with
its physical `copies` (file-server names resolved) plus a peek + count of
uncatalogued (index-only) files. The `/data` page renders this as one browser
with **Entries** and **Servers** tabs; Catalogue + Inventory are dropped from the
nav (their lineage/provenance deep routes remain reachable).

### 4.3 Multi-endpoint file servers (identity + N typed endpoints)
The §4.1 entity carried a single transport (`kind` + `base_path` + `resource_ref`).
That bakes in one assumption that breaks for a real lab NAS: a backend is
reachable *one way*. The same physical server is often reachable several ways at
once — a co-located runner sees it as a `local_mount`, a remote worker over `sftp`,
a downstream job via its S3 gateway. So the model split into a **parent identity**
plus **N typed endpoints**:

- **`file_servers`** is now **identity + topology only** — `key` (== the inventory
  `file_server_id`, soft join, no FK), `display_name`, `status`, `config`,
  workspace-scoped (`UNIQUE(workspace_id, key)`). **No transport, no secrets.**
- **`file_server_endpoints`** (child rows, FK + cascade) are the ways to reach it.
  Each has an `access_method` ∈ `{object_store, s3, sftp, local_mount}`, a `root`
  prefix (the endpoint-namespace anchor mapping onto the server's canonical,
  server-relative paths), an optional `resource_ref` (the workspace `resource`
  holding connection + credentials in **Vault** — never on the row), a `group_id`
  (the capacity-group UUID a `local_mount` is reachable from), its own `status`,
  `priority` (operator routing override), and a `verification_status` /
  `last_verified` reconcile lifecycle. The built-in platform `object_store` bucket
  auto-seeds as one identity-only server + one `object_store` endpoint (no
  `resource_ref` → platform config).
- **Canonical paths** are server-relative: a copy's `file_inventory.path` is
  anchored under whatever endpoint `root` is used to reach it. `adopt` stamps the
  crawl-recorded `provenance.endpoint_root` onto the adopted endpoint so its `root`
  matches where the crawl anchored the paths.

HTTP: `GET/POST/PUT/DELETE /api/v1/file-servers`, `POST …/adopt`, the endpoint
sub-resource `GET/POST/PUT/DELETE /api/v1/file-servers/{key}/endpoints[/{id}]`,
and the on-demand probe `POST …/endpoints/{id}/verify` (§4.3.3).

#### 4.3.1 Read execution — owned by `access_method`
The serve bridge (`service/src/data/serve.rs`, handler
`GET /api/v1/data/entries/{content_hash}/content`) resolves a logical entry to its
physical copies × endpoints, routes (§4.3.2), and reads by method:

- **`local_mount`** — mekhan is **cred-free**. The bytes live on a filesystem mount
  reachable only from the capacity group's co-located runner, so mekhan publishes a
  `ServeRequest` to the runner over NATS and relays the reply frames into the HTTP
  body. The transport is a **streamed-reply protocol** on `fileserve.<group>.read`:
  the runner answers a request inbox with `OPEN → CHUNK* → CLOSE` (or a terminal
  `ERROR{kind}`) frames; mekhan cumulative-**acks** each consumed CHUNK on
  `<reply>.ack` to hold an in-flight **window** (back-pressure), and **path-jails**
  every read under the endpoint `root` on the runner side (an escape is an
  `ERROR{path_jail}`). The wire structs are mirrored byte-for-byte in mekhan and
  `executor-worker/src/fileserve.rs` (separate workspaces; serde shape is the
  contract). Ranges seek from an absolute offset (`bytes=START-[END]`; no suffix).
- **`object_store` / `s3`** — mekhan owns the read: presign a GET URL and **302**
  the browser straight to the store (default; bytes never transit mekhan), or
  **proxy** the bytes in-process when `config.proxy_s3_reads` is set (single-origin
  / firewalled). External `s3` (`resource_ref`) resolves its creds from Vault first.
- **`sftp`** — mekhan streams in-process through an opendal sftp Operator built from
  the resource's Vault creds (sftp has no presign).

#### 4.3.2 Cost-first, verification-gated read routing (+ fallback-on-miss)
`serve::route_candidates` replaces the old static method-preference order. It
**filters** to *routable* endpoints (`status ∈ {online, unknown}` AND
`verification_status ∉ {mismatch, conflict}` — proven-bad endpoints are excluded;
`unverified` is allowed, merely *less preferred*, so serve isn't bricked before a
probe runs — a strict-mode could additionally exclude `unverified`), then **orders**
by: (1) `priority` DESC (operator override wins), (2) effective transport **cost**
ASC (s3-presigned = cheapest since a 302 doesn't transit mekhan; `local_mount`; then
sftp — and when `proxy_s3_reads` flips s3 to a proxy it sorts *below* `local_mount`),
(3) `verified` before `unverified`, (4) stable. `?endpoint=<uuid>` **force-selects**
a single endpoint, bypassing the routable filter.

The handler tries the ordered list and the **first to start streaming wins**: a
candidate that reports the file *missing before the first byte* (local_mount
`ERROR{not_found|path_jail}`; s3/sftp `NotFound`/404) is skipped and the next is
tried (`ServeMiss::NotFound`); a non-recoverable error returns immediately
(`ServeMiss::Fatal`). No routable candidate at all → **409** ("no servable
endpoint … all offline/mismatch"); copies exist but every candidate missed → **404**.

#### 4.3.3 Hash-probe reconcile (`verified | mismatch | conflict`, missing-ok)
An endpoint *claims* to reach the same canonical files a crawl recorded. Reconcile
(`service/src/file_servers/reconcile.rs`) **verifies** the claim by re-reading a
sample of the server's recorded canonical paths *through* the endpoint and
comparing the fresh SHA-256 (bare lowercase hex — the `file_inventory.content_hash`
shape) against the inventory reference:
- present & hash == reference → **pass**;
- present & hash != reference → **`mismatch`** (the endpoint's `root` is mis-mapped —
  it serves the wrong bytes for that canonical path), with offending
  `(path, expected, got)` examples;
- two endpoints that each establish a *different* hash for the **same** canonical
  path → **`conflict`** (the copies genuinely diverge);
- `not_found` for a sampled path → a **coverage gap**, reported informationally,
  **never a failure** (an endpoint may legitimately hold only a subset). All sampled
  *present* paths passing → **`verified`** (a probe that saw only misses is
  vacuously verified). The crawl-source `local_mount` self-verifies — probing it
  re-reads the bytes that produced the reference hash.

Sampling is a **stratified random sample of K ≈ 50** of the server's inventory
paths, grouped by top-level path prefix and round-robined so a per-subtree
mis-mount can't slip through a flat sample (≤ K paths → probe all; a cap is logged).
Triggers: an **auto-probe** is `tokio::spawn`ed (non-blocking — never delays the
HTTP response) on endpoint **create / adopt / PUT**; the **on-demand**
`POST …/endpoints/{id}/verify` blocks and returns a `VerifyResult`
`{ verification_status, sampled, passed, mismatched, missing, examples[] }`. The
verdict + detail persist to `verification_status` / `last_verified` /
`config.verification`. Periodic re-verification is deferred. The probe transport is
abstracted (`ProbeReader`) so the verdict semantics are unit-tested with an
in-memory fake; the live reader reuses the exact `read_local_bytes` /
`read_remote` the serve path uses.

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

### Batch-fold transport + crawl campaigns (the at-scale shape) — BUILT 2026-06-10

The demo-50 shape (crawl batches → engine channel tokens → `join: gather` → per-file
`log_artifact`) does not survive the 4M-file corpus: the gathered collection token outgrows the
NATS max payload, and 4M per-file causality-projector events is ~50× the known 80k
projection-backlog meltdown. The production shape keeps the control plane flat — **cursors and
counts move through the net; files move through a durable side-channel; the DB is written
set-based**:

- **Crawl sink mode** (`CrawlConfig.sink: { mode: "reconcile"|"index", file_server_id }`): the
  op publishes one `FoldBatch` per filled batch to the `INVENTORY_FOLD` JetStream stream
  (subject `inventory.fold.batch.<server>`) instead of emitting channel items. The publish-ack
  lands BEFORE the resume cursor advances; `Nats-Msg-Id = {execution_id}-{episode}-{batch_idx}`
  dedups republishes. The `BatchSink` trait lives in `executor-backend` (backends never touch
  NATS); the NATS impl (`executor-worker/src/fold_sink.rs`) stamps the runner's serve identity
  (`runner_id` || routing partition) onto every batch as `serve_group`.
- **Fold consumer** (`service/src/inventory/fold.rs`, durable `mekhan-inventory-fold`):
  `reconcile` batches reuse `reconcile_batch` (legacy classify, hash inherit); `index` batches
  upsert hashless observations (status `indexed`), and hash-carrying items couple the catalogue
  half in the same tx. Both stamp `endpoint_root` + `serve_group` into provenance, keeping the
  file-server `adopt` autostamp chain intact for batch-crawled servers. All upserts idempotent
  on `(file_server_id, path)` → at-least-once delivery is harmless. Both fold disciplines are
  set-based: the batch binds as parallel arrays and one `UNNEST` statement joins
  `legacy_file_index` (LATERAL `LIMIT 1`), classifies, and upserts — a constant number of
  statements per batch regardless of item count (`reconcile_batch` /
  `inventory::queries::fold_index_batch`), which is what makes the 4M campaign a few hundred
  statements instead of ~8M round-trips. Duplicate paths within one batch collapse to the last
  occurrence.
- **Chunking**: `CrawlConfig.max_batches` caps one invocation; new `exhausted` output (lister
  EOF, not a cap/cancel stop) is the campaign's exit condition. Resume is capability-aware:
  native `start_after` on S3; client-side skip-until-cursor elsewhere (the `fs` lister silently
  ignores `start_after` — without this a resumed fs chunk would re-walk from the start forever).
  A vanished cursor is a hard error, not a silent restart.
- **Campaign template** (demo `55-crawl-campaign`): a Loop with accumulators
  `cursor ← crawl.last_path`, `total ← total + crawl.count`, condition
  `crawl.exhausted == false`, body = sink-mode crawl with
  `resume_from: "{{ campaign.cursor }}"` — a Loop-accumulator borrow interpolated into a backend
  config (file_ops configs now run the same PerField placeholder pipeline as LLM prompts).
  Engine work per iteration is constant regardless of corpus size; cancel/resume granularity is
  one chunk.

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

### Phase 5 — the pipeline DRIVER (`legacy-migration-driver` bin)

The Phase 5 driver ties the pieces together and runs the pipeline end-to-end on a **synthetic
NAS** (a local root path standing in for an NFS mount), using the **REAL** Phase 3 op code:

```text
crawl (real op, in-process)
  └─▶ fold each emitted batch → reconcile_batch  (verified/mismatch/orphan_disk)
        └─▶ hash-pending: real probe op on orphan_disk/mismatch rows
              └─▶ set content_hash + UPSERT catalogue + advance status
```

- **Location.** Transport-agnostic pipeline logic lives in `service/src/migration_driver/`
  (`mod.rs` + `synthetic.rs`), a `#[cfg(feature = "migration-driver")] pub mod` on the
  `mekhan-service` lib so the integration test can call it directly. The
  `legacy-migration-driver` bin (`service/src/bin/driver/main.rs`,
  `required-features = ["migration-driver"]`) is a thin clap wrapper. The feature pulls in
  optional path-deps (`executor-file-ops`, `executor-backend`, `executor-storage` `[opendal]`,
  `opendal`, `tokio-util`, `tempfile`, `fmeta`); **the default `mekhan-service` lib/bins gain no
  new mandatory deps.**
- **Subcommands.** `index-reconcile --file-server-id <id> --root <abs dir> [--batch-size N]`
  (crawl + fold); `hash-pending --file-server-id <id> --root <abs dir> [--sample-verified PCT]`
  (probe pending rows → register); `seed-synthetic --file-server-id <id>` (dev-only fixture
  generator).
- **In-process op invocation (the seam).** The driver builds a `Local`
  `StorageConfig { backend: Local, endpoint: <root> }`, builds an OpenDAL `Operator` via
  `aithericon_executor_storage::build_operator` (the exact path the file-ops backend uses), and
  calls the op FUNCTIONS directly — `ops::crawl::execute(&cfg, &op, prefix, Some(event_stream),
  &cancel)` and `ops::probe::execute(&cfg, &op, prefix, run_dir)` (probe's `run_dir` is a
  tempdir). No `ExecutionBackend`/`RunContext`/`ExecutionJob` reconstruction. The crawl batches
  are folded inline by a small `EventStream` impl (`ReconcileSink`) whose `item()` runs
  `reconcile_batch` per batch.
- **`orphan_disk` → `verified`.** Once an orphan_disk row is hashed (real SHA-256) and a
  `catalogue_entries` row is UPSERTed by that `content_hash` (`category='observed'`, size from
  probe), its status advances to `verified` — it's now hashed + registered.
- **`mismatch` stays `mismatch`.** The freshly-computed probe hash is recorded in
  `content_hash` + `provenance.probed_hash`, but the size disagreement with the legacy baseline
  is a curation decision, not auto-resolved by the driver.
- **Probe checksum-only fallback (Phase 3 op refinement).** A 4M-file NAS corpus is mostly
  arbitrary binaries no metadata extractor models, so `extract_metadata` returns
  `UnsupportedFormat`. Since the **checksum is the reconcile linchpin**, the probe op now falls
  back to `FileMetadata::checksum_only(path)` (new constructor in `fmeta`) when extraction fails
  **and** a `checksum_algo` is requested — the probe still emits the bare-hex SHA-256
  `checksum_digest` instead of failing. Format-modeled probes are unchanged.

> **Transport scope note (deferred "real operations" step).** The driver invokes the ops
> **in-process** as the dev/scaffold harness — no NATS, no runner. In production these SAME ops
> run inside a co-located runner that pulls jobs over NATS (already supported by the file-ops
> backend); the NATS-dispatch + SSH-deployed-runner layer is the deferred real-operations step.
> Only the op-invocation seam changes when it moves behind NATS — the driver's pipeline logic
> (fold + hash + register) is transport-agnostic.

The e2e test `service/tests/driver_pipeline.rs` (gated on `MEKHAN__DATABASE_URL`, unique
`test-drv-<uuid>` server, RAII cleanup) builds a synthetic tree + baseline, runs both phases,
and asserts the four reconcile classes end-to-end.

### Phase 6 — MIGRATE + RETIRE (the destructive end)

Phase 6 adds the campaign's two terminal stages to the SAME `legacy-migration-driver` bin
(`service/src/migration_driver/migrate.rs`, two new clap subcommands), exercising the **REAL**
`executor-file-ops` `copy` / `probe` / `delete` ops **in-process** against a synthetic
**2-server NAS** (two local roots standing in for two NFS mounts):

```text
migrate(serverA → serverB)              retire(serverA)
  copy bytes      (REAL copy op)          eligible IFF a sibling inventory row has the
  probe dest      (REAL probe op)           SAME content_hash on a DIFFERENT server with
  verify hash == content_hash               status IN ('copied','verified')
  INSERT copied row on B                  delete src (REAL delete op) → status='deleted'
```

- **`migrate(source_server, source_root, target_server, target_root, selector)`** → `copied /
  verified / failed`. The selector is a single `content_hash` **or** every `is_canonical`
  source row (optionally filtered to `migration_target == target_server`). Per row: run the
  REAL copy op (`source = destination = row.path`, same relative path; `source_storage =
  Local(source_root)`, `destination_storage = Local(target_root)` — the streaming cross-root
  path), then VERIFY by running the REAL probe op on the destination and comparing its bare-hex
  SHA-256 `checksum_digest` to the row's `content_hash`. On match: UPSERT a new `file_inventory`
  row (`file_server_id = target_server`, `status = 'copied'`, `copy_of = source id`,
  `is_canonical = false`) on `(file_server_id, path)`. On mismatch / copy error: record the
  reason in the source row's `provenance.migrate_error`, count it failed, create NO copied row.
  **`migrate` NEVER deletes anything.**
- **`retire(server, root, honor_delete_queue, dry_run)`** → `deleted /
  skipped_no_verified_copy / deleted_from_queue`. The **hard safety gate**: a source row is
  eligible for deletion **only if** a sibling `file_inventory` row exists with the SAME
  `content_hash`, a DIFFERENT `file_server_id`, and `status IN ('copied','verified')` — i.e. a
  verified copy survives elsewhere. This `EXISTS` predicate is computed in SQL
  (`eligible_for_deletion`); there is **no code path that runs the delete op without it**. With
  `honor_delete_queue`, the candidate set is restricted to rows whose `content_hash` is in
  `legacy_delete_queue`, but the surviving-copy gate STILL applies (a queued deletion with no
  surviving copy is skipped, never deleted). Eligible rows (non-`dry_run`) get the REAL delete
  op (`ignore_missing = false`) and `status = 'deleted'`; rows without a surviving copy are
  counted `skipped_no_verified_copy` and left untouched. `dry_run` lists eligible rows but
  deletes nothing on disk and changes no status.

Subcommands: `migrate --source-server <id> --source-root <dir> --target-server <id>
--target-root <dir> [--hash <h> | --all-canonical [--respect-target]]` and `retire --server
<id> --root <dir> [--honor-delete-queue] [--dry-run]`.

> **Transport (deferred "real operations").** As with Phases 3–5, the copy/probe/delete ops run
> **in-process** here as the dev/scaffold harness; in production these SAME ops run over NATS on
> co-located runners (the deferred SSH-deployed-runner layer). Only the op-invocation seam
> changes — the migrate/retire campaign logic (selector → copy → verify → gated delete) is
> transport-agnostic. **This completes the build up to the point of real operations.**

The e2e test `service/tests/driver_migrate.rs` (gated on `MEKHAN__DATABASE_URL`, unique
`test-mig-<uuid>-{a,b}` servers, RAII cleanup of both tempdirs + all rows) builds two synthetic
roots and asserts: after `migrate`, the bytes exist on server B's disk with a matching probe
hash and a `copied` inventory row (`copy_of` = the A row id); after `retire` server A, the
migrated file is removed from A's disk and its row is `deleted` (because the verified B copy
exists); a second A-only file with no copy is **SKIPPED** (still on disk, status unchanged,
counted `skipped_no_verified_copy`); a delete-queue member with a surviving copy is deleted with
`--honor-delete-queue` while one without is skipped; and `--dry-run` lists an eligible row but
deletes nothing on disk and changes no status.

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
