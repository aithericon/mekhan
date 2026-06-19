-- Resource/pool bindings as first-class RUN-TIME parameters.
--
-- Phase B auto-derives a requirements manifest at compile (one slot per
-- distinct resource/pool reference, keyed by its binding alias). Phase C
-- persists that manifest and lets an effective binding be resolved at launch
-- with a precedence chain (per-instance override -> per-workspace default ->
-- platform auto-bind -> home-workspace name-match baseline).
--
-- Purely additive:
--   (a) `workflow_templates.requirements_json` is a nullable JSONB sidecar (the
--       serialized `RequirementsManifest`). NULL on every pre-feature / no-slot
--       row -> the launcher fast-paths those byte-for-byte as today.
--   (b) `template_resource_bindings` records the per-workspace DEFAULT effective
--       binding for one requirement slot of a template version chain. Keyed by
--       the chain root (so a default survives a version bump) + workspace +
--       slot_key. `resource_id` FK ON DELETE CASCADE: a deleted resource drops
--       its default binding rows (the launcher then falls through to the next
--       precedence tier or run-gates the slot).

ALTER TABLE workflow_templates
    ADD COLUMN IF NOT EXISTS requirements_json JSONB;

CREATE TABLE IF NOT EXISTS template_resource_bindings (
    id            UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    -- Version-chain root (`COALESCE(base_template_id, id)`), so a per-workspace
    -- default binding is shared across every version of the template family.
    chain_root_id UUID NOT NULL,
    -- The tenant whose default this is. A given chain root can carry one
    -- default per (workspace, slot_key).
    workspace_id  UUID NOT NULL,
    -- The requirement slot's stable key (the binding alias) from the manifest.
    slot_key      TEXT NOT NULL,
    -- The bound resource. CASCADE: a deleted resource drops the default so the
    -- launcher re-resolves the slot through the remaining precedence tiers.
    resource_id   UUID NOT NULL REFERENCES resources(id) ON DELETE CASCADE,
    -- Optional pin to a specific resource version; NULL = the resource's
    -- `latest_version` at launch.
    resource_version INT,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    created_by    UUID NOT NULL,
    updated_by    UUID,
    UNIQUE (chain_root_id, workspace_id, slot_key)
);

-- Launch-time lookup keys on (chain_root_id, workspace_id); the UNIQUE index
-- already covers it, but an explicit index documents the read path and a
-- resource-scoped index speeds the CASCADE / "who binds this resource" query.
CREATE INDEX IF NOT EXISTS idx_template_resource_bindings_lookup
    ON template_resource_bindings (chain_root_id, workspace_id);
CREATE INDEX IF NOT EXISTS idx_template_resource_bindings_resource
    ON template_resource_bindings (resource_id);
