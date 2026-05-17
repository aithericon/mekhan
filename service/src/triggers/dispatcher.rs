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

use crate::models::instance::StartToken;
use crate::models::template::{WorkflowGraph, WorkflowNodeData, WorkflowTemplate};
use crate::nats::MekhanNats;
use crate::petri::client::PetriClient;
use crate::petri::launcher::{InstanceLauncher, LaunchSpec};

use super::model::{
    locate_trigger, FireOutcome, FireResult, TriggerError, TriggerKind, TriggerLocator,
    TriggerRecord,
};
use super::waiters::{ResultWaiters, TerminalOutcome};
use tokio::sync::oneshot;

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
            !(rec.template_id == template.id
                && rec.template_version == template.template_version_or_zero())
        });

        let mut registered = 0;
        for node in &graph.nodes {
            let WorkflowNodeData::Trigger {
                source,
                enabled,
                reply_default,
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
                reply_default: *reply_default,
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
        self.triggers.iter().map(|r| r.value().clone()).collect()
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

    /// Fire a trigger, discarding any WaitForResult handle. The path used by
    /// every background source (cron/catalog/lifecycle/webhook) and by
    /// FireAndForget callers.
    pub async fn fire(
        &self,
        node_id: &str,
        event_payload: Value,
    ) -> Result<FireResult, TriggerError> {
        self.fire_impl(node_id, event_payload, None)
            .await
            .map(|(result, _rx)| result)
    }

    /// Fire a trigger and, for a Spawn, register a WaitForResult waiter.
    /// Returns the receiver alongside the `FireResult` (always `None` for
    /// Signal-kind fires — there is no instance to wait on).
    pub async fn fire_waiting(
        &self,
        node_id: &str,
        event_payload: Value,
        waiters: &ResultWaiters,
    ) -> Result<(FireResult, Option<oneshot::Receiver<TerminalOutcome>>), TriggerError> {
        self.fire_impl(node_id, event_payload, Some(waiters)).await
    }

    /// Core fire path. Resolves the trigger, evaluates `payload_mapping`
    /// against `event_payload`, then routes to spawn or signal. When `wait`
    /// is `Some` and the route spawns, a WaitForResult waiter is registered
    /// and its receiver returned.
    async fn fire_impl(
        &self,
        node_id: &str,
        event_payload: Value,
        wait: Option<&ResultWaiters>,
    ) -> Result<(FireResult, Option<oneshot::Receiver<TerminalOutcome>>), TriggerError> {
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
        .ok_or_else(|| TriggerError::TargetMissing {
            node_id: node_id.to_string(),
            target: "template row missing".to_string(),
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

        // Resolve the port the trigger feeds. Shared with the compiler's
        // `validate_triggers` publish-time check so the two can't drift.
        let target_node = graph
            .nodes
            .iter()
            .find(|n| n.id == record.target_node_id)
            .ok_or_else(|| TriggerError::TargetMissing {
                node_id: node_id.to_string(),
                target: format!("target node '{}' missing in graph", record.target_node_id),
            })?;
        let target_port =
            crate::compiler::resolve_trigger_target_port(target_node, &record.target_handle)
                .ok_or_else(|| TriggerError::TargetMissing {
                    node_id: node_id.to_string(),
                    target: format!(
                        "target port '{}' missing on node '{}'",
                        record.target_handle, record.target_node_id
                    ),
                })?;

        let source_kind = record.source.kind().to_string();
        let locator = TriggerLocator {
            template_id: record.template_id,
            template_version: record.template_version,
            node_id: record.node_id.clone(),
        };

        // Record a terminal outcome (metrics + history) and hand back the
        // result. Every non-infra exit goes through here so type-contract
        // violations show up in the history tab instead of vanishing.
        let finalize = |outcome: FireOutcome, errored: bool| -> FireResult {
            self.record_metric(&source_kind, &outcome, errored);
            let result = FireResult {
                locator: locator.clone(),
                fired_at: Utc::now(),
                source_kind: source_kind.clone(),
                outcome,
            };
            self.record_history(&record.node_id, result.clone());
            result
        };

        // Build the token: bind each source-scope identifier as its own Rhai
        // variable and evaluate the mappings. A failed mapping is a trigger
        // *config* problem, not infra — record it as a dropped fire (200, but
        // visible), don't surface a 5xx.
        let token = match evaluate_mapping(
            payload_mapping,
            &event_payload,
            target_port.fields.is_empty(),
        ) {
            Ok(t) => t,
            Err(e) => {
                return Ok((
                    finalize(
                        FireOutcome::Dropped {
                            reason: format!("payload mapping failed: {e}"),
                        },
                        false,
                    ),
                    None,
                ));
            }
        };

        // Single typed-ports gate, identical for spawn and signal — the
        // invariant the 05 typed-ports work exists to enforce, now extended
        // across the trigger boundary. Strict reject, no coercion (matches
        // `parameterize_air`). Spawn's `parameterize_air` re-checks the same
        // Start port; redundant but keeps its other duties intact.
        if let Err(ve) = target_port.validate_token(&token) {
            return Ok((
                finalize(
                    FireOutcome::Dropped {
                        reason: format!(
                            "token rejected by target port '{}': {ve}",
                            target_port.id
                        ),
                    },
                    false,
                ),
                None,
            ));
        }

        let (outcome_result, rx): (
            Result<FireOutcome, TriggerError>,
            Option<oneshot::Receiver<TerminalOutcome>>,
        ) = match record.kind {
            TriggerKind::Spawn => {
                match self
                    .fire_spawn(&record, &template, &graph, token, wait)
                    .await
                {
                    Ok((outcome, rx)) => (Ok(outcome), rx),
                    Err(e) => (Err(e), None),
                }
            }
            TriggerKind::Signal => (self.fire_signal(&record, token).await, None),
        };
        match outcome_result {
            Ok(outcome) => Ok((finalize(outcome, false), rx)),
            Err(e) => {
                // Genuine infra failure (DB / deploy / NATS). Record it so it's
                // visible in history and counted as an error (not the old
                // `NoTargets` mislabel), then surface to the caller.
                let _ = finalize(
                    FireOutcome::Dropped {
                        reason: format!("fire failed: {e}"),
                    },
                    true,
                );
                Err(e)
            }
        }
    }

    async fn fire_spawn(
        &self,
        record: &TriggerRecord,
        template: &WorkflowTemplate,
        graph: &WorkflowGraph,
        token: Value,
        wait: Option<&ResultWaiters>,
    ) -> Result<(FireOutcome, Option<oneshot::Receiver<TerminalOutcome>>), TriggerError> {
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

        // Audit metadata: who triggered this and which template version.
        let metadata = json!({
            "triggered_by": record.node_id,
            "trigger_kind": record.source.kind(),
        });

        // Same parameterize → insert → deploy → rollback sequence as the user
        // POST path, owned by the launcher. A spawn folds every launch failure
        // into InstanceFailed (the dropped-fire is recorded by the caller).
        let launcher = InstanceLauncher::new(&self.db, &self.petri);
        let instance = launcher
            .launch(LaunchSpec {
                instance_id,
                net_id,
                template_id: template.id,
                template_version: template.version,
                created_by,
                metadata,
                air_json: &air_json,
                graph,
                start_tokens: &start_tokens,
            })
            .await
            .map_err(|e| TriggerError::InstanceFailed(e.to_string()))?;

        // WaitForResult: register the waiter, then close the
        // create→deploy→terminal race. The net may already be terminal (the
        // lifecycle consumer's `resolve` was a no-op — no waiter existed when
        // it ran); re-read the row and resolve synchronously if so. `resolve`
        // is idempotent, so a consumer that resolves between our `register`
        // and this re-read is harmless (first writer wins).
        let rx = match wait {
            Some(waiters) => {
                let rx = waiters.register(instance.id);
                if let Ok(Some((status, result))) =
                    sqlx::query_as::<_, (String, Option<serde_json::Value>)>(
                        "SELECT status, result FROM workflow_instances WHERE id = $1",
                    )
                    .bind(instance.id)
                    .fetch_optional(&self.db)
                    .await
                {
                    if matches!(
                        status.as_str(),
                        "completed" | "cancelled" | "failed" | "archived"
                    ) {
                        waiters
                            .resolve(&instance.id, TerminalOutcome { status, result });
                    }
                }
                Some(rx)
            }
            None => None,
        };

        Ok((FireOutcome::Spawned { instance_id: instance.id }, rx))
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
    Uuid::new_v5(
        &Uuid::NAMESPACE_DNS,
        format!("trigger:{node_id}").as_bytes(),
    )
}

/// Evaluate each `FieldMapping.expression` against the source's named event
/// scope and assemble the resulting JSON object.
///
/// `scope_obj` is the flat scope map the source emits — its top-level keys are
/// bound as individual Rhai identifiers (e.g. `fire_time`, `catalogue_entry`,
/// `payload`), matching `triggers::scope::source_scope`. This is what makes the
/// webhook body reachable as `payload` rather than `payload.payload`.
///
/// Empty mapping: forward the source scope verbatim only when the target port
/// is a genuine pass-through (no declared fields). For a typed port an empty
/// mapping yields `{}` — the compiler already rejects an empty mapping into a
/// port with *required* fields, so `{}` here is only ever an all-optional port.
/// The Rhai engine is fresh per fire — short scripts, no shared state.
fn evaluate_mapping(
    mappings: &[crate::models::template::FieldMapping],
    scope_obj: &Value,
    passthrough_ok: bool,
) -> Result<Value, TriggerError> {
    use rhai::{Dynamic, Engine, Scope};

    if mappings.is_empty() {
        return Ok(if passthrough_ok {
            scope_obj.clone()
        } else {
            Value::Object(serde_json::Map::new())
        });
    }

    let scope_map = match scope_obj {
        Value::Object(m) => m.clone(),
        _ => serde_json::Map::new(),
    };

    let engine = Engine::new();
    let mut out = serde_json::Map::new();
    for m in mappings {
        let mut scope = Scope::new();
        for (k, v) in &scope_map {
            let dyn_v: Dynamic = rhai::serde::to_dynamic(v.clone()).map_err(|e| {
                TriggerError::PayloadMappingFailed {
                    field: m.target_field.clone(),
                    message: format!("{k}→Dynamic: {e}"),
                }
            })?;
            scope.push_dynamic(k.as_str(), dyn_v);
        }

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
    fn empty_mapping_passes_scope_through_only_for_passthrough_port() {
        // Fieldless target port → forward the source scope verbatim.
        let result = evaluate_mapping(&[], &json!({ "x": 1 }), true).unwrap();
        assert_eq!(result, json!({ "x": 1 }));
        // Typed target port → empty token (compiler already rejected the
        // empty-mapping-into-required-fields case, so this is all-optional).
        let result = evaluate_mapping(&[], &json!({ "x": 1 }), false).unwrap();
        assert_eq!(result, json!({}));
    }

    #[test]
    fn scope_identifiers_bind_individually() {
        use crate::models::template::FieldMapping;
        // Each top-level scope key is its own Rhai variable — no `payload.`
        // prefix. This is the un-nesting that makes the webhook body reachable
        // as `payload` and cron's `fire_time` reachable directly.
        let mappings = vec![
            FieldMapping {
                target_field: "who".to_string(),
                expression: r#"name"#.to_string(),
            },
            FieldMapping {
                target_field: "doubled".to_string(),
                expression: r#"n * 2"#.to_string(),
            },
            FieldMapping {
                target_field: "body_field".to_string(),
                expression: r#"payload.inner"#.to_string(),
            },
        ];
        let result = evaluate_mapping(
            &mappings,
            &json!({ "name": "alice", "n": 21, "payload": { "inner": "v" } }),
            false,
        )
        .unwrap();
        assert_eq!(result["who"], "alice");
        assert_eq!(result["doubled"], 42);
        assert_eq!(result["body_field"], "v");
    }

    #[test]
    fn evaluate_mapping_reports_failure() {
        use crate::models::template::FieldMapping;
        let mappings = vec![FieldMapping {
            target_field: "bad".to_string(),
            expression: r#"this won't parse"#.to_string(),
        }];
        let err = evaluate_mapping(&mappings, &json!({}), false).unwrap_err();
        match err {
            TriggerError::PayloadMappingFailed { field, .. } => {
                assert_eq!(field, "bad");
            }
            _ => panic!("expected PayloadMappingFailed"),
        }
    }
}
