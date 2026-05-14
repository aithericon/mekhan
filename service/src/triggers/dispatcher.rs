//! Trigger dispatcher core. Owns the in-memory index of registered triggers
//! and the fan-out logic that routes a fire to the right outcome (spawn an
//! instance vs publish a signal). Background sources (cron, catalog,
//! lifecycle, webhook) hang off the same dispatcher in subsequent sub-phases.

use std::sync::Arc;

use chrono::Utc;
use dashmap::DashMap;
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

use crate::models::instance::{StartToken, WorkflowInstance};
use crate::models::template::{WorkflowGraph, WorkflowNodeData, WorkflowTemplate};
use crate::nats::MekhanNats;
use crate::petri::client::PetriClient;
use crate::petri::instance::{deploy_instance, parameterize_air};

use super::model::{
    locate_trigger, FireOutcome, FireResult, TriggerError, TriggerKind, TriggerLocator,
    TriggerRecord,
};

/// In-memory registry of triggers across all published templates plus the
/// runtime collaborators needed to fire one. Cheap to clone (everything is
/// `Arc`/handle-shaped). Held in `AppState`.
pub struct TriggerDispatcher {
    /// Keyed by `node_id`. Trigger node ids should be globally unique within
    /// a deployment because the editor stamps fresh UUID-like ids; if two
    /// published templates happen to collide we keep the latest registered.
    triggers: DashMap<String, TriggerRecord>,
    db: PgPool,
    petri: PetriClient,
    nats: MekhanNats,
    /// Last N fire results, keyed by `node_id`. Bounded per-trigger to keep
    /// memory predictable. The history endpoint (Phase 5f) serves from here.
    history: DashMap<String, Vec<FireResult>>,
    /// Per-source-kind fire counter for observability (Phase 5f). Monotonic
    /// since boot; the metrics endpoint exposes raw counts.
    metrics: DashMap<String, FireMetrics>,
}

#[derive(Debug, Default, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct FireMetrics {
    pub fires: u64,
    pub spawned: u64,
    pub signaled: u64,
    pub no_targets: u64,
    pub dropped: u64,
    pub errors: u64,
}

impl TriggerDispatcher {
    pub fn new(db: PgPool, petri: PetriClient, nats: MekhanNats) -> Self {
        Self {
            triggers: DashMap::new(),
            db,
            petri,
            nats,
            history: DashMap::new(),
            metrics: DashMap::new(),
        }
    }

    /// Snapshot of fire counters per source kind. Cheap clone.
    pub fn metrics_snapshot(&self) -> std::collections::HashMap<String, FireMetrics> {
        self.metrics
            .iter()
            .map(|r| (r.key().clone(), r.value().clone()))
            .collect()
    }

    fn record_metric(&self, source_kind: &str, outcome: &FireOutcome, errored: bool) {
        let mut entry = self.metrics.entry(source_kind.to_string()).or_default();
        entry.fires += 1;
        if errored {
            entry.errors += 1;
            return;
        }
        match outcome {
            FireOutcome::Spawned { .. } => entry.spawned += 1,
            FireOutcome::Signaled { .. } => entry.signaled += 1,
            FireOutcome::NoTargets => entry.no_targets += 1,
            FireOutcome::Dropped { .. } => entry.dropped += 1,
        }
    }

    /// Scan every published template and (re)register its triggers. Called
    /// once at startup and after every publish (the templates handler will
    /// invoke `register_template` directly to avoid re-scanning all templates).
    pub async fn hydrate(&self) -> Result<usize, TriggerError> {
        let templates: Vec<WorkflowTemplate> = sqlx::query_as::<_, WorkflowTemplate>(
            "SELECT * FROM workflow_templates WHERE published = true AND is_latest = true",
        )
        .fetch_all(&self.db)
        .await
        .map_err(|e| TriggerError::Database(e.to_string()))?;

        let mut count = 0;
        for tpl in &templates {
            count += self.register_template(tpl).await;
        }
        tracing::info!(count, "trigger dispatcher hydrated");
        Ok(count)
    }

