# Granular IAM — Master Implementation Plan (Overview)

> Entry point. Read this standalone for the whole picture; the five sibling files
> (`01`–`05`) are per-area deep-dives. Every claim here is verified against the
> real code in the `iam-granular` worktree (see `## Ground-truth verification`).

## 1. Executive Summary

Move the platform from coarse **workspace-role** authorization
(`workspace_members.role`, ~71 `require_role`/`require_member` call sites) to
**per-object grants with folder cascade**, plus complete authorship/audit
display and a full invite lifecycle.

Five areas, dependency-ordered into five phases:

1. **Identity seam** (keystone) — turn any `subject_as_uuid()` UUID into
   `{display_name, email, avatar_url}`; capture the dropped OIDC `picture` claim;
   ship `UserChip`/`Avatar`/profile-cache.
2. **Audit / provenance** — `updated_by`/`updated_at` across eight entities; fix
   `job_templates.created_by` TEXT→UUID; add authorship to catalogue tables;
   widen DTOs.
3. **Object-ACL spine** — `object_grants` table + single `effective_object_role`
   resolver (object > nearest-ancestor folder > workspace; Owner/Admin bypass);
   grant CRUD; rewrite the Yjs gate; **close every instance-read leak across all
   three enforcement surfaces (REST handlers, Yjs WS, `/petri/*` proxy).**
4. **Invites / onboarding** — `pending_invites` + email send + accept-link page +
   create-user-in-Zitadel-on-accept + pre-seeded object grants.
5. **Frontend surface** — rebuilt member page, `ShareDialog`, authorship chips,
   access view, effective-role gating.

## 2. Locked Product Decisions (design to these; do not relitigate)

1. **Folder cascade + override.** Effective role = **most-specific** grant:
   object > nearest-ancestor folder > workspace. Ownership is just the top role
   (Owner) on an object. **Workspace role is a FLOOR** (see decision 7) — a
   more-specific *lower* grant can downgrade an *inherited* higher grant but can
   never drop a user below their own workspace role.
2. **Granular ACL scope = folders, templates, instances ONLY.** Resources, job
   templates, assets, catalogue, saved queries stay workspace-role-gated but get
   authorship display + `updated_by`. **Instances inherit through a TWO-HOP walk:
   direct instance grant > grant on the parent template (chain-root) > the
   template's folder ancestry > workspace role** (see decision 8).
3. **Workspace Owner/Admin BYPASS object ACLs** — implicit full access to every
   object; no object can be hidden from an admin.
4. **Invites = full flow** — `pending_invites` + own email send + accept-link +
   create-in-Zitadel-on-accept + "Pending" badge. `dev_noop` has a working
   offline stub (Log sender + Noop provisioner, **fail-closed** in non-dev).
5. **`updated_by` + `updated_at` audit required** across the entities, alongside
   existing `created_by`/`author_id`.
6. **Reuse the `Role` enum** (`Viewer < Editor < Admin < Owner`, `membership.rs`)
   for object grants.
7. **Workspace role is a FLOOR, not just a tier** (resolved from review). The
   resolver returns `max(most_specific_object_or_folder_grant, workspace_role)`.
   A folder Viewer-override cannot drop a workspace Editor below Editor. Among
   *grant* tiers (object/folder), most-specific wins even if lower — the floor is
   only the workspace role.
8. **Two-hop instance inheritance** (resolved from review). Instances have no
   folder column AND join their template by per-version `template_id` /
   `template_version`. Effective role on an instance =
   `max(direct instance grant, grant on COALESCE(t.base_template_id,t.id),
   grant on that template's folder ancestry, workspace_role)`.
9. **Object grants are members-only and non-escalating** (resolved from review).
   `put_grant` server-side requires the grantee to already be a
   `workspace_members` row of the object's workspace, and caps the granted role
   at the caller's own effective role on that object (≤ caller role; Admin/Owner
   workspace bypass exempts).

