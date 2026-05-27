-- Projects are an M:N grouping inside a workspace. They are NOT an ACL
-- boundary — listing a template via a project still resolves to the
-- underlying template's workspace_id check. Projects exist so that a future
-- per-project OpenAPI bundle endpoint can pick a subset of templates by
-- project membership.
--
-- Tags are free-form labels, workspace-scoped, no ACL semantics.
--
-- Both joins key on `base_template_id` (the chain root), so attachments
-- follow the live `is_latest` version automatically — no need to update
-- when a template is versioned.

CREATE TABLE projects (
    id            UUID         PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id  UUID         NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    slug          TEXT         NOT NULL,
    display_name  TEXT         NOT NULL,
    description   TEXT         NOT NULL DEFAULT '',
    created_at    TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
    created_by    UUID         NOT NULL,
    UNIQUE (workspace_id, slug)
);

CREATE INDEX idx_projects_workspace ON projects(workspace_id);


CREATE TABLE project_templates (
    project_id        UUID         NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    base_template_id  UUID         NOT NULL,
    added_at          TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
    added_by          UUID         NOT NULL,
    PRIMARY KEY (project_id, base_template_id)
);

CREATE INDEX idx_project_templates_base ON project_templates(base_template_id);


CREATE TABLE template_tags (
    workspace_id      UUID  NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    base_template_id  UUID  NOT NULL,
    tag               TEXT  NOT NULL,
    PRIMARY KEY (workspace_id, base_template_id, tag)
);

CREATE INDEX idx_template_tags_tag ON template_tags(workspace_id, tag);
