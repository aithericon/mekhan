-- P3 — Humans as a capacity: workspace-scope + the offered→claimed lifecycle.
--
-- docs/34 §5. A capacity-bound HumanTask is *offered* to eligible available
-- members, a member *claims* it, then completes it — all engine-authoritative,
-- with hpi_tasks the projection of pool-net token state (docs/33 §5).
--
-- Two new columns support that lifecycle:
--   * workspace_id — workspace-scope (docs/33 §4 precondition). Nullable, no FK,
--     mirroring the other projection tables (the row materializes from net
--     events, not a control-plane write, so it can't enforce referential
--     integrity at insert time).
--   * claimed_at  — when a member claimed the offer (set on the offered→claimed
--     projection transition).
--
-- `status` stays free TEXT; the new values 'offered'/'claimed' need no
-- constraint change. The `assignee` column already exists (TEXT) and now
-- carries the member user_id on claim.

ALTER TABLE hpi_tasks ADD COLUMN workspace_id UUID;          -- workspace-scope (docs/33 §4 precondition)
ALTER TABLE hpi_tasks ADD COLUMN claimed_at   TIMESTAMPTZ;   -- when a member claimed

CREATE INDEX idx_hpi_tasks_ws_status ON hpi_tasks (workspace_id, status);
