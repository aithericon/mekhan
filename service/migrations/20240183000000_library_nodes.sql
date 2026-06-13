-- Library / vendor nodes — control-plane metadata layer.
--
-- A "library node" (OpenFOAM, mumax3, …) is a published template wearing a
-- brand, dropped onto the canvas as an ordinary `sub_workflow` node. No new
-- runtime/engine/compiler primitive: all the new state lives on the template
-- row. See `.claude/worktrees/library-nodes/PLAN.md` for the full design and
-- `memory/project_library_nodes_plan.md` for the resolved decisions.
--
-- This migration is purely additive (all columns nullable or defaulted), so
-- every existing `SELECT * FROM workflow_templates` keeps working and existing
-- rows load as plain `workflow`-kind templates.

-- Exclusive intent enum (decision 1): a row is a runnable workflow, a curated
-- reusable library node, or a private sub-workflow child. Defaulted so existing
-- rows are `workflow`; backfilled below for the private-child case.
ALTER TABLE workflow_templates
    ADD COLUMN IF NOT EXISTS template_kind TEXT NOT NULL DEFAULT 'workflow';

-- Provenance/trust axis (decision 3), orthogonal to `visibility` (which keeps
-- owning ACL). NULL for plain workflows; one of system|workspace|community for
-- library nodes.
ALTER TABLE workflow_templates
    ADD COLUMN IF NOT EXISTS origin TEXT;

-- Stable human coordinate (decision 7): `vendor/slug`, e.g.
-- `openfoam/solid-displacement`. Decoupled from the UUID family; GitOps refs,
-- the upgrade prompt, and catalogue links resolve by coordinate. Unique within
-- an origin (a `system` openfoam/x and a `workspace` openfoam/x can coexist).
ALTER TABLE workflow_templates
    ADD COLUMN IF NOT EXISTS coordinate TEXT;

-- Presentation blob (decisions 9, 13): {icon, color, vendor, category, badge}.
-- Frozen onto an embedding sub_workflow node at editor io-contract fetch time so
-- the canvas renders the branded card. JSONB to match the graph/air_json/
-- source_ref `serde_json::Value` + `sqlx::FromRow` convention on this table.
ALTER TABLE workflow_templates
    ADD COLUMN IF NOT EXISTS presentation JSONB;

-- Lifecycle (decision 11): active|deprecated|retired. Version rows are NEVER
-- hard-deleted, so a pinned embed always resolves; retirement only hides the
-- node from the palette.
ALTER TABLE workflow_templates
    ADD COLUMN IF NOT EXISTS lifecycle_status TEXT NOT NULL DEFAULT 'active';

-- Successor coordinate for a deprecated/retired node (decision 11).
ALTER TABLE workflow_templates
    ADD COLUMN IF NOT EXISTS superseded_by TEXT;

-- Fork provenance (decision 5): {coordinate, template_id, version} of the
-- upstream a workspace copy was forked from. Enables the future "upstream vN
-- available" rebase hint.
ALTER TABLE workflow_templates
    ADD COLUMN IF NOT EXISTS forked_from JSONB;

-- Coordinate uniqueness is scoped to origin and only enforced where a
-- coordinate is set (plain workflows leave it NULL).
CREATE UNIQUE INDEX IF NOT EXISTS uq_workflow_templates_origin_coordinate
    ON workflow_templates (origin, coordinate)
    WHERE coordinate IS NOT NULL;

-- Palette/catalogue queries filter by kind; partial index keeps it cheap.
CREATE INDEX IF NOT EXISTS idx_workflow_templates_library
    ON workflow_templates (template_kind)
    WHERE template_kind = 'library_node';

-- Backfill: existing private sub-workflow children (those that may only be
-- embedded by an owning family) become `private_child` so the intent enum is
-- exclusive and accurate from day one.
UPDATE workflow_templates
    SET template_kind = 'private_child'
    WHERE owner_template_id IS NOT NULL
      AND template_kind = 'workflow';
