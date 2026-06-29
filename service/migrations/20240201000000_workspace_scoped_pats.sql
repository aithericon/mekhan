-- Workspace-scoped Personal Access Tokens (PATs) + a user-level default workspace.
--
-- ## Binding model
--
-- Until now a `uat_` PAT carried NO workspace of its own: the verifier
-- (`auth/user_pat.rs`) re-ran the resolver's silent "first membership" pick on
-- every request, so the token's effective tenant drifted with the owner's
-- membership list and was never knowable at mint time. This migration binds the
-- workspace at MINT: `user_pats.workspace_id` is chosen when the token is
-- created and is fixed for its lifetime. Authorization stays LIVE — the verifier
-- still checks the owner's membership in that workspace per request and
-- fail-closed rejects once it is revoked — but the *which workspace* question is
-- now answered deterministically by the row, not by a re-pick.
--
-- ## Backfill rationale
--
-- Existing rows predate the column, so we backfill each via the EXACT pick the
-- verifier used to compute on the fly (the `ORDER BY w.is_system ASC,
-- w.created_at ASC LIMIT 1` correlated over the owner's non-archived
-- memberships). This freezes every live token to the same workspace it would
-- have resolved to on its next request — a no-op in observable behaviour.
--
-- A row that still resolves NULL after the backfill belongs to an owner with
-- ZERO non-archived memberships: that PAT could never resolve a workspace, so
-- `require_workspace` already 403'd every request it made. We DELETE those
-- unresolvable tokens before tightening the column to NOT NULL.
--
-- FK `ON DELETE CASCADE` mirrors the existing `user_pats.user_id` FK. Workspaces
-- are never hard-deleted (only `archived_at` is set), so the cascade is a
-- dormant safety net, not a live behaviour.

-- 1. Add the binding column (nullable for the backfill window).
ALTER TABLE user_pats
    ADD COLUMN workspace_id UUID REFERENCES workspaces(id) ON DELETE CASCADE;

-- 2. Backfill via the OLD resolution: the owner's first non-archived membership,
--    real tenants (is_system = FALSE) before system workspaces, then by age.
UPDATE user_pats p SET workspace_id = (
    SELECT w.id
      FROM workspaces w
      JOIN workspace_members m ON m.workspace_id = w.id
     WHERE m.user_id = p.user_id AND w.archived_at IS NULL
     ORDER BY w.is_system ASC, w.created_at ASC
     LIMIT 1
);

-- 3. Any still-NULL row = a PAT whose owner holds no non-archived membership; it
--    could never have resolved a workspace (the old `require_workspace` already
--    errored on every request it made), so it is dead. Remove it.
DELETE FROM user_pats WHERE workspace_id IS NULL;

-- 4. Scoping is now MANDATORY: every PAT is bound to exactly one workspace.
ALTER TABLE user_pats ALTER COLUMN workspace_id SET NOT NULL;

-- 5. Reverse lookup (e.g. "tokens scoped to this workspace") + FK index.
CREATE INDEX idx_user_pats_workspace ON user_pats (workspace_id);

-- 6. User-level default workspace — step 2 of the shared resolution ladder
--    (`auth/resolver.rs::resolve_active_workspace`). NULL = no default set; the
--    ladder then falls through to a sole membership or fails loud on ambiguity.
--    `ON DELETE SET NULL` so archiving/removing a workspace clears the pointer
--    rather than stranding a dangling id (dormant: workspaces aren't hard-deleted).
ALTER TABLE users
    ADD COLUMN default_workspace_id UUID REFERENCES workspaces(id) ON DELETE SET NULL;
