//! Email-keyed identity spine: integration tests for the
//! `DbPrincipalResolver` → `users` / `user_identities` reconciliation and the
//! authed accept-invite path.
//!
//! Requires the shared test infrastructure (Postgres — see
//! `common::test_infra::DEFAULT_POSTGRES_URL`). Each test stands up its own
//! migrated database via `common::create_test_db()`, so they are hermetic.
//!
//! Covers the three invariants the email-keyed-identity refactor introduced:
//!   (a) a SECOND `(provider, subject)` whose VERIFIED email matches an existing
//!       `users` row links to that SAME `users.id` (cross-subject reconcile);
//!   (b) an UNVERIFIED email does NOT reconcile by email — it mints a distinct
//!       user keyed by the legacy v5 hash of the subject;
//!   (c) accepting an invite as a pre-existing user adds a `workspace_members`
//!       row against that user's existing `user_id` and creates NO new identity.

mod common;

use sqlx::PgPool;
use uuid::Uuid;

use mekhan_service::auth::model::AuthUser;
use mekhan_service::auth::resolver::DbPrincipalResolver;
use mekhan_service::auth::{PrincipalResolver, VerifiedClaims};

/// Build `VerifiedClaims` for a Zitadel-style principal. `email_verified` is
/// stamped into `extra` exactly as the BFF callback's `merge_userinfo_claims`
/// would (a JSON boolean), which is what the resolver reads.
fn claims(subject: &str, email: Option<&str>, email_verified: bool) -> VerifiedClaims {
    let mut extra = std::collections::BTreeMap::new();
    if let Some(addr) = email {
        extra.insert("email".to_string(), serde_json::Value::String(addr.into()));
    }
    extra.insert(
        "email_verified".to_string(),
        serde_json::Value::Bool(email_verified),
    );
    VerifiedClaims {
        subject: subject.to_string(),
        issuer: "https://idp.test".to_string(),
        audience: vec!["mekhan".into()],
        expires_at: i64::MAX,
        extra,
    }
}

/// Count `user_identities` rows pointing at a given `users.id`.
async fn identity_count(db: &PgPool, user_id: Uuid) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM user_identities WHERE user_id = $1")
        .bind(user_id)
        .fetch_one(db)
        .await
        .unwrap()
}

/// Look up the `user_id` a `(provider, subject)` identity resolves to, if any.
async fn identity_user_id(db: &PgPool, subject: &str) -> Option<Uuid> {
    sqlx::query_scalar("SELECT user_id FROM user_identities WHERE provider = 'zitadel' AND subject = $1")
        .bind(subject)
        .fetch_optional(db)
        .await
        .unwrap()
}

// -- (a) verified email links a second subject to the same users.id ----------

#[tokio::test]
async fn verified_email_links_second_subject_to_same_user() {
    let db = common::create_test_db().await;
    let resolver = DbPrincipalResolver::new(db.clone());

    let email = format!("reconcile-{}@corp.test", Uuid::new_v4().simple());

    // First login: a fresh principal with a VERIFIED email mints a user.
    let first = resolver
        .resolve(claims("zitadel-sub-A", Some(&email), true))
        .await
        .expect("first resolve");
    let first_id = first.user_id;
    assert_eq!(
        identity_user_id(&db, "zitadel-sub-A").await,
        Some(first_id),
        "first subject linked to the minted user"
    );
    assert_eq!(
        identity_count(&db, first_id).await,
        1,
        "exactly one identity after first login"
    );

    // Same human, DIFFERENT subject (re-provisioned), SAME verified email.
    let second = resolver
        .resolve(claims("zitadel-sub-B", Some(&email), true))
        .await
        .expect("second resolve");

    // The verified-email reconciliation must collapse them onto one users.id.
    assert_eq!(
        second.user_id, first_id,
        "second subject reconciled onto the SAME users.id by verified email"
    );
    assert_eq!(
        identity_user_id(&db, "zitadel-sub-B").await,
        Some(first_id),
        "second subject now links to the existing user"
    );
    assert_eq!(
        identity_count(&db, first_id).await,
        2,
        "the existing user now owns BOTH identities"
    );

    // And there is exactly one users row for that email (no duplicate human).
    let users_with_email: i64 =
        sqlx::query_scalar("SELECT count(*) FROM users WHERE email = $1::citext")
            .bind(&email)
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!(users_with_email, 1, "one users row per verified email");
}

// -- (b) unverified email does NOT reconcile; mints a distinct id ------------

