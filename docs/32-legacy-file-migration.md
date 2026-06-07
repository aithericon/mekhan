# 32 · Legacy file migration — crawler backend, file inventory, reconcile & migrate

Status: **design** (agreed forks, not yet implemented). Captures the 2026-06-06
design dialogue for ingesting, reconciling, and migrating the ~4M files of the
**predecessor platform** (an ArangoDB-cataloged set across four file servers,
~76 TB) into this platform's catalog, then selectively moving bytes between
servers and retiring the old copies.

Builds on the runner-fleet / enrolled-worker dispatch model
([`21-lab-runner-fleet.md`](21-lab-runner-fleet.md),
[`23-unified-capacity-model.md`](23-unified-capacity-model.md)), the existing
`executor-file-ops` OpenDAL backend, and the artifact catalogue
([`04-causality-projector.md`](04-causality-projector.md)). It adds **one new operation to the existing `executor-file-ops` backend**
(`crawl`), reuses the existing `probe` op for integrity/hash, **reshapes the
catalog to be content-addressed** + adds a slim **`file_inventory`** layer for
physical copies, and a per-server **migration campaign** workflow.

> **Revision note.** An earlier draft proposed a *new* `fs_crawler` executor
> backend, then a `crawl` + `hash` op pair. Both trimmed:
> `executor-file-ops` is already an OpenDAL op-dispatcher (`FileOpsConfig` =
> `Probe/Copy/Move/Delete/Annotate/List/Stat`, one module per op under `ops/`),
> and it already owns operator construction, `StorageConfig`, credentials, and
> `fs://` mounts (`resolve.rs`/`config.rs`). Integrity-hashing is already
> **`probe`** (`ops/probe.rs` reads the file, runs `aithericon_file_metadata`,
> emits `checksum` + `format` + `mime_type` + size). So only **`crawl`** (a
> recursive, streaming `list`) is genuinely new. One backend owns the whole file
> lifecycle (crawl → probe → copy → verify → delete) — exactly what a co-located
> file-server runner should advertise — with no new crate,
> `ExecutionBackendType`, feature flag, or `served_wires`/register wiring.

## 1. The problem

Four file servers hold ~4M files (~76 TB) cataloged by a **legacy ArangoDB** —
the predecessor of this platform. We must:

1. **Index** what is actually on disk now.
2. **Integrity-check** it against the legacy catalog (hashes already recorded).
3. **Register** the surviving files in this platform's catalog (by reference —
   bytes stay on the servers, they are not ingested into rustfs/S3).
4. **Reconcile** — find orphans (on disk ∌ catalog, or catalog ∌ disk) and
   duplicates (same content on multiple servers).
5. **Migrate** — copy selected files to the new server, **partially**: some move,
   some stay.
6. **Retire** — delete source copies, but only after a verified copy.

This is an **ongoing reusable capability**, not a one-shot script — a fourth
server just arrived and more data will keep coming.

## 2. What the legacy dump tells us

From `backup-arangodb-agm-2026-06-06.tar.gz` (2.3 GB):

### `files` — 3,960,793 docs. Every record already carries the integrity data:

```json
{
  "hash":           "SHA256:9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08",
  "size":           4,
  "path":           "/Data/Initializertestfile.txt",
  "file_server_id": "legacy-ftp-server",
  "file_server":    "131.246.221.67:7890",
  "name":           "Initializertestfile.txt",
  "ext":            "txt",
  "owner_id":       "PlatypusTest",
  "node_id":        "",
  "created":        "2022-04-19T15:22:37.180+00:00",
  "modified":       "2022-04-19T15:22:36.434+00:00",
  "compression":    "", "compressed_size": 0,
  "migrated_at":    "2025-07-14T13:01:11.461Z"
}
```

- **`hash` (SHA256) is present on every file.** This is the **join key**.
  Integrity becomes a *compare*, dedup falls out for free, and we almost never
  re-read 76 TB.
- `migrated_at` (a prior 2025-07 migration) is **not relevant** going forward.

### `file_servers` — 4 docs. Heterogeneous on paper, uniform in practice:

| `file_server_id` | declared protocol | how we actually reach the bytes |
|---|---|---|
| `legacy-ftp-server` | FTP (`protocol:1`), HTTP read-proxy | **NFS-S3 mount on the gateway** (FTP is workers-internal only) |
| `a4b00…` "Minio S3" | S3 (`protocol:3`) | MinIO with an **NFS volume from agridos-nas** behind it |
| (+2 more) | — | NFS mounts |

