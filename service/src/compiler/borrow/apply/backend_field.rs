//! LLM / Kreuzberg apply arm: per-field staging + `{{<slug>.<attr>}}`
//! placeholder rewrite in the embedded config blob.

use std::collections::HashMap;

use aithericon_sdk::scenario::{ScenarioDefinition, TransitionLogic};

use crate::compiler::borrow::apply::{
    borrow_input_name, find_prepare_transition_mut, rewrite_placeholders_in_value, sanitize_ident,
};
use crate::compiler::borrow::shape::{Borrow, BorrowResolution, BORROW_MARKER};
use crate::compiler::compile::wire_read_arc;
use crate::compiler::interface::InterfaceRegistry;
use crate::models::template::FieldKind;

/// Apply the LLM / Kreuzberg arm. Per-consumer: dedupe by `(slug, attr)`
/// (multiple placeholder occurrences for the same field stage a single
/// file); find the prepare transition; for each unique key, wire the
/// read-arc, emit a per-field `job_inputs.push` (Raw vs StoragePath vs
/// inline based on path-site + field kind), and rewrite each
/// `{{<slug>.<attr>}}` placeholder in the embedded config Rhai literal
/// to the executor-resolver form (`{{input:NAME}}` for content sites,
/// `{{input_path:NAME}}` for path sites).
pub(crate) fn apply_backend_borrows(
    scenario: &mut ScenarioDefinition,
    interfaces: &InterfaceRegistry,
    consumer_id: &str,
    consumer_borrows: &[Borrow],
    node_configs: &mut HashMap<String, serde_json::Value>,
) {
    let mut seen: std::collections::BTreeSet<(String, String)> = std::collections::BTreeSet::new();
    let mut unique: Vec<&Borrow> = Vec::new();
    for b in consumer_borrows {
        if let BorrowResolution::BackendFieldStage { attr, .. } = &b.resolution {
            if seen.insert((b.slug.clone(), attr.clone())) {
                unique.push(b);
            }
        }
    }

    let Some(t) = find_prepare_transition_mut(scenario, consumer_id) else {
        return;
    };
    let mut pushes = String::new();
    for b in &unique {
        let BorrowResolution::BackendFieldStage {
            attr,
            is_path_site,
            field_kind,
        } = &b.resolution
        else {
            continue;
        };
        let Some(var) = wire_read_arc(t, &b.producer_node, interfaces, true) else {
            continue;
        };

        // Build the Rhai accessor that reaches the producer's field.
        // The envelope nests business data under `data.<attr>`
        // (HumanTask) or `detail.outputs.<attr>` (AutomatedStep);
        // other producer kinds (Start, Loop, SubWorkflow) keep the
        // field at top-level. Same hoist logic as the Python arm's
        // `__h_<producer>` walker, condensed via null-safe `__pluck`.
        let mut path_segs: Vec<String> = interfaces
            .get(&b.producer_node)
            .map(|i| i.kind.hoist_path())
            .unwrap_or(&[])
            .iter()
            .map(|seg| format!("\"{seg}\""))
            .collect();
        path_segs.push(format!("\"{}\"", attr.replace('"', "\\\"")));
        let value_expr = format!("__pluck({var}, [{}])", path_segs.join(", "));

        let input_name = borrow_input_name(&b.slug, attr);

        if *is_path_site && *field_kind == FieldKind::File {
            // Producer field is a FileRef; stage StoragePath so the
            // storage hook downloads the binary into the run dir. The
            // executor's global ArtifactStore concatenates `path` with
            // its configured prefix, so `path` must be the S3 object
            // key (`templates/{id}/blobs/{node_id}/{filename}`) — not
            // the platform-facing URL (`/api/v1/files/<key>`), which would
            // 404 against S3. The `storage` key is *omitted* so the
            // input falls through to the global store; emitting an
            // empty `{}` would deserialize as a partial `StorageConfig`
            // and fail with "missing field `backend`" (the executor
            // domain's `StorageConfig` requires `backend` + `endpoint`).
            let key_segs: Vec<String> = path_segs
                .iter()
                .cloned()
                .chain(std::iter::once("\"key\"".to_string()))
                .collect();
            let key_expr = format!("__pluck({var}, [{}])", key_segs.join(", "));
            pushes.push_str(&format!(
                r#"job_inputs.push(#{{ "name": "{input_name}", "source": #{{ "type": "storage_path", "path": {key_expr} }} }}); "#,
            ));
        } else if *is_path_site {
            // Path-site with non-File producer: stringify the value
            // into a Raw temp file. Kreuzberg with a text upstream
            // (e.g. an LLM narrative output) lands here.
            pushes.push_str(&format!(
                r#"let __c_{slug}_{attr_id} = {value_expr}; if type_of(__c_{slug}_{attr_id}) != "string" {{ __c_{slug}_{attr_id} = to_string(__c_{slug}_{attr_id}); }} job_inputs.push(#{{ "name": "{input_name}", "source": #{{ "type": "raw", "content": __c_{slug}_{attr_id} }} }}); "#,
                slug = sanitize_ident(&b.slug),
                attr_id = sanitize_ident(attr),
                value_expr = value_expr,
                input_name = input_name,
            ));
        } else {
            // Content-site (LLM prompt/system_prompt/history). Stage
            // inline { value } so the executor's `{{input:NAME}}`
            // resolver loads it as the right type.
            pushes.push_str(&format!(
                r#"let __c_{slug}_{attr_id} = {value_expr}; job_inputs.push(#{{ "name": "{input_name}", "source": #{{ "type": "inline", "value": __c_{slug}_{attr_id} }} }}); "#,
                slug = sanitize_ident(&b.slug),
                attr_id = sanitize_ident(attr),
                value_expr = value_expr,
                input_name = input_name,
            ));
        }
    }

    if let TransitionLogic::Rhai { source } = &t.logic {
        if source.contains(BORROW_MARKER) {
            // Prepend pushes before the marker; subsequent arms can
            // still splice. `strip_borrow_markers` cleans up later.
            let replacement = format!("{pushes}{BORROW_MARKER}");
            let new_source = source.replace(BORROW_MARKER, &replacement);
            // Side-channel placeholder rewrite: the same
            // `{{<slug>.<attr>}}` → `{{input:NAME}}` substitution that
            // used to run against the inlined Rhai literal now runs
            // against the parked JSON config blob. Walks every string
            // value of the consumer's `node_configs[consumer_id]`
            // entry. The Rhai source itself is left alone — it
            // references the config by `config_ref { storage_path }`
            // now, so there's no inline literal to rewrite.
            if let Some(config_value) = node_configs.get_mut(consumer_id) {
                for b in &unique {
                    let BorrowResolution::BackendFieldStage {
                        attr, is_path_site, ..
                    } = &b.resolution
                    else {
                        continue;
                    };
                    let input_name = borrow_input_name(&b.slug, attr);
                    let resolver_prefix = if *is_path_site { "input_path" } else { "input" };
                    let replacement = format!("{{{{{resolver_prefix}:{input_name}}}}}");
                    rewrite_placeholders_in_value(config_value, &b.slug, attr, &replacement);
                }
            }
            t.logic = TransitionLogic::Rhai { source: new_source };
        }
    }
}
