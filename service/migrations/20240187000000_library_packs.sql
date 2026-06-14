-- Library packs — a named, importable/exportable bundle of library nodes.
--
-- A "pack" groups the `library_node` templates that ship together (an
-- OpenFOAM pack, a mumax3 pack, …) under one `vendor/slug` coordinate so the
-- whole set can be imported, exported, and removed as a unit. It adds no
-- runtime/engine primitive: a pack is just a control-plane parent row that the
-- pack's `workflow_templates` rows point back at via `pack_id`.
--
-- Purely additive: `library_packs` is new, and the `workflow_templates.pack_id`
-- column is nullable + FK ON DELETE SET NULL, so existing rows (and library
-- nodes promoted ad-hoc, not via a pack) load unchanged with `pack_id = NULL`.

CREATE TABLE IF NOT EXISTS library_packs (
    id           UUID PRIMARY KEY,
    -- Owning tenant. A pack is installed into one workspace; `system`-origin
    -- packs are seeded into the demo workspace but visible everywhere (mirrors
    -- the public library-node visibility rule).
    workspace_id UUID NOT NULL REFERENCES workspaces(id),
    -- `vendor/slug` coordinate halves (mirrors the library-node coordinate
    -- convention; stored split so the unique index can key on them directly).
    vendor       TEXT NOT NULL,
    slug         TEXT NOT NULL,
    -- Pack-level version label (free text, e.g. `2406`). Distinct from the
    -- per-node template `version` integer.
    version      TEXT NOT NULL DEFAULT '1',
    name         TEXT NOT NULL,
    description  TEXT NOT NULL DEFAULT '',
    -- Trust axis, same vocabulary as `workflow_templates.origin`:
    -- system | workspace | community. Imports always create `workspace`.
    origin       TEXT NOT NULL DEFAULT 'workspace',
    installed_by UUID,
    installed_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Coordinate is unique within an origin (mirrors the global-within-origin
-- semantics of the library-node `(origin, coordinate)` partial unique index).
CREATE UNIQUE INDEX IF NOT EXISTS uq_library_packs_origin_vendor_slug
    ON library_packs (origin, vendor, slug);

-- Each library-node template MAY belong to a pack. ON DELETE SET NULL so
-- removing a pack row never cascades into the (separately handled) template
-- families — the pack-delete handler removes the families explicitly inside the
-- same transaction; this FK is the safety net for any stray reference.
ALTER TABLE workflow_templates
    ADD COLUMN IF NOT EXISTS pack_id UUID REFERENCES library_packs(id) ON DELETE SET NULL;

CREATE INDEX IF NOT EXISTS idx_workflow_templates_pack_id
    ON workflow_templates (pack_id) WHERE pack_id IS NOT NULL;
