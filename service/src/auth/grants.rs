//! Phase 3 — object-level authorization. Sibling to [`super::membership`];
//! reuses its [`Role`], [`MembershipError`], and `member_role`.
//!
//! A grant binds `(object_type, object_id, user_id) → role`. The **effective**
//! role of a user on an object is:
//!
//! - **Workspace Owner/Admin bypass:** if the caller's workspace role ≥ Admin,
//!   that role is returned immediately — never constrained by an object ACL.
//! - **Floor:** the workspace role is a FLOOR. The result is
//!   `max(most-specific grant, workspace_role)`. A grant can *elevate* a member
//!   above their workspace role on a specific object, and the most-specific
//!   grant can *downgrade an inherited higher grant* (a deep Viewer overrides a
//!   shallow Owner) — but it can never drop a member below their workspace role.
//! - **Most-specific wins among grant tiers:** instance grant > parent-template
//!   grant > nearest-ancestor folder grant. Folders nest via the materialized
//!   `folders.path`, so ancestry is a path-prefix match — no recursive CTE.
//!
//! Because the workspace role is a floor, a workspace member can *view* every
//! object in their workspace; grants only differentiate the role above that
//! floor. List endpoints therefore stay workspace-scoped (the leak fix for
//! instances is workspace+public scoping, which the list lacked entirely) and
//! annotate each row with the caller's effective role via
//! [`effective_object_roles`]; there is intentionally no grant-scoped list
//! filter (`accessible_object_ids`) in v1 — it would contradict the floor.
//!
//! `object_id` for a TEMPLATE is the chain-root `COALESCE(base_template_id, id)`
//! so a grant follows the whole version chain. Instances carry no workspace_id
//! and join their template by per-version `template_id`; instance resolution is
//! a two-hop join (instance → template → folder) so an object grant on a
//! *folderless* template still propagates to its instances via the template
//! tier.

use std::collections::HashMap;

use sqlx::PgPool;
use uuid::Uuid;

use super::membership::{member_role, MembershipError, Role};
use super::model::AuthUser;

/// The three granular-ACL object kinds. Maps to the `object_kind` Postgres enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectKind {
    Folder,
    Template,
    Instance,
}

impl ObjectKind {
    /// The `object_kind` enum label. Bound with an explicit `::object_kind`
    /// cast at every use site.
    pub fn as_db(self) -> &'static str {
        match self {
            ObjectKind::Folder => "folder",
            ObjectKind::Template => "template",
            ObjectKind::Instance => "instance",
        }
    }

    /// Parse the plural REST path segment (`folders|templates|instances`). The
    /// grant routes register three concrete literals so the resolver never sees
    /// an unknown kind.
    pub fn from_path_segment(s: &str) -> Option<Self> {
        match s {
            "folders" => Some(ObjectKind::Folder),
            "templates" => Some(ObjectKind::Template),
            "instances" => Some(ObjectKind::Instance),
            _ => None,
        }
    }
}

/// A reference to a grantable object: its kind + identity. For a template the
/// `id` may be any version row — the resolver collapses it to the chain root.
#[derive(Debug, Clone, Copy)]
pub struct ObjectRef {
    pub kind: ObjectKind,
    pub id: Uuid,
}

impl ObjectRef {
    pub fn folder(id: Uuid) -> Self {
        Self {
            kind: ObjectKind::Folder,
            id,
        }
    }
    pub fn template(id: Uuid) -> Self {
        Self {
            kind: ObjectKind::Template,
            id,
        }
    }
    pub fn instance(id: Uuid) -> Self {
        Self {
            kind: ObjectKind::Instance,
            id,
        }
    }
}

/// Resolved object context: the workspace it lives in, the grant chain-root id
/// (folder id / template base id), and the home-folder materialized path (None
/// for a folderless template or an instance of one).
struct ObjCtx {
    workspace_id: Uuid,
    /// For folder: the folder id. For template/instance: the template chain root.
    base_id: Uuid,
    home_path: Option<String>,
}

