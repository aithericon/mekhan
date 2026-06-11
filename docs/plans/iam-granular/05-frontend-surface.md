# Phase 5 — Frontend Surface (deep-dive)

Rebuilt member page, one reusable `ShareDialog`, authorship chips, a workspace
access view, and effective-role gating. Pure `app/`; **hard-blocked on Phases 1–4's
OpenAPI regen** — every `components['schemas'][...]` alias is produced by the backend
phases, so this lands in a combined PR or strictly after the regen (else
`ci::openapi-drift` fails). Verify with `(cd app && npx svelte-check)`, NOT the LSP
popup (per CLAUDE.md stale-LSP warning).

## 1. API surface consumed

- `POST /api/v1/users/profiles` (Phase 1) — batch profile cache.
- `GET /api/v1/{folders|templates|instances}/{id}/grants` (Phase 3) → `Vec<GrantView>`
  with `source`/`inherited_from_folder_id`/`inherited_from_folder_path`.
- `PUT`/`DELETE /api/v1/{...}/{id}/grants/{user_id}` (Phase 3).
- `my_effective_role` embedded on template/instance/folder DTOs **per-row on lists**
  (Phase 3 batch resolver) — load-bearing for per-row gating without N+1.
- `PATCH /api/v1/workspaces/{id}/members/{user_id} {role}` (Phase 3).
- `WorkspaceMember`/`RosterMemberSummary` gain `avatar_url` (Phase 1).
- Template/Instance/Folder DTOs gain `updated_by`/`updated_at` (Phase 2).
- Invite endpoints (Phase 4).

## 2. Files

- `app/src/lib/api/iam.ts` (NEW, mirrors `roster.ts`) — `listGrants`, `putGrant`,
  `deleteGrant`, `listInvites`, `createInvite`, `revokeInvite`, `acceptInvite`,
  `updateMemberRole`.
- `app/src/lib/components/iam/AuthorshipChips.svelte` (NEW) — `{createdBy, createdAt,
  updatedBy, updatedAt}` → "Created by `<UserChip sm>` · <relative>"; "Updated by …"
  only when it differs. Add a `timeAgo` util in `$lib/utils` (confirm none exists —
  instance layout only has absolute `formatDate`).
- `app/src/lib/components/iam/ShareDialog.svelte` (NEW) — ONE component
  parameterized by `objectType ∈ {folder,template,instance}` (resist a per-type trio
  / generic ACL framework). Grant list (UserChip + role select + remove);
  `source==='folder'|'workspace'` rows show "inherited from `<path>`" + an Override
  control; add-grant form (email → resolve → PUT). All mutation controls disabled
  when `myEffectiveRole < admin`. **Inherited-downgrade warning (folds review M3):**
  when setting a more-specific role LOWER than an inherited higher role, show "this
  will downgrade inherited access for everyone in this subtree, not just add" — so an
  admin doesn't think they've locked someone out (workspace admins are bypass-safe).
  **Grants are members-only** — inviting non-members is the workspace-level invite
  flow (decision 9; server enforces, FE surfaces the constraint). Handle 400/409 from
  the server members-only / no-escalation check gracefully.
- `app/src/routes/workspaces/[id]/+page.svelte` — rebuild Members card: replace
  `font-mono {m.user_id}` → `<UserChip>`; inline role `<select>` per row →
  `updateMemberRole` (PATCH); **disabled for the last remaining owner** with tooltip
  (compute `ownerCount`); pending-invite rows below with Pending badge +
  Resend/Revoke; gate all add/role/remove on `isWorkspaceAdmin`. Handle 409/422 from
  a server-side last-owner race gracefully (the guard is client UX; the server is
  authoritative).
- `app/src/routes/workspaces/[id]/access/+page.svelte` (or a Members|Access PageTabs
  row) — role legend + roster with effective roles + pending-invite management. Lean:
  a denser admin view of the same member+invite data + legend. Avoid route sprawl.
