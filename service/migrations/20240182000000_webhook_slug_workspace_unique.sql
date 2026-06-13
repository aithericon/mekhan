-- Multi-tenancy Phase 6: per-workspace uniqueness for webhook trigger slugs.
--
-- Webhook trigger slugs (`/api/triggers/webhook/{slug}`) are NOT stored in a
-- table today — they live embedded in each published template's graph JSON
-- (`graph -> 'nodes'[] -> 'data' = {"type":"trigger",
--   "source":{"kind":"webhook","slug":"..."}}`). Uniqueness was a soft
-- editor-side "reserve at publish" with no DB enforcement, and the runtime
-- dispatcher resolves a slug by scanning ALL published templates IN-MEMORY and
-- picking the highest version. That global scan would let a tenant register a
-- slug that shadows another tenant's webhook URL.
--
-- There is no single column on an existing table to hang the constraint on
-- (the slug is one field inside a JSON node array), so the correct home for the
-- DB-level guarantee is a dedicated reservation registry: one row per
-- (workspace, slug), with the hard UNIQUE(workspace_id, slug) constraint that
-- the publish path checks/inserts against. The registry is the source of truth
-- for "is this slug taken in this workspace"; the in-memory dispatcher index
-- stays as the fast fire-time resolver, now scoped per workspace.

CREATE TABLE IF NOT EXISTS webhook_slugs (
    id           UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID        NOT NULL,
    slug         TEXT        NOT NULL,
    -- The template that currently owns the slug. The trigger node lives in the
    -- template graph; (template_id, node_id) locates it. ON DELETE CASCADE so
    -- unpublishing/deleting the template frees the slug.
    template_id  UUID        NOT NULL REFERENCES workflow_templates(id) ON DELETE CASCADE,
    node_id      TEXT        NOT NULL,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- The Phase-6 guarantee: a slug is unique WITHIN a workspace, free to repeat
    -- ACROSS workspaces.
    CONSTRAINT uq_webhook_slug_ws UNIQUE (workspace_id, slug)
);

CREATE INDEX IF NOT EXISTS idx_webhook_slugs_template ON webhook_slugs (template_id);

-- ---------------------------------------------------------------------------
-- Backfill from the authoritative source — each LATEST published template's
-- graph. Extract every webhook trigger node and reserve its slug in the
-- template's own workspace. DISTINCT ON (workspace_id, slug) keeps exactly one
-- owner per (workspace, slug) so the UNIQUE constraint holds even if two
-- templates in the same workspace currently share a slug (the soft pre-DB
-- reservation could be violated for legacy data) — the highest template id /
-- node wins deterministically; the loser simply isn't registered (its in-graph
-- slug is untouched, so nothing breaks, it just no longer resolves until
-- re-published, matching the new hard rule).
-- ---------------------------------------------------------------------------
INSERT INTO webhook_slugs (workspace_id, slug, template_id, node_id)
SELECT DISTINCT ON (t.workspace_id, node->'data'->'source'->>'slug')
       t.workspace_id,
       node->'data'->'source'->>'slug' AS slug,
       t.id,
       node->>'id'                      AS node_id
  FROM workflow_templates t
  CROSS JOIN LATERAL jsonb_array_elements(t.graph->'nodes') AS node
 WHERE t.published = TRUE
   AND t.is_latest = TRUE
   AND t.visibility <> 'private'
   AND node->'data'->>'type' = 'trigger'
   AND node->'data'->'source'->>'kind' = 'webhook'
   AND COALESCE(node->'data'->'source'->>'slug', '') <> ''
 ORDER BY t.workspace_id,
          node->'data'->'source'->>'slug',
          t.id DESC,
          node->>'id'
ON CONFLICT (workspace_id, slug) DO NOTHING;
