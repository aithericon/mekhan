-- Phase 3 — Object-ACL spine. One polymorphic grant table binding
-- (object_type, object_id, user_id) → role across folders, templates, and
-- instances. The effective role for a user on an object is the MAX of the
-- most-specific grant (object > nearest-ancestor folder) and the user's
-- workspace role (the FLOOR — a grant can downgrade an inherited higher tier
-- but never drop a user below their own workspace role). Workspace
-- Owner/Admin bypass object ACLs entirely (resolved in `auth/grants.rs`).
--
-- For a TEMPLATE, `object_id` is the chain-root `COALESCE(base_template_id, id)`
-- (NOT a per-version id) so a grant follows the whole version chain — exactly
-- like `template_folders` / `template_tags`. Folders nest via the materialized
-- `folders.path`; ancestry is a path-prefix self-join (no recursive CTE).
--
-- `object_id` is polymorphic with NO foreign key — referential integrity is
-- enforced by handler cleanup inside the existing delete transactions
-- (folder/template/instance delete adds `DELETE FROM object_grants WHERE
-- object_type=… AND object_id=…`). A periodic GC could backstop later.

CREATE TYPE object_kind AS ENUM ('folder', 'template', 'instance');

CREATE TABLE object_grants (
    id           UUID         PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID         NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    object_type  object_kind  NOT NULL,
    object_id    UUID         NOT NULL,
    user_id      UUID         NOT NULL,
    role         TEXT         NOT NULL CHECK (role IN ('owner', 'admin', 'editor', 'viewer')),
    granted_by   UUID         NOT NULL,
    granted_at   TIMESTAMPTZ  NOT NULL DEFAULT now(),
    updated_at   TIMESTAMPTZ  NOT NULL DEFAULT now(),
    -- Upsert key + single-object/single-user resolve left-prefix.
    UNIQUE (object_type, object_id, user_id)
);

-- list_grants (all users for one object). NOT used by the resolve path — the
-- UNIQUE btree's (object_type, object_id) left-prefix already covers that.
CREATE INDEX idx_object_grants_obj ON object_grants (object_type, object_id);

-- accessible_object_ids + effective_object_roles: a user's whole grant set in
-- one workspace, fetched from the small side before subtree expansion.
CREATE INDEX idx_object_grants_ws_user ON object_grants (workspace_id, user_id);
