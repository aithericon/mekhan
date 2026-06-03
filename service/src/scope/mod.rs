//! Scope resolution (docs/20 §2) — the single source of truth for "what is
//! visible from a binding context, and which definition wins."
//!
//! The platform hierarchy is **workspace → projects (an M:N grouping of
//! templates, *not* a tree) → templates → instances**. A resource / asset /
//! asset-type is owned by **exactly one** scope `(ScopeKind, scope_id)`.
//!
//! Resolution rules:
//! - **Visibility flows downward.** A binding inside template `T` can *see*
//!   anything owned by `T`, by any project that contains `T`, or by the
//!   workspace.
//! - **Most-specific-wins.** `template` shadows `project` shadows `workspace`
//!   for a given ref-key.
//! - **Ambiguity is a hard error.** If `T` belongs to two projects that *both*
//!   define the same ref-key, the scopes are **incomparable** → an error,
//!   never a silent pick (the platform's "compiler is the borrow-checker;
//!   ambiguity is an error, not a guess" ethos).
//!
//! This module is pure (no DB I/O): callers gather the candidate owned items
//! and the binding context's visible scope set, then call [`resolve_refs`] /
//! [`resolve_one`]. The list endpoints, the picker, and the compiler binding
//! all go through this so they cannot drift. [`visible_scopes_for`] is the DB
//! helper that turns a binding context into its downward-visible owner set.

use std::collections::BTreeMap;

use sqlx::PgPool;
use uuid::Uuid;

use crate::models::asset::ScopeKind;

/// A concrete owner scope: a `(kind, id)` pair. For `Workspace` the id is the
/// workspace id; for `Project`, the project id; for `Template`, the template's
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
    pub fn project(id: Uuid) -> Self {
        Self {
            kind: ScopeKind::Project,
            id,
        }
    }
    pub fn template(id: Uuid) -> Self {
        Self {
            kind: ScopeKind::Template,
            id,
        }
    }

    /// Precedence rank for most-specific-wins. Higher = more specific.
    /// `template (2) > project (1) > workspace (0)`.
    pub fn rank(&self) -> u8 {
        match self.kind {
            ScopeKind::Workspace => 0,
            ScopeKind::Project => 1,
            ScopeKind::Template => 2,
        }
    }
}

