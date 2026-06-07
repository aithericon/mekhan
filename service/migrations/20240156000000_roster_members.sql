-- P2 — Humans as a Capacity · the roster (docs/33 §7).
--
-- A "human capacity" is a `capacity` resource (`presence · offer · …`) with a
-- backing `pool-<resource_id>` net. The ROSTER is the set of
-- `workspace_members` enrolled in that capacity — the human realisation of the
-- presence-pool's enrolled fleet, mirroring how `runners` rows back the runner
-- pool. One row per (workspace, capacity, member):
--
--   roster_members    One row per `workspace_member` enrolled in a human
--                     capacity. `capacity_id` references the human-capacity
--                     `resources.id` directly (the pool net is
--                     `pool-<capacity_id>`); `member_user_id` is the
--                     `workspace_members.user_id`. The `caps` blob is
--                     ADMIN-ASSIGNED — an authorised role writes it into this
--                     trusted row, validated against the workspace's
--                     `CapabilityType`s; the client NEVER asserts its own caps
--                     (byte-identical trust model to runner enrollment, where
--                     caps come from the trusted DB row, never the wire). Both
--                     the injected pool unit's caps and the inbox eligibility
--                     filter read from here, so the engine matcher and the
--                     advisory inbox can only ever disagree on "offer already
--                     taken", never on "you weren't eligible". `concurrency` is
--                     the per-person `C` (the presence controller's slot count);
--                     `availability` carries the `liveness_source` / `ttl` /
--                     `grace` knobs (docs/33 §7.1); `available` is the durable
--                     intent toggle. Soft-delete via `revoked_at` (NULL = live).
--
-- `workspace_id` is a UUID without an FK constraint here, mirroring the
-- resources / runners migrations — forward-compatible with the workspaces
-- table; adding a FK later is a single ALTER away.

CREATE TABLE roster_members (
    id              UUID         PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id    UUID         NOT NULL,
    capacity_id     UUID         NOT NULL,                      -- the human-capacity resources.id
    member_user_id  UUID         NOT NULL,                      -- workspace_members.user_id
    caps            JSONB        NOT NULL DEFAULT '{}'::jsonb,  -- admin-assigned, validated vs CapabilityType
    concurrency     INT          NOT NULL DEFAULT 1,            -- per-person C
    availability    JSONB        NOT NULL DEFAULT '{}'::jsonb,  -- {liveness_source, ttl_secs, grace_secs}
    available       BOOLEAN      NOT NULL DEFAULT FALSE,        -- durable intent toggle
    available_since TIMESTAMPTZ,
    enrolled_by     UUID         NOT NULL,
    enrolled_at     TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
    revoked_at      TIMESTAMPTZ,
    UNIQUE (workspace_id, capacity_id, member_user_id)
);

-- live-row indexes
CREATE INDEX idx_roster_members_capacity ON roster_members (workspace_id, capacity_id) WHERE revoked_at IS NULL;
CREATE INDEX idx_roster_members_member   ON roster_members (workspace_id, member_user_id) WHERE revoked_at IS NULL;
