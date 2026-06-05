-- Rename the polymorphic asset/resource owner scope `project` → `folder`.
--
-- The flat `projects` grouping was collapsed into the hierarchical `folders`
-- tree in 20240149000000_folders.sql. The asset layer (docs/20 §2) owns
-- resources / assets / asset_types by a polymorphic `(scope_kind, scope_id)`
-- pair where `scope_kind` was one of `workspace | project | template`. The
-- middle tier was always folder-backed (its `scope_id` is a folder id); this
-- migration finishes the rename so the stored discriminator matches the model
-- (`ScopeKind::Folder`) and the API token (`folder:<uuid>`).
--
-- `scope_kind` is a free TEXT column (no CHECK constraint), so this is a pure
-- value rewrite. Pre-production: no data preservation ceremony needed.

UPDATE resources    SET scope_kind = 'folder' WHERE scope_kind = 'project';
UPDATE assets       SET scope_kind = 'folder' WHERE scope_kind = 'project';
UPDATE asset_types  SET scope_kind = 'folder' WHERE scope_kind = 'project';
