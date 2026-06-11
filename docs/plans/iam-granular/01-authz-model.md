# Phase 3 — Object-ACL Spine (authz-model deep-dive)

The granular authorization layer: one `object_grants` table, one
`effective_object_role` resolver (single-object) + one `effective_object_roles`
batch resolver (per-row list annotation), grant CRUD, and re-gating across the
**three** instance-enforcement surfaces (REST handlers, Yjs WS, `/petri/*` proxy).

## 1. Model

A grant binds `(object_type, object_id, user_id) → role`. Effective role for a
user on an object is:

- **Folders / templates:** `max(most-specific grant among {object grant, nearest
  ancestor folder grant}, workspace_role)`.
- **Instances (two-hop, decision 8):** `max(direct instance grant, grant on the
  parent template keyed `COALESCE(t.base_template_id,t.id)`, nearest ancestor of
  the template's folder, workspace_role)`.
- **Workspace Owner/Admin bypass:** if `member_role ≥ Admin`, return that workspace
  role immediately — never constrained by an object ACL.
- **Workspace role is a FLOOR (decision 7):** the final value is `max(grant_tier,
  workspace_role)`. Most-specific wins only *among* the object/folder grant tiers;
  it can downgrade an *inherited* higher grant but never drop a user below their own
  workspace role.

Folders genuinely nest (`folders.parent_id` self-FK + materialized `folders.path`
with `idx_folders_ws_path ON folders(workspace_id, path text_pattern_ops)`), so
ancestry is a **path-prefix self-join — no recursive CTE**.

Templates attach to a folder via `template_folders.base_template_id` (the PK, the
chain root). Grants on a template store `object_id = COALESCE(base_template_id,id)`
so a grant follows the whole version chain — exactly like `template_folders` /
`template_tags`. Instances have NO folder column and join their template by
per-version `template_id` / `template_version`.

The existing `require_role`/`require_member` workspace gates stay; granular checks
are NEW additive functions handlers opt into.

## 2. Migration — `service/migrations/20240171000000_object_grants.sql`

```sql
CREATE TYPE object_kind AS ENUM ('folder','template','instance');

CREATE TABLE object_grants (
  id           UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  workspace_id UUID NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
  object_type  object_kind NOT NULL,
  -- polymorphic across folders / workflow_templates(base id) / workflow_instances;
  -- NO FK. Referential integrity is enforced by handler cleanup in the existing
  -- delete tx (folder/template/instance delete adds a DELETE FROM object_grants).
  -- For a TEMPLATE, object_id = COALESCE(base_template_id, id) (chain root),
  -- NOT a per-version id — grants follow the version chain like template_folders.
  object_id    UUID NOT NULL,
  user_id      UUID NOT NULL,
  role         TEXT NOT NULL CHECK (role IN ('owner','admin','editor','viewer')),
  granted_by   UUID NOT NULL,
  granted_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
  updated_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
  UNIQUE (object_type, object_id, user_id)        -- upsert key + single-object/single-user lookup left-prefix
);

CREATE INDEX idx_object_grants_user    ON object_grants(user_id);
-- list_grants (all users for one object). NOT used by the resolve path
-- (the UNIQUE btree's (object_type,object_id) left-prefix covers that).
CREATE INDEX idx_object_grants_obj     ON object_grants(object_type, object_id);
-- accessible_object_ids + effective_object_roles: a user's whole grant set in a ws.
CREATE INDEX idx_object_grants_ws_user ON object_grants(workspace_id, user_id);
```

**Index justification (folds the perf-lens minor):** the `UNIQUE` constraint's
btree on `(object_type, object_id, user_id)` serves both the upsert AND the
single-object single-user resolve as a left-prefix. `idx_object_grants_obj` exists
purely for `list_grants`. `idx_object_grants_ws_user` drives the batch/set paths.

## 3. Resolver module — `service/src/auth/grants.rs` (NEW)

Sibling to `membership.rs`; reuses `Role`, `MembershipError`, `map_to_api_error`.

```rust
pub enum ObjectKind { Folder, Template, Instance }
pub struct ObjectRef { pub kind: ObjectKind, pub id: Uuid, pub workspace_id: Uuid }
pub enum AccessSet { All, Ids(Vec<Uuid>) }
```

