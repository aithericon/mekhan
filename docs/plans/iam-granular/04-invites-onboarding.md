# Phase 4 — Invites / Onboarding (deep-dive)

Full pending-invite lifecycle: an Admin/Owner invites by email (optionally
pre-seeding workspace role + object grants), email is sent (dev = log), a public
accept-link page creates the user in Zitadel on accept, and grants apply atomically.
Hard-depends on Phase 3 (`object_grants` + `apply_grant` + `effective_object_role`).

## 1. Migration — `service/migrations/20240172000000_pending_invites.sql`

Numbered AFTER ACL (`171`) because `invite_object_grants` mirrors the `object_grants`
shape and accept writes into `object_grants`.

```sql
CREATE TABLE pending_invites (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  workspace_id UUID NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
  email TEXT NOT NULL,                       -- normalized lower-case
  role TEXT NOT NULL CHECK (role IN ('owner','admin','editor','viewer')),
  token_hash BYTEA NOT NULL,                 -- SHA-256; raw token NEVER stored
  invited_by UUID NOT NULL,
  created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  expires_at TIMESTAMPTZ NOT NULL,
  accepted_at TIMESTAMPTZ NULL,
  accepted_user_id UUID NULL,
  revoked_at TIMESTAMPTZ NULL,
  status TEXT NOT NULL DEFAULT 'pending'
    CHECK (status IN ('pending','accepted','revoked','expired'))
);
CREATE UNIQUE INDEX pending_invites_token_hash_uniq ON pending_invites(token_hash);
-- one live invite per email per workspace (resend rotates the row); mirrors
-- roster_members live-unique partial-index (migr 20240163).
CREATE UNIQUE INDEX pending_invites_active_email_uniq
  ON pending_invites(workspace_id, lower(email)) WHERE status='pending';
CREATE INDEX idx_pending_invites_ws ON pending_invites(workspace_id, status);

CREATE TABLE invite_object_grants (
  invite_id UUID NOT NULL REFERENCES pending_invites(id) ON DELETE CASCADE,
  object_type TEXT NOT NULL CHECK (object_type IN ('folder','template','instance')),
  object_id UUID NOT NULL,
  role TEXT NOT NULL CHECK (role IN ('owner','admin','editor','viewer')),
  PRIMARY KEY (invite_id, object_type, object_id)
);
```

## 2. API

- `POST /api/v1/workspaces/{id}/invites` — Admin-gated
  (`require_role(..., Role::Admin)`). `CreateInviteRequest { email, role,
  object_grants: Option<Vec<InviteObjectGrantSpec>> }`. Validates role; each grant
  object in workspace `id` (cross-workspace → 400); caller's effective role on each
  object ≥ granted role OR ws Admin/Owner bypass. 201 `InviteSummary` (NEVER returns
  the raw token). Duplicate active → rotate + resend, 200.
- `GET /api/v1/workspaces/{id}/invites` — Admin-gated list (`InviteSummary` carries
  `invited_by` + `invited_by_display_name`).
- `POST .../invites/{invite_id}/resend` — Admin; rotate token + expiry + re-send, 200
  (old token hash overwritten → leaked old link dies).
- `DELETE .../invites/{invite_id}` — Admin; revoke. 204; accepted → 409;
  already-revoked → 204 (idempotent).
- `GET /api/v1/invites/{token}/preview` — **PUBLIC**. `InvitePreview {
  workspace_display_name, email, role, status, expires_at }`. Generic 404 for
  unknown/expired/revoked/accepted (no enumeration). Rate-limited.
- `POST /api/v1/invites/{token}/accept` — **PUBLIC**. See §3.

## 3. Accept flow

Server hashes token → `SELECT ... FOR UPDATE` a `status='pending' AND
expires_at > NOW()` row inside a tx (single-use atomicity). Then:

1. `resolve_subject_by_email(email)` → `Some` reuses the existing sub (re-invite),
   `None` → `create_human_user` → new sub. **Provisioner call FIRST** (idempotent on
   retry via resolve-by-email), THEN the db tx — guards orphan-on-tx-failure.
