# Phase 2 — Audit / Provenance (deep-dive)

Add `updated_by`/`updated_at` across the eight core entities; fix
`job_templates.created_by` TEXT→UUID; add `created_by` to the catalogue tables;
widen DTOs to expose authorship. Every authorship column is a UUID equal to
`AuthUser::subject_as_uuid()`, so `user_profiles` (PK `user_id` = that UUID) is the
single resolution seam (LEFT JOIN → name/avatar, rendered via Phase 1's `UserChip`).

## 1. Migration — `service/migrations/20240170000000_audit_provenance.sql`

Single migration. **All additive columns use `ADD COLUMN IF NOT EXISTS`** (folds the
perf-lens minor — re-apply-safe on partially-migrated slots); backfills are
naturally idempotent (`WHERE updated_by IS NULL`).

```sql
ALTER TABLE workflow_templates  ADD COLUMN IF NOT EXISTS updated_by UUID;
UPDATE workflow_templates SET updated_by = author_id WHERE updated_by IS NULL;

ALTER TABLE workflow_instances  ADD COLUMN IF NOT EXISTS updated_by UUID,
                                ADD COLUMN IF NOT EXISTS updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW();
UPDATE workflow_instances SET updated_by = created_by WHERE updated_by IS NULL;

ALTER TABLE folders  ADD COLUMN IF NOT EXISTS updated_by UUID,
                     ADD COLUMN IF NOT EXISTS updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW();
UPDATE folders SET updated_by = created_by WHERE updated_by IS NULL;

ALTER TABLE resources   ADD COLUMN IF NOT EXISTS updated_by UUID;
UPDATE resources SET updated_by = created_by WHERE updated_by IS NULL;

ALTER TABLE assets      ADD COLUMN IF NOT EXISTS updated_by UUID;
ALTER TABLE asset_types ADD COLUMN IF NOT EXISTS updated_by UUID;
UPDATE assets      SET updated_by = created_by WHERE updated_by IS NULL;
UPDATE asset_types SET updated_by = created_by WHERE updated_by IS NULL;

-- job_templates TEXT->UUID fix: add a NEW UUID column; the legacy TEXT created_by
-- holds a raw OIDC subject (one-way-hashable only) so it CANNOT be recovered to
-- its uuid_v5 in SQL. Keep the TEXT column one release (deprecated), drop later.
ALTER TABLE job_templates          ADD COLUMN IF NOT EXISTS created_by_uuid UUID NULL,
                                   ADD COLUMN IF NOT EXISTS updated_by UUID NULL;
ALTER TABLE job_template_versions  ADD COLUMN IF NOT EXISTS created_by_uuid UUID NULL;
-- legacy rows: created_by_uuid stays NULL (unrecoverable). No backfill.

-- catalogue: created_by INHERITED from the producing instance (projector path),
-- NOT the executor. Legacy / by-reference rows stay NULL. NO backfill (intentional).
ALTER TABLE catalogue_entries        ADD COLUMN IF NOT EXISTS created_by UUID;
ALTER TABLE catalogue_saved_queries  ADD COLUMN IF NOT EXISTS created_by UUID,
                                     ADD COLUMN IF NOT EXISTS updated_by UUID;
```

`updated_by = created_by` backfill is a convenience seed (synthetic for
pre-migration history). No blanket indexes — lookups are by id then JOIN
`user_profiles`.

## 2. Per-entity change table

| Entity | Has today | Adds | Handler write sites |
|---|---|---|---|
| `workflow_templates` | `author_id`, `published_by`, `created_at`, `updated_at` | `updated_by` | create INSERT (~149, beside `author_id`), name/desc/graph UPDATE (~561), publish/new_version UPDATE (~923); apply-air INSERTs (~1455/1489) bind `updated_by = author_id`. |
| `workflow_instances` | `created_by`, `created_at` | `updated_by`, `updated_at` | create INSERT (~108) sets `updated_by` = same principal (`updated_at` via DEFAULT); user lifecycle UPDATEs set `updated_at = NOW(), updated_by`. **Projector status transitions (causality/ingest.rs) set `updated_at = NOW()` but leave `updated_by` NULL** — no fabricated principal. |
| `folders` | `created_by`, `created_at` | `updated_by`, `updated_at` | create sets `updated_by = created_by`; move/rename UPDATE (the one rewriting materialized `path`) sets `updated_at = NOW(), updated_by`. **Coordinate with Phase 3's re-gating of the same handlers.** |
| `resources` | `created_by`, `created_at`, `updated_at` | `updated_by` | create (~916) + the three `updated_at = NOW()` UPDATEs (~1318, ~1350/1508, ~1436). `resource_versions.created_by` immutable — leave. |
| `assets` / `asset_types` | `created_by` (nullable), `created_at`, `updated_at` | `updated_by` | create + `updated_at = NOW()` UPDATEs. |
| `job_templates` / `_versions` | `created_by TEXT` (raw subject — verified `user.subject.clone()` at lines 273, 441) | `created_by_uuid`, `updated_by` | **THE FIX** — see §3. |
| `catalogue_entries` | (no authorship) | `created_by` | projector inherit — see §4. |
| `catalogue_saved_queries` | `created_at`, `updated_at` (no authorship) | `created_by`, `updated_by` | `create()`/`update()` thread the real `AuthUser` (rename `_user`→`user`). |

## 3. job_templates TEXT→UUID fix (the load-bearing one)

`service/src/handlers/job_templates.rs` — verified `let created_by =
user.subject.clone()` at lines 273 and 441 (the raw OIDC subject string, which
**cannot** join `user_profiles`).

- Replace both with `let created_by = user.subject_as_uuid();` bound to the NEW
  `created_by_uuid` UUID column (INSERT ~284, version INSERT helper ~126/129).
- Version-insert helper signature `created_by: Option<&str>` (line 126) →
  `Option<Uuid>`.
- Add `updated_by = user.subject_as_uuid()` to create + every `updated_at = NOW()`
  UPDATE (~468, ~518, ~568 soft-delete).
- `service/src/models/job_template.rs`: `JobTemplateRow.created_by` reads the new
  `created_by_uuid` as `Option<Uuid>`; add `updated_by: Option<Uuid>`; same for
  `JobTemplateVersionRow`.

**Contract semantics (folds the contract-lens minor):** the DTO keeps BOTH fields
for one release — legacy `created_by` (TEXT, deprecated) **and** new
`created_by_uuid` — so no FE consumer silently breaks (both are `string` in TS but
the values differ; legacy rows have `created_by_uuid = null`). Grep `app/` for
`.created_by` reads on job templates before regen. The TEXT column drops in the
follow-up migration alongside the DTO field.

## 4. Catalogue authorship — inherited, not the executor

`service/src/causality/ingest.rs` — the catalogue INSERT (~2225) runs in the NATS
projector with NO `AuthUser`. Source `created_by` from the **producing instance**:
resolve `workflow_instances.created_by` via the event's `net_id`/`source_net` (the
projector already joins instances ~1078/1159). NULL when unresolvable (legacy /
by-reference). **Document clearly: catalogue authorship is the producing instance's
owner, not the executor identity and not a request user.**

