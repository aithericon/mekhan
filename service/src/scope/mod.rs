//! Scope resolution (docs/20 §2) — the single source of truth for "what is
//! visible from a binding context, and which definition wins."
//!
//! The platform hierarchy is **workspace → folders (a single-parent tree that
//! is a template's one home) → templates → instances**. A resource / asset /
//! asset-type is owned by **exactly one** scope `(ScopeKind, scope_id)`.
//!
//! Resolution rules:
//! - **Visibility flows downward.** A binding inside template `T` can *see*
//!   anything owned by `T`, by the folder that homes `T`, or by the
//!   workspace.
//! - **Most-specific-wins.** `template` shadows `folder` shadows `workspace`
//!   for a given ref-key, and a DEEPER folder shadows a SHALLOWER ancestor
//!   folder. Folder specificity is the folder's depth rank within the binding
//!   context's ancestor chain (home/deepest highest), so a definition owned by
//!   any ancestor is inherited unless a deeper scope redefines the ref-key.
//! - **Ambiguity is a hard error.** If two equally-specific scopes *both*
//!   define the same ref-key, the scopes are **incomparable** → an error,
//!   never a silent pick (the platform's "compiler is the borrow-checker;
//!   ambiguity is an error, not a guess" ethos). A context's folder set is a
//!   single linear ancestor chain with distinct per-folder depth ranks, so
//!   folder-vs-folder clashes cannot arise via the real entry points — the
//!   incomparable path is retained as defense-in-depth for the generic case.
//!
//! This module is pure (no DB I/O): callers gather the candidate owned items
//! and the binding context's visible scope set, then call [`resolve_visible`] /
//! [`resolve_one_visible`]. The list endpoints, the picker, and the compiler
//! binding all go through this so they cannot drift. [`visible_scopes_for`] is
//! the DB helper that turns a binding context into its downward-visible owner
//! set (workspace + home-folder ancestor chain + template).

use std::collections::BTreeMap;

use sqlx::PgPool;
use uuid::Uuid;

use crate::models::asset::ScopeKind;

/// A concrete owner scope: a `(kind, id)` pair. For `Workspace` the id is the
/// workspace id; for `Folder`, the folder id; for `Template`, the template's
/// chain-root (`base_template_id`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Scope {
    pub kind: ScopeKind,
    pub id: Uuid,
}

impl Scope {
    pub fn workspace(id: Uuid) -> Self {
        Self {
            kind: ScopeKind::Workspace,
            id,
        }
    }
    pub fn folder(id: Uuid) -> Self {
        Self {
            kind: ScopeKind::Folder,
            id,
        }
    }
    pub fn template(id: Uuid) -> Self {
        Self {
            kind: ScopeKind::Template,
            id,
        }
    }
}

/// Specificity of an owner scope FROM a given binding context. Higher = more
/// specific. `workspace = 0`; a folder gets its depth rank within the context's
/// ancestor chain (`1..=chain_len`, the home/deepest folder highest); a template
/// gets the maximum. Folder ranks sit strictly between workspace (0) and template
/// (`u32::MAX`). In a linear ancestor chain every folder has a DISTINCT rank, so
/// two ancestor folders never tie — the chain is totally ordered and a unique
/// most-specific winner always exists.
fn specificity(scope: &Scope, visible: &VisibleScopes) -> u32 {
    match scope.kind {
        ScopeKind::Workspace => 0,
        ScopeKind::Folder => visible.folder_rank(scope.id).map(|r| r as u32).unwrap_or(0),
        ScopeKind::Template => u32::MAX,
    }
}

/// The downward-visible owner set for a binding context, plus the context
/// itself. Built by [`visible_scopes_for`]. The set is small (one workspace,
/// the home-folder ancestor chain, 0..1 template).
#[derive(Debug, Clone, Default)]
pub struct VisibleScopes {
    /// The workspace owner (always present for a real binding context).
    pub workspace: Option<Uuid>,
    /// The context's home folder PLUS its full ancestor chain, ordered
    /// MOST-SPECIFIC-FIRST: index 0 = the deepest (home) folder, the last
    /// element = the nearest-to-root ancestor. A workspace context has an empty
    /// chain. A definition owned by ANY folder in this chain is visible, with
    /// deeper folders shadowing shallower ones (see [`Self::folder_rank`]).
    pub folders: Vec<Uuid>,
    /// The context template itself, if the binding is template-scoped.
    pub template: Option<Uuid>,
}