/// SQL fragment ranking the four role tiers by privilege. Shared so the
/// single-object and batch paths order identically.
const ROLE_RANK: &str =
    "(CASE role WHEN 'owner' THEN 3 WHEN 'admin' THEN 2 WHEN 'editor' THEN 1 ELSE 0 END)";

/// Resolve `(workspace_id, base_id, home_path)` for an object, or `None` if the
/// object row doesn't exist.
async fn resolve_ctx(db: &PgPool, obj: ObjectRef) -> Result<Option<ObjCtx>, MembershipError> {
    let row: Option<(Uuid, Uuid, Option<String>)> = match obj.kind {
        ObjectKind::Folder => sqlx::query_as(
            "SELECT workspace_id, id, path FROM folders WHERE id = $1",
        )
        .bind(obj.id)
        .fetch_optional(db)
        .await?
        .map(|(ws, id, path): (Uuid, Uuid, String)| (ws, id, Some(path))),
        ObjectKind::Template => sqlx::query_as(
            "SELECT t.workspace_id, COALESCE(t.base_template_id, t.id) AS base_id, f.path \
               FROM workflow_templates t \
               LEFT JOIN template_folders tf ON tf.base_template_id = COALESCE(t.base_template_id, t.id) \
               LEFT JOIN folders f ON f.id = tf.folder_id \
              WHERE t.id = $1",
        )
        .bind(obj.id)
        .fetch_optional(db)
        .await?,
        ObjectKind::Instance => sqlx::query_as(
            "SELECT t.workspace_id, COALESCE(t.base_template_id, t.id) AS base_id, f.path \
               FROM workflow_instances i \
               JOIN workflow_templates t ON t.id = i.template_id \
               LEFT JOIN template_folders tf ON tf.base_template_id = COALESCE(t.base_template_id, t.id) \
               LEFT JOIN folders f ON f.id = tf.folder_id \
              WHERE i.id = $1",
        )
        .bind(obj.id)
        .fetch_optional(db)
        .await?,
    };

    Ok(row.map(|(workspace_id, base_id, home_path)| ObjCtx {
        workspace_id,
        base_id,
        home_path,
    }))
}

/// Resolved grant target for the CRUD endpoints: the workspace the object
/// lives in, the id a grant is keyed on (folder id / template chain-root /
/// instance id), and the home-folder path for synthesizing inherited rows.
pub struct GrantContext {
    pub workspace_id: Uuid,
    /// The `object_grants.object_id` value for a grant on this object: the
    /// folder id, the template chain-root, or the instance id (NOT the template
    /// base for an instance).
    pub grant_object_id: Uuid,
    pub home_path: Option<String>,
}

/// Resolve the [`GrantContext`] for an object, or `None` if it doesn't exist.
pub async fn grant_context(
    db: &PgPool,
    obj: ObjectRef,
) -> Result<Option<GrantContext>, MembershipError> {
    Ok(resolve_ctx(db, obj).await?.map(|ctx| GrantContext {
        workspace_id: ctx.workspace_id,
        grant_object_id: match obj.kind {
            // An instance grant keys on the instance itself; the template base
            // is only the inheritance hop.
            ObjectKind::Instance => obj.id,
            _ => ctx.base_id,
        },
        home_path: ctx.home_path,
    }))
}

