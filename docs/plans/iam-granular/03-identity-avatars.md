# Phase 1 — Identity Seam (keystone, deep-dive)

Turn any `subject_as_uuid()` value into a renderable `{display_name, email,
avatar_url}`; capture the dropped OIDC `picture` claim; ship the
`UserChip`/`Avatar`/profile-cache primitives every later phase renders through. This
area is a **dependency of, not dependent on**, the others — it lands first.

## 1. Migration — `service/migrations/20240169000000_user_profiles_avatar.sql`

```sql
ALTER TABLE user_profiles ADD COLUMN avatar_url TEXT;  -- nullable, no default, no backfill
```

`user_profiles` (migr 20240162) has PK `user_id`, `email`, `display_name`,
`updated_at` — no avatar column today. Seeds dev user
`3bb26085-29f3-5fbf-8a8c-a2e485a1f55b`; `avatar_url` stays NULL → initials "DU".

## 2. Backend

### 2.1 `AuthUser` — `service/src/auth/model.rs`

- Add `#[serde(default, skip_serializing_if = "Option::is_none")] pub avatar_url:
  Option<String>`.
- **`user_id` is a serialize-only DERIVED field, NOT a constructor field (folds
  review M8).** A plain `#[serde(default)] user_id: Option<Uuid>` would default to
  `None` at every construction site (resolver, dev, runner_token, worker_token,
  every test fixture) and serialize as `null` in `GET /api/auth/session` — silently
  breaking the SPA profile-cache seed. Instead implement a custom `Serialize` (or a
  `#[serde(serialize_with)]` shim / a flatten-injected getter) that always emits
  `user_id = subject_as_uuid()`. This means **no `AuthUser { .. }` literal needs to
  set `user_id`**, it is never `null`, and the namespace constant is never
  duplicated in JS. `subject_as_uuid()` itself is unchanged.

### 2.2 Resolver — `service/src/auth/resolver.rs`

In `StaticPrincipalResolver::resolve` (verified: extracts `email`,
`name`/`preferred_username`, **drops `picture`**), add:
```rust
let avatar_url = string_claim(&claims, "picture");
```
and set it on the returned `AuthUser`. `DbPrincipalResolver` mutates in place → rides
through.

### 2.3 Extractor upsert — `service/src/auth/extractor.rs`

Extend `upsert_user_profile`: add `avatar_url` to the INSERT column list + `EXCLUDED`
set; widen the `IS DISTINCT FROM` guard with `OR user_profiles.avatar_url IS DISTINCT
FROM EXCLUDED.avatar_url`; bind `user.avatar_url.as_deref()`. Keep the early-return
when email AND display_name are both None.

### 2.4 Batch resolver — `service/src/handlers/users.rs`

NEW `resolve_profiles` handler beside the existing `resolve_user_by_email`:
- `BatchProfilesRequest { ids: Vec<Uuid> }` — cap 256 (→ 400 over cap; confirm cap
  vs. largest realistic roster).
- `UserProfileDto { user_id, display_name, email, avatar_url }`.
- `SELECT user_id, display_name, email, avatar_url FROM user_profiles WHERE user_id =
  ANY($1)`; unknown UUIDs omitted (never 404 the batch).
- Auth: any authenticated member (`_user: AuthUser`), mirroring
  `resolve_user_by_email`'s posture.

### 2.5 Wiring

- `service/src/lib.rs` — `.routes(routes!(handlers::users::resolve_profiles))` next
  to `resolve_user_by_email` (~line 680, protected router).
- `service/src/openapi.rs` — register `UserProfileDto` explicitly in `schemas()`.
  `AuthUser` (registered ~line 139) picks up `avatar_url` (and the serialized
  `user_id`) automatically.
- `service/src/handlers/workspaces.rs` + `roster.rs` — add `up.avatar_url` to the
  existing `LEFT JOIN user_profiles` SELECTs + an `avatar_url: Option<String>` field
  on `WorkspaceMember`/`RosterMemberSummary`. **Decision: denormalize on
  member/grant/roster rows (parity with the already-joined `display_name`/`email`),
  use the cache for scattered authorship UUIDs.**
- `service/src/auth/dev.rs` — no change; dev `picture` stays absent → `avatar_url`
  NULL → "DU" initials (dogfoods the common no-picture path).

