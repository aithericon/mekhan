//! Phase 3 — object-level authorization. Sibling to [`super::membership`];
//! reuses its [`Role`], [`MembershipError`], and `member_role`.
//!
//! A grant binds `(object_type, object_id, user_id) → role`. The **effective**
//! role of a user on an object is:
//!
//! - **Workspace Owner/Admin bypass:** if the caller's workspace role ≥ Admin,
//!   that role is returned immediately — never constrained by an object ACL.
//! - **Floor (default):** the workspace role is a FLOOR. The result is
//!   `max(most-specific grant, workspace_role)`. A grant can *elevate* a member
//!   above their workspace role on a specific object, and the most-specific
//!   grant can *downgrade an inherited higher grant* (a deep Viewer overrides a
//!   shallow Owner) — but it can never drop a member below their workspace role.
//! - **`restricted` opt-out:** when an object (or an ancestor folder) is
//!   `restricted`, the floor is REMOVED — access is exactly the grant (direct
//!   or inherited), or none at all. ws Owner/Admin still bypass. This is what
//!   makes an object genuinely private (hidden from ordinary members). A
//!   restricted folder cascades privacy to its whole subtree.
//! - **Most-specific wins among grant tiers:** object grant > parent-template
//!   grant > nearest-ancestor folder grant. Folders nest via the materialized
//!   `folders.path`, so ancestry is a path-prefix match — no recursive CTE.
//!
//! For a non-restricted object the floor means a member views every object in
//! their workspace, so the list annotation in [`effective_object_roles`] covers
//! every input id. For a RESTRICTED object the returned map OMITS ids the caller
//! cannot reach — so a list endpoint filters to the map's keys (the
//! grant-scoped visibility the floor otherwise made moot).
//!
//! `object_id` for a TEMPLATE is the chain-root `COALESCE(base_template_id, id)`
//! so a grant follows the whole version chain. Instances carry no workspace_id
//! and join their template by per-version `template_id` (two-hop). RESOURCES and
//! ASSETS are ACL objects too: their `(scope_kind, scope_id)` placement is the
//! inheritance parent — folder-scoped inherits that folder's chain, template-
//! scoped inherits the owning template (and its folder chain), workspace-scoped
//! inherits nothing but the floor. A grant on a resource/asset keys on its own
//! id (like an instance).

use std::collections::HashMap;

use sqlx::PgPool;
use uuid::Uuid;

use super::membership::{member_role, MembershipError, Role};
use super::model::AuthUser;

/// The granular-ACL object kinds. Maps to the `object_kind` Postgres enum.
/// Resources and assets are full ACL objects too — their existing
/// `(scope_kind, scope_id)` placement is reused as the inheritance parent
/// (folder path / owning template / workspace); see [`resolve_ctx`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectKind {
    Folder,
    Template,
    Instance,
    Resource,
    Asset,
}

impl ObjectKind {
    /// The `object_kind` enum label. Bound with an explicit `::object_kind`
    /// cast at every use site.
    pub fn as_db(self) -> &'static str {
        match self {
            ObjectKind::Folder => "folder",
            ObjectKind::Template => "template",
            ObjectKind::Instance => "instance",
            ObjectKind::Resource => "resource",
            ObjectKind::Asset => "asset",
        }
    }

    /// Parse the plural REST path segment. The grant routes register concrete
    /// literals so the resolver never sees an unknown kind.
    pub fn from_path_segment(s: &str) -> Option<Self> {
        match s {
            "folders" => Some(ObjectKind::Folder),
            "templates" => Some(ObjectKind::Template),
            "instances" => Some(ObjectKind::Instance),
            "resources" => Some(ObjectKind::Resource),
            "assets" => Some(ObjectKind::Asset),
            _ => None,
        }
    }

    /// Resources and assets key a direct grant on their OWN id (like an
    /// instance), not on a chain-root base.
    fn keys_on_self(self) -> bool {
        matches!(self, ObjectKind::Instance | ObjectKind::Resource | ObjectKind::Asset)
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
    pub fn resource(id: Uuid) -> Self {
        Self {
            kind: ObjectKind::Resource,
            id,
        }
    }
    pub fn asset(id: Uuid) -> Self {
        Self {
            kind: ObjectKind::Asset,
            id,
        }
    }
}