/// The most-specific grant role for one already-resolved object context, or
/// `None` if the user has no grant on it (caller applies the workspace floor).
async fn best_grant_role(
    db: &PgPool,
    kind: ObjectKind,
    ctx: &ObjCtx,
    instance_id: Uuid,
    uid: Uuid,
) -> Result<Option<Role>, MembershipError> {
    // Folder-ancestry tier (rank 2): any folder grant whose path is a prefix of
    // (or equal to) the object's home path; deeper path = more specific. Shared
    // by all three kinds. For a folder object, home_path IS the folder's own
    // path, so its self-grant participates as the deepest match.
    let folder_tier = "SELECT g.role, 2 AS source_rank, length(f.path) AS depth \
           FROM object_grants g JOIN folders f ON f.id = g.object_id \
          WHERE g.object_type = 'folder'::object_kind AND g.user_id = $1 \
            AND $2::text IS NOT NULL AND ($2 = f.path OR $2 LIKE f.path || '/%')";

    let sql = match kind {
        ObjectKind::Folder => format!(
            "SELECT role FROM ( {folder_tier} ) s \
              ORDER BY source_rank DESC, depth DESC, {ROLE_RANK} DESC LIMIT 1"
        ),
        ObjectKind::Template => format!(
            "SELECT role FROM ( \
               SELECT role, 3 AS source_rank, 0 AS depth FROM object_grants \
                WHERE object_type = 'template'::object_kind AND object_id = $3 AND user_id = $1 \
               UNION ALL {folder_tier} \
             ) s ORDER BY source_rank DESC, depth DESC, {ROLE_RANK} DESC LIMIT 1"
        ),
        ObjectKind::Instance => format!(
            "SELECT role FROM ( \
               SELECT role, 4 AS source_rank, 0 AS depth FROM object_grants \
                WHERE object_type = 'instance'::object_kind AND object_id = $4 AND user_id = $1 \
               UNION ALL \
               SELECT role, 3 AS source_rank, 0 AS depth FROM object_grants \
                WHERE object_type = 'template'::object_kind AND object_id = $3 AND user_id = $1 \
               UNION ALL {folder_tier} \
             ) s ORDER BY source_rank DESC, depth DESC, {ROLE_RANK} DESC LIMIT 1"
        ),
    };

    // All three query shapes bind the same four params positionally so the
    // builder stays uniform; unused binds (e.g. $4 for a folder) are harmless.
    let role: Option<(String,)> = sqlx::query_as(&sql)
        .bind(uid) // $1
        .bind(ctx.home_path.as_deref()) // $2
        .bind(ctx.base_id) // $3
        .bind(instance_id) // $4
        .fetch_optional(db)
        .await?;

    Ok(role.and_then(|(r,)| Role::from_db(&r)))
}

/// Effective role for a single object. `Ok(None)` when the user is not a member
/// of the object's workspace OR the object doesn't exist (callers do a
/// fetch-then-gate, so a missing object has already 404'd; conflating the two
/// here avoids leaking object existence to non-members).
pub async fn effective_object_role(
    db: &PgPool,
    user: &AuthUser,
    obj: ObjectRef,
) -> Result<Option<Role>, MembershipError> {
    let ctx = match resolve_ctx(db, obj).await? {
        Some(c) => c,
        None => return Ok(None),
    };

    let ws_role = match member_role(db, user, ctx.workspace_id).await {
        Ok(r) => r,
        Err(MembershipError::NotMember(_)) => return Ok(None),
        Err(e) => return Err(e),
    };

    // Workspace Owner/Admin bypass — never downgraded by an object grant.
    if ws_role >= Role::Admin {
        return Ok(Some(ws_role));
    }

    let grant = best_grant_role(db, obj.kind, &ctx, obj.id, user.subject_as_uuid()).await?;
    // Floor: the workspace role is the minimum; the grant can only raise it (or,
    // when more specific than an inherited grant, override down to the grant —
    // but the final `max` keeps it at/above the workspace floor).
    Ok(Some(grant.map_or(ws_role, |g| g.max(ws_role))))
}

/// Hard-gate an object operation: effective role must exist and be ≥ `need`.
pub async fn require_object_role(
    db: &PgPool,
    user: &AuthUser,
    obj: ObjectRef,
    need: Role,
) -> Result<Role, MembershipError> {
    match effective_object_role(db, user, obj).await? {
        Some(have) if have >= need => Ok(have),
        Some(have) => Err(MembershipError::InsufficientRole { have, need }),
        // No access (non-member or vanished object). Maps to 403 — callers that
        // want 404-on-missing fetch the row first.
        None => Err(MembershipError::NotMember(Uuid::nil())),
    }
}

