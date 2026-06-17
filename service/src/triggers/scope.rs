//! Per-source Rhai scope contract for trigger payload mappings.
//!
//! Each [`TriggerSource`] exposes a fixed set of named identifiers (with
//! declared [`FieldKind`]s) that a `Trigger.payload_mapping` expression may
//! reference. This is the single source of truth consumed by:
//!   - the compiler (`validate_triggers`) — every referenced root identifier
//!     must be one of these names, else a compile error;
//!   - the dispatcher (`evaluate_mapping`) — binds exactly these names as
//!     top-level Rhai variables from the flat scope map each source emits;
//!   - the editor — surfaces the list under each mapping expression.
//!
//! The flat scope map a source emits at fire time MUST be keyed by exactly
//! these names. Keep `source_scope` and the `sources::*` payload construction
//! in lockstep.

use utoipa::ToSchema;

use crate::models::template::{FieldKind, TaskFieldKind, TriggerSource};

/// One identifier available to a trigger's payload-mapping expressions.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, ToSchema)]
pub struct ScopeVar {
    /// Rhai identifier the expression references (e.g. `fire_time`).
    pub name: String,
    /// Declared kind. Advisory for the editor; the fire path validates the
    /// produced token against the *target port*, not against this.
    pub kind: FieldKind,
}

/// JSON Schema for a trigger scope — an object whose properties are the
/// scope vars' base types. `additionalProperties` is intentionally `true`:
/// trigger scopes (especially webhook `payload`/`headers`/`query`) are loose
/// Rhai-projected bags, and fire-time validation happens against the target
/// port, not this scope. Nothing is marked required (a `ScopeVar` carries no
/// required flag).
pub fn scope_json_schema(vars: &[ScopeVar]) -> serde_json::Value {
    use serde_json::json;
    let properties: serde_json::Map<String, serde_json::Value> = vars
        .iter()
        .map(|v| (v.name.clone(), v.kind.base_schema()))
        .collect();
    json!({
        "type": "object",
        "properties": properties,
        "additionalProperties": true,
    })
}

fn var(name: &str, kind: FieldKind) -> ScopeVar {
    ScopeVar {
        name: name.to_string(),
        kind,
    }
}

fn task_kind_to_field_kind(k: TaskFieldKind) -> FieldKind {
    // Single source of truth: the `From<TaskFieldKind> for FieldKind` impl
    // on `crate::models::template`. Keeps the trigger scope mapping in
    // sync when a new TaskFieldKind variant lands.
    FieldKind::from(k)
}

/// Scope for the source *kinds* whose identifier set is fixed regardless of
/// config (everything except `Manual`, whose scope is its form). Single source
/// of truth shared by [`source_scope`] and [`scope_for_kind`].
fn static_scope(kind: &str) -> Vec<ScopeVar> {
    match kind {
        "cron" => vec![
            var("fire_time", FieldKind::Timestamp),
            var("scheduled_time", FieldKind::Timestamp),
        ],
        // Every column of `catalogue::model::CatalogueEntry`, flattened to the
        // top level, plus the whole entry as a `Json` escape hatch and the
        // dispatch `fire_time`. Keep in sync with `sources::catalog`.
        "catalog" => vec![
            var("id", FieldKind::Text),
            var("execution_id", FieldKind::Text),
            var("job_id", FieldKind::Text),
            var("name", FieldKind::Text),
            var("category", FieldKind::Text),
            var("filename", FieldKind::Text),
            var("mime_type", FieldKind::Text),
            var("size_bytes", FieldKind::Number),
            var("storage_path", FieldKind::Text),
            var("source_net", FieldKind::Text),
            var("source_place", FieldKind::Text),
            var("signal_key", FieldKind::Text),
            var("process_id", FieldKind::Text),
            var("process_step", FieldKind::Text),
            var("source_event_sequence", FieldKind::Number),
            var("file_metadata", FieldKind::Json),
            var("user_metadata", FieldKind::Json),
            var("created_at", FieldKind::Timestamp),
            var("catalogued_at", FieldKind::Timestamp),
            var("catalogue_entry", FieldKind::Json),
            var("fire_time", FieldKind::Timestamp),
        ],
        "net_completion" => vec![
            var("source_instance_id", FieldKind::Text),
            var("source_template_id", FieldKind::Text),
            var("source_version", FieldKind::Number),
            var("completion_status", FieldKind::Text),
            var("completion_time", FieldKind::Timestamp),
            // `final_token: Json` (proposal §4.3) is intentionally absent —
            // recovering it means walking the completed instance's event log.
            // Add it here and in `sources::net_completion` together.
        ],
        "webhook" => vec![
            var("payload", FieldKind::Json),
            var("headers", FieldKind::Json),
            var("query", FieldKind::Json),
            var("fire_time", FieldKind::Timestamp),
        ],
        // `manual` is form-dependent (see `source_scope`); unknown kinds empty.
        _ => vec![],
    }
}

