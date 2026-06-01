//! Catalogue effect handlers.
//!
//! `CatalogueRegisterHandler` builds a `CatalogueRegisterCommand` and returns
//! it as the `effect_result`. The Mekhan causality projector picks up the
//! `EffectCompleted` event from PETRI_GLOBAL and creates the catalogue entry.
//!
//! `CatalogueLookupHandler`, `CatalogueSubscribeHandler`, and
//! `CatalogueUnsubscribeHandler` use NATS request-reply via `CatalogueClient`.
//!
//! Hooked into the executor lifecycle's artifact event flow:
//!   sig_artifact → log_artifact → catalogue_pending → catalogue_artifact (this handler)

use std::collections::HashMap;
use std::sync::Arc;

use chrono::Utc;
use petri_domain::catalogue::{
    CatalogueClient, CatalogueLookupRequest, CatalogueRegisterCommand, CatalogueSubscribeRequest,
};
use serde_json::Value as JsonValue;

use crate::effect::{EffectError, EffectHandler, EffectInput, EffectOutput};

/// Known provenance keys that get extracted into dedicated fields
/// rather than staying in user_metadata.
const PROVENANCE_KEYS: &[&str] = &[
    "source_net",
    "petri_net_id",
    "source_place",
    "petri_place",
    "signal_key",
    "petri_signal_key",
    "process_id",
    "process_step",
];

/// Effect handler that validates and extracts artifact data for the data catalogue.
///
/// The actual catalogue entry is created by the Mekhan causality projector when
/// it processes the `EffectCompleted` event. This handler builds the
/// `CatalogueRegisterCommand` and returns it as the `effect_result` so the
/// projector has all artifact-specific data with full provenance context.
///
/// Input: an `ExecutorEventSignal` token (artifact category) with shape:
/// ```json
/// {
///   "execution_id": "...",
///   "category": "artifact",
///   "detail": {
///     "event_type": "artifact_logged",
///     "artifact_id": "gp_model",
///     "name": "gp_model",
///     "category": "model",
///     "size_bytes": 400270,
///     "storage_path": "artifacts/...",
///     "metadata": { "source_net": "...", ... },
///     "file_metadata": { ... }
///   },
///   "sequence": 42,
///   "source": "exec-1"
/// }
/// ```
///
/// Output: passes through the input token unchanged.
pub struct CatalogueRegisterHandler {
    input_port: String,
    output_port: String,
}

impl CatalogueRegisterHandler {
    pub fn new(input_port: impl Into<String>, output_port: impl Into<String>) -> Self {
        Self {
            input_port: input_port.into(),
            output_port: output_port.into(),
        }
    }
}

#[async_trait::async_trait]
impl EffectHandler for CatalogueRegisterHandler {
    async fn execute(&self, input: EffectInput) -> Result<EffectOutput, EffectError> {
        let token_data = input.inputs.get(&self.input_port).ok_or_else(|| {
            EffectError::Fatal(format!(
                "Missing input port '{}' in catalogue_register handler",
                self.input_port,
            ))
        })?;

        let execution_id = token_data
            .get("execution_id")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();

        // The detail field contains the ArtifactLogged data
        let detail = token_data.get("detail").ok_or_else(|| {
            EffectError::ExecutionFailed("missing 'detail' in artifact event signal".into())
        })?;

        // Routing metadata (petri_signal_key, petri_net_id, etc.) is a sibling
        // of detail in the signal token, not nested inside detail.
        let routing_meta = token_data
            .get("metadata")
            .and_then(|v| v.as_object())
            .cloned()
            .unwrap_or_default();

        let command = build_command_from_event(detail, &routing_meta, &execution_id)?;

        // Serialize the full command as effect_result. The causality projector
        // in Mekhan will deserialize this and create the catalogue entry with
        // full provenance from the causality graph context.
        let result = serde_json::to_value(&command).map_err(|e| {
            EffectError::ExecutionFailed(format!("catalogue command serialization failed: {e}"))
        })?;

        tracing::info!(
            artifact_id = %command.artifact_id,
            execution_id = %execution_id,
            "catalogue_register effect completed (registration deferred to causality projector)",
        );

        // Pass through the token data unchanged
        let mut tokens = HashMap::new();
        tokens.insert(self.output_port.clone(), token_data.clone());

        Ok(EffectOutput { tokens, result })
    }