/// Per-row effective role for a list of candidate objects of one kind, in one
/// workspace. ONE role-resolution query regardless of row count. Returns a map
/// covering EVERY input id — ids with no grant fall back to the workspace floor.
/// Empty input or a non-member caller yields an empty map.
pub async fn effective_object_roles(
    db: &PgPool,
    user: &AuthUser,
    kind: ObjectKind,
    workspace_id: Uuid,
    ids: &[Uuid],
) -> Result<HashMap<Uuid, Role>, MembershipError> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let ws_role = match member_role(db, user, workspace_id).await {
        Ok(r) => r,
        Err(MembershipError::NotMember(_)) => return Ok(HashMap::new()),
        Err(e) => return Err(e),
    };

    // Workspace Owner/Admin bypass: every row is the workspace role.
    if ws_role >= Role::Admin {
        return Ok(ids.iter().map(|&id| (id, ws_role)).collect());
    }

    // Floor for every candidate, overlaid below by the single grant query.
    let mut out: HashMap<Uuid, Role> = ids.iter().map(|&id| (id, ws_role)).collect();

    let folder_tier = "SELECT g.role, 2 AS source_rank, length(f.path) AS depth \
           FROM object_grants g JOIN folders f ON f.id = g.object_id \
          WHERE g.object_type = 'folder'::object_kind AND g.user_id = $2 \
            AND ctx.home_path IS NOT NULL \
            AND (ctx.home_path = f.path OR ctx.home_path LIKE f.path || '/%')";

    let ctx_cte = match kind {
        ObjectKind::Folder => {
            "SELECT id AS cand_id, id AS base_id, path AS home_path \
               FROM folders WHERE id = ANY($1)"
        }
        ObjectKind::Template => {
            "SELECT t.id AS cand_id, COALESCE(t.base_template_id, t.id) AS base_id, f.path AS home_path \
               FROM workflow_templates t \
               LEFT JOIN template_folders tf ON tf.base_template_id = COALESCE(t.base_template_id, t.id) \
               LEFT JOIN folders f ON f.id = tf.folder_id \
              WHERE t.id = ANY($1)"
        }
        ObjectKind::Instance => {
            "SELECT i.id AS cand_id, COALESCE(t.base_template_id, t.id) AS base_id, f.path AS home_path \
               FROM workflow_instances i \
               JOIN workflow_templates t ON t.id = i.template_id \
               LEFT JOIN template_folders tf ON tf.base_template_id = COALESCE(t.base_template_id, t.id) \
               LEFT JOIN folders f ON f.id = tf.folder_id \
              WHERE i.id = ANY($1)"
        }
    };

    // The object/instance tiers reference ctx.base_id / ctx.cand_id from the CTE.
    let object_tiers: String = match kind {
        ObjectKind::Folder => String::new(),
        ObjectKind::Template => "SELECT g.role, 3 AS source_rank, 0 AS depth FROM object_grants g \
              WHERE g.object_type = 'template'::object_kind AND g.object_id = ctx.base_id AND g.user_id = $2 \
             UNION ALL "
            .to_string(),
        ObjectKind::Instance => "SELECT g.role, 4 AS source_rank, 0 AS depth FROM object_grants g \
              WHERE g.object_type = 'instance'::object_kind AND g.object_id = ctx.cand_id AND g.user_id = $2 \
             UNION ALL \
             SELECT g.role, 3 AS source_rank, 0 AS depth FROM object_grants g \
              WHERE g.object_type = 'template'::object_kind AND g.object_id = ctx.base_id AND g.user_id = $2 \
             UNION ALL "
            .to_string(),
    };

    let sql = format!(
        "WITH ctx AS ( {ctx_cte} ) \
         SELECT ctx.cand_id, best.role FROM ctx \
         JOIN LATERAL ( \
           SELECT role FROM ( {object_tiers} {folder_tier} ) s \
           ORDER BY source_rank DESC, depth DESC, {ROLE_RANK} DESC LIMIT 1 \
         ) best ON TRUE"
    );

    let rows: Vec<(Uuid, String)> = sqlx::query_as(&sql)
        .bind(ids) // $1
        .bind(user.subject_as_uuid()) // $2
        .fetch_all(db)
        .await?;

    for (cand_id, role) in rows {
        if let Some(grant) = Role::from_db(&role) {
            // Floor: a grant only takes effect when it raises the row above the
            // workspace role.
            out.entry(cand_id).and_modify(|r| *r = grant.max(ws_role));
        }
    }

    Ok(out)
}