## 5. Model / DTO widening

- `models/resource.rs` — `ResourceSummary`/`ResourceDetail` gain `created_by: Uuid`,
  `updated_by: Option<Uuid>`; `ResourceRow` gains `updated_by`.
- `models/asset.rs` — `AssetSummary`/`Detail`/`AssetTypeSummary`/`Detail` gain
  `created_by: Option<Uuid>`, `updated_by: Option<Uuid>`; rows updated.
- `models/template.rs` — `WorkflowTemplate` gains `updated_by: Option<Uuid>`; test
  fixtures (~2047/2052) updated.
- `models/instance.rs` — `WorkflowInstance` gains `updated_by: Option<Uuid>`,
  `updated_at: DateTime<Utc>`.
- `models/workspace.rs` — `Folder` gains `updated_by: Option<Uuid>`,
  `updated_at: DateTime<Utc>`.
- `models/job_template.rs` — see §3.
- `catalogue/saved_queries.rs` — add `created_by`/`updated_by` to
  `SAVED_QUERY_COLUMNS` + `SavedQuery`.

No new gate logic. Exposing `created_by`/`author_id` is purely additive display and
does NOT change who can read an object (these fields ride existing already-authorized
GET responses; the ACL area owns access control).

## 6. OpenAPI

Every widened DTO is a `ToSchema` change (all additive/nullable). Run
`just dev::openapi`. Verify `svelte-check` after regen (job_templates `created_by`
type is still `string`).

## 7. Activity-log recommendation (DEFERRED)

A generic append-only `activity_log` (who/what/when stream, generalizing
`resource_audit` migr 20240121 with an `entity_kind` discriminator) is RECOMMENDED
as a fast-follow but OUT OF SCOPE here. Per-row `updated_by`/`updated_at` answers the
stated "who last touched this" need; full history is a separate, larger surface. If
pulled in, it gets its own migration/area, not folded here.

## 8. Tests

- Unit: `subject_as_uuid()` == seeded dev profile `3bb26085-29f3-5fbf-8a8c-
  a2e485a1f55b` (proves the JOIN).
- Integration (live): create a job template as dev user → `created_by_uuid ==
  subject_as_uuid()` (not the raw string), DTO surfaces the UUID (TEXT→UUID
  regression guard); update a resource → `updated_by` = caller, `updated_at`
  advanced, `created_by` unchanged; saved-query create+PATCH → authorship set;
  catalogue projector → entry `created_by` == producing instance's `created_by`,
  by-reference row → NULL without error; folder move → `updated_by` set, `created_by`
  preserved; template new_version+publish → `author_id` preserved, `updated_by` =
  publisher.
- `openapi-drift` green.

## 9. Risks

- job_templates legacy TEXT is one-way-hashable → `created_by_uuid` NULL for
  pre-migration rows; keep TEXT one release; dev recompute trivial, prod needs ops
  input.
- Catalogue authorship semantics are subtle (instance owner, not executor) —
  reviewers must confirm.
- Projector/system mutations leave `updated_by` NULL (FE renders "System") rather
  than fabricating a principal.
- `updated_by = created_by` backfill is synthetic for pre-migration rows.
- Migration `170` between identity `169` and ACL `171`; sccache `migrate!`-dir miss
  → forced rebuild.