### 3.1 `effective_object_role(db, user, obj) -> Result<Option<Role>, MembershipError>`

1. `member_role(db, user, obj.workspace_id)`; `NotMember → Ok(None)` (callers keep
   the public short-circuit BEFORE calling this).
2. If `ws_role ≥ Admin` → `Ok(Some(ws_role))` immediately (bypass; never downgraded).
3. Else compute the most-specific grant in ONE query and return
   `Some(max(grant, ws_role))` — the **floor** (decision 7).

**Template SQL (the heart, no N+1):** resolve `base_id = COALESCE(base_template_id,
id)` and the home folder `path` (`LEFT JOIN template_folders + folders`), then:

```sql
SELECT role, source_rank
FROM (
  -- object grant on the template chain-root
  SELECT role, 3 AS source_rank, 0 AS depth
    FROM object_grants
   WHERE object_type='template' AND object_id=$base_id AND user_id=$uid
  UNION ALL
  -- ancestor-folder grants: folders whose path is a prefix of the home path
  SELECT g.role, 2 AS source_rank, length(f.path) AS depth
    FROM object_grants g
    JOIN folders f ON f.id=g.object_id
   WHERE g.object_type='folder' AND g.user_id=$uid
     AND ($home_path = f.path OR $home_path LIKE f.path || '/%')
) s
ORDER BY source_rank DESC,                 -- object beats folder
         depth DESC,                       -- nearest ancestor (deepest path) beats shallower
         (CASE role WHEN 'owner' THEN 3 WHEN 'admin' THEN 2
                    WHEN 'editor' THEN 1 ELSE 0 END) DESC
LIMIT 1;
```

Final effective = `max(that grant, ws_role)`.

### 3.2 Two-hop instance resolution (decision 8 — folds review M1/M5)

Instances have no `workspace_id` and join their template by per-version
`template_id`. `effective_object_role(instance)` is a **3-table join**
(instance→template→template_folders→folders). Resolve the instance's
`base_id = COALESCE(t.base_template_id, t.id)` and `t.workspace_id` via
`JOIN workflow_templates t ON t.id = i.template_id`, then UNION:

```
max(
  direct instance grant   (object_type='instance', object_id=$instance_id, rank 4),
  parent-template grant   (object_type='template',  object_id=$base_id,    rank 3),
  template-folder ancestry(object_type='folder',    prefix of home path,   rank 2),
  workspace_role          (floor)
)
```

This is the fix for the folderless-template case: an object-Editor grant on a
template with NO folder row still propagates to that template's instances via the
rank-3 tier.

### 3.3 `effective_object_roles(db, user, kind, ws, ids) -> HashMap<Uuid, Role>` (batch — folds review M7/M10)