/// Upsert a grant on the UNIQUE `(object_type, object_id, user_id)` key.
/// Generic over the executor so it runs on a pool or inside a transaction
/// (Phase 4 invites call this on accept).
pub async fn apply_grant<'e, E>(
    executor: E,
    workspace_id: Uuid,
    kind: ObjectKind,
    object_id: Uuid,
    user_id: Uuid,
    role: Role,
    granted_by: Uuid,
) -> Result<(), sqlx::Error>
where
    E: sqlx::PgExecutor<'e>,
{
    let role_db = match role {
        Role::Owner => "owner",
        Role::Admin => "admin",
        Role::Editor => "editor",
        Role::Viewer => "viewer",
    };
    sqlx::query(
        "INSERT INTO object_grants (workspace_id, object_type, object_id, user_id, role, granted_by) \
              VALUES ($1, $2::object_kind, $3, $4, $5, $6) \
         ON CONFLICT (object_type, object_id, user_id) \
           DO UPDATE SET role = EXCLUDED.role, granted_by = EXCLUDED.granted_by, updated_at = now()",
    )
    .bind(workspace_id)
    .bind(kind.as_db())
    .bind(object_id)
    .bind(user_id)
    .bind(role_db)
    .bind(granted_by)
    .execute(executor)
    .await
    .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn object_kind_path_roundtrip() {
        assert_eq!(
            ObjectKind::from_path_segment("folders"),
            Some(ObjectKind::Folder)
        );
        assert_eq!(
            ObjectKind::from_path_segment("templates"),
            Some(ObjectKind::Template)
        );
        assert_eq!(
            ObjectKind::from_path_segment("instances"),
            Some(ObjectKind::Instance)
        );
        assert_eq!(ObjectKind::from_path_segment("widgets"), None);
        assert_eq!(ObjectKind::Folder.as_db(), "folder");
        assert_eq!(ObjectKind::Template.as_db(), "template");
        assert_eq!(ObjectKind::Instance.as_db(), "instance");
    }

    /// The floor + most-specific-override role math, isolated from SQL. Mirrors
    /// what `effective_object_role` computes once `best_grant_role` and
    /// `member_role` have run.
    fn effective(grant: Option<Role>, ws_role: Role) -> Role {
        grant.map_or(ws_role, |g| g.max(ws_role))
    }

    #[test]
    fn grant_elevates_above_floor() {
        // object Editor grant on a ws Viewer → Editor.
        assert_eq!(effective(Some(Role::Editor), Role::Viewer), Role::Editor);
    }

    #[test]
    fn floor_is_never_dropped() {
        // A folder Viewer-override on a ws Editor stays Editor (floor wins).
        assert_eq!(effective(Some(Role::Viewer), Role::Editor), Role::Editor);
    }

    #[test]
    fn no_grant_falls_back_to_floor() {
        assert_eq!(effective(None, Role::Viewer), Role::Viewer);
        assert_eq!(effective(None, Role::Editor), Role::Editor);
    }

    #[test]
    fn admin_bypass_returns_ws_role() {
        // `effective_object_role` short-circuits Admin/Owner before computing a
        // grant; modelled here as: an object Viewer grant can't downgrade Admin.
        // (The real function never even queries the grant for Admin+.)
        assert_eq!(effective(Some(Role::Viewer), Role::Admin), Role::Admin);
        assert_eq!(effective(Some(Role::Editor), Role::Owner), Role::Owner);
    }
}