impl VisibleScopes {
    /// Does this visible set include `scope` as an owner? Candidate items owned
    /// by a scope NOT in this set are invisible and must be filtered out before
    /// resolution.
    pub fn contains(&self, scope: &Scope) -> bool {
        match scope.kind {
            ScopeKind::Workspace => self.workspace == Some(scope.id),
            ScopeKind::Folder => self.folders.contains(&scope.id),
            ScopeKind::Template => self.template == Some(scope.id),
        }
    }

    /// Depth rank of a folder within this context's ancestor chain.
    /// index 0 (home / deepest) gets the highest rank; the root-most ancestor gets 1.
    /// Returns None when the folder is not part of this context's chain.
    fn folder_rank(&self, id: Uuid) -> Option<usize> {
        self.folders
            .iter()
            .position(|f| *f == id)
            .map(|idx| self.folders.len() - idx)
    }
}

/// One candidate owned item presented to the resolver. The resolver is generic
/// over what the item *is* (a resource, an asset, an asset-type) via `T`.
#[derive(Debug, Clone)]
pub struct ScopedItem<T> {
    pub scope: Scope,
    /// The flat ref-key (`path` for resources, `ref_key`/`name` for assets).
    pub ref_key: String,
    pub item: T,
}

/// Resolution failed because two equally-specific scopes both define the same
/// ref-key — the scopes are incomparable, so picking one would be a silent
/// guess (docs/20 §2). This maps to a `CompileError`-style hard error at the
/// API edge.
///
/// This guard is retained as defense-in-depth (and its error type still feeds
/// the API error mapping), but it is UNREACHABLE for folders via the real entry
/// points: a context's folder set is a single linear ancestor chain, so every
/// folder has a DISTINCT depth rank and folder-vs-folder ties cannot arise. It
/// is kept for the generic case (e.g. a direct `resolve_refs_ranked` call) and
/// for future widening of the scope model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncomparableClash {
    pub ref_key: String,
    /// The two (or more) incomparable owner scopes.
    pub scopes: Vec<Scope>,
}

impl std::fmt::Display for IncomparableClash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "ambiguous binding ref '{}': defined in {} incomparable scopes ({})",
            self.ref_key,
            self.scopes.len(),
            self.scopes
                .iter()
                .map(|s| format!("{}:{}", s.kind.as_db(), s.id))
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}

impl std::error::Error for IncomparableClash {}

/// Resolve a set of candidate items (already filtered to the visible set) to a
/// single winner per ref-key, applying most-specific-wins ranked FROM the
/// binding context `visible`. Returns a map `ref_key -> winning item`, or the
/// first incomparable clash encountered.
///
/// "Most-specific-wins" = the highest [`specificity`] for a given ref-key wins
/// (workspace < ancestor folders by depth < template). If two items share the
/// same ref-key AND the same (highest) specificity but different scopes — two
/// distinct scopes at the same top specificity — that is incomparable → error.
/// For folders this is unreachable through the real entry points (a context's
/// folder set is a single linear chain with distinct per-folder ranks); the
/// guard is retained for the generic case.
fn resolve_refs_ranked<T: Clone>(
    visible: &VisibleScopes,
    items: Vec<ScopedItem<T>>,
) -> Result<BTreeMap<String, ScopedItem<T>>, IncomparableClash> {
    // Group by ref_key.
    let mut by_ref: BTreeMap<String, Vec<ScopedItem<T>>> = BTreeMap::new();
    for it in items {
        by_ref.entry(it.ref_key.clone()).or_default().push(it);
    }

    let mut winners: BTreeMap<String, ScopedItem<T>> = BTreeMap::new();
    for (ref_key, candidates) in by_ref {
        // Find the maximum specificity among candidates for this ref-key.
        let max_spec = candidates
            .iter()
            .map(|c| specificity(&c.scope, visible))
            .max()
            .unwrap();
        let top: Vec<&ScopedItem<T>> = candidates
            .iter()
            .filter(|c| specificity(&c.scope, visible) == max_spec)
            .collect();

        // Distinct owner scopes at the top specificity. More than one distinct
        // scope at the same specificity = incomparable.
        let mut distinct_scopes: Vec<Scope> = Vec::new();
        for c in &top {
            if !distinct_scopes.contains(&c.scope) {
                distinct_scopes.push(c.scope);
            }
        }

        if distinct_scopes.len() > 1 {
            return Err(IncomparableClash {
                ref_key,
                scopes: distinct_scopes,
            });
        }

        // Exactly one winning scope. (If multiple items share that one scope —
        // impossible under the DB unique (scope_kind, scope_id, ref_key) — we
        // deterministically take the first.)
        winners.insert(ref_key, top[0].clone());
    }

    Ok(winners)
}

