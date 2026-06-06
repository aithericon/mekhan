-- Folders replace the flat M:N "projects" grouping with a hierarchical,
-- single-parent tree (a filesystem model). A template lives in **at most one**
-- folder; absence of a `template_folders` row == the workspace root.
--
-- `path` is a denormalized materialized path ('/parent/child/leaf') kept in
-- sync by the handler layer on create / move / delete. It powers subtree
-- queries (`path = $sel OR path LIKE $sel || '/%'`) and the per-folder OpenAPI
-- bundle without recursive CTEs.
--
-- Tags (template_tags) are a SEPARATE cross-cutting label system and are left
-- exactly as migration 20240125 created them — only the projects tables are
-- dropped here. Pre-production: no back-compat, drop outright.

DROP TABLE IF EXISTS project_templates CASCADE;
DROP TABLE IF EXISTS projects CASCADE;

CREATE TABLE folders (
    id            UUID         PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id  UUID         NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    parent_id     UUID         NULL REFERENCES folders(id) ON DELETE CASCADE,
    slug          TEXT         NOT NULL,
    display_name  TEXT         NOT NULL,
    description   TEXT         NOT NULL DEFAULT '',
    path          TEXT         NOT NULL,
    created_at    TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
    created_by    UUID         NOT NULL,
    UNIQUE (workspace_id, parent_id, slug),
    UNIQUE (workspace_id, path)
);

CREATE INDEX idx_folders_ws_path ON folders(workspace_id, path text_pattern_ops);

-- A UNIQUE(workspace_id, parent_id, slug) treats NULL parent_id rows as
-- distinct (Postgres NULL semantics), so it does NOT prevent two root folders
-- sharing a slug. This partial unique index closes that gap.
CREATE UNIQUE INDEX folders_root_slug_uniq
    ON folders(workspace_id, slug) WHERE parent_id IS NULL;


CREATE TABLE template_folders (
    base_template_id  UUID         PRIMARY KEY,
    folder_id         UUID         NOT NULL REFERENCES folders(id) ON DELETE CASCADE,
    workspace_id      UUID         NOT NULL,
    moved_at          TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
    moved_by          UUID         NOT NULL
);

CREATE INDEX idx_template_folders_folder ON template_folders(folder_id);