Every server is reachable as a **local/NFS filesystem mount**. The encrypted
`credentials` / FTP / S3 / HTTP paths are **deliberately bypassed** — we put the
crawler *on the data* and walk the mount directly (decision §3.2).

### `files_to_delete` — 96,864 docs. A **pre-existing deletion queue** from prior
dedup/orphan work, keyed by path with `fingerprint{hash,size,modified}`. We
**import and honor** it rather than recomputing those decisions.

### `to_files` — 3,949,632 edges (`nodes/… → files/…`). Near-1:1 **provenance**:
which node produced/owns each file. The user wants this **stamped as metadata**
on the new records (§4).

`datacenter_to_file_servers` is empty.

> **Bootstrap shortcut.** The dump already contains all 4M records *with hashes*,
> so the entire reconcile **baseline can be built offline from the dump** — no
> live Arango/AQL needed for v1. Live AQL pulls become the *incremental* refresh
> path later.

## 3. Architecture

### 3.1 Two pieces — one extended backend, one new domain

- **`executor-file-ops`** *(extended)* — the single backend for the whole file
  lifecycle, all over OpenDAL. Existing ops already cover most of it:
  `copy`/`move`/`delete`/`stat` for migration, and **`probe`** for
  integrity-hashing (`ops/probe.rs` reads the file, runs
  `aithericon_file_metadata`, emits `checksum` + `format` + `mime_type` + size —
  richer than a bare hash, and the extras get stamped onto the inventory record).
  **One new op:**
  - `crawl` — recursive `fs://` walk over a server's mount, streaming
    `{path, size, mtime}` in batches via the existing `EventStream`
    `item()`/`close()` channel mechanism (docs/25). Checkpointed/resumable —
    **mandatory at 4M files**. It is `list`'s recursive, streaming sibling.

  `crawl` is one new `FileOpsConfig` variant + `ops/` module in the existing
  crate — no new backend, `ExecutionBackendType`, feature flag, or register
  wiring.
  - *Caveat:* `probe`'s `checksum` is fmeta-config-dependent and optional —
    we must ensure it emits **SHA-256** (to match the legacy `"SHA256:<hex>"`
    format), likely a checksum/algorithm flag on `ProbeConfig`.
- **`file_inventory`** *(new mekhan domain)* — the by-reference catalog the whole
  pipeline writes into (§4). The existing artifact `catalogue` is
  execution/S3-bound and stays as-is; inventory is separate and may cross-link.

### 3.2 Crawlers live on the data — placement is dispatch, not code

The user's deployment model — *"crawlers directly on the hosts; SSH+bastion to
deploy; NATS for comms"* — maps 1:1 onto the platform's **runner fleet**, and
separates three concerns cleanly:

- **Runtime comms = NATS.** `executor-file-ops` runs *inside a runner* that
  enrolls over NATS and drains its own partition. Crawl/copy/delete jobs for a given server
  land on the runner co-located with it (enrolled-runner-per-partition dispatch
  already exists — see [`21-lab-runner-fleet.md`](21-lab-runner-fleet.md)).
- **Deployment = SSH + bastion.** SSH is only the *bootstrap*: `scp` the
  aarch64-musl runner binary through the bastion, drop a runner config (NATS URL,
  group = `file-server-<id>`, OpenDAL operator config = `fs://<mount-root>`), start
  it as a service. It self-enrolls; mekhan's fleet shows it. **Only 4 servers** →
  v1 is a hand-run bootstrap script; an automated *"provision runner over
  SSH-bastion"* capability is a later phase, not a v1 blocker.
- **Protocol = OpenDAL `fs://` everywhere.** Co-located → walk the NFS mount as a
  local filesystem (cheap `stat`, no MinIO/Traefik/FTP in the path, no
  credentials). Placement decides *which runner*; the backend code never
  branches on protocol.

```
                         NATS (jetstream)
   mekhan ── crawl/copy/delete jobs ──▶ executor-<wire>-grp.<prio>.<server-partition>
     ▲                                          │
     │ status / streamed file batches           ▼
  file_inventory  ◀───────────────  runner (file-ops)  ── fs:// ──▶  NFS mount
  (Postgres)                       co-located on / near the server      (the bytes)
```