/// Filter a candidate set to only items owned by a scope in `visible`, then
/// resolve most-specific-wins. This is the one-call entry point the list
/// endpoints / picker / compiler should use.
pub fn resolve_visible<T: Clone>(
    visible: &VisibleScopes,
    items: Vec<ScopedItem<T>>,
) -> Result<BTreeMap<String, ScopedItem<T>>, IncomparableClash> {
    let filtered: Vec<ScopedItem<T>> = items
        .into_iter()
        .filter(|i| visible.contains(&i.scope))
        .collect();
    resolve_refs_ranked(visible, filtered)
}

/// Resolve a single ref-key against the candidate set, ranked from the binding
/// context `visible`. Convenience over [`resolve_visible`] for the compiler
/// binding path (one alias at a time).
pub fn resolve_one_visible<T: Clone>(
    visible: &VisibleScopes,
    ref_key: &str,
    items: Vec<ScopedItem<T>>,
) -> Result<Option<ScopedItem<T>>, IncomparableClash> {
    let matching: Vec<ScopedItem<T>> = items.into_iter().filter(|i| i.ref_key == ref_key).collect();
    if matching.is_empty() {
        return Ok(None);
    }
    let mut resolved = resolve_refs_ranked(visible, matching)?;
    Ok(resolved.remove(ref_key))
}