    fn replay(&self, _input: &EffectInput, _stored_result: &JsonValue) {
        // Stateless handler — nothing to rebuild on replay.
    }

    fn name(&self) -> &str {
        "catalogue_register"
    }
}

/// Build a `CatalogueRegisterCommand` from a `StatusDetail::ArtifactLogged` JSON payload.
fn build_command_from_event(
    detail: &JsonValue,
    routing_meta: &serde_json::Map<String, JsonValue>,
    execution_id: &str,
) -> Result<CatalogueRegisterCommand, EffectError> {
    let artifact_id = detail
        .get("artifact_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| EffectError::ExecutionFailed("missing artifact_id in detail".into()))?;

    let name = detail
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or(artifact_id);

    let category = detail
        .get("category")
        .and_then(|v| v.as_str())
        .unwrap_or("other");

    // Extract provenance from two sources:
    // 1. detail.metadata — artifact-level metadata set by the user script
    // 2. routing_meta — petri routing metadata (petri_signal_key, petri_net_id, etc.)
    //    stamped by the executor watcher from job metadata
    let artifact_meta = detail
        .get("metadata")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();

    // Merge: routing_meta takes precedence for provenance keys
    let mut merged = artifact_meta.clone();
    for (k, v) in routing_meta {
        merged.entry(k.clone()).or_insert_with(|| v.clone());
    }

    let source_net = extract_provenance(&merged, "source_net", "petri_net_id");
    let source_place = extract_provenance(&merged, "source_place", "petri_place");
    let signal_key = extract_provenance(&merged, "signal_key", "petri_signal_key");
    let process_id = merged
        .get("process_id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let process_step = merged
        .get("process_step")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    // Job ID from the execution context (not from signal_key — that's a UUID now)
    let job_id = detail
        .get("job_id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| execution_id.to_string());

    // Build user_metadata: everything that isn't a provenance key
    let user_metadata: HashMap<String, String> = artifact_meta
        .iter()
        .filter(|(k, _)| !PROVENANCE_KEYS.contains(&k.as_str()))
        .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
        .collect();

    Ok(CatalogueRegisterCommand {
        execution_id: execution_id.to_string(),
        job_id,
        artifact_id: artifact_id.to_string(),
        name: name.to_string(),
        category: category.to_string(),
        filename: format!("{name}.json"), // best guess from name
        mime_type: detail
            .get("mime_type")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        size_bytes: detail.get("size_bytes").and_then(|v| v.as_u64()),
        storage_path: detail
            .get("storage_path")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        file_metadata: detail.get("file_metadata").cloned(),
        user_metadata,
        source_net,
        source_place,
        signal_key,
        process_id,
        process_step,
        created_at: Utc::now(),
    })
}

/// Extract a provenance value, checking the canonical key first, then the
/// petri-prefixed fallback.
fn extract_provenance(
    metadata: &serde_json::Map<String, JsonValue>,
    key: &str,
    fallback_key: &str,
) -> Option<String> {
    metadata
        .get(key)
        .or_else(|| metadata.get(fallback_key))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

// ---------------------------------------------------------------------------
// CatalogueLookupHandler — queries the data catalogue via NATS request-reply
// ---------------------------------------------------------------------------

/// Effect handler that queries the data catalogue for matching artifact entries.
///
/// Accepts either the ADR-17 convenience token format (with top-level fields like
/// `source_process_id`, `source_net`, `category`, `limit`) or the direct
/// `CatalogueLookupRequest` format (with `filters`, `page`, `page_size`, `sort`,
/// `search`).
///
/// Output token shape:
/// ```json
/// {
///   "artifacts": [...CatalogueEntry...],
///   "total_count": N,
///   "source_process_ids": ["unique", "process", "ids"]
/// }
/// ```
pub struct CatalogueLookupHandler {
    client: Arc<dyn CatalogueClient>,
    input_port: String,
    output_port: String,
}

impl CatalogueLookupHandler {
    pub fn new(
        client: Arc<dyn CatalogueClient>,
        input_port: impl Into<String>,
        output_port: impl Into<String>,
    ) -> Self {
        Self {
            client,
            input_port: input_port.into(),
            output_port: output_port.into(),
        }
    }
}

#[async_trait::async_trait]
impl EffectHandler for CatalogueLookupHandler {
    async fn execute(&self, input: EffectInput) -> Result<EffectOutput, EffectError> {
        let token_data = input.inputs.get(&self.input_port).ok_or_else(|| {
            EffectError::Fatal(format!(
                "Missing input port '{}' in catalogue_lookup handler",
                self.input_port,
            ))
        })?;

        let request = build_lookup_request(token_data)?;

        let response =
            self.client.lookup(request).await.map_err(|e| {
                EffectError::ExecutionFailed(format!("catalogue lookup failed: {e}"))
            })?;

        // Collect unique process_ids from returned entries
        let mut process_id_set = std::collections::HashSet::new();
        for entry in &response.items {
            if let Some(ref pid) = entry.process_id {
                process_id_set.insert(pid.clone());
            }
        }
        let source_process_ids: Vec<String> = process_id_set.into_iter().collect();

        let output_token = serde_json::json!({
            "artifacts": response.items,
            "total_count": response.total,
            "source_process_ids": source_process_ids,
        });

        let result = serde_json::json!({
            "items": response.items,
            "total": response.total,
            "page": response.page,
            "page_size": response.page_size,
        });

        tracing::info!(
            total = response.total,
            returned = response.items.len(),
            "Catalogue lookup completed",
        );

        let mut tokens = HashMap::new();
        tokens.insert(self.output_port.clone(), output_token);

        Ok(EffectOutput { tokens, result })
    }

    fn replay(&self, _input: &EffectInput, _stored_result: &JsonValue) {
        // Stateless handler — nothing to rebuild on replay.
    }

    fn name(&self) -> &str {
        "catalogue_lookup"
    }
}

/// Build a `CatalogueLookupRequest` from the input token.
///
/// Supports two formats:
/// 1. ADR-17 convenience format: top-level `source_process_id`, `source_net`,
///    `category`, `filters`, `sort_by`, `limit` fields.
/// 2. Direct format: `filters`, `page`, `page_size`, `sort`, `search` fields
///    matching `CatalogueLookupRequest` directly.
fn build_lookup_request(token: &JsonValue) -> Result<CatalogueLookupRequest, EffectError> {
    // If the token already has a `filters` field that is an object, try direct
    // deserialization first (the direct CatalogueLookupRequest format).
    if token.get("filters").and_then(|f| f.as_object()).is_some()
        && (token.get("page").is_some()
            || token.get("page_size").is_some()
            || token.get("sort").is_some()
            || token.get("search").is_some())
    {
        if let Ok(req) = serde_json::from_value::<CatalogueLookupRequest>(token.clone()) {
            return Ok(req);
        }
    }

    // ADR-17 convenience format: map top-level fields to CatalogueLookupRequest
    let mut filters: HashMap<String, HashMap<String, String>> = token
        .get("filters")
        .and_then(|f| serde_json::from_value(f.clone()).ok())
        .unwrap_or_default();

    // Map convenience fields into filter entries
    if let Some(process_id) = token.get("source_process_id").and_then(|v| v.as_str()) {
        filters
            .entry("process_id".to_string())
            .or_default()
            .insert("eq".to_string(), process_id.to_string());
    }
    if let Some(source_net) = token.get("source_net").and_then(|v| v.as_str()) {
        filters
            .entry("source_net".to_string())
            .or_default()
            .insert("eq".to_string(), source_net.to_string());
    }
    if let Some(category) = token.get("category").and_then(|v| v.as_str()) {
        filters
            .entry("category".to_string())
            .or_default()
            .insert("eq".to_string(), category.to_string());
    }

    let sort = token
        .get("sort_by")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| {
            token
                .get("sort")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        });

    let page_size = token
        .get("limit")
        .and_then(|v| v.as_i64())
        .or_else(|| token.get("page_size").and_then(|v| v.as_i64()));

    let page = token.get("page").and_then(|v| v.as_i64());

    let search = token
        .get("search")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let metadata = token
        .get("metadata")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let file_metadata = token
        .get("file_metadata")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    Ok(CatalogueLookupRequest {
        filters,
        page,
        page_size,
        sort,
        search,
        metadata,
        file_metadata,
    })
}

// ---------------------------------------------------------------------------
// CatalogueSubscribeHandler — creates a reactive catalogue subscription
// ---------------------------------------------------------------------------

/// Effect handler that creates a reactive subscription for catalogue changes.
///
/// Input token shape:
/// ```json
/// {
///   "signal_place": "inbox",
///   "query": { "category": { "eq": "model" } },
///   "backfill": true
/// }
/// ```
///
/// Output token: input clone + `subscription_id` field.
pub struct CatalogueSubscribeHandler {
    client: Arc<dyn CatalogueClient>,
    net_id: String,
    input_port: String,
    output_port: String,
}

impl CatalogueSubscribeHandler {
    pub fn new(
        client: Arc<dyn CatalogueClient>,
        net_id: impl Into<String>,
        input_port: impl Into<String>,
        output_port: impl Into<String>,
    ) -> Self {
        Self {
            client,
            net_id: net_id.into(),
            input_port: input_port.into(),
            output_port: output_port.into(),
        }
    }
}

#[async_trait::async_trait]
impl EffectHandler for CatalogueSubscribeHandler {
    async fn execute(&self, input: EffectInput) -> Result<EffectOutput, EffectError> {
        let token_data = input.inputs.get(&self.input_port).ok_or_else(|| {
            EffectError::Fatal(format!(
                "Missing input port '{}' in catalogue_subscribe handler",
                self.input_port,
            ))
        })?;

        let signal_place = token_data
            .get("signal_place")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                EffectError::Fatal("Missing required 'signal_place' in subscribe input".into())
            })?
            .to_string();

        // Extract filters from either "query" or "filters" key
        let filters: HashMap<String, HashMap<String, String>> = token_data
            .get("query")
            .or_else(|| token_data.get("filters"))
            .and_then(|f| serde_json::from_value(f.clone()).ok())
            .unwrap_or_default();

        let backfill = token_data
            .get("backfill")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let request = CatalogueSubscribeRequest {
            net_id: self.net_id.clone(),
            signal_place: signal_place.clone(),
            filters,
            backfill,
        };

        let subscription_id = self.client.subscribe(request).await.map_err(|e| {
            EffectError::ExecutionFailed(format!("catalogue subscribe failed: {e}"))
        })?;

        tracing::info!(
            subscription_id = %subscription_id,
            signal_place = %signal_place,
            net_id = %self.net_id,
            "Catalogue subscription created",
        );

        // Build output token: input clone + subscription_id
        let mut output_data = token_data.clone();
        if let Some(obj) = output_data.as_object_mut() {
            obj.insert(
                "subscription_id".to_string(),
                JsonValue::String(subscription_id.clone()),
            );
        }

        let mut tokens = HashMap::new();
        tokens.insert(self.output_port.clone(), output_data);

        Ok(EffectOutput {
            tokens,
            result: serde_json::json!({
                "subscription_id": subscription_id,
                "signal_place": signal_place,
            }),
        })
    }

    fn replay(&self, _input: &EffectInput, _stored_result: &JsonValue) {
        // No-op: subscriptions are ephemeral and will be re-created by net re-fire.
    }

    fn name(&self) -> &str {
        "catalogue_subscribe"
    }
}

// ---------------------------------------------------------------------------
// CatalogueUnsubscribeHandler — removes a catalogue subscription
// ---------------------------------------------------------------------------

/// Effect handler that removes a previously created catalogue subscription.
///
/// Input token shape:
/// ```json
/// {
///   "subscription_id": "sub-123"
/// }
/// ```
///
/// Output token: input clone + `unsubscribed` field.
pub struct CatalogueUnsubscribeHandler {
    client: Arc<dyn CatalogueClient>,
    input_port: String,
    output_port: String,
}

impl CatalogueUnsubscribeHandler {
    pub fn new(
        client: Arc<dyn CatalogueClient>,
        input_port: impl Into<String>,
        output_port: impl Into<String>,
    ) -> Self {
        Self {
            client,
            input_port: input_port.into(),
            output_port: output_port.into(),
        }
    }
}

#[async_trait::async_trait]
impl EffectHandler for CatalogueUnsubscribeHandler {
    async fn execute(&self, input: EffectInput) -> Result<EffectOutput, EffectError> {
        let token_data = input.inputs.get(&self.input_port).ok_or_else(|| {
            EffectError::Fatal(format!(
                "Missing input port '{}' in catalogue_unsubscribe handler",
                self.input_port,
            ))
        })?;

        let subscription_id = token_data
            .get("subscription_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                EffectError::Fatal("Missing required 'subscription_id' in unsubscribe input".into())
            })?;

        let unsubscribed = self
            .client
            .unsubscribe(subscription_id)
            .await
            .map_err(|e| {
                EffectError::ExecutionFailed(format!("catalogue unsubscribe failed: {e}"))
            })?;

        tracing::info!(
            subscription_id = %subscription_id,
            unsubscribed = %unsubscribed,
            "Catalogue unsubscribe completed",
        );

        // Build output token: input clone + unsubscribed
        let mut output_data = token_data.clone();
        if let Some(obj) = output_data.as_object_mut() {
            obj.insert("unsubscribed".to_string(), JsonValue::Bool(unsubscribed));
        }

        let mut tokens = HashMap::new();
        tokens.insert(self.output_port.clone(), output_data);

        Ok(EffectOutput {
            tokens,
            result: serde_json::json!({
                "unsubscribed": unsubscribed,
                "subscription_id": subscription_id,
            }),
        })
    }

    fn replay(&self, _input: &EffectInput, _stored_result: &JsonValue) {
        // No-op: subscription state is ephemeral.
    }

    fn name(&self) -> &str {
        "catalogue_unsubscribe"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use petri_domain::catalogue::{
        CatalogueError, CatalogueLookupRequest, CatalogueLookupResponse, CatalogueSubscribeRequest,
    };
    use petri_domain::TransitionId;
    use std::sync::RwLock;

    use petri_domain::catalogue::CatalogueEntry;

    /// Mock catalogue client for testing lookup/subscribe/unsubscribe handlers.
    struct MockCatalogueClient {
        last_lookup: RwLock<Option<CatalogueLookupRequest>>,
        lookup_response: RwLock<Option<CatalogueLookupResponse>>,
    }

    impl MockCatalogueClient {
        fn new() -> Self {
            Self {
                last_lookup: RwLock::new(None),
                lookup_response: RwLock::new(None),
            }
        }

        fn last_lookup_request(&self) -> Option<CatalogueLookupRequest> {
            self.last_lookup.read().unwrap().clone()
        }

        fn set_lookup_response(&self, response: CatalogueLookupResponse) {
            *self.lookup_response.write().unwrap() = Some(response);
        }
    }

    #[async_trait::async_trait]
    impl CatalogueClient for MockCatalogueClient {
        async fn lookup(
            &self,
            request: CatalogueLookupRequest,
        ) -> Result<CatalogueLookupResponse, CatalogueError> {
            *self.last_lookup.write().unwrap() = Some(request);
            self.lookup_response
                .read()
                .unwrap()
                .clone()
                .ok_or_else(|| CatalogueError::QueryFailed("no mock response configured".into()))
        }

        async fn subscribe(
            &self,
            _request: CatalogueSubscribeRequest,
        ) -> Result<String, CatalogueError> {
            Ok("mock-sub-id".to_string())
        }

        async fn unsubscribe(&self, _subscription_id: &str) -> Result<bool, CatalogueError> {
            Ok(true)
        }

        fn name(&self) -> &str {
            "mock_catalogue"
        }
    }

    fn make_catalogue_entry(process_id: Option<&str>) -> CatalogueEntry {
        CatalogueEntry {
            id: "entry-1".to_string(),
            execution_id: "exec-1".to_string(),
            job_id: Some("job-1".to_string()),
            name: "gp_model".to_string(),
            category: "model".to_string(),
            filename: "gp_model.json".to_string(),
            mime_type: None,
            size_bytes: Some(400270),
            storage_path: Some("artifacts/gp_model.json".to_string()),
            source_net: Some("bo-pipeline".to_string()),
            source_place: None,
            signal_key: None,
            process_id: process_id.map(|s| s.to_string()),
            process_step: None,
            file_metadata: serde_json::Value::Null,
            user_metadata: serde_json::Value::Null,
            created_at: chrono::Utc::now(),
        }
    }

    fn make_artifact_event_token() -> serde_json::Value {
        serde_json::json!({
            "execution_id": "exec-1",
            "category": "artifact",
            "detail": {
                "event_type": "artifact_logged",
                "artifact_id": "gp_model",
                "name": "gp_model",
                "category": "model",
                "size_bytes": 400270,
                "storage_path": "artifacts/gp_model.json",
                "metadata": {
                    "source_net": "bo-pipeline"
                }
            },
            "sequence": 42,
            "source": "exec-1"
        })
    }

    // -----------------------------------------------------------------------
    // CatalogueLookupHandler tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn lookup_handler_adr17_convenience_format() {
        let client = Arc::new(MockCatalogueClient::new());
        client.set_lookup_response(CatalogueLookupResponse {
            items: vec![make_catalogue_entry(Some("trace-abc"))],
            total: 1,
            page: 0,
            page_size: 20,
        });

        let handler = CatalogueLookupHandler::new(client.clone(), "query", "results");

        let mut inputs = HashMap::new();
        inputs.insert(
            "query".to_string(),
            serde_json::json!({
                "source_process_id": "trace-abc",
                "source_net": "bo-pipeline",
                "category": "model",
                "limit": 10,
                "sort_by": "-created_at",
            }),
        );

        let input = EffectInput {
            transition_id: TransitionId::named("lookup-test"),
            inputs,
            config: None,
            read_inputs: HashMap::new(),
            process_step: None,
        };

        let output = handler.execute(input).await.unwrap();
        let result_token = output.tokens.get("results").expect("output token");

        assert_eq!(result_token["total_count"], 1);
        assert_eq!(result_token["artifacts"].as_array().unwrap().len(), 1);
        assert!(result_token["source_process_ids"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("trace-abc")));

        // Verify the request was built correctly
        let req = client
            .last_lookup_request()
            .expect("lookup should have been called");
        assert_eq!(
            req.filters
                .get("process_id")
                .and_then(|m| m.get("eq"))
                .map(|s| s.as_str()),
            Some("trace-abc"),
        );
        assert_eq!(
            req.filters
                .get("source_net")
                .and_then(|m| m.get("eq"))
                .map(|s| s.as_str()),
            Some("bo-pipeline"),
        );
        assert_eq!(
            req.filters
                .get("category")
                .and_then(|m| m.get("eq"))
                .map(|s| s.as_str()),
            Some("model"),
        );
        assert_eq!(req.page_size, Some(10));
        assert_eq!(req.sort, Some("-created_at".to_string()));
    }

    #[tokio::test]
    async fn lookup_handler_direct_request_format() {
        let client = Arc::new(MockCatalogueClient::new());
        client.set_lookup_response(CatalogueLookupResponse {
            items: vec![],
            total: 0,
            page: 0,
            page_size: 50,
        });

        let handler = CatalogueLookupHandler::new(client.clone(), "query", "results");

        let mut inputs = HashMap::new();
        inputs.insert(
            "query".to_string(),
            serde_json::json!({
                "filters": {
                    "category": { "eq": "dataset" }
                },
                "page": 0,
                "page_size": 50,
                "sort": "name",
            }),
        );

        let input = EffectInput {
            transition_id: TransitionId::named("lookup-direct"),
            inputs,
            config: None,
            read_inputs: HashMap::new(),
            process_step: None,
        };

        let output = handler.execute(input).await.unwrap();
        let result_token = output.tokens.get("results").expect("output token");

        assert_eq!(result_token["total_count"], 0);
        assert_eq!(result_token["artifacts"].as_array().unwrap().len(), 0);

        let req = client
            .last_lookup_request()
            .expect("lookup should have been called");
        assert_eq!(req.page_size, Some(50));
        assert_eq!(req.sort, Some("name".to_string()));
    }

    #[tokio::test]
    async fn lookup_handler_deduplicates_process_ids() {
        let client = Arc::new(MockCatalogueClient::new());
        client.set_lookup_response(CatalogueLookupResponse {
            items: vec![
                make_catalogue_entry(Some("trace-1")),
                make_catalogue_entry(Some("trace-1")),
                make_catalogue_entry(Some("trace-2")),
                make_catalogue_entry(None),
            ],
            total: 4,
            page: 0,
            page_size: 20,
        });

        let handler = CatalogueLookupHandler::new(client.clone(), "query", "results");

        let mut inputs = HashMap::new();
        inputs.insert("query".to_string(), serde_json::json!({}));

        let input = EffectInput {
            transition_id: TransitionId::named("lookup-dedup"),
            inputs,
            config: None,
            read_inputs: HashMap::new(),
            process_step: None,
        };

        let output = handler.execute(input).await.unwrap();
        let result_token = output.tokens.get("results").expect("output token");

        let process_ids = result_token["source_process_ids"].as_array().unwrap();
        assert_eq!(process_ids.len(), 2, "should have 2 unique process IDs");
        assert!(process_ids.contains(&serde_json::json!("trace-1")));
        assert!(process_ids.contains(&serde_json::json!("trace-2")));
    }

    #[tokio::test]
    async fn lookup_handler_missing_input_port_returns_fatal() {
        let client = Arc::new(MockCatalogueClient::new());
        client.set_lookup_response(CatalogueLookupResponse {
            items: vec![],
            total: 0,
            page: 0,
            page_size: 20,
        });

        let handler = CatalogueLookupHandler::new(client.clone(), "query", "results");

        let input = EffectInput {
            transition_id: TransitionId::named("lookup-missing"),
            inputs: HashMap::new(),
            config: None,
            read_inputs: HashMap::new(),
            process_step: None,
        };

        let err = handler.execute(input).await.unwrap_err();
        assert!(
            matches!(err, EffectError::Fatal(_)),
            "missing port should be Fatal, got: {err:?}"
        );
    }

    // -----------------------------------------------------------------------
    // CatalogueSubscribeHandler tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_subscribe_handler_basic() {
        let client = Arc::new(MockCatalogueClient::new());
        let handler = CatalogueSubscribeHandler::new(
            client.clone(),
            "test-net",
            "subscription",
            "subscribed",
        );

        let mut inputs = HashMap::new();
        inputs.insert(
            "subscription".to_string(),
            serde_json::json!({
                "signal_place": "inbox",
                "query": { "category": { "eq": "model" } },
            }),
        );

        let input = EffectInput {
            transition_id: TransitionId::named("subscribe-test"),
            inputs,
            config: None,
            read_inputs: HashMap::new(),
            process_step: None,
        };

        let output = handler.execute(input).await.unwrap();
        let result_token = output.tokens.get("subscribed").expect("output token");

        assert_eq!(
            result_token["subscription_id"].as_str().unwrap(),
            "mock-sub-id",
        );
        // Original fields preserved
        assert_eq!(result_token["signal_place"].as_str().unwrap(), "inbox");

        // Effect result
        assert_eq!(
            output.result["subscription_id"].as_str().unwrap(),
            "mock-sub-id",
        );
        assert_eq!(output.result["signal_place"].as_str().unwrap(), "inbox",);
    }

    #[tokio::test]
    async fn test_subscribe_handler_missing_signal_place_returns_fatal() {
        let client = Arc::new(MockCatalogueClient::new());
        let handler = CatalogueSubscribeHandler::new(
            client.clone(),
            "test-net",
            "subscription",
            "subscribed",
        );

        let mut inputs = HashMap::new();
        inputs.insert(
            "subscription".to_string(),
            serde_json::json!({
                "query": { "category": { "eq": "model" } },
            }),
        );

        let input = EffectInput {
            transition_id: TransitionId::named("subscribe-missing"),
            inputs,
            config: None,
            read_inputs: HashMap::new(),
            process_step: None,
        };

        let err = handler.execute(input).await.unwrap_err();
        assert!(
            matches!(err, EffectError::Fatal(_)),
            "missing signal_place should be Fatal, got: {err:?}"
        );
    }

    // -----------------------------------------------------------------------
    // CatalogueUnsubscribeHandler tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_unsubscribe_handler_basic() {
        let client = Arc::new(MockCatalogueClient::new());
        let handler = CatalogueUnsubscribeHandler::new(client.clone(), "handle", "unsubscribed");

        let mut inputs = HashMap::new();
        inputs.insert(
            "handle".to_string(),
            serde_json::json!({
                "subscription_id": "sub-123",
            }),
        );

        let input = EffectInput {
            transition_id: TransitionId::named("unsubscribe-test"),
            inputs,
            config: None,
            read_inputs: HashMap::new(),
            process_step: None,
        };

        let output = handler.execute(input).await.unwrap();
        let result_token = output.tokens.get("unsubscribed").expect("output token");

        assert_eq!(result_token["unsubscribed"].as_bool().unwrap(), true);
        // Original fields preserved
        assert_eq!(result_token["subscription_id"].as_str().unwrap(), "sub-123",);

        // Effect result
        assert_eq!(output.result["unsubscribed"].as_bool().unwrap(), true);
        assert_eq!(
            output.result["subscription_id"].as_str().unwrap(),
            "sub-123",
        );
    }

    #[tokio::test]
    async fn test_unsubscribe_handler_missing_subscription_id_returns_fatal() {
        let client = Arc::new(MockCatalogueClient::new());
        let handler = CatalogueUnsubscribeHandler::new(client.clone(), "handle", "unsubscribed");

        let mut inputs = HashMap::new();
        inputs.insert("handle".to_string(), serde_json::json!({}));

        let input = EffectInput {
            transition_id: TransitionId::named("unsubscribe-missing"),
            inputs,
            config: None,
            read_inputs: HashMap::new(),
            process_step: None,
        };

        let err = handler.execute(input).await.unwrap_err();
        assert!(
            matches!(err, EffectError::Fatal(_)),
            "missing subscription_id should be Fatal, got: {err:?}"
        );
    }
}