/// Resolved object context: the workspace it lives in, the grant chain-root id
/// (folder id / template base id), the home-folder materialized path (None for
/// a folderless template / workspace-scoped resource), and whether access is
/// `restricted` (the object itself or an ancestor folder turned off the
/// workspace-role floor — see [`is_restricted`]).
struct ObjCtx {
    workspace_id: Uuid,
    /// For folder: the folder id. For template/instance: the template chain
    /// root. For resource/asset: the owning template's chain root when
    /// template-scoped, else `Uuid::nil()` (no template tier to inherit).
    base_id: Uuid,
    home_path: Option<String>,
    /// `true` ⇒ no workspace floor; access is grants + inheritance + ws
    /// Owner/Admin bypass only.
    restricted: bool,
}

/// SQL fragment ranking the four role tiers by privilege. Shared so the
/// single-object and batch paths order identically.
const ROLE_RANK: &str =
    "(CASE role WHEN 'owner' THEN 3 WHEN 'admin' THEN 2 WHEN 'editor' THEN 1 ELSE 0 END)";

/// Resolve a `(scope_kind, scope_id)` placement (used by resources & assets)
/// into `(workspace_id, base_id, home_path)`:
/// - `workspace` → the workspace itself, no folder, no template tier.
/// - `folder`    → the folder's workspace + its materialized path.
/// - `template`  → the owning template's workspace, chain-root base (template
///   tier), and the template's home-folder path.
///
/// Returns `None` when the referenced scope row no longer exists.
async fn resolve_scope(
    db: &PgPool,
    scope_kind: &str,
    scope_id: Uuid,
) -> Result<Option<(Uuid, Uuid, Option<String>)>, MembershipError> {
    match scope_kind {
        "workspace" => Ok(Some((scope_id, Uuid::nil(), None))),
        "folder" => Ok(sqlx::query_as::<_, (Uuid, String)>(
            "SELECT workspace_id, path FROM folders WHERE id = $1",
        )
        .bind(scope_id)
        .fetch_optional(db)
        .await?
        .map(|(ws, path)| (ws, Uuid::nil(), Some(path)))),
        "template" => Ok(sqlx::query_as::<_, (Uuid, Uuid, Option<String>)>(
            "SELECT t.workspace_id, COALESCE(t.base_template_id, t.id) AS base_id, f.path \
               FROM workflow_templates t \
               LEFT JOIN template_folders tf ON tf.base_template_id = COALESCE(t.base_template_id, t.id) \
               LEFT JOIN folders f ON f.id = tf.folder_id \
              WHERE t.id = $1",
        )
        .bind(scope_id)
        .fetch_optional(db)
        .await?),
        _ => Ok(None),
    }
}

/// Whether an object is `restricted`: its own flag OR any ANCESTOR folder
/// (a path-prefix of `home_path`) carrying `restricted = true`. A restricted
/// folder thus makes its whole subtree private.
async fn is_restricted(
    db: &PgPool,
    workspace_id: Uuid,
    home_path: Option<&str>,
    self_restricted: bool,
) -> Result<bool, MembershipError> {
    if self_restricted {
        return Ok(true);
    }
    let Some(home) = home_path else {
        return Ok(false);
    };
    let row: Option<(bool,)> = sqlx::query_as(
        "SELECT EXISTS(SELECT 1 FROM folders \
           WHERE workspace_id = $1 AND restricted \
             AND ($2 = path OR $2 LIKE path || '/%'))",
    )
    .bind(workspace_id)
    .bind(home)
    .fetch_optional(db)
    .await?;
    Ok(row.map(|(b,)| b).unwrap_or(false))
}