/// The downward-visible owner set for a binding context, plus the context
/// itself. Built by [`visible_scopes_for`]. The set is small (one workspace,
/// 0..n projects, 0..1 template).
#[derive(Debug, Clone, Default)]
pub struct VisibleScopes {
    /// The workspace owner (always present for a real binding context).
    pub workspace: Option<Uuid>,
    /// Every project that contains the context template (M:N — may be many).
    pub projects: Vec<Uuid>,
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
            ScopeKind::Project => self.projects.contains(&scope.id),
            ScopeKind::Template => self.template == Some(scope.id),
        }
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
/// single winner per ref-key, applying most-specific-wins. Returns a map
/// `ref_key -> winning item`, or the first incomparable clash encountered.
///
/// "Most-specific-wins" = the highest [`Scope::rank`] for a given ref-key wins.
/// If two items share the same ref-key AND the same (highest) rank but
/// different scope ids — e.g. two *projects* both containing the template each
/// define `steel` — that is incomparable → error.
pub fn resolve_refs<T: Clone>(
    items: Vec<ScopedItem<T>>,
) -> Result<BTreeMap<String, ScopedItem<T>>, IncomparableClash> {
    // Group by ref_key.
    let mut by_ref: BTreeMap<String, Vec<ScopedItem<T>>> = BTreeMap::new();
    for it in items {
        by_ref.entry(it.ref_key.clone()).or_default().push(it);
    }

    let mut winners: BTreeMap<String, ScopedItem<T>> = BTreeMap::new();
    for (ref_key, candidates) in by_ref {
        // Find the maximum rank among candidates for this ref-key.
        let max_rank = candidates.iter().map(|c| c.scope.rank()).max().unwrap();
        let top: Vec<&ScopedItem<T>> = candidates
            .iter()
            .filter(|c| c.scope.rank() == max_rank)
            .collect();

        // Distinct owner scopes at the top rank. More than one distinct scope
        // at the same rank = incomparable (e.g. two sibling projects).
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

/// Resolve a single ref-key against the candidate set. Convenience over
/// [`resolve_refs`] for the compiler binding path (one alias at a time).
pub fn resolve_one<T: Clone>(
    ref_key: &str,
    items: Vec<ScopedItem<T>>,
) -> Result<Option<ScopedItem<T>>, IncomparableClash> {
    let matching: Vec<ScopedItem<T>> = items
        .into_iter()
        .filter(|i| i.ref_key == ref_key)
        .collect();
    if matching.is_empty() {
        return Ok(None);
    }
    let mut resolved = resolve_refs(matching)?;
    Ok(resolved.remove(ref_key))
}

/// Filter a candidate set to only items owned by a scope in `visible`, then
/// resolve most-specific-wins. This is the one-call entry point the list
/// endpoints / picker / compiler should use.
pub fn resolve_visible<T: Clone>(
    visible: &VisibleScopes,
    items: Vec<ScopedItem<T>>,
) -> Result<BTreeMap<String, ScopedItem<T>>, IncomparableClash> {
    let filtered: Vec<ScopedItem<T>> =
        items.into_iter().filter(|i| visible.contains(&i.scope)).collect();
    resolve_refs(filtered)
}

/// DB helper: compute the downward-visible owner set for a binding context.
///
/// - `Workspace` context: visible = just that workspace.
/// - `Template` context: visible = the template's chain-root + every project
///   that contains it + the template's workspace.
/// - `Project` context: visible = the project + its workspace (used by the
///   picker when browsing project-scoped definitions directly).
///
/// `scope_id` semantics per kind: workspace id / template chain-root
/// (`base_template_id`) / project id.
pub async fn visible_scopes_for(
    db: &PgPool,
    kind: ScopeKind,
    scope_id: Uuid,
) -> Result<VisibleScopes, sqlx::Error> {
    match kind {
        ScopeKind::Workspace => Ok(VisibleScopes {
            workspace: Some(scope_id),
            projects: Vec::new(),
            template: None,
        }),
        ScopeKind::Project => {
            // project -> its workspace
            let ws: Option<(Uuid,)> =
                sqlx::query_as("SELECT workspace_id FROM projects WHERE id = $1")
                    .bind(scope_id)
                    .fetch_optional(db)
                    .await?;
            Ok(VisibleScopes {
                workspace: ws.map(|(w,)| w),
                projects: vec![scope_id],
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
                    // owner with no workspace/projects (defensive).
                    return Ok(VisibleScopes {
                        workspace: None,
                        projects: Vec::new(),
                        template: Some(scope_id),
                    });
                }
            };

            let projects: Vec<(Uuid,)> = sqlx::query_as(
                "SELECT project_id FROM project_templates WHERE base_template_id = $1",
            )
            .bind(base_id)
            .fetch_all(db)
            .await?;

            Ok(VisibleScopes {
                workspace: Some(workspace_id),
                projects: projects.into_iter().map(|(p,)| p).collect(),
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

    #[test]
    fn workspace_only_resolves() {
        let items = vec![item(ScopeKind::Workspace, 1, "prod_db", "ws_def")];
        let resolved = resolve_refs(items).expect("no clash");
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved["prod_db"].item, "ws_def");
        assert_eq!(resolved["prod_db"].scope.kind, ScopeKind::Workspace);
    }

    #[test]
    fn project_overrides_workspace() {
        // Same ref-key defined at workspace and at a (single) project: the
        // more-specific project wins.
        let items = vec![
            item(ScopeKind::Workspace, 1, "prod_db", "ws_def"),
            item(ScopeKind::Project, 2, "prod_db", "proj_def"),
        ];
        let resolved = resolve_refs(items).expect("no clash");
        assert_eq!(resolved["prod_db"].item, "proj_def");
        assert_eq!(resolved["prod_db"].scope.kind, ScopeKind::Project);
    }

    #[test]
    fn template_overrides_project_and_workspace() {
        let items = vec![
            item(ScopeKind::Workspace, 1, "prod_db", "ws_def"),
            item(ScopeKind::Project, 2, "prod_db", "proj_def"),
            item(ScopeKind::Template, 3, "prod_db", "tpl_def"),
        ];
        let resolved = resolve_refs(items).expect("no clash");
        assert_eq!(resolved["prod_db"].item, "tpl_def");
        assert_eq!(resolved["prod_db"].scope.kind, ScopeKind::Template);
    }

    #[test]
    fn two_projects_same_ref_is_incomparable_clash() {
        // Template belongs to two projects, both define `steel` — neither
        // dominates → hard error.
        let items = vec![
            item(ScopeKind::Project, 10, "steel", "projA"),
            item(ScopeKind::Project, 11, "steel", "projB"),
        ];
        let err = resolve_refs(items).expect_err("expected incomparable clash");
        assert_eq!(err.ref_key, "steel");
        assert_eq!(err.scopes.len(), 2);
    }

    #[test]
    fn clash_at_lower_rank_is_shadowed_not_an_error() {
        // Two projects define `steel`, BUT the template also defines it — the
        // template wins outright; the project ambiguity never surfaces.
        let items = vec![
            item(ScopeKind::Project, 10, "steel", "projA"),
            item(ScopeKind::Project, 11, "steel", "projB"),
            item(ScopeKind::Template, 12, "steel", "tpl_def"),
        ];
        let resolved = resolve_refs(items).expect("template shadows the project clash");
        assert_eq!(resolved["steel"].item, "tpl_def");
    }

    #[test]
    fn distinct_ref_keys_coexist() {
        let items = vec![
            item(ScopeKind::Workspace, 1, "prod_db", "ws_db"),
            item(ScopeKind::Project, 2, "steel", "proj_steel"),
        ];
        let resolved = resolve_refs(items).expect("no clash");
        assert_eq!(resolved.len(), 2);
        assert_eq!(resolved["prod_db"].item, "ws_db");
        assert_eq!(resolved["steel"].item, "proj_steel");
    }

    #[test]
    fn resolve_one_picks_most_specific() {
        let items = vec![
            item(ScopeKind::Workspace, 1, "prod_db", "ws_def"),
            item(ScopeKind::Project, 2, "prod_db", "proj_def"),
        ];
        let got = resolve_one("prod_db", items).expect("no clash");
        assert_eq!(got.unwrap().item, "proj_def");
    }

    #[test]
    fn resolve_one_missing_is_none() {
        let items = vec![item(ScopeKind::Workspace, 1, "prod_db", "ws_def")];
        let got = resolve_one("nope", items).expect("no clash");
        assert!(got.is_none());
    }

    #[test]
    fn resolve_visible_filters_invisible_scopes() {
        // An item owned by a project NOT in the visible set must be dropped.
        let visible = VisibleScopes {
            workspace: Some(Uuid::from_u128(1)),
            projects: vec![Uuid::from_u128(2)],
            template: None,
        };
        let items = vec![
            item(ScopeKind::Workspace, 1, "prod_db", "ws_def"),
            item(ScopeKind::Project, 2, "steel", "visible_proj"),
            item(ScopeKind::Project, 99, "hidden", "invisible_proj"),
        ];
        let resolved = resolve_visible(&visible, items).expect("no clash");
        assert_eq!(resolved.len(), 2);
        assert!(resolved.contains_key("prod_db"));
        assert!(resolved.contains_key("steel"));
        assert!(!resolved.contains_key("hidden"));
    }
}