2. `subject_as_uuid(sub)` — **the REAL resolved sub→uuid, never a synthetic one**
   (folds review M4), so a later real login maps to the same membership/grants.
3. SAME tx: upsert `workspace_members`; `invite_object_grants → object_grants` via
   `apply_grant` keyed by that user_id; mark `accepted_at`/`accepted_user_id`/
   `status='accepted'`.
4. Commit. Returns `AcceptInviteResponse { workspace_id, requires_login }`
   (`requires_login=true` → the SPA redirects to `/api/auth/login`; the new Zitadel
   user completes IdP login — mekhan does not mint their session). dev_noop:
   `requires_login=false`.

## 4. Backend

- `service/src/auth/mgmt.rs` — EXTEND `ZitadelMgmt` with `create_human_user(email,
  display_name) -> Result<String, MgmtError>` (POST `/management/v1/users/human`;
  derive givenName/familyName from display_name/email local-part; the existing
  ORG_OWNER grant authorizes it — verify in gated e2e). Reuse `resolve_subject_by_email`
  for the "already exists" branch.
- `service/src/auth/provisioner.rs` (NEW, or in mgmt.rs) — `trait UserProvisioner {
  async fn provision_or_resolve(email, display_name) -> Result<(String /*sub*/, bool
  /*newly_created*/), MgmtError>; }`. Impls: `ZitadelMgmt` (real) +
  `NoopUserProvisioner` (dev — deterministic synthetic sub `dev-invite-{slug(email)}`
  so `subject_as_uuid` is stable and rows are real).
- **Fail-closed + loud provisioner selection (folds review M4):**
  `build_user_provisioner(&config)` returns `ZitadelMgmt` for ANY Bff/auth mode and
  `NoopUserProvisioner` ONLY when auth mode == `dev_noop`. **Boot-time invariant:
  panic if `auth != dev_noop && provisioner is Noop`** (add a startup-invariant test).
  `AppState` carries `Option<Arc<dyn UserProvisioner>>`; `None` → accept 503 (logged
  error). Under dev_noop the Noop is the DEFAULT (Some) so accept never 503s offline.
- `service/src/notify/email.rs` (NEW) — `trait EmailSender { async fn
  send_invite(to, accept_url, workspace_name, inviter_name) -> Result<(),
  EmailError>; }`. `LogEmailSender` (default + dev: `tracing::info!` the accept URL —
  the dev_noop link path) + optional `SmtpEmailSender` (`lettre`, config-gated). No
  provider SDK hardcoded. `LogEmailSender` is the DEFAULT (Some) so offline e2e works.
- `service/src/config.rs` — `EmailConfig { mode: log|smtp, from_address, smtp_*,
  public_base_url, invite_ttl_secs (default 7d) }`. Accept link =
  `{public_base_url}/invite/accept?token=...`.
