-- Phase 3 — Runner interface catalog.
--
-- A single workspace-scoped projection of the ROS (or other) interface surface a
-- runner self-reports after introspecting its environment. One row per runner
-- (PRIMARY KEY = runner_id, FK runners.id), upserted on every discovery push via
-- `POST /api/v1/runners/{id}/interfaces` (runner-token authed, self-only). The
-- operator UI reads it back via `GET /api/v1/runners/{id}/interfaces`.
--
-- `catalog` is an opaque-to-Postgres JSONB blob with the agreed shape:
--   { "topics":   [ {"name":"/turtle1/cmd_vel", "type":"geometry_msgs/msg/Twist"}, ... ],
--     "services": [ {"name":"/turtle1/teleport_absolute", "type":"turtlesim/srv/TeleportAbsolute"}, ... ],
--     "actions":  [ {"name":"/turtle1/rotate_absolute", "type":"turtlesim/action/RotateAbsolute"}, ... ] }
-- The typed shape lives in the Rust DTO (`RunnerInterfaceCatalog`); the column is
-- JSONB so the catalog can grow new collections without an ALTER.
--
-- `workspace_id` is a UUID without an FK constraint here, mirroring the runners
-- migration — forward-compatible with the workspaces table.

CREATE TABLE runner_interfaces (
    -- One row per runner. FK to runners.id with ON DELETE CASCADE so revoking +
    -- hard-deleting a runner reaps its interface row (soft-delete leaves it).
    runner_id           UUID         PRIMARY KEY REFERENCES runners (id) ON DELETE CASCADE,

    workspace_id        UUID         NOT NULL,

    -- The agreed topics/services/actions catalog blob (see header).
    catalog             JSONB        NOT NULL,

    -- Optional runner-reported version/hash of the catalog (e.g. a content hash
    -- or a ROS distro tag). Lets the UI show "discovered at version X".
    catalog_version     TEXT,

    -- When the runner first reported this catalog row, and when it was last
    -- upserted. `discovered_at` is preserved across upserts; `updated_at` bumps.
    discovered_at       TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
    updated_at          TIMESTAMPTZ  NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_runner_interfaces_workspace
    ON runner_interfaces (workspace_id);

COMMENT ON TABLE runner_interfaces IS
    'Per-runner self-reported interface catalog (ROS topics/services/actions). One row per runner, upserted on discovery push.';
COMMENT ON COLUMN runner_interfaces.catalog IS
    'JSONB { topics[], services[], actions[] } each {name, type}. Typed by the RunnerInterfaceCatalog DTO.';
COMMENT ON COLUMN runner_interfaces.discovered_at IS
    'First-discovery timestamp; preserved across upserts (updated_at bumps instead).';