    /// Register every trigger node found in a template's `graph_json`.
    /// Returns the number of triggers registered.
    pub async fn register_template(&self, template: &WorkflowTemplate) -> usize {
        let graph: WorkflowGraph = match serde_json::from_value(template.graph.clone()) {
            Ok(g) => g,
            Err(e) => {
                tracing::warn!(
                    template_id = %template.id,
                    "failed to deserialize template graph during trigger registration: {e}"
                );
                return 0;
            }
        };

        // First clear any prior records for this template/version so that
        // editing a trigger in place doesn't leak the old config.
        self.triggers.retain(|_, rec| {
            !(rec.template_id == template.id && rec.template_version == template.template_version_or_zero())
        });

        let mut registered = 0;
        for node in &graph.nodes {
            let WorkflowNodeData::Trigger {
                source,
                enabled,
                ..
            } = &node.data
            else {
                continue;
            };
            let Some((_, edge)) = locate_trigger(&graph.nodes, &graph.edges, &node.id) else {
                tracing::warn!(
                    template_id = %template.id,
                    node_id = %node.id,
                    "trigger has no outgoing edge — skipping registration"
                );
                continue;
            };

            let Some(target_node) = graph.nodes.iter().find(|n| n.id == edge.target) else {
                continue;
            };
            let kind = match target_node.data {
                WorkflowNodeData::Start { .. } => TriggerKind::Spawn,
                _ => TriggerKind::Signal,
            };
            let target_handle = edge
                .target_handle
                .clone()
                .unwrap_or_else(|| "in".to_string());

            let record = TriggerRecord {
                template_id: template.id,
                template_version: template.version,
                node_id: node.id.clone(),
                kind,
                target_node_id: target_node.id.clone(),
                target_handle,
                source: source.clone(),
                enabled: *enabled,
                registered_at: Utc::now(),
            };
            self.triggers.insert(node.id.clone(), record);
            registered += 1;
        }
        if registered > 0 {
            tracing::info!(
                template_id = %template.id,
                registered,
                "registered triggers for template"
            );
        }
        registered
    }

    /// Remove every trigger associated with a template (called on unpublish /
    /// version supersede).
    pub fn forget_template(&self, template_id: Uuid) {
        let before = self.triggers.len();
        self.triggers
            .retain(|_, rec| rec.template_id != template_id);
        let after = self.triggers.len();
        if before != after {
            tracing::info!(
                template_id = %template_id,
                removed = before - after,
                "removed triggers for template"
            );
        }
    }

    /// Snapshot of all currently-registered triggers. Used by the list
    /// endpoints. Cheap (clones the DashMap entries).
    pub fn list_all(&self) -> Vec<TriggerRecord> {
        self.triggers
            .iter()
            .map(|r| r.value().clone())
            .collect()
    }

    pub fn list_for_template(&self, template_id: Uuid) -> Vec<TriggerRecord> {
        self.triggers
            .iter()
            .filter(|r| r.value().template_id == template_id)
            .map(|r| r.value().clone())
            .collect()
    }

    pub fn get(&self, node_id: &str) -> Option<TriggerRecord> {
        self.triggers.get(node_id).map(|r| r.value().clone())
    }

    pub fn history_for(&self, node_id: &str) -> Vec<FireResult> {
        self.history
            .get(node_id)
            .map(|h| h.value().clone())
            .unwrap_or_default()
    }