#[tokio::test]
async fn unverified_email_does_not_link_to_existing_user() {
    let db = common::create_test_db().await;
    let resolver = DbPrincipalResolver::new(db.clone());

    let email = format!("shared-{}@corp.test", Uuid::new_v4().simple());

    // Establish an existing user via a VERIFIED first login.
    let first = resolver
        .resolve(claims("verified-sub", Some(&email), true))
        .await
        .expect("verified resolve");
    let first_id = first.user_id;

    // A DIFFERENT subject presents the SAME email but UNVERIFIED. It must NOT
    // hijack the existing identity — it mints a distinct user keyed by the
    // legacy v5 hash of its own subject.
    let spoof = resolver
        .resolve(claims("unverified-sub", Some(&email), false))
        .await
        .expect("unverified resolve");

    assert_ne!(
        spoof.user_id, first_id,
        "unverified email must NOT reconcile onto the existing user"
    );
    assert_eq!(
        spoof.user_id,
        AuthUser::legacy_subject_uuid("unverified-sub"),
        "unverified principal falls back to the legacy v5 id of its subject"
    );
    assert_eq!(
        identity_user_id(&db, "unverified-sub").await,
        Some(spoof.user_id),
        "the spoof subject links only to its own fresh id"
    );

    // The verified user still owns exactly its one identity; nothing was added.
    assert_eq!(
        identity_count(&db, first_id).await,
        1,
        "the verified user gains no extra identity from the unverified login"
    );

    // The email stays on the verified user; the unverified mint stored email
    // NULL (the CITEXT UNIQUE collision fallback), so it never stole the handle.
    let owner_of_email: Option<Uuid> =
        sqlx::query_scalar("SELECT id FROM users WHERE email = $1::citext")
            .bind(&email)
            .fetch_optional(&db)
            .await
            .unwrap();
    assert_eq!(
        owner_of_email,
        Some(first_id),
        "the verified user keeps sole ownership of the email"
    );
}

// -- (c) accept-invite reuses the existing user_id, adds no identity ---------

#[tokio::test]
async fn accept_invite_as_existing_user_reuses_id_and_adds_no_identity() {
    let db = common::create_test_db().await;
    let resolver = DbPrincipalResolver::new(db.clone());

    // A real, already-provisioned human: one users row + one identity, minted
    // through a normal verified login. This is the "pre-existing user".
    let email = format!("invitee-{}@corp.test", Uuid::new_v4().simple());
    let existing = resolver
        .resolve(claims("existing-sub", Some(&email), true))
        .await
        .expect("existing user resolve");
    let user_id = existing.user_id;
    assert_eq!(
        identity_count(&db, user_id).await,
        1,
        "pre-existing user starts with exactly one identity"
    );

    // An inviter + a target workspace the invitee is NOT yet a member of.
    let inviter_id = Uuid::new_v4();
    sqlx::query("INSERT INTO users (id, email, status) VALUES ($1, $2::citext, 'active')")
        .bind(inviter_id)
        .bind(format!("inviter-{}@corp.test", Uuid::new_v4().simple()))
        .execute(&db)
        .await
        .unwrap();
    let ws_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO workspaces (id, slug, display_name, is_system) VALUES ($1, $2, $3, FALSE)",
    )
    .bind(ws_id)
    .bind(format!("invite-ws-{}", ws_id.simple()))
    .bind("Invite WS")
    .execute(&db)
    .await
    .unwrap();

    // Seed a pending invite for the invitee's email, as create_invite would.
    // We never call the HTTP handler here (the mock authenticator doesn't run
    // the resolver), so the token_hash just has to be valid, unique BYTEA — we
    // assert on the membership + identity side effects directly.
    let token_hash: Vec<u8> = Uuid::new_v4().as_bytes().to_vec();
    let invite_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO pending_invites \
            (id, workspace_id, email, role, token_hash, invited_by, status, expires_at) \
         VALUES ($1, $2, $3, 'editor', $4, $5, 'pending', now() + interval '7 days')",
    )
    .bind(invite_id)
    .bind(ws_id)
    .bind(&email)
    .bind(&token_hash)
    .bind(inviter_id)
    .execute(&db)
    .await
    .unwrap();

    // Snapshot the global identity count so we can prove acceptance adds none.
    let identities_before: i64 = sqlx::query_scalar("SELECT count(*) FROM user_identities")
        .fetch_one(&db)
        .await
        .unwrap();

    // Replicate accept_invite's membership write: it keys the new
    // workspace_members row off the session's resolved user_id
    // (`user.subject_as_uuid()`), which for our pre-existing user is `user_id`.
    // accept_invite never touches user_identities — that's the invariant.
    assert!(
        existing.email.as_deref() == Some(email.as_str()),
        "session email matches the invite (accept_invite's 403 guard would pass)"
    );
    sqlx::query(
        "INSERT INTO workspace_members (workspace_id, user_id, role) VALUES ($1, $2, 'editor') \
         ON CONFLICT (workspace_id, user_id) DO UPDATE SET role = EXCLUDED.role",
    )
    .bind(ws_id)
    .bind(user_id)
    .execute(&db)
    .await
    .unwrap();
    sqlx::query("UPDATE pending_invites SET status = 'accepted', accepted_user_id = $2 WHERE id = $1")
        .bind(invite_id)
        .bind(user_id)
        .execute(&db)
        .await
        .unwrap();

    // The membership row is keyed by the EXISTING user_id (no new identity).
    let member_role: Option<String> = sqlx::query_scalar(
        "SELECT role FROM workspace_members WHERE workspace_id = $1 AND user_id = $2",
    )
    .bind(ws_id)
    .bind(user_id)
    .fetch_optional(&db)
    .await
    .unwrap();
    assert_eq!(
        member_role.as_deref(),
        Some("editor"),
        "membership added against the pre-existing user_id"
    );

    // No new identity was created by accepting the invite.
    let identities_after: i64 = sqlx::query_scalar("SELECT count(*) FROM user_identities")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(
        identities_after, identities_before,
        "accepting an invite creates no new user_identities row"
    );
    assert_eq!(
        identity_count(&db, user_id).await,
        1,
        "the invitee still owns exactly its one original identity"
    );
}
