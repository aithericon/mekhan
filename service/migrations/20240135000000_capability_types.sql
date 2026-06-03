-- Phase 4 — Typed capability registry.
--
-- One workspace-scoped table backing the admin-curated capability vocabulary
-- that presence-pool runner enrollment + step Requirements matching are typed
-- against:
--
--   capability_types   One row per defined capability shape in a workspace.
--                      Identified by (workspace_id, name). `fields` is the
--                      typed field list a runner must satisfy when it
--                      advertises this capability in `runners.capabilities`,
--                      and that step Requirements constrain over. Each field
--                      is { name, kind: <FieldKind wire value>, required,
--                      options? }. Soft-delete via `revoked_at` (NULL = live).
--
-- `workspace_id` is a UUID without an FK constraint, mirroring the resources +
-- runners migrations — forward-compatible with the workspaces table; adding a
-- FK later is a single ALTER away.

CREATE TABLE capability_types (
    id                  UUID         PRIMARY KEY DEFAULT gen_random_uuid(),

    workspace_id        UUID         NOT NULL,

    -- Capability name, unique within a workspace. This is the KEY a runner
    -- advertises in its `capabilities` blob (`{ "<name>": { ... } }`) and the
    -- `capability` a step Constraint names.
    name                TEXT         NOT NULL,

    -- Typed field list: [ { name, kind, required, options? } ]. `kind` is a
    -- `FieldKind` wire value (text|number|bool|select|…). Enroll-time
    -- validation checks every advertised field against this list.
    fields              JSONB        NOT NULL DEFAULT '[]'::jsonb,

    created_by          UUID         NOT NULL,
    created_at          TIMESTAMPTZ  NOT NULL DEFAULT NOW(),

    -- Soft-delete tombstone. NULL = live.
    revoked_at          TIMESTAMPTZ,

    UNIQUE (workspace_id, name)
);

CREATE INDEX idx_capability_types_workspace
    ON capability_types (workspace_id)
    WHERE revoked_at IS NULL;