/// Resolve `(workspace_id, base_id, home_path, restricted)` for an object, or
/// `None` if the object row doesn't exist.
async fn resolve_ctx(db: &PgPool, obj: ObjectRef) -> Result<Option<ObjCtx>, MembershipError> {
    // (workspace_id, base_id, home_path, self_restricted)
    let resolved: Option<(Uuid, Uuid, Option<String>, bool)> = match obj.kind {
        ObjectKind::Folder => sqlx::query_as::<_, (Uuid, Uuid, String, bool)>(
            "SELECT workspace_id, id, path, restricted FROM folders WHERE id = $1",
        )
        .bind(obj.id)
        .fetch_optional(db)
        .await?
        .map(|(ws, id, path, r)| (ws, id, Some(path), r)),
        ObjectKind::Template => sqlx::query_as::<_, (Uuid, Uuid, Option<String>)>(
            "SELECT t.workspace_id, COALESCE(t.base_template_id, t.id) AS base_id, f.path \
               FROM workflow_templates t \
               LEFT JOIN template_folders tf ON tf.base_template_id = COALESCE(t.base_template_id, t.id) \
               LEFT JOIN folders f ON f.id = tf.folder_id \
              WHERE t.id = $1",
        )
        .bind(obj.id)
        .fetch_optional(db)
        .await?
        .map(|(ws, base, path)| (ws, base, path, false)),
        ObjectKind::Instance => sqlx::query_as::<_, (Uuid, Uuid, Option<String>)>(
            "SELECT t.workspace_id, COALESCE(t.base_template_id, t.id) AS base_id, f.path \
               FROM workflow_instances i \
               JOIN workflow_templates t ON t.id = i.template_id \
               LEFT JOIN template_folders tf ON tf.base_template_id = COALESCE(t.base_template_id, t.id) \
               LEFT JOIN folders f ON f.id = tf.folder_id \
              WHERE i.id = $1",
        )
        .bind(obj.id)
        .fetch_optional(db)
        .await?
        .map(|(ws, base, path)| (ws, base, path, false)),
        ObjectKind::Resource => {
            // resources keep a denormalized workspace_id; scope_id may be NULL
            // for legacy workspace-scoped rows → coalesce to the workspace.
            let row: Option<(Uuid, String, Option<Uuid>, bool)> = sqlx::query_as(
                "SELECT workspace_id, scope_kind, scope_id, restricted FROM resources WHERE id = $1",
            )
            .bind(obj.id)
            .fetch_optional(db)
            .await?;
            match row {
                None => None,
                Some((ws, sk, sid, restr)) => {
                    let sid = sid.unwrap_or(ws);
                    match resolve_scope(db, &sk, sid).await? {
                        Some((_, base, home)) => Some((ws, base, home, restr)),
                        // Scope row vanished — fall back to workspace placement.
                        None => Some((ws, Uuid::nil(), None, restr)),
                    }
                }
            }
        }
        ObjectKind::Asset => {
            // assets are pure scope-addressed (no workspace_id column).
            let row: Option<(String, Uuid, bool)> = sqlx::query_as(
                "SELECT scope_kind, scope_id, restricted FROM assets WHERE id = $1",
            )
            .bind(obj.id)
            .fetch_optional(db)
            .await?;
            match row {
                None => None,
                Some((sk, sid, restr)) => resolve_scope(db, &sk, sid)
                    .await?
                    .map(|(ws, base, home)| (ws, base, home, restr)),
            }
        }
    };

    let Some((workspace_id, base_id, home_path, self_restricted)) = resolved else {
        return Ok(None);
    };
    let restricted = is_restricted(db, workspace_id, home_path.as_deref(), self_restricted).await?;
    Ok(Some(ObjCtx {
        workspace_id,
        base_id,
        home_path,
        restricted,
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
        grant_object_id: if obj.kind.keys_on_self() {
            // Instance/resource/asset grants key on the object itself; the
            // template base (if any) is only the inheritance hop.
            obj.id
        } else {
            ctx.base_id
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
        // Resource/asset: direct tier on the object's own id ($4), the optional
        // template tier ($3 = owning template base, Uuid::nil() when not
        // template-scoped → matches nothing), and the folder-ancestry tier.
        ObjectKind::Resource | ObjectKind::Asset => {
            let direct = kind.as_db();
            format!(
                "SELECT role FROM ( \
                   SELECT role, 4 AS source_rank, 0 AS depth FROM object_grants \
                    WHERE object_type = '{direct}'::object_kind AND object_id = $4 AND user_id = $1 \
                   UNION ALL \
                   SELECT role, 3 AS source_rank, 0 AS depth FROM object_grants \
                    WHERE object_type = 'template'::object_kind AND object_id = $3 AND user_id = $1 \
                   UNION ALL {folder_tier} \
                 ) s ORDER BY source_rank DESC, depth DESC, {ROLE_RANK} DESC LIMIT 1"
            )
        }
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
    // Restricted: NO floor — access is exactly the grant (direct or inherited),
    // or None (no access) when the user has no grant. ws Owner/Admin already
    // bypassed above. This is what makes an object genuinely private.
    if ctx.restricted {
        return Ok(grant);
    }
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
/// workspace. ONE role-resolution query regardless of row count. For
/// non-restricted objects every input id is present (floor fallback); for a
/// RESTRICTED object an id the caller cannot reach is OMITTED — so the map
/// doubles as an accessibility filter (keys = visible ids). Empty input or a
/// non-member caller yields an empty map.
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

    let folder_tier = "SELECT g.role, 2 AS source_rank, length(f.path) AS depth \
           FROM object_grants g JOIN folders f ON f.id = g.object_id \
          WHERE g.object_type = 'folder'::object_kind AND g.user_id = $2 \
            AND ctx.home_path IS NOT NULL \
            AND (ctx.home_path = f.path OR ctx.home_path LIKE f.path || '/%')";

    // Each CTE yields (cand_id, base_id, home_path, self_restricted). For
    // resource/asset, the scope (folder/template/workspace) is resolved inline
    // via LEFT JOINs guarded on scope_kind so one row is produced per candidate.
    let ctx_cte = match kind {
        ObjectKind::Folder => {
            "SELECT id AS cand_id, id AS base_id, path AS home_path, restricted AS self_restricted \
               FROM folders WHERE id = ANY($1)"
        }
        ObjectKind::Template => {
            "SELECT t.id AS cand_id, COALESCE(t.base_template_id, t.id) AS base_id, f.path AS home_path, false AS self_restricted \
               FROM workflow_templates t \
               LEFT JOIN template_folders tf ON tf.base_template_id = COALESCE(t.base_template_id, t.id) \
               LEFT JOIN folders f ON f.id = tf.folder_id \
              WHERE t.id = ANY($1)"
        }
        ObjectKind::Instance => {
            "SELECT i.id AS cand_id, COALESCE(t.base_template_id, t.id) AS base_id, f.path AS home_path, false AS self_restricted \
               FROM workflow_instances i \
               JOIN workflow_templates t ON t.id = i.template_id \
               LEFT JOIN template_folders tf ON tf.base_template_id = COALESCE(t.base_template_id, t.id) \
               LEFT JOIN folders f ON f.id = tf.folder_id \
              WHERE i.id = ANY($1)"
        }
        ObjectKind::Resource => {
            "SELECT r.id AS cand_id, \
                    CASE WHEN r.scope_kind = 'template' THEN COALESCE(t.base_template_id, t.id) END AS base_id, \
                    CASE WHEN r.scope_kind = 'folder' THEN ff.path \
                         WHEN r.scope_kind = 'template' THEN tff.path END AS home_path, \
                    r.restricted AS self_restricted \
               FROM resources r \
               LEFT JOIN folders ff ON r.scope_kind = 'folder' AND ff.id = r.scope_id \
               LEFT JOIN workflow_templates t ON r.scope_kind = 'template' AND t.id = r.scope_id \
               LEFT JOIN template_folders ttf ON r.scope_kind = 'template' AND ttf.base_template_id = COALESCE(t.base_template_id, t.id) \
               LEFT JOIN folders tff ON tff.id = ttf.folder_id \
              WHERE r.id = ANY($1)"
        }
        ObjectKind::Asset => {
            "SELECT a.id AS cand_id, \
                    CASE WHEN a.scope_kind = 'template' THEN COALESCE(t.base_template_id, t.id) END AS base_id, \
                    CASE WHEN a.scope_kind = 'folder' THEN ff.path \
                         WHEN a.scope_kind = 'template' THEN tff.path END AS home_path, \
                    a.restricted AS self_restricted \
               FROM assets a \
               LEFT JOIN folders ff ON a.scope_kind = 'folder' AND ff.id = a.scope_id \
               LEFT JOIN workflow_templates t ON a.scope_kind = 'template' AND t.id = a.scope_id \
               LEFT JOIN template_folders ttf ON a.scope_kind = 'template' AND ttf.base_template_id = COALESCE(t.base_template_id, t.id) \
               LEFT JOIN folders tff ON tff.id = ttf.folder_id \
              WHERE a.id = ANY($1)"
        }
    };

    // Object-self / template tiers, referencing ctx.cand_id / ctx.base_id.
    let object_tiers: String = match kind {
        ObjectKind::Folder => String::new(),
        ObjectKind::Template => "SELECT g.role, 3 AS source_rank, 0 AS depth FROM object_grants g \
              WHERE g.object_type = 'template'::object_kind AND g.object_id = ctx.base_id AND g.user_id = $2 \
             UNION ALL "
            .to_string(),
        ObjectKind::Instance | ObjectKind::Resource | ObjectKind::Asset => format!(
            "SELECT g.role, 4 AS source_rank, 0 AS depth FROM object_grants g \
              WHERE g.object_type = '{direct}'::object_kind AND g.object_id = ctx.cand_id AND g.user_id = $2 \
             UNION ALL \
             SELECT g.role, 3 AS source_rank, 0 AS depth FROM object_grants g \
              WHERE g.object_type = 'template'::object_kind AND g.object_id = ctx.base_id AND g.user_id = $2 \
             UNION ALL ",
            direct = kind.as_db()
        ),
    };

    // LEFT JOIN LATERAL: candidates with no grant still return a row (role
    // NULL) so the floor / restricted decision is made per candidate in Rust.
    // `restricted` = self flag OR any ancestor folder ($3 = workspace) carrying
    // restricted.
    let sql = format!(
        "WITH ctx AS ( {ctx_cte} ) \
         SELECT ctx.cand_id, best.role, \
                (ctx.self_restricted OR EXISTS( \
                   SELECT 1 FROM folders fr \
                    WHERE fr.workspace_id = $3 AND fr.restricted \
                      AND ctx.home_path IS NOT NULL \
                      AND (ctx.home_path = fr.path OR ctx.home_path LIKE fr.path || '/%') \
                )) AS restricted \
         FROM ctx \
         LEFT JOIN LATERAL ( \
           SELECT role FROM ( {object_tiers} {folder_tier} ) s \
           ORDER BY source_rank DESC, depth DESC, {ROLE_RANK} DESC LIMIT 1 \
         ) best ON TRUE"
    );

    let rows: Vec<(Uuid, Option<String>, bool)> = sqlx::query_as(&sql)
        .bind(ids) // $1
        .bind(user.subject_as_uuid()) // $2
        .bind(workspace_id) // $3
        .fetch_all(db)
        .await?;

    let mut out: HashMap<Uuid, Role> = HashMap::with_capacity(rows.len());
    for (cand_id, role, restricted) in rows {
        let grant = role.and_then(|r| Role::from_db(&r));
        if restricted {
            // No floor: only an explicit/inherited grant grants access. No
            // grant ⇒ the candidate is omitted (no access).
            if let Some(g) = grant {
                out.insert(cand_id, g);
            }
        } else {
            // Floor: a grant only takes effect when it raises the row above the
            // workspace role.
            out.insert(cand_id, grant.map_or(ws_role, |g| g.max(ws_role)));
        }
    }

    Ok(out)
}

/// A list-row DTO carrying the `my_effective_role` ACL annotation. One-line
/// impls live next to each summary/list model so the generic list helpers
/// below ([`filter_and_annotate_visible`] / [`annotate_roles_keep_all`]) can
/// stamp — and, for the default helper, filter — any of them.
pub trait AclAnnotated {
    /// The id [`effective_object_roles`] keys the role map on.
    fn acl_id(&self) -> Uuid;
    /// Write the caller's effective-role wire label (`None` = no annotation).
    fn set_my_effective_role(&mut self, role: Option<String>);
}

/// Stamp pass: write each row's [`Role::as_label`] from the role map; rows the
/// map omits get `None`.
fn stamp_roles<T: AclAnnotated>(items: &mut [T], roles: &HashMap<Uuid, Role>) {
    for item in items.iter_mut() {
        item.set_my_effective_role(roles.get(&item.acl_id()).map(|r| r.as_label().to_string()));
    }
}

/// Pure core shared by the async list helpers: optionally drop rows omitted
/// from the role map, then stamp the labels.
fn apply_role_map<T: AclAnnotated>(
    items: &mut Vec<T>,
    roles: &HashMap<Uuid, Role>,
    drop_missing: bool,
) {
    if drop_missing {
        items.retain(|i| roles.contains_key(&i.acl_id()));
    }
    stamp_roles(items, roles);
}

/// THE default for list endpoints: resolve the caller's effective role for a
/// page of rows in ONE query ([`effective_object_roles`]), stamp
/// `my_effective_role`, and DROP rows omitted from the map — dropping omitted
/// rows is exactly what hides `restricted` objects the caller has no grant
/// for (non-restricted rows always resolve via the workspace floor).
pub async fn filter_and_annotate_visible<T: AclAnnotated>(
    db: &PgPool,
    user: &AuthUser,
    kind: ObjectKind,
    workspace_id: Uuid,
    items: &mut Vec<T>,
) -> Result<(), MembershipError> {
    let ids: Vec<Uuid> = items.iter().map(|i| i.acl_id()).collect();
    let roles = effective_object_roles(db, user, kind, workspace_id, &ids).await?;
    apply_role_map(items, &roles, true);
    Ok(())
}

/// EXPLICIT opt-out of the restricted-row filtering in
/// [`filter_and_annotate_visible`]: stamp `my_effective_role` but keep every
/// row (omitted ⇒ `None`). For surfaces that intentionally show rows the
/// caller cannot open — e.g. the folder tree, where navigation needs the full
/// path structure and detail access is gated by [`require_object_role`].
pub async fn annotate_roles_keep_all<T: AclAnnotated>(
    db: &PgPool,
    user: &AuthUser,
    kind: ObjectKind,
    workspace_id: Uuid,
    items: &mut [T],
) -> Result<(), MembershipError> {
    let ids: Vec<Uuid> = items.iter().map(|i| i.acl_id()).collect();
    let roles = effective_object_roles(db, user, kind, workspace_id, &ids).await?;
    stamp_roles(items, &roles);
    Ok(())
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
        assert_eq!(
            ObjectKind::from_path_segment("resources"),
            Some(ObjectKind::Resource)
        );
        assert_eq!(
            ObjectKind::from_path_segment("assets"),
            Some(ObjectKind::Asset)
        );
        assert_eq!(ObjectKind::from_path_segment("widgets"), None);
        assert_eq!(ObjectKind::Folder.as_db(), "folder");
        assert_eq!(ObjectKind::Template.as_db(), "template");
        assert_eq!(ObjectKind::Instance.as_db(), "instance");
        assert_eq!(ObjectKind::Resource.as_db(), "resource");
        assert_eq!(ObjectKind::Asset.as_db(), "asset");
    }

    #[test]
    fn resource_asset_key_on_self() {
        assert!(ObjectKind::Resource.keys_on_self());
        assert!(ObjectKind::Asset.keys_on_self());
        assert!(ObjectKind::Instance.keys_on_self());
        assert!(!ObjectKind::Folder.keys_on_self());
        assert!(!ObjectKind::Template.keys_on_self());
    }

    struct Row {
        id: Uuid,
        role: Option<String>,
    }

    impl AclAnnotated for Row {
        fn acl_id(&self) -> Uuid {
            self.id
        }
        fn set_my_effective_role(&mut self, role: Option<String>) {
            self.role = role;
        }
    }

    fn rows(ids: &[Uuid]) -> Vec<Row> {
        ids.iter().map(|&id| Row { id, role: None }).collect()
    }

    #[test]
    fn apply_role_map_drop_mode_drops_omitted_and_stamps_labels() {
        let visible = Uuid::new_v4();
        let hidden = Uuid::new_v4();
        let mut items = rows(&[visible, hidden]);
        let roles: HashMap<Uuid, Role> = [(visible, Role::Editor)].into_iter().collect();

        apply_role_map(&mut items, &roles, true);

        assert_eq!(items.len(), 1, "omitted id must be dropped");
        assert_eq!(items[0].id, visible);
        assert_eq!(items[0].role.as_deref(), Some("editor"));
    }

    #[test]
    fn apply_role_map_keep_mode_stamps_none_and_keeps_rows() {
        let granted = Uuid::new_v4();
        let omitted = Uuid::new_v4();
        let mut items = rows(&[granted, omitted]);
        let roles: HashMap<Uuid, Role> = [(granted, Role::Viewer)].into_iter().collect();

        apply_role_map(&mut items, &roles, false);

        assert_eq!(items.len(), 2, "keep mode must retain every row");
        assert_eq!(items[0].role.as_deref(), Some("viewer"));
        assert_eq!(items[1].role, None, "omitted row is stamped None");
    }

    #[test]
    fn apply_role_map_empty_map_non_member() {
        let ids = [Uuid::new_v4(), Uuid::new_v4()];
        let empty = HashMap::new();

        // Drop mode: a non-member (empty map) sees nothing.
        let mut dropped = rows(&ids);
        apply_role_map(&mut dropped, &empty, true);
        assert!(dropped.is_empty());

        // Keep mode: rows survive, all annotations None.
        let mut kept = rows(&ids);
        apply_role_map(&mut kept, &empty, false);
        assert_eq!(kept.len(), 2);
        assert!(kept.iter().all(|r| r.role.is_none()));
    }

    /// The floor + most-specific-override role math, isolated from SQL. Mirrors
    /// what `effective_object_role` computes once `best_grant_role` and
    /// `member_role` have run.
    fn effective(grant: Option<Role>, ws_role: Role) -> Role {
        grant.map_or(ws_role, |g| g.max(ws_role))
    }

    /// The full effective-role decision including the `restricted` opt-out and
    /// the ws Owner/Admin bypass — mirrors `effective_object_role`'s branches.
    fn effective_full(
        grant: Option<Role>,
        ws_role: Role,
        restricted: bool,
    ) -> Option<Role> {
        if ws_role >= Role::Admin {
            return Some(ws_role); // bypass
        }
        if restricted {
            return grant; // no floor — None means no access
        }
        Some(grant.map_or(ws_role, |g| g.max(ws_role)))
    }

    #[test]
    fn restricted_drops_the_floor() {
        // A ws Viewer with NO grant on a restricted object → no access.
        assert_eq!(effective_full(None, Role::Viewer, true), None);
        // A ws Editor with no grant on a restricted object → also no access
        // (the floor that would have admitted them is gone).
        assert_eq!(effective_full(None, Role::Editor, true), None);
        // An explicit Viewer grant on a restricted object → exactly Viewer,
        // NOT raised to the ws Editor floor.
        assert_eq!(
            effective_full(Some(Role::Viewer), Role::Editor, true),
            Some(Role::Viewer)
        );
    }

    #[test]
    fn restricted_still_honours_admin_bypass() {
        // ws Admin/Owner see restricted objects regardless of any grant.
        assert_eq!(
            effective_full(None, Role::Admin, true),
            Some(Role::Admin)
        );
        assert_eq!(
            effective_full(None, Role::Owner, true),
            Some(Role::Owner)
        );
    }

    #[test]
    fn unrestricted_keeps_the_floor() {
        // Same inputs, restricted = false → the floor admits the member.
        assert_eq!(
            effective_full(None, Role::Editor, false),
            Some(Role::Editor)
        );
        assert_eq!(
            effective_full(Some(Role::Owner), Role::Viewer, false),
            Some(Role::Owner)
        );
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