    /// Core fire path. Resolves the trigger, evaluates `payload_mapping`
    /// against `event_payload`, then routes to spawn or signal.
    pub async fn fire(
        &self,
        node_id: &str,
        event_payload: Value,
    ) -> Result<FireResult, TriggerError> {
        let record = self
            .triggers
            .get(node_id)
            .map(|r| r.value().clone())
            .ok_or_else(|| TriggerError::NotFound(node_id.to_string()))?;

        if !record.enabled {
            return Err(TriggerError::Disabled(node_id.to_string()));
        }

        // Pull the template's graph so we can read the trigger's stored
        // `payload_mapping` (not duplicated into `TriggerRecord` — the source
        // of truth is `graph_json`).
        let template: WorkflowTemplate = sqlx::query_as::<_, WorkflowTemplate>(
            "SELECT * FROM workflow_templates WHERE id = $1 AND version = $2",
        )
        .bind(record.template_id)
        .bind(record.template_version)
        .fetch_optional(&self.db)
        .await
        .map_err(|e| TriggerError::Database(e.to_string()))?
        .ok_or_else(|| {
            TriggerError::TargetMissing {
                node_id: node_id.to_string(),
                target: "template row missing".to_string(),
            }
        })?;

        let graph: WorkflowGraph = serde_json::from_value(template.graph.clone())
            .map_err(|e| TriggerError::Database(format!("graph parse: {e}")))?;

        let trigger_node = graph
            .nodes
            .iter()
            .find(|n| n.id == record.node_id)
            .ok_or_else(|| TriggerError::TargetMissing {
                node_id: node_id.to_string(),
                target: "trigger node missing in graph".to_string(),
            })?;
        let WorkflowNodeData::Trigger {
            payload_mapping, ..
        } = &trigger_node.data
        else {
            return Err(TriggerError::TargetMissing {
                node_id: node_id.to_string(),
                target: "node is not a trigger".to_string(),
            });
        };

        // Build the token by evaluating each mapping expression against the
        // event_payload scope. Phase 5a uses a simple Rhai engine with
        // `payload` bound to the input — sources can extend the scope later
        // (e.g. `fire_time` for cron, `catalogue_entry` for catalog).
        let token = evaluate_mapping(payload_mapping, &event_payload)?;

        let source_kind = record.source.kind().to_string();
        let outcome_result = match record.kind {
            TriggerKind::Spawn => self.fire_spawn(&record, &template, &graph, token).await,
            TriggerKind::Signal => self.fire_signal(&record, token).await,
        };
        let outcome = match &outcome_result {
            Ok(o) => o.clone(),
            Err(_) => FireOutcome::NoTargets, // sentinel for metric path
        };
        self.record_metric(&source_kind, &outcome, outcome_result.is_err());
        let outcome = outcome_result?;

        let result = FireResult {
            locator: TriggerLocator {
                template_id: record.template_id,
                template_version: record.template_version,
                node_id: record.node_id.clone(),
            },
            fired_at: Utc::now(),
            source_kind,
            outcome,
        };
        self.record_history(&record.node_id, result.clone());
        Ok(result)
    }

    async fn fire_spawn(
        &self,
        record: &TriggerRecord,
        template: &WorkflowTemplate,
        graph: &WorkflowGraph,
        token: Value,
    ) -> Result<FireOutcome, TriggerError> {
        let air_json = template
            .air_json
            .clone()
            .ok_or_else(|| TriggerError::TargetMissing {
                node_id: record.node_id.clone(),
                target: "template has no compiled AIR".to_string(),
            })?;

        // Synthetic principal — see proposal §9.3. Stable per trigger so audit
        // queries can attribute fires.
        let created_by = synthetic_principal_for_trigger(&record.node_id);
        let instance_id = Uuid::new_v4();
        let net_id = format!("mekhan-{instance_id}");

        let start_tokens = vec![StartToken {
            start_block_id: record.target_node_id.clone(),
            token,
        }];

        let parameterized = parameterize_air(
            &air_json,
            instance_id,
            template.id,
            template.version,
            created_by,
            graph,
            &start_tokens,
        )
        .map_err(|e| TriggerError::InstanceFailed(e.to_string()))?;

        // Audit metadata: who triggered this and which template version.
        let metadata = json!({
            "triggered_by": record.node_id,
            "trigger_kind": record.source.kind(),
        });

        let instance = sqlx::query_as::<_, WorkflowInstance>(
            r#"
            INSERT INTO workflow_instances (id, template_id, template_version, net_id, status, created_by, started_at, metadata)
            VALUES ($1, $2, $3, $4, 'running', $5, NOW(), $6)
            RETURNING *
            "#,
        )
        .bind(instance_id)
        .bind(template.id)
        .bind(template.version)
        .bind(&net_id)
        .bind(created_by)
        .bind(&metadata)
        .fetch_one(&self.db)
        .await
        .map_err(|e| TriggerError::Database(e.to_string()))?;

        if let Err(e) = deploy_instance(&self.petri, &net_id, &parameterized).await {
            // Roll back the row to keep lifecycle from observing a phantom.
            let _ = sqlx::query("DELETE FROM workflow_instances WHERE id = $1")
                .bind(instance_id)
                .execute(&self.db)
                .await;
            return Err(TriggerError::InstanceFailed(format!("deploy: {e}")));
        }

        Ok(FireOutcome::Spawned {
            instance_id: instance.id,
        })
    }

