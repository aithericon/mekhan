//! Catalogue repository trait and Postgres implementation.
//!
//! Provides a `CatalogueRepository` abstraction so that both HTTP handlers
//! and the NATS request-reply responder share the same data-access layer.

use async_trait::async_trait;
use sqlx::PgPool;
use uuid::Uuid;

use crate::query::builder::QueryError;
use crate::query::extractor::QueryParams;
use crate::query::pagination::Paginated;

use super::model::*;
use super::queries;

/// Read-only catalogue repository.
///
/// Implementations must be `Send + Sync` so they can be shared behind `Arc`
/// across Axum handlers and background NATS tasks.
///
/// Every read method takes the calling tenant's `workspace_id` explicitly so
/// the per-workspace scope is injected by the query layer (never inferred), the
/// single point of enforcement for both the HTTP handlers and the NATS
/// responder.
#[async_trait]
pub trait CatalogueRepository: Send + Sync {
    async fn list_entries(
        &self,
        workspace_id: Uuid,
        params: &QueryParams,
    ) -> Result<Paginated<CatalogueEntry>, QueryError>;

    async fn get_entry(
        &self,
        workspace_id: Uuid,
        execution_id: &str,
        id: &str,
    ) -> Result<Option<CatalogueEntry>, QueryError>;

    async fn stats(
        &self,
        workspace_id: Uuid,
        params: &QueryParams,
    ) -> Result<CatalogueStats, QueryError>;

    async fn stats_by_net(&self, workspace_id: Uuid) -> Result<Vec<NetStats>, QueryError>;

    async fn lineage_grouped(
        &self,
        workspace_id: Uuid,
        process_id: &str,
    ) -> Result<LineageResponse, QueryError>;

    async fn distinct_values(
        &self,
        workspace_id: Uuid,
        column: &str,
    ) -> Result<Vec<String>, QueryError>;

    async fn distinct_jsonb_values(
        &self,
        workspace_id: Uuid,
        column: &str,
        key: &str,
    ) -> Result<Vec<String>, QueryError>;
}

/// Postgres-backed implementation that delegates to the existing `queries` module.
#[derive(Clone)]
pub struct PgCatalogueRepository {
    pool: PgPool,
}

impl PgCatalogueRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl CatalogueRepository for PgCatalogueRepository {
    async fn list_entries(
        &self,
        workspace_id: Uuid,
        params: &QueryParams,
    ) -> Result<Paginated<CatalogueEntry>, QueryError> {
        queries::list_entries(&self.pool, workspace_id, params).await
    }

    async fn get_entry(
        &self,
        workspace_id: Uuid,
        execution_id: &str,
        id: &str,
    ) -> Result<Option<CatalogueEntry>, QueryError> {
        queries::get_entry(&self.pool, workspace_id, execution_id, id)
            .await
            .map_err(QueryError::Database)
    }

    async fn stats(
        &self,
        workspace_id: Uuid,
        params: &QueryParams,
    ) -> Result<CatalogueStats, QueryError> {
        queries::stats(&self.pool, workspace_id, params).await
    }

    async fn stats_by_net(&self, workspace_id: Uuid) -> Result<Vec<NetStats>, QueryError> {
        queries::stats_by_net(&self.pool, workspace_id)
            .await
            .map_err(QueryError::Database)
    }

    async fn lineage_grouped(
        &self,
        workspace_id: Uuid,
        process_id: &str,
    ) -> Result<LineageResponse, QueryError> {
        queries::lineage_grouped(&self.pool, workspace_id, process_id)
            .await
            .map_err(QueryError::Database)
    }

    async fn distinct_values(
        &self,
        workspace_id: Uuid,
        column: &str,
    ) -> Result<Vec<String>, QueryError> {
        queries::distinct_values(&self.pool, workspace_id, column).await
    }

    async fn distinct_jsonb_values(
        &self,
        workspace_id: Uuid,
        column: &str,
        key: &str,
    ) -> Result<Vec<String>, QueryError> {
        queries::distinct_jsonb_values(&self.pool, workspace_id, column, key).await
    }
}
