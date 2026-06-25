-- GitOps coordinate-keyed upsert — per-workspace uniqueness for the new
-- `mekhan apply` create-if-absent path.
--
-- The GitOps `apply` flow keys an idempotent create-or-version on a stable
-- `vendor/slug` coordinate carried in the git artifact (the
-- `mekhan.lock.json`), rather than on an opaque server-minted UUID. A
-- git-managed chain stamps `origin = 'gitops'` + its `coordinate` so a later
-- apply re-resolves the SAME chain instead of duplicating it.
--
-- This migration is purely additive: `origin`, `coordinate`, and
-- `workspace_id` all already exist (origin/coordinate from migration
-- 20240186000000_library_nodes; workspace_id from the workspaces migration).
-- No column changes and no data backfill — there are no `origin = 'gitops'`
-- rows yet (the adopt-by-name cutover stamps pre-seeded chains lazily on first
-- coordinate apply).
--
-- Uniqueness scope is PER-WORKSPACE (two tenants must each own
-- `online-clinic/document-pipeline-v1`) and scoped to the `is_latest` row (a
-- gitops chain is a versioned family; only one CURRENT row per
-- (workspace, coordinate) may exist while superseded versions coexist).
CREATE UNIQUE INDEX IF NOT EXISTS uq_workflow_templates_gitops_coordinate
    ON workflow_templates (workspace_id, coordinate)
    WHERE origin = 'gitops' AND coordinate IS NOT NULL AND is_latest;

-- Carve gitops rows OUT of the pre-existing library-node coordinate index.
-- That index (migration 20240186000000) is `ON (origin, coordinate) WHERE
-- coordinate IS NOT NULL AND is_latest` — its predicate does NOT pin `origin`,
-- so it ALSO governs gitops rows and makes (origin='gitops', coordinate)
-- GLOBALLY unique. That defeats the per-workspace gitops index above: two
-- tenants applying the same coordinate string would collide on the old index
-- instead of each owning an independent chain. Recreate it with
-- `origin IS DISTINCT FROM 'gitops'` (NULL-safe — a NULL-origin row stays
-- covered) so library-node uniqueness (origin system|workspace|community) is
-- byte-identical and gitops rows are governed SOLELY by the per-workspace
-- index above.
DROP INDEX IF EXISTS uq_workflow_templates_origin_coordinate;
CREATE UNIQUE INDEX IF NOT EXISTS uq_workflow_templates_origin_coordinate
    ON workflow_templates (origin, coordinate)
    WHERE coordinate IS NOT NULL AND is_latest AND origin IS DISTINCT FROM 'gitops';