- Object detail pages: `folders/+page.svelte` (Share in the per-row `actions` snippet
  + AuthorshipChips), `templates/[id]/+page.svelte` (Share + AuthorshipChips; disable
  Edit/IDE/Delete when `my_effective_role < editor`),
  `instances/[id]/+layout.svelte` (Share + upgrade the muted "created" line to
  UserChip; gate Cancel `≥ editor`).
- List views (`templates/+page.svelte`, instances list): muted AuthorshipChips per
  row, one batch `profiles.ensure()` over all visible `author_id`/`created_by`; hide
  per-row Share/Delete on insufficient `my_effective_role`.
- **Yjs live-downgrade UX (folds review M3/Yjs flag):** re-read `my_effective_role`
  after a grant change and disable the editor surface — do NOT rely on the WS
  silently rejecting writes (the backend enforces; the FE must not show stale edit
  affordances). `my_effective_role` is fetched once at mount, so it must be
  re-fetched on grant mutation.

## 3. Effective-role gating signal

- `auth.isWorkspaceAdmin` = the workspace-scope gate; `my_effective_role` = the
  object-scope gate. Workspace Admin/Owner implies full object access (bypass), so UI
  may short-circuit `auth.isWorkspaceAdmin || objRole >= needed`.
- Per-row gating MUST read the per-row `my_effective_role` from the list DTO (Phase 3
  batch resolver) — not a separate call per row (folds review N+1).

## 4. UserChip / ProfileCache (shared with Phase 1)

`UserChip` accepts BOTH a denormalized `profile` prop (member/grant/roster rows,
where the backend embeds `member_display_name`/`email`/`avatar_url`) and a
userId-only path (scattered `created_by`/`author_id` UUIDs → cache). This split
prevents double-fetch and is the single divergence-proof identity primitive.

## 5. Tests

- vitest: ProfileCache coalescing/dedup/negative-cache (re-asserted at the consuming
  layer); UserChip denormalized vs userId-only; ShareDialog inherited-row Override
  calls `putGrant`; mutation controls disabled for viewer; **inherited-downgrade
  warning renders when setting a lower-than-inherited role**; last-owner guard (one
  owner → select-away + remove disabled w/ tooltip; two owners → enabled);
  AuthorshipChips created-only vs both-lines; `timeAgo`.
- Playwright `iam-share.test.ts` (dev_noop, needs a 2nd seeded member): add a grant
  by email; inherited grant + override on a child template; Viewer-effective object
  hides Share.
- Playwright `iam-invite.test.ts`: invite → Pending badge; accept link in dev_noop
  stub → land in workspace; revoke removes.
- `svelte-check 0/0` + vitest green.

## 6. Risks

- OpenAPI ordering: hard-blocked on Phases 1–4 regen — combined PR or strict
  land-after. Stub local interfaces ONLY as a last resort, replace with generated
  aliases before merge.
- Denormalize-vs-cache split-brain: UserChip MUST accept both paths (above).
- Yjs editor gate: re-read `my_effective_role` on grant change (above).
- Avatar null until the IdP `picture` claim is captured (Phase 1) — degrade to
  initials, don't block chip work.
- Last-owner guard is client UX; server is authoritative — handle the race response.
- dev_noop invite stub + email-send must have a working offline path (Phase 4 Log
  sender + Noop provisioner) or e2e/dogfooding breaks.
- Over-engineering: ShareDialog is ONE component parameterized by 3 types; the access
  view reuses member+invite data; no generic ACL framework.

## 7. Open questions

- Denormalize on member/grant DTOs (recommended, roster pattern) vs. always-cache —
  resolved: embed on member/grant/roster rows, cache for scattered UUIDs.
- `/access` as a separate route vs. a richer Members card + small legend section —
  lean toward the latter unless the out-of-folder-grants audit is wanted.
- Confirm no `timeAgo` util exists before adding to `$lib/utils`.
- Avatar storage: Zitadel picture URL (external) vs. proxied — affects CSP/img
  handling (Phase 1 uses `referrerpolicy=no-referrer`).
