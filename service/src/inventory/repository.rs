//! Inventory repository trait + Postgres implementation.
//!
//! Mirrors `catalogue::repository` so HTTP handlers share one data-access layer.

use async_trait::async_trait;
use sqlx::PgPool;

use crate::query::builder::QueryError;
use crate::query::extractor::QueryParams;
use crate::query::pagination::Paginated;

use super::model::*;
use super::queries;

#[async_trait]
pub trait InventoryRepository: Send + Sync {
    async fn list_entries(
        &self,
        params: &QueryParams,
    ) -> Result<Paginated<InventoryEntry>, QueryError>;

    async fn stats(&self) -> Result<InventoryStats, QueryError>;

    async fn register(
        &self,
        req: &InventoryRegisterRequest,
    ) -> Result<InventoryRegisterResponse, QueryError>;
}

#[derive(Clone)]
pub struct PgInventoryRepository {
    pool: PgPool,
}

impl PgInventoryRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl InventoryRepository for PgInventoryRepository {
    async fn list_entries(
        &self,
        params: &QueryParams,
    ) -> Result<Paginated<InventoryEntry>, QueryError> {
        queries::list_entries(&self.pool, params).await
    }

    async fn stats(&self) -> Result<InventoryStats, QueryError> {
        queries::stats(&self.pool)
            .await
            .map_err(QueryError::Database)
    }

    async fn register(
        &self,
        req: &InventoryRegisterRequest,
    ) -> Result<InventoryRegisterResponse, QueryError> {
        queries::register(&self.pool, req)
            .await
            .map_err(QueryError::Database)
    }
}