    async fn fire_signal(
        &self,
        record: &TriggerRecord,
        token: Value,
    ) -> Result<FireOutcome, TriggerError> {
        let nets: Vec<(String,)> = sqlx::query_as::<_, (String,)>(
            "SELECT net_id FROM workflow_instances WHERE template_id = $1 AND template_version = $2 AND status = 'running'",
        )
        .bind(record.template_id)
        .bind(record.template_version)
        .fetch_all(&self.db)
        .await
        .map_err(|e| TriggerError::Database(e.to_string()))?;

        if nets.is_empty() {
            return Ok(FireOutcome::NoTargets);
        }

        // Place id convention: `p_{target_node_id}_{handle}` for AutomatedStep
        // signal places, but in general we just use the target node id +
        // `_signal` for human_task style places. The dispatcher delegates to
        // a helper so the convention stays in one place.
        let place_id = signal_place_id(&record.target_node_id, &record.target_handle);
        let payload = json!({
            "source": "trigger",
            "signal_key": format!(
                "trig-{}-{}",
                record.node_id,
                chrono::Utc::now().timestamp_millis()
            ),
            "payload": token,
            "timestamp": Utc::now().to_rfc3339(),
        });

        let mut delivered = 0;
        for (net_id,) in &nets {
            let subject = format!("petri.signal.{net_id}.{place_id}");
            let payload_bytes = match serde_json::to_vec(&payload) {
                Ok(b) => b,
                Err(e) => {
                    tracing::warn!(
                        net_id = %net_id,
                        "failed to serialize trigger signal: {e}"
                    );
                    continue;
                }
            };
            match self
                .nats
                .jetstream()
                .publish(subject.clone(), payload_bytes.into())
                .await
            {
                Ok(ack) => match ack.await {
                    Ok(_) => {
                        delivered += 1;
                    }
                    Err(e) => {
                        tracing::warn!(
                            net_id = %net_id,
                            subject = %subject,
                            "trigger signal publish ack failed: {e}"
                        );
                    }
                },
                Err(e) => {
                    tracing::warn!(
                        net_id = %net_id,
                        subject = %subject,
                        "trigger signal publish failed: {e}"
                    );
                }
            }
        }

        Ok(FireOutcome::Signaled {
            delivered_to: delivered,
        })
    }

    fn record_history(&self, node_id: &str, result: FireResult) {
        let mut entry = self.history.entry(node_id.to_string()).or_default();
        entry.push(result);
        // Keep last 50 fires per trigger — enough for the editor's history
        // tab, bounded enough to avoid unbounded growth on chatty triggers.
        let len = entry.len();
        if len > 50 {
            let drop = len - 50;
            entry.drain(..drop);
        }
    }
}

/// Entry point spawned from `main`. Hydrates the dispatcher and kicks off
/// background source listeners. Returns the shared dispatcher so the caller
/// can stash it in `AppState`.
pub async fn start_trigger_dispatcher(
    db: PgPool,
    petri: PetriClient,
    nats: MekhanNats,
) -> Arc<TriggerDispatcher> {
    let dispatcher = Arc::new(TriggerDispatcher::new(db, petri, nats.clone()));
    if let Err(e) = dispatcher.hydrate().await {
        tracing::warn!("trigger dispatcher initial hydrate failed: {e}");
    }

    // Cron source (Phase 5b). The bucket is shared with future state stores —
    // any source that needs persistence between restarts writes through it.
    let kv = match nats.ensure_trigger_state_kv().await {
        Ok(kv) => Some(kv),
        Err(e) => {
            tracing::warn!("TRIGGER_STATE KV unavailable, cron catch-up disabled: {e}");
            None
        }
    };
    crate::triggers::sources::cron::register_all(dispatcher.clone(), kv).await;

    dispatcher
}