- `service/src/lib.rs` — `AppState` gains `email: Arc<dyn EmailSender>` +
  `user_provisioner`. Register the 3 Admin invite routes in the workspaces block.
  **Register the 2 PUBLIC token endpoints in `build_public_openapi_router()`**
  (verified seam at `lib.rs:172`) — NOT a raw merged router (folds review M9: a raw
  `auth_router`/`webhook_router`-style merge does NOT put endpoints into the OpenAPI
  spec, so `acceptInvite`/`previewInvite` typed wrappers wouldn't generate and
  `openapi-drift` wouldn't catch it). `build_public_openapi_router` is the seam where
  unauthenticated handlers (`/healthz`, runner/worker-enroll) enter the spec AND stay
  outside `require_auth_middleware`. Document as the auth-bootstrap exception
  (mirroring `/api/auth/*`).
- `service/src/main.rs` — `build_email_sender(&config)` + `build_user_provisioner`
  (Bff → ZitadelMgmt; dev_noop → Noop); add both to the `AppState` literal.
- `service/src/handlers/invites.rs` (NEW) — 5 handlers. create: per object_grant
  resolve caller's effective role via `effective_object_role` (Phase 3);
  admins/owners bypass. accept: provisioner call first, then the tx; `apply_grant`
  per `invite_object_grant`.
- `service/src/models/invite.rs` (NEW) — `CreateInviteRequest`,
  `InviteObjectGrantSpec`, `InviteSummary`, `InvitePreview`, `AcceptInviteResponse`.
  Register in openapi.

## 5. Frontend

- `app/src/routes/invite/accept/+page.svelte` (NEW, PUBLIC) — reads `?token`, GET
  preview, Accept POSTs accept; `requires_login` → `/api/auth/login?return_to=/`;
  404 → generic "no longer valid". **Add `/invite/*` to the SPA auth-redirect
  allowlist** (else a logged-out invitee bounces to login).
- `app/src/lib/api/invites.ts` — typed wrappers.
- Workspace member page: "Invite member" affordance (email + role + optional grant
  picker) gated on `isWorkspaceAdmin`; pending-invite rows with "Pending" badge +
  Resend/Revoke.

## 6. Security

- Token: 32-byte CSPRNG (`getrandom`) → base64url (~43 chars); SHA-256 hash stored;
  raw token never serialized (no `token` field on any DTO). 32 bytes makes brute
  force infeasible regardless of timing (folds review minor).
- **Concrete rate-limit (folds review minor):** per-IP token-bucket
  (`tower-governor`, no existing middleware does this) on the two PUBLIC invite
  routes + a per-token attempt ceiling.
- Lookup is a single indexed equality on `token_hash` (`pending_invites_token_hash_uniq`).
  The generic 404 for unknown/expired/revoked/accepted shares ONE code path (no
  early-return timing divergence) — same body, same path.
- `create_human_user` may trigger Zitadel's own init/verify mail — decide canonical
  sender: Zitadel owns credential-set mail, our mail owns the workspace-context
  accept-link.
- Abuse: rate-limit invite creation per workspace + a per-workspace pending-invite cap.

## 7. Tests

- Unit (wiremock): `create_human_user` body + userId parse; resolve-reuse branch;
  token entropy/hashing; NoopProvisioner determinism.
- **Boot-invariant test (folds M4):** `auth != dev_noop && provisioner is Noop` →
  panic at boot; under dev_noop `state.user_provisioner.is_some()` and
  `state.email` is `LogEmailSender`.
- Integration (live): create as Admin → 201, as Editor → 403, cross-workspace grant
  → 400, grant role > caller → 403. Accept happy path (dev_noop) → member + grants +
  `status='accepted'`; second accept → 404; after expiry/revoke → 404. Resend
  rotates (old token 404s). Re-invite existing email to a 2nd workspace → reuse sub,
  no dup Zitadel user, both memberships.
- Gated live-Zitadel (`MEKHAN_E2E_ZITADEL=1`): create→log→accept creating a REAL
  human user (proves ORG_OWNER authorizes `POST /users/human`).
- Playwright: admin invites → Pending badge; revoke removes; dev_noop copy-logged
  link → accept → member appears.
- Security: preview/accept rate-limited; unknown/expired/revoked/accepted all return
  identical generic 404.

## 8. Risks / open questions

- Hard-depends on Phase 3 (`object_grants` + `apply_grant` + `effective_object_role`).
  Migration `172` must exceed `171`.
- Zitadel double-email — pick the canonical sender.
- Accept spans an external Zitadel call + a local tx — provision FIRST (idempotent),
  then tx; `SELECT...FOR UPDATE` guards double-apply.
- dev_noop synthetic sub never matches a real login — throwaway; tests assert DB
  rows, not a second session.
- Open: temp-password vs. Zitadel init-user email (one-vs-two emails);
  token-possession vs. login-required for re-invites of existing users (recommend
  token-possession v1); grant-picker UX now vs. membership-only v1.