For per-row list `my_effective_role`. ONE query: `DISTINCT ON (base_id)` over the
user's grant set joined to the candidate `ids`, `ORDER BY base_id, source_rank DESC,
depth DESC, role_rank DESC`. For ws Admin/Owner short-circuit every id to the ws
role (no query). **Test asserts exactly one role-resolution query regardless of row
count.**

### 3.4 `accessible_object_ids(db, user, kind, ws) -> AccessSet`

`AccessSet::All` for ws Owner/Admin; else ONE set-returning union:
(a) direct object-grant base_ids; (b) base_ids whose home folder path is at/under a
folder the user has a grant on; (c) existing public/visibility rule for templates.
For instances, (a)/(b) resolve through the template tier per §3.2.

**Folder-subtree expansion (folds review M6):** drive from the SMALL side. First
`SELECT` the user's granted folder *paths* (bounded by grant count via
`idx_object_grants_ws_user`), then expand with parameterized
`f.path LIKE $bound || '/%'` (bound VALUE, not a column-to-column join) so
`idx_folders_ws_path text_pattern_ops` is usable. **Acceptance requires an `EXPLAIN`
check** that the index is chosen; documented fallback is a bounded recursive CTE
over `parent_id` (the tree is shallow) if `EXPLAIN` shows a seq scan.

### 3.5 `require_object_role` / `apply_grant`

- `require_object_role(db, user, obj, need)` — `None` or `< need` → error.
- `apply_grant(tx, user_id, object_type, object_id, role, granted_by)` — upsert on
  the UNIQUE key. **Phase 4 invites call this on accept.**

`service/src/auth/mod.rs`: `pub mod grants;` + re-export `effective_object_role`,
`effective_object_roles`, `require_object_role`, `accessible_object_ids`,
`apply_grant`, `ObjectKind`, `ObjectRef`, `AccessSet`.

## 4. Grant endpoints — `service/src/handlers/object_grants.rs` (NEW)

`object_type` is constrained to literals `folders|templates|instances` (three
concrete route registrations so utoipa models it and the resolver never sees an
unknown kind). Each resolves the `ObjectRef`:
- folder → `SELECT workspace_id FROM folders`;
- template → workspace + `base_id`;
- instance → `JOIN workflow_templates t ON t.id = i.template_id` for `workspace_id`
  + the template's folder for inheritance.

Endpoints (gate = `require_object_role(..., Admin)`):

- `GET /api/v1/{folders|templates|instances}/{id}/grants` → `Vec<GrantView>`.
  Synthesizes inherited folder + workspace rows marked `source: 'object'|'folder'|
  'workspace'` so the UI shows the full effective picture; only `source=='object'`
  rows are editable here.
  `GrantView { id, user_id, member_display_name, member_email, avatar_url, role,
  granted_by, granted_at, source, inherited_from_folder_id,
  inherited_from_folder_path }` (denormalized identity per Phase-1 decision).
- `PUT /api/v1/{...}/{id}/grants/{user_id}` body `{role}` → upsert. **Server-side
  enforcement (decision 9, folds review M2):** (a) grantee MUST be a
  `workspace_members` row of the object's workspace → else 400/409; (b) granted role
  capped at the caller's own effective role on the object (Admin/Owner ws bypass
  exempts). 200 `GrantView`.
- `DELETE /api/v1/{...}/{id}/grants/{user_id}` → 204. Removing the last Owner grant
  is allowed (ws Owner/Admin retain bypass — no object orphaned).

Register the 9 routes + `GrantView`/`PutGrantRequest` in `openapi.rs`/`lib.rs`.
`ObjectKind` is a path-literal, not a schema enum.

## 5. PATCH member role (folds review minor — missing endpoint)

Only list/add/delete members exist today. Add
`PATCH /api/v1/workspaces/{id}/members/{user_id} {role}` (Admin-gated) with a
**server-side last-owner guard** (block dropping `ownerCount` to 0 → 409/422).
Phase 5 inline-role-edit consumes it.

## 6. Yjs gate rewrite — `service/src/handlers/yjs_sync.rs`

REPLACE the bare check (verified lines 69–71):
```rust
if template.visibility != "public" && template.workspace_id != user_ws { forbidden }
let readonly = template.published;
```
with: keep the `visibility=='public'` short-circuit (read-only connect), else build
`ObjectRef::template` → `effective_object_role`. `None → 403`; `Some(role)` →
connect, `readonly = template.published OR role < Editor` (a Viewer object-grant gets
a read-only socket even on an unpublished draft; `handle_socket` drops writes).
**Load-bearing cross-area dependency** — a folder-scoped Editor cannot collaborate
unless this honors grants.

## 7. `/petri/*` proxy gate rewrite — `service/src/petri/proxy.rs` (folds review B1)

REPLACE the bare `member_role` in `gate_petri_instance`:
- Keep the `Err(TemplateNotFound) → safe-method-allow` branch for genuine infra nets
  (`resource-pool-net`, `executor-net`).
- Keep the `is_safe && visibility == "public"` short-circuit.
- For mekhan-owned instances, resolve `ObjectRef::instance(net_id→instance→template)`
  via `effective_object_role`: require `≥ Viewer` for safe (GET/HEAD/OPTIONS/TRACE)
  methods (drop the blanket member-only safe-allow), `≥ Editor` for state-changing.
- e2e: a no-grant member gets 403 on `GET /petri/nets/{their-instance-net}/...`.

## 8. REST instance handler re-gating — `service/src/handlers/instances.rs`

Verified signatures: `create_instance(State, AuthUser, Json)` (no membership check),
`list_instances(State, Query)` (no AuthUser), `get_instance(State, Path)` (no
AuthUser), `stream_instance`/`get_instance_state`/`get_instance_events` (`_user`).

- **`create_instance` (folds review B3 — behavior change):** today only checks
  `published`/`visibility`. Add `require_object_role(ObjectRef::template(
  req.template_id), Editor)` BEFORE launch (the instance doesn't exist yet — key on
  the TEMPLATE + its folder). Document: public-but-not-member launches are now
  rejected. Test: non-member POST with a public template → 403.
- **`list_instances` (close the leak):** add `user: AuthUser`; filter via
  `accessible_object_ids(Instance)` → push `AND COALESCE(wt.base_template_id, wt.id)
  = ANY($base_ids)` into the existing version-pinned `wt` JOIN (add `base_template_id`
  to its projection). Embed per-row `my_effective_role` via `effective_object_roles`.
- **All instance READS (folds review B2):** `get_instance`, `stream_instance`,
  `get_instance_state`, `get_instance_events`, spawn/children listing — each takes
  `user: AuthUser` (not `_user`) + `require_object_role(instance→template, Viewer)`.
  `cancel_instance` + mutate paths require Editor. Regression tests: 403 on a
  no-grant read of each endpoint, not just the list.

## 9. Template handler re-gating — `service/src/handlers/templates.rs`

- `gate_template_read` → public short-circuit, then `effective_object_role ≥ Viewer`.
- `gate_template_write` → `≥ Editor`.
- `list_templates` → `accessible_object_ids(Template)`; `Ids` pushes
  `AND COALESCE(t.base_template_id, t.id) = ANY($N)` into `append_template_where`
  (composes with the existing folder filter). Embed per-row `my_effective_role` via
  `effective_object_roles`.

## 10. Folder handler re-gating + cleanup — `service/src/handlers/folders.rs`

- Swap `require_role(ws, Editor)` → `require_object_role(folder, Editor)` for
  create-in-subtree / update / delete / `set_template_folder`. **Keep**
  `require_role(ws, Editor)` for create-at-root (no parent object).
- On folder delete AND template/instance delete, add
  `DELETE FROM object_grants WHERE object_type=... AND object_id=...` inside the
  existing tx (polymorphic cleanup, since `object_id` has no FK).
- **Coordinate with Phase 2's `updated_by` adds on the same handlers** (same UPDATEs).

## 11. OpenAPI

Adds 9 grant endpoints + `GrantView`/`PutGrantRequest`; PATCH member role;
`my_effective_role` on template/instance/folder DTOs (detail **and per-row on
lists**); `list_instances`/`get_instance`/stream/state/events now require auth. Run
`just dev::openapi`.

## 12. Tests

- **Unit:** object Editor > folder Viewer > ws Viewer → Editor; deeper-folder Viewer
  beats shallower-folder Owner among grant tiers (most-specific, named as intended);
  **floor:** folder Viewer-override on a ws Editor → still Editor; Admin/Owner bypass
  can't be downgraded by an object Viewer grant; NotMember → None (public read via
  caller); **two-hop:** object-Editor on a folderless template → its instances Editor.
- **Integration (live):** PUT grant as object-Admin → 200; PUT role > caller →
  403; PUT for a non-member grantee → 400/409; `list_templates`/`list_instances` as
  a folder-scoped Editor return exactly in/under the granted folder + public (assert
  ONE filter query + ONE role-annotation query); leak regressions on `get_instance`,
  stream, state, events, `/petri/nets/{net}` for a no-grant member → 403;
  `create_instance` non-member → 403; `EXPLAIN` uses `idx_folders_ws_path`.
- **e2e (Yjs):** folder-scoped Editor → writable socket; non-granted → 403;
  Viewer-grant → read-only socket dropping updates.
- **dev_noop:** seeded dev user is ws Owner → bypass → Owner everywhere;
  `accessible_object_ids → All`.

## 13. Risks

- `list_instances`/`get_instance`/etc. behavior change (were unscoped) — re-check
  the instance playwright specs.
- `create_instance` now rejects cross-workspace/non-member launches that succeed
  today — explicit, tested.
- Polymorphic `object_id` has no FK — mitigated by delete-tx cleanup; a periodic GC
  could backstop (not v1).
- `/petri/*` and Yjs are load-bearing — backend enforces; FE re-reads
  `my_effective_role` to avoid stale edit affordances.
- Migration `171` must land between `170` (audit) and `172` (invites).
