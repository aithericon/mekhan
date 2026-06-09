-- file_server_endpoints: N access methods per file server (docs/32 §4.1).
--
-- A `file_server` is now identity-only — the logical backend the platform tracks
-- files ON. The *ways to reach it* (object_store, s3, sftp, local_mount) are
-- modelled as N child rows here, each with its own `root` prefix mapping into
-- this namespace, optional `resource_ref` (secrets stay in Vault via the
-- resource), and its own status / verification lifecycle. This lets one logical
-- server expose, e.g., both an S3 face and an SFTP face onto the same canonical
-- tree, and lets local_mount endpoints carry the capacity `group_id` they
-- dispatch against.
--
-- The old single-valued transport columns on `file_servers`
-- (`kind`/`base_path`/`resource_ref`) move here as the first endpoint per server,
-- then are dropped — `file_servers` becomes pure identity. Test lab: clean
-- forward migration, no down-migration.

CREATE TABLE file_server_endpoints (
    id                  UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    file_server_id      UUID        NOT NULL REFERENCES file_servers(id) ON DELETE CASCADE,
    access_method       TEXT        NOT NULL,                       -- object_store | s3 | sftp | local_mount
    root                TEXT        NOT NULL DEFAULT '',            -- prefix in this namespace mapping to the canonical root
    resource_ref        TEXT,                                       -- resource `path` holding connection+secrets (Vault); NULL for object_store
    group_id            TEXT,                                       -- capacity-group UUID for local_mount dispatch; nullable
    status              TEXT        NOT NULL DEFAULT 'unknown',     -- unknown|online|offline|error
    verification_status TEXT        NOT NULL DEFAULT 'unverified',  -- unverified|verified|mismatch|conflict
    last_verified       TIMESTAMPTZ,
    last_seen           TIMESTAMPTZ,
    priority            INT         NOT NULL DEFAULT 0,             -- operator routing override; higher = preferred
    config              JSONB       NOT NULL DEFAULT '{}',
    created_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (file_server_id, access_method, root)
);

CREATE INDEX idx_file_server_endpoints_server ON file_server_endpoints (file_server_id);
CREATE INDEX idx_file_server_endpoints_method ON file_server_endpoints (access_method);

-- Backfill: each existing server gets one endpoint carrying its old transport.
-- `kind` becomes `access_method` (legacy `nfs`/`local` → `local_mount`),
-- `base_path` becomes `root` (NULL → ''), `resource_ref` carries over. Inherit
-- the server's status/last_seen so the lone endpoint reflects the old state.
INSERT INTO file_server_endpoints
    (file_server_id, access_method, root, resource_ref, status, last_seen)
SELECT
    fs.id,
    CASE WHEN fs.kind IN ('nfs', 'local') THEN 'local_mount' ELSE fs.kind END,
    COALESCE(fs.base_path, ''),
    fs.resource_ref,
    fs.status,
    fs.last_seen
FROM file_servers fs;

-- file_servers is now identity-only.
ALTER TABLE file_servers DROP COLUMN kind;
ALTER TABLE file_servers DROP COLUMN base_path;
ALTER TABLE file_servers DROP COLUMN resource_ref;
DROP INDEX IF EXISTS idx_file_servers_kind;
