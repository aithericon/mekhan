-- Phase 2 (Granular IAM) — Audit / Provenance.
--
-- Add `updated_by` / `updated_at` across the eight core entities, fix
-- `job_templates.created_by` (raw OIDC subject TEXT) by adding a joinable
-- `created_by_uuid` column, and add authorship to the catalogue tables.
--
-- Every authorship column is a UUID equal to `AuthUser::subject_as_uuid()`, so
-- `user_profiles` (PK `user_id` = that UUID) is the single resolution seam.
--
-- All additive columns use `ADD COLUMN IF NOT EXISTS` so a partially-migrated
-- slot survives re-apply; backfills are idempotent (`WHERE ... IS NULL`).

-- ── workflow_templates ───────────────────────────────────────────────────────
-- Already has author_id / published_by / created_at / updated_at.
ALTER TABLE workflow_templates  ADD COLUMN IF NOT EXISTS updated_by UUID;
UPDATE workflow_templates SET updated_by = author_id WHERE updated_by IS NULL;

-- ── workflow_instances ───────────────────────────────────────────────────────
-- Has created_by / created_at only.
ALTER TABLE workflow_instances  ADD COLUMN IF NOT EXISTS updated_by UUID,
                                ADD COLUMN IF NOT EXISTS updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW();
UPDATE workflow_instances SET updated_by = created_by WHERE updated_by IS NULL;

-- ── folders ──────────────────────────────────────────────────────────────────
ALTER TABLE folders  ADD COLUMN IF NOT EXISTS updated_by UUID,
                     ADD COLUMN IF NOT EXISTS updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW();
UPDATE folders SET updated_by = created_by WHERE updated_by IS NULL;

-- ── resources ────────────────────────────────────────────────────────────────
-- Has created_by / created_at / updated_at.
ALTER TABLE resources   ADD COLUMN IF NOT EXISTS updated_by UUID;
UPDATE resources SET updated_by = created_by WHERE updated_by IS NULL;

-- ── assets / asset_types ─────────────────────────────────────────────────────
-- created_by is nullable on both.
ALTER TABLE assets      ADD COLUMN IF NOT EXISTS updated_by UUID;
ALTER TABLE asset_types ADD COLUMN IF NOT EXISTS updated_by UUID;
UPDATE assets      SET updated_by = created_by WHERE updated_by IS NULL;
UPDATE asset_types SET updated_by = created_by WHERE updated_by IS NULL;

-- ── job_templates / job_template_versions — TEXT->UUID fix ───────────────────
-- The legacy TEXT `created_by` holds a raw OIDC subject (one-way hashable into
-- the uuid_v5 only) so it CANNOT be recovered to its uuid in SQL. Add a NEW
-- UUID column; legacy rows stay NULL (unrecoverable, no backfill). Keep the
-- TEXT column one release (deprecated), drop in a follow-up migration.
ALTER TABLE job_templates          ADD COLUMN IF NOT EXISTS created_by_uuid UUID NULL,
                                   ADD COLUMN IF NOT EXISTS updated_by UUID NULL;
ALTER TABLE job_template_versions  ADD COLUMN IF NOT EXISTS created_by_uuid UUID NULL;

-- ── catalogue ────────────────────────────────────────────────────────────────
-- catalogue_entries.created_by is INHERITED from the producing instance in the
-- projector path (NOT the executor identity, NOT a request user). Legacy /
-- by-reference rows stay NULL. No backfill (intentional).
ALTER TABLE catalogue_entries        ADD COLUMN IF NOT EXISTS created_by UUID;
ALTER TABLE catalogue_saved_queries  ADD COLUMN IF NOT EXISTS created_by UUID,
                                     ADD COLUMN IF NOT EXISTS updated_by UUID;