## 3. Dependency DAG & Build Order

```
            ┌─────────────────────────────────────────────┐
            │  PHASE 1: Identity seam (KEYSTONE)           │
            │  avatar_url + picture claim + batch resolver │
            │  + UserChip/Avatar/profile-cache             │
            └───────────────┬─────────────────────────────┘
                            │ (UserChip + UserProfileDto + profile cache)
        ┌───────────────────┼───────────────────────────────┐
        ▼                   ▼                                 │
┌──────────────────┐ ┌──────────────────────────┐            │
│ PHASE 2: Audit/   │ │ PHASE 3: Object-ACL spine │            │
│ provenance        │ │ object_grants + resolver  │            │
│ updated_by cols   │ │ + Yjs gate + petri gate   │            │
│ + jt TEXT→UUID    │ │ + ALL instance reads      │            │
│ + DTO widening    │ │ + grant endpoints         │            │
└────────┬──────────┘ └────────────┬──────────────┘            │
         │                         │ (effective_object_role,   │
         │                         │  effective_object_roles,  │
         │                         │  apply_grant, grants API) │
         │            ┌────────────┴───────────┐               │
         │            ▼                         ▼               │
         │   ┌──────────────────┐    ┌────────────────────────┐│
         └──▶│ PHASE 5: Frontend │◀───│ PHASE 4: Invites        ││
             │ surface           │    │ pending_invites +       ││
             │                   │    │ Zitadel create-on-accept││
             └──────────────────┘    │ + invite_object_grants  ││
                            ▲         └─────────────────────────┘│
                            └────────────────────────────────────┘
```

**Ordering justification:**

- **Phase 1 (identity) is the keystone, lands first.** Every later area renders
  authorship/grantee/grantor/invited-by UUIDs through `UserChip` + the batch
  resolver. It also captures the dropped `picture` claim into `avatar_url`.
- **Phases 2 & 3 are parallel-capable** but both edit `folders.rs` and
  `instances.rs` (audit adds `updated_by` to the same UPDATEs the ACL area
  re-gates). No hard ordering; audit can land first (purely additive DTO
  widening). **Phase 3 is the spine.**
- **Phase 4 (invites) hard-depends on Phase 3** — it applies pre-seeded grants on
  accept via `apply_grant`, so the grant table + helpers must exist first and the
  invite migration number must be *higher* than the grants migration.
- **Phase 5 (frontend) hard-depends on Phases 1–4's OpenAPI regen** — lands
  strictly after (or combined-PR with) the regen, or `ci::openapi-drift` fails.

## 4. Phase Breakdown & Acceptance Criteria

Detailed file-level changes live in `01`–`05`. Summary + acceptance here.

### PHASE 1 — Identity Seam (`03-identity-avatars.md`)
**Migration `20240169000000_user_profiles_avatar.sql`:** `ALTER TABLE user_profiles ADD COLUMN avatar_url TEXT`.
**Key change vs. review:** `AuthUser.user_id` is a **serialize-only derived field**
(custom `Serialize` injecting `subject_as_uuid()`), NOT a constructor field — so
it can never serialize as `null` and no `AuthUser { .. }` literal needs to set it.
- **Acceptance:** `GET /api/auth/session` carries `avatar_url` + `user_id` (always
  present); `POST /api/v1/users/profiles` resolves a batch in one query; member
  list renders names/avatars; `svelte-check 0/0`; `openapi-drift` green;
  dev_noop → "Dev User" + "DU" initials offline.

### PHASE 2 — Audit / Provenance (`02-audit-provenance.md`)
**Migration `20240170000000_audit_provenance.sql`** (all `ADD COLUMN IF NOT EXISTS`
for re-apply safety on partially-migrated slots): `updated_by`/`updated_at` across
the eight entities; `job_templates`/`job_template_versions` add `created_by_uuid`
(keep legacy TEXT one release); catalogue tables add `created_by`(+`updated_by`).
- **Acceptance:** all eight entities carry `updated_by`/`updated_at`; DTOs expose
  `created_by`+`updated_by`; `job_templates.created_by_uuid` joins `user_profiles`
  (verified by a create-then-read-name round trip); catalogue entries inherit
  authorship from the producing instance; `openapi-drift` + svelte-check green.

