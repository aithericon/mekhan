-- Workspaces are the tenant boundary and the ACL root for everything that
-- carries a workspace_id (templates, resources, instances by extension).
-- One row = one tenant. Membership is Mekhan-owned (workspace_members below);
-- `zitadel_org_id` is an optional binding the resolver consults to auto-
-- provision membership when a JWT carrying that org_id arrives.
--
-- Note on the default workspace UUID: we deliberately pick `Uuid::nil()`
-- (all zeros) so the existing `default_workspace() -> Uuid::nil()` constants
-- in `service/src/handlers/resources.rs` and `service/src/process/publish.rs`
-- keep resolving to a real, FK-targetable workspace row without code changes
-- or vault-path rewrites. Existing `resources.workspace_id = nil` data is
-- already at "the default workspace" by this definition.

CREATE TABLE workspaces (
    id              UUID         PRIMARY KEY DEFAULT gen_random_uuid(),
    slug            TEXT         NOT NULL UNIQUE,
    display_name    TEXT         NOT NULL,
    -- Nullable: set by the resolver when a Zitadel org claim is seen. UNIQUE
    -- prevents accidentally binding two workspaces to the same upstream org.
    zitadel_org_id  TEXT         UNIQUE,
    -- System workspaces (currently just `demos`) are owned by the platform
    -- and not user-editable. Surfaces in admin endpoints; never deletable.
    is_system       BOOLEAN      NOT NULL DEFAULT FALSE,
    created_at      TIMESTAMPTZ  NOT NULL DEFAULT NOW()
);


-- Single permission edge. `user_id` is `AuthUser::subject_as_uuid()` —
-- a UUIDv5 of the OIDC subject, populated lazily on first login (resolver
-- upserts when the JWT's org claim matches a workspace).
CREATE TABLE workspace_members (
    workspace_id  UUID         NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    user_id       UUID         NOT NULL,
    role          TEXT         NOT NULL CHECK (role IN ('owner','admin','editor','viewer')),
    added_at      TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
    PRIMARY KEY (workspace_id, user_id)
);

CREATE INDEX idx_workspace_members_user ON workspace_members(user_id);


-- Seed: default workspace (id = nil so default_workspace() keeps working)
-- and the system-owned demos workspace. The dev-noop user is added as
-- owner of the default workspace; its UUID is `uuid_v5(SUBJECT_UUID_NAMESPACE,
-- "dev-user")` per `service/src/auth/model.rs:11` — verified out-of-band.
INSERT INTO workspaces (id, slug, display_name, is_system) VALUES
    ('00000000-0000-0000-0000-000000000000', 'default', 'Default Workspace', FALSE),
    ('00000000-0000-0000-0000-0000000000de', 'demos',   'Demos',             TRUE);

INSERT INTO workspace_members (workspace_id, user_id, role) VALUES
    ('00000000-0000-0000-0000-000000000000',
     '3bb26085-29f3-5fbf-8a8c-a2e485a1f55b',
     'owner');
