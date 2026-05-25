-- Phase B.3 — Resources schema.
--
-- Three tables plus an ALTER on workflow_instances:
--
--   resources           One row per logical credential. Identified by
--                       (workspace_id, path). `latest_version` is bumped on
--                       rotation; old `resource_versions` rows are retained
--                       so any workflow instance pinned at an older version
--                       continues to resolve cleanly.
--   resource_versions   Immutable per-version snapshot. Public, non-secret
--                       fields live inline as JSONB; secret-bearing fields
--                       live in Vault at the deterministic path
--                       `aithericon/resources/{workspace_id}/{resource_id}/v{version}`
--                       and are referenced from compiled AIR via
--                       `{{secret:resources/<id>/v<n>#<field>}}` (Plan B.5).
--   resource_acl        Per-principal allow rules. Checked by the resolver
--                       (B.5) before each resolve call; the audit row is
--                       written only after the ACL check passes.
--   workflow_instances.resource_pins
--                       Frozen `alias -> {resource_id, version}` map captured
--                       at instance-launch time. Plan Risk #1: existing rows
--                       get NULL, replay paths in `lifecycle.rs` must tolerate
--                       NULL (verify when B.5 lands).
--
-- `workspace_id` is a UUID without a FK constraint in v1 — no `workspaces`
-- table exists yet in the schema. The column shape is forward-compatible
-- with the upcoming workspaces migration; adding a FK later is a single
-- ALTER away.

CREATE TABLE resources (
    id                  UUID         PRIMARY KEY DEFAULT gen_random_uuid(),

    workspace_id        UUID         NOT NULL,

    -- Snake_case identifier, e.g. `prod_pg`. Unique within a workspace.
    -- Also doubles as the reference key in workflow Python source —
    -- `prod_pg.host` resolves to this row at publish time (compiler
    -- matches `path = $head` against scanned `<head>.<field>` patterns),
    -- so the value must be a valid Python identifier.
    path                TEXT         NOT NULL,

    -- Stable wire identifier from `ResourceTypeDescriptor.name`
    -- (`postgres`, `openai`, `slack`, `s3`, `google_oauth`).
    resource_type       TEXT         NOT NULL,

    display_name        TEXT         NOT NULL,

    -- Cursor of the most recent `resource_versions.version`. Bumped on
    -- rotation. Reads always join on (resource_id, latest_version) unless a
    -- specific pinned version is requested.
    latest_version      INT          NOT NULL DEFAULT 1,

    -- Soft-delete tombstone. NULL = live. Plan §B.14 keeps Vault paths intact
    -- for pinned instances after soft-delete; GC of orphaned paths is v2.
    deleted_at          TIMESTAMPTZ,

    created_by          UUID         NOT NULL,
    created_at          TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
    updated_at          TIMESTAMPTZ  NOT NULL DEFAULT NOW(),

    UNIQUE (workspace_id, path)
);

CREATE INDEX idx_resources_workspace_type
    ON resources (workspace_id, resource_type)
    WHERE deleted_at IS NULL;

CREATE INDEX idx_resources_workspace_path
    ON resources (workspace_id, path)
    WHERE deleted_at IS NULL;


CREATE TABLE resource_versions (
    resource_id         UUID         NOT NULL REFERENCES resources(id) ON DELETE CASCADE,
    version             INT          NOT NULL,

    -- Deterministic Vault path:
    --   aithericon/resources/{workspace_id}/{resource_id}/v{version}
    -- Stored explicitly so a future per-workspace Vault policy can be applied
    -- without recomputing the path on every read.
    vault_path          TEXT         NOT NULL,

    -- Non-secret fields from the typed resource struct, keyed by field name.
    -- Matches `ResourceTypeDescriptor.public_fields`. Resolver merges this
    -- inline with the secret-template refs at resolve time.
    public_config       JSONB        NOT NULL DEFAULT '{}'::jsonb,

    created_by          UUID         NOT NULL,
    created_at          TIMESTAMPTZ  NOT NULL DEFAULT NOW(),

    PRIMARY KEY (resource_id, version)
);


-- Plan §B.3 — ACL. `permission` is intentionally a string (not an enum) so
-- v2 can grow `rotate`, `share`, `audit_read` without an ALTER TYPE. v1
-- recognized values: `read`, `write`, `delete`.
CREATE TABLE resource_acl (
    resource_id         UUID         NOT NULL REFERENCES resources(id) ON DELETE CASCADE,
    principal_id        UUID         NOT NULL,
    principal_kind      TEXT         NOT NULL CHECK (principal_kind IN ('user', 'group', 'service')),
    permission          TEXT         NOT NULL,

    granted_by          UUID         NOT NULL,
    granted_at          TIMESTAMPTZ  NOT NULL DEFAULT NOW(),

    PRIMARY KEY (resource_id, principal_id, principal_kind, permission)
);

CREATE INDEX idx_resource_acl_principal
    ON resource_acl (principal_id, principal_kind);


-- Plan §B.7 — instance-level pin map. JSONB so the resolver writes the whole
-- envelope in one shot; queries (`which instances pinned resource X`) are
-- handled with a GIN index when the v2 telemetry dashboard lands.
ALTER TABLE workflow_instances
    ADD COLUMN IF NOT EXISTS resource_pins JSONB;