## 3. Frontend

- `app/src/lib/components/iam/Avatar.svelte` — thin wrapper over **bits-ui v2 Avatar**
  primitive. Initials: up-to-two display_name words → email local-part → 2 hex of
  UUID; deterministic bg color hashed from `user_id`; `onerror` → fallback;
  `referrerpolicy="no-referrer"` (SSRF/referrer hygiene). **`pnpm install` first** —
  node_modules absent in worktree; confirm the bits-ui v2 Avatar import path.
- `app/src/lib/components/iam/UserChip.svelte` — props `{userId?, profile?, size?,
  showEmail?}`. Given a denormalized `profile`, render directly; else
  `profiles.ensure([userId])` in `$effect` + reactive `profiles.get(userId)` with a
  UUID-skeleton fallback. Tooltip: full name + email + raw UUID. **Accepts BOTH a
  denormalized-profile prop and a userId-only path** so the denormalize-vs-cache
  split can't double-fetch.
- `app/src/lib/stores/profiles.svelte.ts` — rune cache keyed by UUID. `ensure(ids)`
  coalesces unknown ids into ONE `POST /api/v1/users/profiles` per microtask;
  `get(id)` synchronous reactive read; dedups in-flight via `#inflight`;
  negative-caches misses; seeds from `auth.session.user` (now has `user_id`).
- `app/src/lib/api/client.ts` (or `iam.ts`) — `resolveProfiles(ids)` wrapper +
  `UserProfileDto` type alias.
- Retrofit raw-UUID sites: `app/src/routes/workspaces/[id]/+page.svelte:162`
  (`font-mono {m.user_id}` → `<UserChip>`); `fleet/PoolMembersHumans.svelte` +
  `HumanEnrollSheet.svelte` (absorb the hand-rolled `display_name ?? email ??
  short(uuid)`); `tasks/inbox/+page.svelte`.
- `app/src/lib/auth/store.svelte.ts` — `SessionUserDto` + `toUser` carry `avatarUrl`
  + `userId`.

## 4. OpenAPI

Two triggers: `AuthUser` gains `avatar_url` + serialized `user_id`; new
`POST /users/profiles` + DTOs. Run `just dev::openapi`, commit `openapi-mekhan.json`
+ `schema.d.ts`.

## 5. Tests

- Unit (`resolver.rs`): claims with `picture` → `avatar_url == Some(url)`; without →
  None.
- Unit: `AuthUser` serializes `user_id == subject_as_uuid()` always (even for a
  default/dev construction) — guards M8.
- Integration (live): `POST /users/profiles` returns dev-user "Dev User"; omits
  unknown UUIDs (no 404); >256 → 400; requires auth (401 without cookie); migration
  applies on fresh DB; dev row has NULL avatar.
- vitest: cache coalesces N `get()` into ONE POST; dedups concurrent overlapping
  `ensure`; negative-caches; UserChip denormalized vs userId-only paths; initials
  derivation + stable color; AvatarImage onerror → initials.
- Playwright: member list shows "Dev User" + avatar, not a raw UUID; tooltip exposes
  email + UUID.

## 6. Risks

- **`AuthUser` shape change is wide-blast.** `#[serde(default)]` keeps old session
  JSON deserializing; the serialize-only `user_id` avoids literal edits, but
  `avatar_url` is still a new field — grep all `AuthUser {` literals (resolver, dev,
  runner_token.rs, worker_token.rs, every `#[cfg(test)]` fixture in `service/src`
  AND `service/tests/`) before declaring green. `--lib` won't catch `service/tests/`
  (the exact prior failure mode in memory).
- Privacy: any authenticated member can resolve any UUID's identity
  (cross-workspace enumeration), consistent with existing `resolve_user_by_email`.
  Filtering to co-members is a one-join product decision — default workspace-wide v1.
- Avatar SSRF/CSP: external IdP URL; `no-referrer` + CSP must allow the host or
  images silently fail (initials mask it). Stale-name eventual consistency
  (refreshes on the named user's next request) — acceptable, documented.
- node_modules absent in worktree — confirm bits-ui v2 Avatar export path.
- Migration `169` is the first slot; sccache `migrate!`-dir miss → forced rebuild.