### PHASE 3 — Object-ACL Spine (`01-authz-model.md`)
**Migration `20240171000000_object_grants.sql`:** `object_kind` enum + polymorphic
`object_grants` + indexes.
**Key changes vs. review (all folded):**
- **Three enforcement surfaces, not one.** Re-gate (a) every instance REST read
  (`get_instance`, `stream_instance`, `get_instance_state`, `get_instance_events`,
  spawn/children listing) — all currently take no/`_user` AuthUser; (b) the Yjs WS
  gate; (c) **`service/src/petri/proxy.rs::gate_petri_instance`** which today gates
  on bare `member_role`.
- **`create_instance` is an explicit behavior change** — today no membership
  check at all. Gate on `effective_object_role(template) ≥ Editor` against the
  TEMPLATE (instance doesn't exist yet).
- **Two-hop instance inheritance** (decision 8) in the resolver SQL + a
  `effective_object_roles` **batch** variant for per-row list annotation (no N+1).
- **Folder-subtree expansion driven from the small side** (bound path strings, not
  column-to-column LIKE) so `idx_folders_ws_path text_pattern_ops` is usable.
- **Workspace role is a floor** (decision 7).
- **`put_grant` enforces members-only + no-escalation server-side** (decision 9).
- **PATCH member-role endpoint added** (only list/add/delete exist today) with
  server-side last-owner guard.
- **Acceptance:** `effective_object_role` resolves object > nearest-folder >
  workspace with Owner/Admin bypass in one query; grant CRUD enforces members-only
  + no-escalation; `list_templates`/`list_instances` filter via
  `accessible_object_ids` with no N+1 and embed per-row `my_effective_role` via the
  batch resolver; Yjs **and** `/petri/*` **and** all instance REST reads honor
  grants (a no-grant member gets 403 on `GET /petri/nets/{their-instance-net}` and
  on `GET /instances/{id}`); `EXPLAIN` confirms `idx_folders_ws_path` is used.

### PHASE 4 — Invites / Onboarding (`04-invites-onboarding.md`)
**Migration `20240172000000_pending_invites.sql`** (after ACL `171`):
`pending_invites` + `invite_object_grants`.
**Key changes vs. review (all folded):**
- **Public token endpoints go in `build_public_openapi_router()`** (the seam that
  puts unauth routes INTO the OpenAPI spec) — NOT a raw merged router, which would
  silently drop them from `schema.d.ts`.
- **Provisioner selection fails closed + loud:** `ZitadelMgmt` for any Bff/auth
  mode, `NoopUserProvisioner` ONLY under `dev_noop`, asserted at boot (panic if
  `auth != dev_noop && provisioner is Noop`). `LogEmailSender`/`NoopUserProvisioner`
  are the DEFAULT (Some) under dev_noop so accept never 503s offline.
- **Accept keys membership/grants on the resolved real sub→uuid**, never a
  synthetic one, so re-login maps to the same grants.
- **Concrete rate-limit** (`tower-governor` per-IP on the two public routes) +
  per-token attempt ceiling; identical 404 code-path for unknown/expired/revoked/
  accepted (no timing divergence).
- **Acceptance:** Admin invites by email with optional pre-seeded grants applied
  atomically on accept; token single-use, hashed-at-rest, expiring; no enumeration;
  full flow offline under dev_noop; gated e2e proves a real Zitadel human-user
  creation.

### PHASE 5 — Frontend Surface (`05-frontend-surface.md`)
Pure `app/`; hard-blocked on Phases 1–4 regen.
- **Acceptance:** member page shows identities + inline role edit + last-owner
  guard + pending rows; `ShareDialog` lists object + inherited grants with override
  + inherited-downgrade warning; authorship chips on detail + lists; Yjs editor
  surface reflects a live grant downgrade (re-reads `my_effective_role`, doesn't
  rely on the WS silently rejecting); `svelte-check 0/0`, vitest + playwright +
  `openapi-drift` green.

## 5. Consolidated Migration Sequence

Highest existing migration verified: **`20240168000000_catalogue_query.sql`**.

| Number | Phase | Purpose |
|---|---|---|
| `20240169000000_user_profiles_avatar.sql` | 1 | `ALTER user_profiles ADD avatar_url TEXT`. |
| `20240170000000_audit_provenance.sql` | 2 | `updated_by`/`updated_at` × 8 entities; `job_templates(+versions)` add `created_by_uuid` (keep legacy TEXT 1 release); catalogue tables add `created_by`(+`updated_by`). `ADD COLUMN IF NOT EXISTS`; idempotent backfills. |
| `20240171000000_object_grants.sql` | 3 | `object_kind` enum + polymorphic `object_grants` (UNIQUE(object_type,object_id,user_id)) + indexes. |
| `20240172000000_pending_invites.sql` | 4 | `pending_invites` (token_hash, partial-unique active-email, status) + `invite_object_grants`. |
| *(later)* `2024017x_drop_job_templates_created_by_text.sql` | follow-up | Drop deprecated `job_templates.created_by` TEXT after the UUID column is proven. |

**Collision risk (headline footgun):** all four areas independently wanted
`20240169+`. `sqlx` rejects duplicate/out-of-order versions and the repo has prior
renumber incidents (catalog-coupling/file_servers, dup `20240151`). The four slots
are **pre-assigned**; each branch MUST rebase its filename to its slot before merge,
and invites (`172`) MUST be numbered after ACL (`171`).

**sccache gotcha (every migration-adding phase):** the umbrella builds from repo
root and `pool.rs` has a `migrate!`-dir note — sccache can miss new migration files.
After adding one, force a rebuild (`touch service/src/db/pool.rs` or
`RUSTC_WRAPPER='' cargo build`) then restart mekhan; on a live slot DB, `just dev
reset` re-applies the chain cleanly.

## 6. OpenAPI Contract Touchpoints

Every phase except 5 changes the Rust contract → run `just dev::openapi` and commit
`openapi-mekhan.json` + `app/src/lib/api/schema.d.ts` in the same change
(`ci::openapi-drift` gate).

| Phase | Contract change | Regen? |
|---|---|---|
| 1 | `AuthUser` gains `avatar_url` + `user_id` (serialize-only); new `POST /users/profiles` + `BatchProfilesRequest`/`UserProfileDto`; `WorkspaceMember`/`RosterMemberSummary` gain `avatar_url`. | Yes |
| 2 | Widen Resource/Asset/AssetType/JobTemplate(+Version)/Template/Instance/Folder/SavedQuery/catalogue DTOs with `created_by`/`updated_by` (all additive/nullable). | Yes |
| 3 | 9 grant endpoints (3 verbs × 3 object types) + `GrantView`/`PutGrantRequest`; PATCH member role; `my_effective_role` on template/instance/folder DTOs (detail **and per-row on lists**); `list_instances`/`get_instance`/stream/state/events now require auth. | Yes |
| 4 | 5 invite handlers + DTOs; 2 PUBLIC token endpoints registered in `build_public_openapi_router()` so they appear in the spec. | Yes |
| 5 | None (consumes regenerated `schema.d.ts`). Combined-PR or land strictly after 1–4. | No (verify `npx svelte-check`) |

## 7. Test Strategy

- **Unit (no DB):** role precedence + floor + Owner/Admin bypass + NotMember
  (`grants.rs`); two-hop instance inheritance (folderless-template grant →
  instance Editor); `subject_as_uuid` round-trips the seeded dev profile
  (`3bb26085-29f3-5fbf-8a8c-a2e485a1f55b`); resolver picture-claim extraction;
  token entropy/hashing; NoopProvisioner determinism; ProfileCache coalescing.
- **Integration (live stack):** grant CRUD members-only + no-escalation;
  `list_templates`/`list_instances` set-filter (assert single list query **and**
  single role-annotation query); the closed instance-read leaks (regression-guard
  `get_instance`, stream, state, events, AND `/petri/nets/{net}` for a no-grant
  member → 403); job_templates TEXT→UUID round trip; resource/folder/saved-query/
  template audit columns; catalogue authorship inherited from instance; invite
  create/accept/resend/single-use/expiry/revoke; provisioner-fail-closed boot
  invariant.
- **e2e:** Yjs gate honors grants (writable / 403 / read-only Viewer); playwright
  `iam-share` + `iam-invite` (need a 2nd seeded member — document the seed).
- **Gated live-Zitadel (`MEKHAN_E2E_ZITADEL=1`):** invite→accept creating a real
  human user (proves ORG_OWNER authorizes `POST /users/human`).
- **dev_noop parity:** seeded dev user is ws Owner → bypass everywhere; invite flow
  end-to-end via LogEmailSender + NoopUserProvisioner; avatar NULL → "DU".
- **Gates per phase:** `cargo fmt --check` + `clippy -D warnings`,
  `cargo test --workspace`, `svelte-check 0/0`, vitest, `openapi-drift`. **Trust
  `npx svelte-check`, not the LSP popup**, after a schema regen.

## 8. Risk Register — how each review BLOCKER/MAJOR was resolved

### Blockers (security-lockout lens)
- **B1 — `/petri/*` is a third instance-ACL surface not covered.** `gate_petri_instance`
  gates on bare `member_role`, so a no-grant member could still read engine state /
  drive the net by net_id after the REST list was scoped. **Resolved:** Phase 3 adds
  `/petri/*` to scope — rewrite `gate_petri_instance` to resolve
  `ObjectRef::instance(net_id→instance→template)` via `effective_object_role`
  (≥Viewer for safe methods, ≥Editor for state-changing), keeping the public-template
  short-circuit and the genuine-infra-net `TemplateNotFound→safe-allow` branch.
- **B2 — `get_instance`/stream/state/events leak by id.** Only `list_instances` was
  named. **Resolved:** Phase 3 expands the leak-closure to ALL instance read paths;
  each takes `user: AuthUser` (not `_user`) + `require_object_role(instance→template,
  Viewer)`; mutate paths require Editor. Regression tests on each endpoint.
- **B3 — `create_instance` has no membership check today.** **Resolved:** stated as an
  explicit behavior change; gate via `effective_object_role(template) ≥ Editor`
  (against the template, which exists) BEFORE launch. Test: non-member POST → 403.

### Majors (security-lockout)
- **M1 — folderless template breaks instance inheritance.** **Resolved:** decision 8 —
  two-hop walk (`direct instance > parent-template grant > template-folder ancestry >
  ws role`); resolver SQL + `accessible_object_ids(Instance)` union include the
  template-grant tier. Test: object-Editor on a folderless template → its instances
  resolve Editor.
- **M2 — object-Owner re-share escalation + non-member grants.** **Resolved:**
  decision 9 — `put_grant` verifies the grantee is a workspace member of the object's
  workspace (else 400/409) and caps the granted role at `min(caller effective role,
  …)`. Tests for both.
- **M3 — most-specific override can REDUCE access silently.** **Resolved:** kept
  most-specific among grant tiers (locked) but pinned **workspace role as a floor**
  (decision 7) so a folder Viewer-override can't drop a workspace Editor below
  Editor; added a unit test naming the downgrade-of-inherited as intended; Phase 5
  surfaces a "this will downgrade inherited access for everyone" warning in
  `ShareDialog`.
- **M4 — dev_noop provisioner ship-to-prod hazard.** **Resolved:** Phase 4 fails
  closed + loud — `NoopUserProvisioner` only under `dev_noop`, boot-time panic
  otherwise; accept keys on the real resolved sub→uuid, never the synthetic one.

### Majors (performance-correctness)
- **M5 — instances have no `workspace_id` + per-version `template_id`.** **Resolved:**
  decision 8 + Phase 3 SQL — derive instance access entirely from the template/folder
  set; `list_instances` filter pushes `COALESCE(wt.base_template_id, wt.id) =
  ANY($base_ids)` into the existing version-pinned `wt` JOIN; documented as a 3-table
  join (instance→template→template_folders→folders).
- **M6 — folder-subtree LIKE has no usable index.** **Resolved:** Phase 3 drives the
  expansion from the small side — fetch the user's granted folder *paths* first
  (bounded by grant count, `idx_object_grants_ws_user`), then emit parameterized
  `f.path LIKE $bound || '/%'` so `idx_folders_ws_path text_pattern_ops` applies;
  acceptance requires an `EXPLAIN` check. (Recursive-CTE-over-`parent_id` is the
  documented fallback if `EXPLAIN` shows a seq scan.)
- **M7 — per-row `my_effective_role` asserted "no N+1" without a query.** **Resolved:**
  Phase 3 adds `effective_object_roles(db, user, kind, ws, ids) -> HashMap<Uuid,Role>`
  computed in ONE `DISTINCT ON (base_id)` query; list handlers zip it in; test asserts
  exactly one role-resolution query regardless of row count.

### Majors (contract-integration)
- **M8 — `AuthUser.user_id` would serialize null everywhere.** **Resolved:** Phase 1
  makes it a serialize-only derived field (custom `Serialize` injecting
  `subject_as_uuid()`), not a constructor field — always present, no literal edits.
- **M9 — public invite router wouldn't reach the OpenAPI spec.** **Resolved:** Phase 4
  registers the 2 public token endpoints in `build_public_openapi_router()` (verified
  seam at `lib.rs:172`), not a raw merged router.
- **M10 — set membership ≠ per-row role for gating.** **Resolved:** same as M7 — the
  batch `effective_object_roles` returns the role-per-id the FE needs.

## 9. Considered & Deferred (minor / declined findings)

- **Generic activity-log table** (generalizing `resource_audit` migr 20240121):
  deferred. Per-row `updated_by`/`updated_at` answers the stated "who last touched
  this" need; full history is a larger separate surface.
- **Tenant isolation on the batch profile resolver:** any authenticated member can
  resolve any UUID's identity (consistent with existing `resolve_user_by_email`).
  Filtering to co-members is a one-join product decision; left open, default
  workspace-wide for v1.
- **Avatar SSRF/CSP:** `AvatarImage` must not send referrer; CSP must allow the IdP
  host or images silently fail (initials mask it). Documented; not a blocker.
- **Redundant `idx_object_grants_obj`:** the `UNIQUE(object_type,object_id,user_id)`
  btree serves single-object single-user resolution as a left-prefix; keep
  `idx_object_grants_obj` ONLY for `list_grants` (all-users-for-one-object), with a
  one-line justification per index.
- **Migration re-runnability:** use `ADD COLUMN IF NOT EXISTS` + idempotent
  `... WHERE updated_by IS NULL` backfills so a partially-migrated slot survives.
- **job_templates DTO semantics shift** (raw subject string → UUID, both `string`
  in TS): keep BOTH `created_by` (legacy TEXT) and `created_by_uuid` in the DTO for
  one release; grep `app/` for `.created_by` reads on job templates before regen.
- **Constant-time token compare / pepper:** 32-byte CSPRNG entropy makes brute force
  infeasible; lookup is a single indexed equality on `token_hash`; the generic 404
  shares one code path (no early-return timing divergence). Documented.
- **Direct instance grants:** the enum supports them and the two-hop resolver unions
  them; product confirms they're wanted (decision 8).

## 10. Rollout / Backfill Notes

- **`object_grants`: NO backfill, starts empty.** Safe because of decision 3 +
  decision 7: existing Owners/Admins keep full access via bypass; existing
  Editors/Viewers keep workspace-role access via the floor. No object becomes
  inaccessible. Do not "default everyone" into grants.
- **`updated_by` backfilled to `created_by`** — a convenience seed (synthetic for
  pre-migration history); UI/reviewers treat historical `updated_by` as best-effort.
- **`job_templates.created_by` legacy TEXT → `created_by_uuid` NULL** for
  pre-migration rows (the v5 hash is one-way). Keep the TEXT column one release;
  drop in a follow-up. In dev a one-shot recompute is trivial; prod needs ops input
  on whether the stored TEXT is a recoverable sub.
- **`catalogue_entries.created_by`** NOT backfilled — new projector writes inherit
  from the producing instance; legacy/by-reference rows stay NULL.
- **`user_profiles.avatar_url`** stays NULL until each user next hits a gated request
  carrying a `picture` claim (eventual consistency via the extractor upsert).
- **dev_noop:** seeded dev user is ws Owner → bypass everywhere; whole IAM surface
  exercisable offline. The invite stub's synthetic sub is throwaway.
- **Per-slot dev DBs:** any slot carrying these migrations partially-applied needs
  `just dev reset`; the sccache `migrate!`-dir miss may require a forced rebuild.

## Ground-truth verification (read against the worktree)

- Highest committed migration: `20240168000000_catalogue_query.sql`.
- `folders` (migr 20240149): `parent_id UUID NULL REFERENCES folders(id) ON DELETE
  CASCADE`, materialized `path`, `UNIQUE(workspace_id, parent_id, slug)`,
  `CREATE INDEX idx_folders_ws_path ON folders(workspace_id, path text_pattern_ops)`.
- `workflow_instances` (migr 20240101): **NO `workspace_id`**; `template_id UUID NOT
  NULL REFERENCES workflow_templates(id)`; `template_version INTEGER NOT NULL`;
  `created_by UUID NOT NULL`. `idx_wi_template` on `template_id`.
- `list_instances` join: `JOIN workflow_templates wt ON wt.id = wi.template_id AND
  wt.version = wi.template_version` (per-version).
- `workflow_templates.base_template_id UUID REFERENCES workflow_templates(id)` +
  `idx_wt_base_template`.
- `create_instance(State, AuthUser, Json<CreateInstanceRequest>)` — only checks
  `published` + `visibility`, **no membership check**.
- `list_instances(State, Query)` — no AuthUser, no scoping.
- `get_instance(State, Path<Uuid>)` — no AuthUser. `stream_instance`,
  `get_instance_state`, `get_instance_events` take `_user: AuthUser`.
- `service/src/petri/proxy.rs::gate_petri_instance` — gates writes on bare
  `member_role`; allows safe methods for public templates and for
  `TemplateNotFound` net_ids.
- `yjs_sync.rs` gate (lines 69–71): `if template.visibility != "public" &&
  template.workspace_id != user_ws { forbidden }`; `readonly = template.published`.
- `resolver.rs` `StaticPrincipalResolver::resolve` extracts `email`,
  `name`/`preferred_username`; **drops `picture`**.
- `job_templates.rs` lines 273, 441: `let created_by = user.subject.clone()` (raw
  OIDC string; cannot join `user_profiles`).
- `build_public_openapi_router()` at `lib.rs:172` is the seam for unauthenticated
  routes that still appear in the OpenAPI spec.
- ~71 `require_role`/`require_member` call sites (Phase 3 converts the
  folder/template/instance gates; the rest stay).