/// Best-effort signal place id from a node id + handle. The compiler emits
/// `p_{node_id}_signal` for human-task signal places and `p_{node_id}_{handle}`
/// for general input ports — we default to the latter so this stays stable
/// even as new block kinds gain signal-style ports.
fn signal_place_id(target_node_id: &str, handle: &str) -> String {
    format!("p_{target_node_id}_{handle}")
}

/// Synthetic principal id used as `created_by` for trigger-spawned instances.
/// Derived deterministically from the trigger node id so audit queries can
/// group fires-from-the-same-trigger without inventing per-trigger users.
fn synthetic_principal_for_trigger(node_id: &str) -> Uuid {
    // Use the well-known DNS namespace UUID as a stable seed so the value is
    // reproducible across restarts. The exact namespace doesn't matter — we
    // just need a fixed UUID to v5-hash against.
    Uuid::new_v5(&Uuid::NAMESPACE_DNS, format!("trigger:{node_id}").as_bytes())
}

/// Evaluate each `FieldMapping.expression` against the event scope and
/// assemble the resulting JSON object. The Rhai engine here is fresh per fire
/// — short scripts, no shared state, safe to throw away.
fn evaluate_mapping(
    mappings: &[crate::models::template::FieldMapping],
    event_payload: &Value,
) -> Result<Value, TriggerError> {
    use rhai::{Dynamic, Engine, Scope};

    // No mappings → pass through the payload itself. Useful for the empty-port
    // case where the trigger just forwards whatever the source delivered.
    if mappings.is_empty() {
        return Ok(event_payload.clone());
    }

    let engine = Engine::new();
    let mut out = serde_json::Map::new();
    for m in mappings {
        let mut scope = Scope::new();
        let payload_dyn: Dynamic = rhai::serde::to_dynamic(event_payload.clone()).map_err(|e| {
            TriggerError::PayloadMappingFailed {
                field: m.target_field.clone(),
                message: format!("payload→Dynamic: {e}"),
            }
        })?;
        scope.push_dynamic("payload", payload_dyn);

        let result: Dynamic = engine
            .eval_expression_with_scope::<Dynamic>(&mut scope, &m.expression)
            .map_err(|e| TriggerError::PayloadMappingFailed {
                field: m.target_field.clone(),
                message: e.to_string(),
            })?;
        let json_value: Value =
            rhai::serde::from_dynamic(&result).map_err(|e| TriggerError::PayloadMappingFailed {
                field: m.target_field.clone(),
                message: format!("Dynamic→JSON: {e}"),
            })?;
        out.insert(m.target_field.clone(), json_value);
    }
    Ok(Value::Object(out))
}

// --- Local helpers ---

trait WorkflowTemplateExt {
    fn template_version_or_zero(&self) -> i32;
}

impl WorkflowTemplateExt for WorkflowTemplate {
    fn template_version_or_zero(&self) -> i32 {
        self.version
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evaluate_mapping_passes_payload_through_when_empty() {
        let result = evaluate_mapping(&[], &json!({ "x": 1 })).unwrap();
        assert_eq!(result, json!({ "x": 1 }));
    }

    #[test]
    fn evaluate_mapping_projects_fields() {
        use crate::models::template::FieldMapping;
        let mappings = vec![
            FieldMapping {
                target_field: "name".to_string(),
                expression: r#"payload.name"#.to_string(),
            },
            FieldMapping {
                target_field: "doubled".to_string(),
                expression: r#"payload.n * 2"#.to_string(),
            },
        ];
        let result = evaluate_mapping(
            &mappings,
            &json!({ "name": "alice", "n": 21 }),
        )
        .unwrap();
        assert_eq!(result["name"], "alice");
        assert_eq!(result["doubled"], 42);
    }

    #[test]
    fn evaluate_mapping_reports_failure() {
        use crate::models::template::FieldMapping;
        let mappings = vec![FieldMapping {
            target_field: "bad".to_string(),
            expression: r#"this won't parse"#.to_string(),
        }];
        let err = evaluate_mapping(&mappings, &json!({})).unwrap_err();
        match err {
            TriggerError::PayloadMappingFailed { field, .. } => {
                assert_eq!(field, "bad");
            }
            _ => panic!("expected PayloadMappingFailed"),
        }
    }
}
