-- file_servers: first-class storage-backend entities (docs/32 §4.1).
--
-- A file server is the entity the platform tracks files ON. Until now
-- `file_inventory.file_server_id` was a bare TEXT string with no backing
-- entity; this table gives each server an identity, a transport `kind`, an
-- optional pointer to a `resource` holding its connection + secrets (secrets
-- NEVER live here — they stay in Vault via the resource), and a place for
-- status / lifecycle.
--
-- The join to `file_inventory` is SOFT (by `key` == `file_server_id`, no FK):
-- a crawl can observe a file before its server is registered, and an unknown
-- server string still renders (with an "adopt" affordance to promote it). The
-- entity is workspace-scoped so `resource_ref` resolves where the secret lives;
-- `file_inventory` is currently global/single-workspace (multi-tenant inventory
-- scoping is a follow-up), so in practice `key` is globally unique.
--
-- Rollups (file count, total size, status breakdown) are DERIVED by joining
-- `file_inventory` on `key` — never stored here.

CREATE TABLE file_servers (
    id            UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id  UUID        NOT NULL,
    key           TEXT        NOT NULL,   -- == file_inventory.file_server_id (soft join)
    display_name  TEXT        NOT NULL,
    kind          TEXT        NOT NULL,   -- object_store | s3 | sftp  (nfs|local reserved, deferred)
    resource_ref  TEXT,                   -- resource `path` holding connection+secrets; NULL for built-in object_store
    base_path     TEXT,                   -- root/prefix within the backend
    status        TEXT        NOT NULL DEFAULT 'unknown',  -- unknown|online|offline|error
    last_seen     TIMESTAMPTZ,
    config        JSONB       NOT NULL DEFAULT '{}',        -- kind-specific non-secret extras
    created_at    TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (workspace_id, key)
);

CREATE INDEX idx_file_servers_workspace ON file_servers (workspace_id);
CREATE INDEX idx_file_servers_kind ON file_servers (kind);
