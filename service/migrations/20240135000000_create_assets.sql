-- docs/20 §4/§7 — The asset layer: user-typed, curated, *static* content stored
-- as schema-validated JSONB rows (+ S3 for File fields), consumed by workflow
-- nodes as ordinary staged inputs.
--
-- Three tables + an ALTER on workflow_instances:
--
--   asset_types    User-defined schemas. `fields_json` is a `Vec<PortField>`
--                  (the existing unified field language — NOT an asset-specific
--                  vocabulary). Scoped + foldered like resources.
--   assets         Named, version-pinned, scope-owned collections of records of
--                  one asset type. An "object" asset is the 1-row degenerate
--                  case (`cardinality='object'`).
--   asset_records  Schema-validated JSONB rows, versioned with the asset. File
--                  fields store an S3 storage path *inside* the row JSONB.
--   workflow_instances.asset_pins
--                  Frozen `alias -> {asset_id, version}` map captured at launch,
--                  mirroring `resource_pins`, so asset edits after launch don't
--                  retroactively change running instances.
--
-- `scope_id` carries no FK (it is polymorphic across workspaces/projects/
-- templates by `scope_kind`), matching the `resources` transitional shape.

CREATE TABLE asset_types (
    id            UUID         PRIMARY KEY DEFAULT gen_random_uuid(),

    -- Polymorphic owner: workspace | project | template (docs/20 §2).
    scope_kind    TEXT         NOT NULL,
    scope_id      UUID         NOT NULL,

    -- Flat identifier ref-key, ^[a-z][a-z0-9_]*$. The borrow-checker /
    -- resolver / `<slug>.<field>` grammar is unchanged (docs/20 §3).
    name          TEXT         NOT NULL,
    display_name  TEXT         NOT NULL,

    -- Virtual folder prefix (e.g. `materials/metals`). Emergent folders, no
    -- folders table (docs/20 §3).
    display_path  TEXT,

    -- The schema: a `Vec<PortField>` (reused wholesale from the port model).
    fields_json   JSONB        NOT NULL DEFAULT '[]'::jsonb,

    -- 'object' (1-row degenerate) | 'collection'.
    cardinality   TEXT         NOT NULL DEFAULT 'collection',

    version       INT          NOT NULL DEFAULT 1,

    created_by    UUID,
    created_at    TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
    updated_at    TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
    deleted_at    TIMESTAMPTZ
);

-- (scope_kind, scope_id, name) unique among live rows.
CREATE UNIQUE INDEX idx_asset_types_scope_name
    ON asset_types (scope_kind, scope_id, name)
    WHERE deleted_at IS NULL;

CREATE INDEX idx_asset_types_scope
    ON asset_types (scope_kind, scope_id)
    WHERE deleted_at IS NULL;


CREATE TABLE assets (
    id            UUID         PRIMARY KEY DEFAULT gen_random_uuid(),

    scope_kind    TEXT         NOT NULL,
    scope_id      UUID         NOT NULL,

    type_id       UUID         NOT NULL REFERENCES asset_types(id),

    -- Flat identifier, ^[a-z][a-z0-9_]*$ — the binding ref-key.
    ref_key       TEXT         NOT NULL,
    display_name  TEXT         NOT NULL,
    display_path  TEXT,

    -- Bumped on record edits; running instances pin the version they launched
    -- against (docs/20 §6).
    version       INT          NOT NULL DEFAULT 1,

    created_by    UUID,
    created_at    TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
    updated_at    TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
    deleted_at    TIMESTAMPTZ
);

CREATE UNIQUE INDEX idx_assets_scope_ref_key
    ON assets (scope_kind, scope_id, ref_key)
    WHERE deleted_at IS NULL;

CREATE INDEX idx_assets_scope
    ON assets (scope_kind, scope_id)
    WHERE deleted_at IS NULL;

CREATE INDEX idx_assets_type
    ON assets (type_id)
    WHERE deleted_at IS NULL;


CREATE TABLE asset_records (
    asset_id  UUID         NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
    version   INT          NOT NULL,
    row_idx   INT          NOT NULL,

    -- Validated against the asset type's `fields_json` at write time. File
    -- fields store an S3 storage path (or catalogue-entry-derived path) inside
    -- this JSONB (docs/20 §4.1/§4.2).
    data      JSONB        NOT NULL,

    PRIMARY KEY (asset_id, version, row_idx)
);


-- Instance-level pin map, mirrors `resource_pins`. Shape:
-- `{ alias -> { asset_id, version } }` captured at instance-launch time.
ALTER TABLE workflow_instances
    ADD COLUMN IF NOT EXISTS asset_pins JSONB NOT NULL DEFAULT '{}'::jsonb;
