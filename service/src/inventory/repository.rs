//! Inventory repository trait + Postgres implementation.
//!
//! Mirrors `catalogue::repository` so HTTP handlers share one data-access layer.

use async_trait::async_trait;
use sqlx::PgPool;
use uuid::Uuid;

use crate::query::builder::QueryError;
use crate::query::extractor::QueryParams;
use crate::query::pagination::Paginated;

use super::model::*;
use super::queries;

/// Read/write inventory repository. Every method takes the calling tenant's
/// `workspace_id` explicitly so the per-workspace scope is injected by the
/// query layer (never inferred) — one enforcement point for the HTTP handlers.
#[async_trait]
pub trait InventoryRepository: Send + Sync {
    async fn list_entries(
        &self,
        workspace_id: Uuid,
        params: &QueryParams,
    ) -> Result<Paginated<InventoryEntry>, QueryError>;

    async fn stats(&self, workspace_id: Uuid) -> Result<InventoryStats, QueryError>;

    async fn register(
        &self,
        workspace_id: Uuid,
        req: &InventoryRegisterRequest,
    ) -> Result<InventoryRegisterResponse, QueryError>;

    async fn index(
        &self,
        workspace_id: Uuid,
        req: &InventoryIndexRequest,
    ) -> Result<InventoryIndexResponse, QueryError>;
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
        workspace_id: Uuid,
        params: &QueryParams,
    ) -> Result<Paginated<InventoryEntry>, QueryError> {
        queries::list_entries(&self.pool, workspace_id, params).await
    }

    async fn stats(&self, workspace_id: Uuid) -> Result<InventoryStats, QueryError> {
        queries::stats(&self.pool, workspace_id)
            .await
            .map_err(QueryError::Database)
    }

    async fn register(
        &self,
        workspace_id: Uuid,
        req: &InventoryRegisterRequest,
    ) -> Result<InventoryRegisterResponse, QueryError> {
        queries::register(&self.pool, workspace_id, req).await
    }

    async fn index(
        &self,
        workspace_id: Uuid,
        req: &InventoryIndexRequest,
    ) -> Result<InventoryIndexResponse, QueryError> {
        queries::index(&self.pool, workspace_id, req)
            .await
            .map_err(QueryError::Database)
    }
}
