//! Data catalogue contract types.
//!
//! Defines the abstract interface between the Petri engine's catalogue effect
//! handlers and the catalogue backend (Mekhan). Catalogue registration is handled
//! by the causality projector (Mekhan reads `EffectCompleted` events from
//! PETRI_GLOBAL). The `CatalogueClient` trait covers synchronous operations:
//! lookup, subscribe, and unsubscribe via NATS request-reply.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Command to register a single artifact in the data catalogue.
///
/// Published by `CatalogueRegisterHandler` to the catalogue NATS stream.
/// The backend (Mekhan) deserializes and inserts into Postgres.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CatalogueRegisterCommand {
    /// Execution that produced this artifact.
    pub execution_id: String,
    /// Logical job ID from the Petri net.
    pub job_id: String,
    /// Unique artifact identifier within the execution.
    pub artifact_id: String,
    /// Human-readable artifact name.
    pub name: String,
    /// Artifact category (model, dataset, plot, etc.).
    pub category: String,
    /// Original filename.
    pub filename: String,
    /// MIME type if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    /// File size in bytes if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    /// Path in the artifact store (S3/RustFS).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage_path: Option<String>,
    /// Format-specific metadata extracted by fmeta.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_metadata: Option<serde_json::Value>,
    /// User-defined key-value metadata (remaining after provenance extraction).
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub user_metadata: HashMap<String, String>,

    // -- Provenance fields (extracted from executor job metadata) --
    /// Originating Petri net ID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_net: Option<String>,
    /// Place in the net that triggered the job.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_place: Option<String>,
    /// Signal key for causality tracking across nets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal_key: Option<String>,
    /// Process ID for causality tracking.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_id: Option<String>,
    /// Process step name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_step: Option<String>,

    /// When the artifact was originally created.
    pub created_at: DateTime<Utc>,
}

/// Query request for catalogue lookup effect.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CatalogueLookupRequest {
    /// Filter fields: field_name -> { operator -> value }.
    #[serde(default)]
    pub filters: HashMap<String, HashMap<String, String>>,
    #[serde(default)]
    pub page: Option<i64>,
    #[serde(default)]
    pub page_size: Option<i64>,
    #[serde(default)]
    pub sort: Option<String>,
    #[serde(default)]
    pub search: Option<String>,
    /// JSONB containment filter on user_metadata.
    #[serde(default)]
    pub metadata: Option<String>,
    /// JSONB containment filter on file_metadata.
    #[serde(default)]
    pub file_metadata: Option<String>,
}

/// A single catalogue entry returned from queries.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CatalogueEntry {
    pub id: String,
    pub execution_id: String,
    #[serde(default)]
    pub job_id: Option<String>,
    pub name: String,
    pub category: String,
    pub filename: String,
    #[serde(default)]
    pub mime_type: Option<String>,
    #[serde(default)]
    pub size_bytes: Option<i64>,
    #[serde(default)]
    pub storage_path: Option<String>,
    #[serde(default)]
    pub source_net: Option<String>,
    #[serde(default)]
    pub source_place: Option<String>,
    #[serde(default)]
    pub signal_key: Option<String>,
    #[serde(default)]
    pub process_id: Option<String>,
    #[serde(default)]
    pub process_step: Option<String>,
    #[serde(default)]
    pub file_metadata: serde_json::Value,
    #[serde(default)]
    pub user_metadata: serde_json::Value,
    pub created_at: DateTime<Utc>,
}

/// Paginated lookup response.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CatalogueLookupResponse {
    pub items: Vec<CatalogueEntry>,
    pub total: i64,
    pub page: i64,
    pub page_size: i64,
}

/// Subscribe request for reactive catalogue monitoring.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CatalogueSubscribeRequest {
    /// Net that owns this subscription.
    pub net_id: String,
    /// Place to inject matching catalogue entries into.
    pub signal_place: String,
    /// Filter fields: field_name -> { operator -> value }.
    #[serde(default)]
    pub filters: HashMap<String, HashMap<String, String>>,
    /// Whether to backfill existing matching entries on subscribe.
    #[serde(default)]
    pub backfill: bool,
}

/// Errors from catalogue operations.
#[derive(Debug, thiserror::Error)]
pub enum CatalogueError {
    #[error("catalogue publish failed: {0}")]
    PublishFailed(String),
    #[error("catalogue query failed: {0}")]
    QueryFailed(String),
}

/// Abstract client for catalogue query operations (NATS request-reply).
///
/// Registration is no longer done through this client — the causality projector
/// in Mekhan picks up `EffectCompleted` events from PETRI_GLOBAL and creates
/// catalogue entries with full provenance context.
#[async_trait::async_trait]
pub trait CatalogueClient: Send + Sync {
    /// Query catalogue entries matching the given filters.
    async fn lookup(
        &self,
        request: CatalogueLookupRequest,
    ) -> Result<CatalogueLookupResponse, CatalogueError>;

    /// Create a reactive subscription for catalogue changes.
    ///
    /// Returns a subscription handle ID that can be passed to `unsubscribe`.
    async fn subscribe(
        &self,
        request: CatalogueSubscribeRequest,
    ) -> Result<String, CatalogueError>;

    /// Remove a previously created subscription.
    ///
    /// Returns `true` if the subscription existed and was removed.
    async fn unsubscribe(&self, subscription_id: &str) -> Result<bool, CatalogueError>;

    /// Human-readable name for this client.
    fn name(&self) -> &str;
}
