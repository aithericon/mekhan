-- Entity Pages: generalize the Yjs layer (Phase 0) + pages metadata (Phase 1).
--
-- One opaque-UUID-keyed Yjs stack now backs BOTH workflow-template graph
-- canvases and free-form rich-text pages. We rename `template_id` -> `doc_id`,
-- add an immutable `doc_kind` discriminator, and DROP the `workflow_templates`
-- FK (template ids and page ids share one keyspace; there is no single host
-- table to FK against). Losing `ON DELETE CASCADE` is intentional and the
-- replacement is explicit handler-side DELETEs (the same discipline
-- `object_grants` already uses for its polymorphic `object_id`).

-- ── Part 1: generalize the Yjs tables (Phase 0) ──────────────────────────────
ALTER TABLE yjs_documents  DROP CONSTRAINT yjs_documents_template_id_fkey;
ALTER TABLE yjs_snapshots  DROP CONSTRAINT yjs_snapshots_template_id_fkey;
ALTER TABLE yjs_documents  RENAME COLUMN template_id TO doc_id;
ALTER TABLE yjs_snapshots  RENAME COLUMN template_id TO doc_id;
ALTER TABLE yjs_documents  ADD COLUMN doc_kind TEXT NOT NULL DEFAULT 'graph';
ALTER TABLE yjs_snapshots  ADD COLUMN doc_kind TEXT NOT NULL DEFAULT 'graph';
ALTER TABLE yjs_documents  ADD CONSTRAINT yjs_documents_doc_kind_chk CHECK (doc_kind IN ('graph','page'));
ALTER TABLE yjs_snapshots  ADD CONSTRAINT yjs_snapshots_doc_kind_chk CHECK (doc_kind IN ('graph','page'));
-- (indexes + the snapshots UNIQUE follow the renamed column automatically; the
--  index NAMES still say _template — cosmetic. ON CONFLICT must use doc_id now.)

-- ── Part 2: pages metadata table (Phase 1) ───────────────────────────────────
-- Free-form collaborative rich-text documents. The rich content lives entirely
-- in the generalized Yjs stack (yjs_documents/yjs_snapshots WHERE doc_kind =
-- 'page', keyed on pages.id) — this table holds metadata + placement only.
--
-- The "both" attachment model: a page either rides a host entity 1:1
-- (attached_kind/attached_id — a singleton "Notes"/"Report" tab on a template
-- or instance) OR lives free-standing inside a folder (folder_id). The XOR
-- CHECK enforces exactly one placement; the partial UNIQUE makes the attached
-- form a true singleton. `attached_id` is polymorphic (template chain-root id
-- OR instance id) with NO FK — the same discipline `object_grants.object_id`
-- uses — so its cleanup is explicit handler-side DELETEs.
CREATE TABLE pages (
    id            UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id  UUID        NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    title         TEXT        NOT NULL DEFAULT '',
    attached_kind TEXT        NULL CHECK (attached_kind IN ('template','instance')),
    attached_id   UUID        NULL,                       -- polymorphic, NO FK (like object_grants)
    folder_id     UUID        NULL REFERENCES folders(id) ON DELETE CASCADE,
    created_by    UUID        NOT NULL,
    updated_by    UUID        NOT NULL,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT pages_placement_xor CHECK (
        (attached_kind IS NOT NULL AND attached_id IS NOT NULL AND folder_id IS NULL)
        OR (attached_kind IS NULL AND attached_id IS NULL AND folder_id IS NOT NULL)
    )
);
CREATE UNIQUE INDEX pages_attachment_uniq ON pages (attached_kind, attached_id) WHERE attached_id IS NOT NULL;
CREATE INDEX idx_pages_folder    ON pages (folder_id) WHERE folder_id IS NOT NULL;
CREATE INDEX idx_pages_workspace ON pages (workspace_id);
