-- Roster uniqueness must scope to LIVE enrollments, not all-time.
--
-- The original `UNIQUE (workspace_id, capacity_id, member_user_id)` constraint
-- (migration 20240156) ignored `revoked_at`, so once a member was revoked from a
-- pool the tombstone row kept the key occupied — re-enrolling that same member
-- into that same pool failed with a 409 forever. Revoke→re-enroll is a routine
-- admin action, so scope uniqueness to live rows: drop the all-time constraint
-- and replace it with a PARTIAL unique index over `revoked_at IS NULL`. Revoked
-- tombstones may now accumulate (harmless) while a member can still be enrolled
-- at most once LIVE per capacity (the engine `t_claim` invariant the handler's
-- 409 protects).
ALTER TABLE roster_members
    DROP CONSTRAINT roster_members_workspace_id_capacity_id_member_user_id_key;

CREATE UNIQUE INDEX roster_members_live_unique
    ON roster_members (workspace_id, capacity_id, member_user_id)
    WHERE revoked_at IS NULL;
