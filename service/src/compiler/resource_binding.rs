//! Workspace-resource head discovery for AutomatedSteps.
//!
//! Drives the publisher's "which resources does this step need" pass via
//! [`collect_resource_heads`]. Source of truth is the backend registry
//! (`crate::backends`):
//!
//! - `BackendDecl::resource_alias_paths` — static JSON paths where the
//!   step's config stores a resource alias (e.g. `["resource_alias"]`,
//!   `["storage", "resource_alias"]`).
//! - `BackendDecl::ref_scanner` — dynamic `<head>.<attr>` scanner whose
//!   emitted heads might resolve to either graph slugs or workspace
//!   resources; the caller (validate_resource_refs, publish handler)
//!   filters by namespace.
//!
//! Phase 3 collapse — the legacy `ResourceBindingDecl` / `BINDINGS` /
//! `python_scanner` / `smtp_scanner` are gone; the registry covers the
//! same surface with one source of truth.

use serde_json::Value;

use crate::backends::ScanCtx;
use crate::models::template::ExecutionBackendType;

/// Collect every workspace resource head a step might reference. Empty
/// alias strings are filtered. Unknown heads (not registered in the
/// workspace `resources` table) are the caller's concern — this just
/// returns the surface set.
pub(crate) fn collect_resource_heads(
    ctx: &ScanCtx<'_>,
    backend_type: ExecutionBackendType,
) -> Vec<String> {
    let Some(decl) = crate::backends::lookup(backend_type) else {
        return Vec::new();
    };
    let mut heads = Vec::new();
    for path in decl.resource_alias_paths {
        if let Some(alias) = extract_str_at_path(ctx.config, path) {
            if !alias.is_empty() {
                heads.push(alias);
            }
        }
    }
    if let Some(scanner) = decl.ref_scanner {
        for r in scanner(ctx) {
            heads.push(r.head);
        }
    }
    heads
}

fn extract_str_at_path(v: &Value, path: &[&str]) -> Option<String> {
    let mut cur = v;
    for key in path {
        cur = cur.get(*key)?;
    }
    cur.as_str().map(String::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::HashMap;

    fn empty_ctx<'a>(config: &'a Value) -> ScanCtx<'a> {
        static EMPTY: std::sync::OnceLock<HashMap<String, HashMap<String, String>>> =
            std::sync::OnceLock::new();
        ScanCtx {
            config,
            node_id: "",
            inline_sources: EMPTY.get_or_init(HashMap::new),
            entrypoint: None,
        }
    }

    #[test]
    fn extracts_top_level_alias() {
        let cfg = json!({ "resource_alias": "openai_prod" });
        let heads = collect_resource_heads(&empty_ctx(&cfg), ExecutionBackendType::Llm);
        assert_eq!(heads, vec!["openai_prod".to_string()]);
    }

    #[test]
    fn empty_alias_filtered() {
        let cfg = json!({ "resource_alias": "" });
        let heads = collect_resource_heads(&empty_ctx(&cfg), ExecutionBackendType::Llm);
        assert!(heads.is_empty());
    }

    #[test]
    fn missing_alias_no_heads() {
        let cfg = json!({});
        let heads = collect_resource_heads(&empty_ctx(&cfg), ExecutionBackendType::Llm);
        assert!(heads.is_empty());
    }

    #[test]
    fn unresourced_backend_no_heads() {
        // Process has no resource_alias_paths and no ref_scanner — no
        // resource heads expected.
        let cfg = json!({ "resource_alias": "x" });
        let heads = collect_resource_heads(&empty_ctx(&cfg), ExecutionBackendType::Process);
        assert!(heads.is_empty());
    }

    #[test]
    fn smtp_combines_alias_and_template_refs() {
        let cfg = json!({
            "resource_alias": "mail",
            "subject": { "source": "Hello from {{ greeter.name }}" },
            "body_text": { "source": "" },
            "body_html": { "source": "" },
            "to": ["{{ recipient.email }}"],
        });
        let heads = collect_resource_heads(&empty_ctx(&cfg), ExecutionBackendType::Smtp);
        let mut sorted: Vec<String> = heads;
        sorted.sort();
        assert_eq!(
            sorted,
            vec!["greeter".to_string(), "mail".to_string(), "recipient".to_string()]
        );
    }

    #[test]
    fn file_ops_picks_up_storage_alias_on_single_storage_op() {
        let cfg = json!({
            "operation": "list",
            "storage": { "resource_alias": "minio_dev" }
        });
        let heads = collect_resource_heads(&empty_ctx(&cfg), ExecutionBackendType::FileOps);
        assert_eq!(heads, vec!["minio_dev".to_string()]);
    }

    #[test]
    fn file_ops_picks_up_both_aliases_on_copy() {
        let cfg = json!({
            "operation": "copy",
            "source_storage": { "resource_alias": "src_bucket" },
            "destination_storage": { "resource_alias": "dst_bucket" }
        });
        let mut heads = collect_resource_heads(&empty_ctx(&cfg), ExecutionBackendType::FileOps);
        heads.sort();
        assert_eq!(
            heads,
            vec!["dst_bucket".to_string(), "src_bucket".to_string()]
        );
    }

    #[test]
    fn file_ops_ignores_unset_destination_alias() {
        let cfg = json!({
            "operation": "copy",
            "source_storage": { "resource_alias": "src" },
            "destination_storage": { "endpoint": "/local" }
        });
        let heads = collect_resource_heads(&empty_ctx(&cfg), ExecutionBackendType::FileOps);
        assert_eq!(heads, vec!["src".to_string()]);
    }

    #[test]
    fn python_scanner_uses_inline_source() {
        let cfg = json!({});
        let mut sources: HashMap<String, HashMap<String, String>> = HashMap::new();
        let mut files = HashMap::new();
        files.insert("main.py".to_string(), "x = local_pg.host".to_string());
        sources.insert("node-1".to_string(), files);
        let ctx = ScanCtx {
            config: &cfg,
            node_id: "node-1",
            inline_sources: &sources,
            entrypoint: Some("main.py"),
        };
        let heads = collect_resource_heads(&ctx, ExecutionBackendType::Python);
        assert!(heads.contains(&"local_pg".to_string()));
    }
}