/// The fixed identifier set available to a trigger's `payload_mapping`
/// expressions. Single source of truth for compiler validation, the dispatcher
/// scope binding, and the editor hint.
pub fn source_scope(source: &TriggerSource) -> Vec<ScopeVar> {
    match source {
        TriggerSource::Manual(m) => m
            .form
            .iter()
            .map(|f| var(&f.name, task_kind_to_field_kind(f.kind)))
            .collect(),
        other => static_scope(other.kind()),
    }
}

/// Scope for a source-kind string. Used by the editor-hint endpoint. `manual`
/// returns empty here because its scope is the (client-side) form schema; the
/// editor derives that locally rather than round-tripping.
pub fn scope_for_kind(kind: &str) -> Vec<ScopeVar> {
    static_scope(kind)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::template::{
        CatalogTrigger, CronTrigger, ManualTrigger, TaskFieldConfig, WebhookAuth, WebhookTrigger,
    };

    fn names(s: &[ScopeVar]) -> Vec<&str> {
        s.iter().map(|v| v.name.as_str()).collect()
    }

    #[test]
    fn cron_scope() {
        let s = source_scope(&TriggerSource::Cron(CronTrigger {
            schedule: "0 9 * * *".into(),
            timezone: "UTC".into(),
            jitter_secs: 0,
            catchup: Default::default(),
        }));
        assert_eq!(names(&s), vec!["fire_time", "scheduled_time"]);
    }

    #[test]
    fn webhook_scope_has_unnested_payload() {
        let s = source_scope(&TriggerSource::Webhook(WebhookTrigger {
            slug: "x".into(),
            auth: WebhookAuth::None,
            require_method: None,
        }));
        assert_eq!(names(&s), vec!["payload", "headers", "query", "fire_time"]);
    }

    #[test]
    fn catalog_scope_flattens_entry_fields_plus_escape_hatch() {
        let s = source_scope(&TriggerSource::Catalog(CatalogTrigger {
            query: String::new(),
            backfill: false,
        }));
        let n = names(&s);
        assert!(n.contains(&"category"));
        assert!(n.contains(&"filename"));
        assert!(n.contains(&"catalogue_entry"));
        assert!(n.contains(&"fire_time"));
    }

    #[test]
    fn manual_scope_derives_from_form() {
        let s = source_scope(&TriggerSource::Manual(ManualTrigger {
            form: vec![
                TaskFieldConfig {
                    name: "customer".into(),
                    label: "Customer".into(),
                    kind: TaskFieldKind::Text,
                    required: Some(true),
                    ..TaskFieldConfig::default()
                },
                TaskFieldConfig {
                    name: "urgent".into(),
                    label: "Urgent".into(),
                    kind: TaskFieldKind::Checkbox,
                    ..TaskFieldConfig::default()
                },
            ],
        }));
        assert_eq!(names(&s), vec!["customer", "urgent"]);
        assert!(matches!(s[1].kind, FieldKind::Bool));
    }

    #[test]
    fn scope_json_schema_is_loose_object() {
        let vars = vec![
            var("fire_time", FieldKind::Timestamp),
            var("payload", FieldKind::Json),
            var("count", FieldKind::Number),
        ];
        let schema = scope_json_schema(&vars);
        assert_eq!(schema["type"], serde_json::json!("object"));
        // Loose bag: extra keys allowed, nothing required.
        assert_eq!(schema["additionalProperties"], serde_json::json!(true));
        assert!(schema.get("required").is_none());
        let props = schema["properties"].as_object().unwrap();
        assert!(props.contains_key("fire_time"));
        assert!(props.contains_key("payload"));
        assert!(props.contains_key("count"));
        assert_eq!(props["fire_time"]["format"], serde_json::json!("date-time"));
        assert_eq!(props["count"]["type"], serde_json::json!("number"));
    }
}