## 4. Two layers: content-addressed catalog + physical inventory

The clean split (per the 2026-06-06 review) separates three things the first
draft conflated — *content facts*, *copy facts*, and *provenance*:

- **Catalog = the logical file.** Source of truth for everything intrinsic to the
  *bytes*: `content_hash` (the identity), `size`, `mime_type`, `format`, fmeta
  output, user metadata. One row per unique content; dedup is automatic (N copies
  → one catalog row).
- **`file_inventory` = a physical copy.** *Where a copy lives and its state* —
  nothing descriptive. Links up to the catalog by `content_hash`.
- **Legacy provenance → JSONB** on the copy (it's foreign — no columns). This
  platform's *native* provenance (instance/net/execution ids) stays as **optional
  first-class columns** on the catalog — our own queryable concepts keep columns.

### 4.1 The catalog becomes content-addressed (decoupled from job nets)

Today `catalogue_entries` is keyed `(execution_id, id)`, has **no `content_hash`**,
and assumes every entry was *born from a job* with bytes at one S3 `storage_path`
— a NAS file with no execution can't live in it. We **reshape it so
`content_hash` is the logical identity** and the job-net coupling becomes optional
provenance:

- add `content_hash` as the indexed/unique **logical identity**;
- **keep** `execution_id`/`instance_id`, `job_id`, `source_net`, `source_place`,
  `process_id`, `process_step`, `storage_path` as **optional first-class
  columns** — native provenance stays queryable, it just stops being *mandatory*
  (a NAS file leaves them null);
- keep `size_bytes`, `mime_type`, `name`, `file_metadata`, `user_metadata` as the
  intrinsic truth;
- legacy-stack provenance does **not** get columns — it lives in the inventory
  copy's `provenance` JSONB.

No-back-compat is the standing convention — we just migrate (dev catalogue rows
are disposable). The reshape updates the causality
projector that writes catalogue, the NATS responder, and the frontend reader.

> **End-state this points at (separable, not Phase 1).** Once the catalog is
> logical and inventory holds *physical copies across all backends*, a job
> artifact is simply *"a copy that lives in S3"* — its `execution_id` / `S3 key`
> become an **inventory** row's location+provenance, and the catalog keeps only
> logical metadata. That fully unifies legacy-NAS files and job artifacts under
> one model. We **do not** rewrite the artifact path in this project; Phase 1 only
> makes the catalog content-addressed and adds inventory for NAS copies. The
> artifact-copy fold-in is a later refactor.

### 4.2 Slim `file_inventory`

```
file_inventory
  id              UUID PK
  content_hash    TEXT            -- FK → catalog (logical file). INDEXED.
                                  --   NULL only while an orphan is un-probed.
  file_server_id  TEXT NOT NULL
  path            TEXT NOT NULL
  UNIQUE (file_server_id, path)   -- the physical key

  status          TEXT NOT NULL   -- this copy's lifecycle (see below)
  is_canonical    BOOLEAN         -- the copy we keep within a hash group
  copy_of         UUID            -- self-ref: a copy this migration created
  migration_target TEXT           -- server we plan to copy to (phase 6)

  provenance      JSONB DEFAULT '{}'  -- legacy_key, node_id, owner_id, original
                                      --   file_server addr, ext, legacy timestamps…
  first_seen, last_seen, last_verified, updated_at  TIMESTAMPTZ
```

No `size`/`mime`/`name`/`legacy_*` columns — those are content facts (catalog) or
provenance (JSONB). Indexed on `content_hash`, `status`, `(file_server_id, status)`.

**Status lifecycle (the copy):**
`indexed → verified → registered → copied → deleted`, plus the off-happy-path
states `mismatch`, `orphan_disk`, `orphan_db`. Integrity at verify-time = probe
the copy, compare against the catalog's authoritative `content_hash`/`size`; a
divergence sets `status=mismatch` (detail in `provenance`/a JSONB note). Observed
mtime/size are transient observations, **not** persisted as the file's metadata.

### 4.3 Companion staging tables (pristine, re-importable)

```
legacy_file_index   -- raw dump of `files` (4M); index: hash, (file_server_id, path)
  legacy_key PK, file_server_id, path, hash, size, node_id, owner_id,
  created, modified, raw JSONB
legacy_delete_queue -- raw dump of `files_to_delete` (97k): key PK, hash, size, modified
```

The dump import then writes **both** layers in one pass: `files` → **catalog**
deduped to one row per unique `hash` (carries size, ext→mime — the dump already
has it all), **and** → **inventory**, one row per physical copy (server + path +
provenance JSONB). `files_to_delete` pre-flags inventory rows; the catalog row
survives as long as a canonical copy does.

**The missing API.** The catalog has no by-reference register path today; we add a
**bulk register (catalog upsert by `content_hash` + inventory rows, no bytes)**
plus inventory list/filter/stats and reconcile-report endpoints (orphans /
duplicates / mismatches as SQL views over inventory ⋈ catalog ⋈ legacy-staging).

## 5. Pipeline & phase gates

1. **Import baseline** *(offline, from the dump)* — load `files` (4M) as the
   *expected* baseline keyed by `hash`; import `files_to_delete` (97k) as honored
   deletion decisions; stamp `node_id`/`to_files` provenance. Pure Postgres,
   testable immediately, no NAS access.
2. **Crawl-index** — file-ops `crawl` walks each server's mount → observed
   `{path,size,mtime}` rows (`status=indexed`). Minutes–low hours per server.
3. **Reconcile** — inventory ⋈ catalog (via `content_hash`) ⋈ legacy-staging (via
   `(file_server_id, path)`):
   - present both, size matches → `verified` (**catalog hash inherited from
     legacy, no re-read**)
   - size mismatch → `mismatch` (needs re-hash)
   - on disk ∌ legacy → `orphan_disk` (needs hashing to link to a catalog row)
   - legacy ∌ disk → `orphan_db` (missing / already deleted)
   - same `content_hash` on >1 server → duplicate copies of one catalog row;
     pick `is_canonical`
4. **Targeted hash** *(gated)* — file-ops `probe` op (SHA-256) for **only**
   orphans + mismatches + an audit sample. Resumable. This bounds the expensive read to a
   fraction of 76 TB, not the whole.
5. **Register** *(gated)* — catalog row exists per `content_hash`; mark the
   inventory copies `registered`.
6. **Migrate** *(operator-gated, destructive-adjacent)* — per-rule move-to-new vs
   stay; `file-ops` copy on the co-located runner → **verify-by-hash** → mark
   `copied`. Per-batch, resumable.
7. **Retire** *(operator-gated, destructive)* — delete source **only** after a
   verified copy **and** a grace window; honors the imported deletion queue.

Heavy lifting is in the backends; the engine coordinates phases and **human
gates** — never one Petri token per file. Per-server **campaign workflow** wires
the phases with the gates as `Decision`/approval steps.

## 6. Open decisions (do not block Phase 1)

- **Scope.** Catalogue has no workspace/folder scope. The reshape can add it on
  the catalog (logical) and/or inventory (copy) — do these files attach to a
  workspace/folder hierarchy, or sit global? (Default: global + later scoping.)
- **Artifact-copy fold-in.** Whether/when job artifacts become S3-located
  inventory rows (the §4.1 end-state). Out of scope for this project; tracked.
- **Migrate-vs-stay rule** (Phase 6 only) — operator-picks-per-batch, or a rule
  (age / project / owner / access)? Needed before migration, not before indexing.
- **SSH-bastion provisioner** — v1 hand-run for 4 servers; automate later.
- **Live Arango incremental** — dump bootstraps v1; AQL `POST /_api/cursor` pull
  (via reuse of `executor-http`, or a thin `arango` reader) is the refresh path.

## 7. Build order (each independently testable)

1. Content-address the catalog (`content_hash` identity, execution/storage
   optional) + slim `file_inventory` + staging tables + bulk register API
   (mekhan/Postgres). Touches the causality projector / responder / frontend.
2. Dump importer → catalog (dedup by hash) + inventory copies + deletion-queue +
   provenance JSONB (offline).
3. file-ops `crawl` op (recursive streaming walk, checkpointed).
4. Reconcile (SQL views + report endpoints).
5. Targeted hashing — reuse file-ops `probe` (ensure SHA-256), resumable driver.
6. Migration (file-ops copy → verify → delete, gated).
7. SSH-bastion runner provisioner + campaign workflow + inventory/reconcile UI.
