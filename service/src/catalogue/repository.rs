//! Catalogue repository trait and Postgres implementation.
//!
//! Provides a `CatalogueRepository` abstraction so that both HTTP handlers
//! and the NATS request-reply responder share the same data-access layer.

use async_trait::async_trait;
use sqlx::PgPool;

use crate::query::builder::QueryError;
use crate::query::extractor::QueryParams;
use crate::query::pagination::Paginated;

use super::model::*;
use super::queries;

/// Read-only catalogue repository.
///
/// Implementations must be `Send + Sync` so they can be shared behind `Arc`
/// across Axum handlers and background NATS tasks.
#[async_trait]
pub trait CatalogueRepository: Send + Sync {
    async fn list_entries(
        &self,
        params: &QueryParams,
    ) -> Result<Paginated<CatalogueEntry>, QueryError>;

    async fn get_entry(
        &self,
        execution_id: &str,
        id: &str,
    ) -> Result<Option<CatalogueEntry>, QueryError>;

    async fn stats(
        &self,
        params: &QueryParams,
    ) -> Result<CatalogueStats, QueryError>;

    async fn stats_by_net(&self) -> Result<Vec<NetStats>, QueryError>;

    async fn lineage_grouped(
        &self,
        process_id: &str,
    ) -> Result<LineageResponse, QueryError>;

    async fn distinct_values(
        &self,
        column: &str,
    ) -> Result<Vec<String>, QueryError>;

    async fn distinct_jsonb_values(
        &self,
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
        params: &QueryParams,
    ) -> Result<Paginated<CatalogueEntry>, QueryError> {
        queries::list_entries(&self.pool, params).await
    }

    async fn get_entry(
        &self,
        execution_id: &str,
        id: &str,
    ) -> Result<Option<CatalogueEntry>, QueryError> {
        queries::get_entry(&self.pool, execution_id, id)
            .await
            .map_err(QueryError::Database)
    }

    async fn stats(
        &self,
        params: &QueryParams,
    ) -> Result<CatalogueStats, QueryError> {
        queries::stats(&self.pool, params).await
    }

    async fn stats_by_net(&self) -> Result<Vec<NetStats>, QueryError> {
        queries::stats_by_net(&self.pool)
            .await
            .map_err(QueryError::Database)
    }

    async fn lineage_grouped(
        &self,
        process_id: &str,
    ) -> Result<LineageResponse, QueryError> {
        queries::lineage_grouped(&self.pool, process_id)
            .await
            .map_err(QueryError::Database)
    }

    async fn distinct_values(
        &self,
        column: &str,
    ) -> Result<Vec<String>, QueryError> {
        queries::distinct_values(&self.pool, column).await
    }

    async fn distinct_jsonb_values(
        &self,
        column: &str,
        key: &str,
    ) -> Result<Vec<String>, QueryError> {
        queries::distinct_jsonb_values(&self.pool, column, key).await
    }
}