/// DB helper: compute the downward-visible owner set for a binding context.
///
/// - `Workspace` context: visible = just that workspace.
/// - `Template` context: visible = the template's chain-root + its home
///   folder's full ancestor chain (deepest-first) + the template's workspace.
/// - `Folder` context: visible = the folder's ancestor chain (deepest-first) +
///   its workspace (used by the picker when browsing folder-scoped definitions
///   directly).
///
/// `scope_id` semantics per kind: workspace id / template chain-root
/// (`base_template_id`) / folder id.
pub async fn visible_scopes_for(
    db: &PgPool,
    kind: ScopeKind,
    scope_id: Uuid,
) -> Result<VisibleScopes, sqlx::Error> {
    // The home folder + its ancestors, ordered MOST-SPECIFIC-FIRST (deepest
    // first). Uses the materialized `path` column: an ancestor's path is a
    // segment-boundary prefix of the home path. The segment test is a literal
    // `left(...)` comparison rather than `LIKE f.path || '/%'` on purpose — a
    // LIKE pattern built from another folder's stored `path` would let a slug
    // containing `%`/`_` act as a wildcard and falsely match as an ancestor
    // (cross-scope visibility leak). `left(home.path, len(f.path)+1)` cannot be
    // injected; it also still stops `/research-x/a` from matching `/research`.
    const CHAIN_SQL: &str = "SELECT f.id \
          FROM folders home \
          JOIN folders f \
            ON f.workspace_id = home.workspace_id \
           AND (home.path = f.path \
                OR left(home.path, length(f.path) + 1) = f.path || '/') \
         WHERE home.id = $1 \
         ORDER BY length(f.path) DESC";

    match kind {
        ScopeKind::Workspace => Ok(VisibleScopes {
            workspace: Some(scope_id),
            folders: Vec::new(),
            template: None,
        }),
        ScopeKind::Folder => {
            // Folder scope -> resolve its workspace for upward visibility.
            let ws: Option<(Uuid,)> =
                sqlx::query_as("SELECT workspace_id FROM folders WHERE id = $1")
                    .bind(scope_id)
                    .fetch_optional(db)
                    .await?;
            // The folder's ancestor chain (folder itself + ancestors,
            // deepest-first) so a picker browsing /research/robots also sees
            // /research and the workspace.
            let chain: Vec<(Uuid,)> = sqlx::query_as(CHAIN_SQL)
                .bind(scope_id)
                .fetch_all(db)
                .await?;
            Ok(VisibleScopes {
                workspace: ws.map(|(w,)| w),
                folders: chain.into_iter().map(|(f,)| f).collect(),
                template: None,
            })
        }
        ScopeKind::Template => {
            // Normalize to the chain root so project membership (which keys on
            // base_template_id) and template-scoped ownership agree.
            let base: Option<(Uuid, Uuid)> = sqlx::query_as(
                "SELECT COALESCE(base_template_id, id), workspace_id \
                   FROM workflow_templates WHERE id = $1",
            )
            .bind(scope_id)
            .fetch_optional(db)
            .await?;

            let (base_id, workspace_id) = match base {
                Some(b) => b,
                None => {
                    // Unknown template — treat scope_id itself as the template
                    // owner with no workspace/folders (defensive).
                    return Ok(VisibleScopes {
                        workspace: None,
                        folders: Vec::new(),
                        template: Some(scope_id),
                    });
                }
            };

            // A template has at most ONE home folder (filesystem model). Find
            // it, then expand to its full ancestor chain (deepest-first) so an
            // ancestor folder's definitions are inherited.
            let home: Option<(Uuid,)> = sqlx::query_as(
                "SELECT folder_id FROM template_folders WHERE base_template_id = $1",
            )
            .bind(base_id)
            .fetch_optional(db)
            .await?;

            let folders: Vec<Uuid> = match home {
                Some((home_id,)) => {
                    let chain: Vec<(Uuid,)> = sqlx::query_as(CHAIN_SQL)
                        .bind(home_id)
                        .fetch_all(db)
                        .await?;
                    chain.into_iter().map(|(f,)| f).collect()
                }
                None => Vec::new(),
            };

            Ok(VisibleScopes {
                workspace: Some(workspace_id),
                folders,
                template: Some(base_id),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(kind: ScopeKind, id: u128, ref_key: &str, tag: &str) -> ScopedItem<&'static str> {
        // leak the tag so it is 'static for the test
        let tag: &'static str = Box::leak(tag.to_string().into_boxed_str());
        ScopedItem {
            scope: Scope {
                kind,
                id: Uuid::from_u128(id),
            },
            ref_key: ref_key.to_string(),
            item: tag,
        }
    }

    /// A binding context with the given workspace + ancestor chain (deepest
    /// folder id first) + optional template id.
    fn ctx(workspace: u128, chain: &[u128], template: Option<u128>) -> VisibleScopes {
        VisibleScopes {
            workspace: Some(Uuid::from_u128(workspace)),
            folders: chain.iter().map(|id| Uuid::from_u128(*id)).collect(),
            template: template.map(Uuid::from_u128),
        }
    }

    #[test]
    fn workspace_only_resolves() {
        let visible = ctx(1, &[], None);
        let items = vec![item(ScopeKind::Workspace, 1, "prod_db", "ws_def")];
        let resolved = resolve_visible(&visible, items).expect("no clash");
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved["prod_db"].item, "ws_def");
        assert_eq!(resolved["prod_db"].scope.kind, ScopeKind::Workspace);
    }

    #[test]
    fn deeper_folder_shadows_shallower_ancestor() {
        // chain = [deep (2), shallow (3)]; both define "steel". The deeper home
        // folder (higher depth rank) wins over its shallower ancestor.
        let visible = ctx(1, &[2, 3], None);
        let items = vec![
            item(ScopeKind::Folder, 3, "steel", "shallow_def"),
            item(ScopeKind::Folder, 2, "steel", "deep_def"),
        ];
        let resolved = resolve_visible(&visible, items).expect("no clash");
        assert_eq!(resolved["steel"].item, "deep_def");
        assert_eq!(resolved["steel"].scope.id, Uuid::from_u128(2));
    }

    #[test]
    fn ancestor_inherited_when_home_does_not_define_it() {
        // chain = [deep (2), shallow (3)]; only the shallow ancestor defines
        // "steel" — it is still inherited (visible) by the deeper context.
        let visible = ctx(1, &[2, 3], None);
        let items = vec![item(ScopeKind::Folder, 3, "steel", "shallow_def")];
        let resolved = resolve_visible(&visible, items).expect("no clash");
        assert_eq!(resolved["steel"].item, "shallow_def");
        assert_eq!(resolved["steel"].scope.id, Uuid::from_u128(3));
    }

    #[test]
    fn template_beats_deepest_folder() {
        let visible = ctx(1, &[2, 3], Some(9));
        let items = vec![
            item(ScopeKind::Workspace, 1, "prod_db", "ws_def"),
            item(ScopeKind::Folder, 3, "prod_db", "shallow_def"),
            item(ScopeKind::Folder, 2, "prod_db", "deep_def"),
            item(ScopeKind::Template, 9, "prod_db", "tpl_def"),
        ];
        let resolved = resolve_visible(&visible, items).expect("no clash");
        assert_eq!(resolved["prod_db"].item, "tpl_def");
        assert_eq!(resolved["prod_db"].scope.kind, ScopeKind::Template);
    }

    #[test]
    fn distinct_ref_keys_coexist() {
        let visible = ctx(1, &[2], None);
        let items = vec![
            item(ScopeKind::Workspace, 1, "prod_db", "ws_db"),
            item(ScopeKind::Folder, 2, "steel", "proj_steel"),
        ];
        let resolved = resolve_visible(&visible, items).expect("no clash");
        assert_eq!(resolved.len(), 2);
        assert_eq!(resolved["prod_db"].item, "ws_db");
        assert_eq!(resolved["steel"].item, "proj_steel");
    }

    #[test]
    fn resolve_visible_filters_invisible_scopes() {
        // An item owned by a folder NOT in the chain must be dropped.
        let visible = ctx(1, &[2], None);
        let items = vec![
            item(ScopeKind::Workspace, 1, "prod_db", "ws_def"),
            item(ScopeKind::Folder, 2, "steel", "visible_folder"),
            item(ScopeKind::Folder, 99, "hidden", "invisible_folder"),
        ];
        let resolved = resolve_visible(&visible, items).expect("no clash");
        assert_eq!(resolved.len(), 2);
        assert!(resolved.contains_key("prod_db"));
        assert!(resolved.contains_key("steel"));
        assert!(!resolved.contains_key("hidden"));
    }

    #[test]
    fn resolve_one_visible_picks_most_specific() {
        // chain = [deep (2), shallow (3)]; the deeper folder wins for "prod_db".
        let visible = ctx(1, &[2, 3], None);
        let items = vec![
            item(ScopeKind::Workspace, 1, "prod_db", "ws_def"),
            item(ScopeKind::Folder, 3, "prod_db", "shallow_def"),
            item(ScopeKind::Folder, 2, "prod_db", "deep_def"),
        ];
        let got = resolve_one_visible(&visible, "prod_db", items).expect("no clash");
        assert_eq!(got.unwrap().item, "deep_def");
    }

    #[test]
    fn resolve_one_visible_missing_is_none() {
        let visible = ctx(1, &[2], None);
        let items = vec![item(ScopeKind::Workspace, 1, "prod_db", "ws_def")];
        let got = resolve_one_visible(&visible, "nope", items).expect("no clash");
        assert!(got.is_none());
    }

    #[test]
    fn incomparable_clash_guard_still_fires() {
        // Defense-in-depth: two distinct Folder-scoped items whose ids are NOT
        // in the (empty) chain both score specificity 0 (the same top), so the
        // guard fires. The real entry points never produce this — `contains`
        // filters out-of-chain folders before resolution — so this exercises the
        // retained generic guard via a direct `resolve_refs_ranked` call.
        let visible = ctx(1, &[], None);
        let items = vec![
            item(ScopeKind::Folder, 10, "steel", "folderA"),
            item(ScopeKind::Folder, 11, "steel", "folderB"),
        ];
        let err = resolve_refs_ranked(&visible, items).expect_err("expected incomparable clash");
        assert_eq!(err.ref_key, "steel");
        assert_eq!(err.scopes.len(), 2);
    }
}
