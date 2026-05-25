-- Phase B.12 — Resource access audit log.
--
-- One row per *successful* resolver invocation. The resolver writes this row
-- after the ACL check passes and before the resolved envelope is returned
-- to the launcher, so an aborted launch can still be reconciled against the
-- audit trail.
--
-- Granularity is intentionally **per-instance-launch**, not per-step (Plan
-- Risk #3). The "pinned at instance creation" guarantee means a resource is
-- materialized exactly once per launch; per-step audit rows would be
-- duplicative and inconsistent with that guarantee. Per-step access logging
-- can be added in v2 as a separate stream if needed.

CREATE TABLE resource_audit (
    id                  BIGSERIAL    PRIMARY KEY,

    -- Reverse-join key for "show me the audit trail for this run". Nullable
    -- because not every resolve happens during a workflow launch — CRUD-side
    -- audit rows (create, rotate, delete) leave this NULL.
    instance_id         UUID,

    -- Optional step attribution for the day we do go per-step. NULL for
    -- launch-time resolves (the v1 case).
    step_id             TEXT,

    resource_id         UUID         NOT NULL REFERENCES resources(id) ON DELETE CASCADE,
    resource_version    INT          NOT NULL,

    principal_id        UUID         NOT NULL,

    -- Action vocabulary v1: `resolve`, `create`, `update`, `rotate`, `delete`,
    -- `oauth_refresh`. Stored as TEXT for forward-compat — handlers fill it
    -- in deliberately rather than relying on a CHECK constraint that would
    -- require a migration for every new verb.
    action              TEXT         NOT NULL,

    -- Where the call originated (`api`, `launcher`, `oauth_refresher`, etc.).
    -- Useful for distinguishing user-initiated rotation from the background
    -- OAuth refresh worker (B.11).
    site                TEXT         NOT NULL,

    occurred_at         TIMESTAMPTZ  NOT NULL DEFAULT NOW()
);

-- "Audit trail for this run" — covers the dominant query.
CREATE INDEX idx_resource_audit_instance
    ON resource_audit (instance_id, occurred_at DESC)
    WHERE instance_id IS NOT NULL;

-- "Show me the history of this resource" — used by the resource detail view.
CREATE INDEX idx_resource_audit_resource
    ON resource_audit (resource_id, occurred_at DESC);

-- "Show me what this principal has touched" — security/compliance angle.
CREATE INDEX idx_resource_audit_principal
    ON resource_audit (principal_id, occurred_at DESC);
