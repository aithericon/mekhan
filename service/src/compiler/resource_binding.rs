//! Declarative registry of which AutomatedStep backends bind to workspace
//! resources, and how.
//!
//! Replaces the per-backend match arms that used to live in `publish.rs`
//! (`discover_known_resources`) and `token_shape.rs`
//! (`automated_step_resource_borrow_plan`). Adding a new resource-bound
//! backend is one [`ResourceBindingDecl`] entry.
//!
//! Two reference kinds are expressible:
//! - `alias_paths` — the alias lives at a static path in the step's config
//!   JSON (e.g. `resource_alias`, or `source_storage.resource_alias`).
//!   Picked up by [`collect_resource_heads`] with no scanner.
//! - `extra_scanner` — for backends whose resource references aren't a
//!   single field lookup. Python scans its `<name>.<attr>` accesses;
//!   SMTP scans Tera placeholders across template surfaces.

use std::collections::HashMap;

use serde_json::Value;

use crate::models::template::ExecutionBackendType;

pub(crate) struct ResourceBindingDecl {
    pub backend_type: ExecutionBackendType,
    pub alias_paths: &'static [&'static [&'static str]],
    pub extra_scanner: Option<ExtraScanner>,
}

pub(crate) type ExtraScanner = fn(&ScanCtx<'_>) -> Vec<String>;

pub(crate) struct ScanCtx<'a> {
    pub config: &'a Value,
    pub node_id: &'a str,
    pub inline_sources: &'a HashMap<String, HashMap<String, String>>,
    pub entrypoint: Option<&'a str>,
}

const SMTP_PATHS: &[&[&str]] = &[&["resource_alias"]];
const LLM_PATHS: &[&[&str]] = &[&["resource_alias"]];

// File-ops binds resources at the StorageConfig level. The op variants
// each carry one or two StorageConfig fields; absent paths are no-ops, so
// this single declaration covers every variant.
const FILE_OPS_PATHS: &[&[&str]] = &[
    &["storage", "resource_alias"],
    &["source_storage", "resource_alias"],
    &["destination_storage", "resource_alias"],
];

fn smtp_scanner(ctx: &ScanCtx<'_>) -> Vec<String> {
    use crate::compiler::token_shape::smtp_template_placeholder_refs;
    smtp_template_placeholder_refs(ctx.config)
        .into_iter()
        .map(|(head, _)| head)
        .collect()
}

fn python_scanner(ctx: &ScanCtx<'_>) -> Vec<String> {
    use crate::compiler::python_refs::extract_python_refs;
    let entrypoint = ctx.entrypoint.unwrap_or("main.py");
    let Some(node_files) = ctx.inline_sources.get(ctx.node_id) else {
        return Vec::new();
    };
    let Some(source) = node_files.get(entrypoint) else {
        return Vec::new();
    };
    extract_python_refs(source)
        .into_iter()
        .map(|r| r.head)
        .collect()
}

pub(crate) const BINDINGS: &[ResourceBindingDecl] = &[
    ResourceBindingDecl {
        backend_type: ExecutionBackendType::Python,
        alias_paths: &[],
        extra_scanner: Some(python_scanner),
    },
    ResourceBindingDecl {
        backend_type: ExecutionBackendType::Smtp,
        alias_paths: SMTP_PATHS,
        extra_scanner: Some(smtp_scanner),
    },
    ResourceBindingDecl {
        backend_type: ExecutionBackendType::Llm,
        alias_paths: LLM_PATHS,
        extra_scanner: None,
    },
    ResourceBindingDecl {
        backend_type: ExecutionBackendType::FileOps,
        alias_paths: FILE_OPS_PATHS,
        extra_scanner: None,
    },
];

/// Collect every workspace resource head a step might reference. Empty
/// alias strings are filtered. Unknown heads (not registered in the
/// workspace `resources` table) are the caller's concern — this just
/// returns the surface set.
pub(crate) fn collect_resource_heads(
    ctx: &ScanCtx<'_>,
    backend_type: ExecutionBackendType,
) -> Vec<String> {
    let Some(decl) = BINDINGS.iter().find(|d| d.backend_type == backend_type) else {
        return Vec::new();
    };
    let mut heads = Vec::new();
    for path in decl.alias_paths {
        if let Some(alias) = extract_str_at_path(ctx.config, path) {
            if !alias.is_empty() {
                heads.push(alias);
            }
        }
    }
    if let Some(scanner) = decl.extra_scanner {
        heads.extend(scanner(ctx));
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
    fn unknown_backend_no_heads() {
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
