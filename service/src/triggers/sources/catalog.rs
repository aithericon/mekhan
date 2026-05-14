//! Catalog trigger source (Phase 5c).
//!
//! Catalog triggers are an *additional* authoring surface that lives alongside
//! the engine's runtime `catalogue_subscribe` effect handler (ADR 17). Both
//! surfaces produce signals through the same `SubscriptionManager` /
//! `CatalogueEntry` source of truth; this module just adds a static,
//! template-authored entry point that does not require modeling a startup
//! transition in the workflow itself.
//!
//! Static catalog triggers earn their place for the common case where the
//! filter values are known at authoring time — "fire whenever a `category =
//! receipt` artifact appears". The dynamic `catalogue_subscribe` effect
//! remains the right answer when filters depend on runtime token data.
//!
//! Phase 5c scope: filter evaluation, fan-out to spawn (Start target) and
//! in-flight signal (any other target). The dispatcher's `fire` path already
//! handles both cases via the trigger's resolved `kind` (Spawn vs Signal), so
//! this module is mostly a filter walker that calls `fire`.

use std::collections::HashMap;

use serde_json::json;

use crate::catalogue::model::CatalogueEntry;
use crate::models::template::TriggerSource;
use crate::triggers::dispatcher::TriggerDispatcher;

/// Walk every registered Catalog trigger and fire those whose filters match
/// `entry`. Called from the causality ingest pipeline on each new artifact.
pub async fn evaluate(dispatcher: &TriggerDispatcher, entry: &CatalogueEntry) {
    for rec in dispatcher.list_all() {
        if !rec.enabled {
            continue;
        }
        let TriggerSource::Catalog(cat) = &rec.source else {
            continue;
        };
        if !matches_filters(&cat.filters, entry) {
            continue;
        }

        // Payload scope: `payload.catalogue_entry` plus a hoisted `payload.category`
        // for the common case where authors just want to map the kind into a
        // typed field. The mapping expression can access any field of the
        // entry via `payload.catalogue_entry.<field>`.
        let payload = json!({
            "catalogue_entry": entry,
            "category": entry.category,
            "name": entry.name,
            "filename": entry.filename,
            "fire_time": chrono::Utc::now().to_rfc3339(),
        });

        match dispatcher.fire(&rec.node_id, payload).await {
            Ok(result) => {
                tracing::info!(
                    node_id = %rec.node_id,
                    artifact_id = %entry.id,
                    outcome = ?result.outcome,
                    "catalog trigger fired"
                );
            }
            Err(e) => {
                tracing::warn!(
                    node_id = %rec.node_id,
                    artifact_id = %entry.id,
                    "catalog trigger fire failed: {e}"
                );
            }
        }
    }
}

/// Re-implementation of `subscriptions::matches_filters` over the same wire
/// shape but accepting the dispatcher-side `HashMap` directly. Currently only
/// `eq` is supported, matching the existing subscription contract.
fn matches_filters(
    filters: &HashMap<String, HashMap<String, String>>,
    entry: &CatalogueEntry,
) -> bool {
    for (field, ops) in filters {
        for (operator, expected) in ops {
            if operator != "eq" {
                tracing::debug!(field, operator, "unsupported filter operator");
                return false;
            }
            let actual: Option<&str> = match field.as_str() {
                "category" => Some(&entry.category),
                "source_net" => entry.source_net.as_deref(),
                "source_place" => entry.source_place.as_deref(),
                "process_id" => entry.process_id.as_deref(),
                "process_step" => entry.process_step.as_deref(),
                "name" => Some(&entry.name),
                "filename" => Some(&entry.filename),
                _ => return false,
            };
            match actual {
                Some(v) if v == expected.as_str() => {}
                _ => return false,
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry() -> CatalogueEntry {
        CatalogueEntry {
            id: "art-1".to_string(),
            execution_id: "exec-1".to_string(),
            job_id: None,
            name: "Receipt 1".to_string(),
            category: "receipt".to_string(),
            filename: "r.pdf".to_string(),
            mime_type: Some("application/pdf".to_string()),
            size_bytes: Some(1234),
            storage_path: Some("bucket/key".to_string()),
            source_net: Some("mekhan-1".to_string()),
            source_place: Some("p_out".to_string()),
            signal_key: None,
            process_id: None,
            process_step: None,
            source_event_sequence: None,
            file_metadata: serde_json::Value::Null,
            user_metadata: serde_json::Value::Null,
            created_at: chrono::Utc::now(),
            catalogued_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn matches_when_filter_equals_field() {
        let mut filters = HashMap::new();
        let mut eq = HashMap::new();
        eq.insert("eq".to_string(), "receipt".to_string());
        filters.insert("category".to_string(), eq);
        assert!(matches_filters(&filters, &entry()));
    }

    #[test]
    fn no_match_when_value_differs() {
        let mut filters = HashMap::new();
        let mut eq = HashMap::new();
        eq.insert("eq".to_string(), "invoice".to_string());
        filters.insert("category".to_string(), eq);
        assert!(!matches_filters(&filters, &entry()));
    }

    #[test]
    fn no_match_on_unsupported_operator() {
        let mut filters = HashMap::new();
        let mut ne = HashMap::new();
        ne.insert("ne".to_string(), "invoice".to_string());
        filters.insert("category".to_string(), ne);
        assert!(!matches_filters(&filters, &entry()));
    }

    #[test]
    fn empty_filters_match_everything() {
        let filters = HashMap::new();
        assert!(matches_filters(&filters, &entry()));
    }
}
