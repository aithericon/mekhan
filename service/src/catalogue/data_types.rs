//! Registered data types — schema-fingerprint digests promoted to named,
//! described types (`catalogue_data_types` + `catalogue_data_type_digests`,
//! migration 20240173).
//!
//! A "data type" is a user-facing name over one or more fmeta schema digests
//! (FNV-1a 64 hex16, the `meta.schema` virtual field). At promote/attach time
//! the server resolves an EXEMPLAR catalogue entry carrying the digest,
//! re-deserializes its `file_metadata` into the real [`FileMetadata`], clears
//! and recomputes the fingerprint ([`compute_schema_fingerprint`]) to verify
//! the stored digest, and projects the canonical column set through
//! [`humanize_data_type`].
//!
//! The stored `columns` JSONB is a HUMANIZED display projection (e.g.
//! `timestamp<UTC>`) — never "re-verify" a digest from it; the fingerprint is
//! only verifiable against a typed exemplar (documented in the migration too).

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use aithericon_file_metadata::compute_schema_fingerprint;
use aithericon_file_metadata::types::FileMetadata;

use crate::auth::AuthUser;
use crate::models::error::{ApiError, ErrorResponse};
use crate::AppState;

use super::metadata_view::humanize_data_type;
use super::saved_queries::is_unique_violation;

/// One column of a registered data type's canonical schema.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DataTypeColumn {
    pub name: String,
    /// Humanized display type (e.g. `int64`, `timestamp<UTC>`), NOT the
    /// fingerprint-canonical serde form.
    pub data_type: String,
    pub nullable: bool,
}

/// A registered data type: a named set of schema digests with the canonical
/// column projection derived from an exemplar entry at promote time.
#[derive(Debug, Serialize, sqlx::FromRow, ToSchema)]
pub struct CatalogueDataType {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
    /// Canonical display columns (derived from the promote-time exemplar).
    #[sqlx(json)]
    pub columns: Vec<DataTypeColumn>,
    /// Schema digests owned by this type (hex16; a digest belongs to ≤1 type).
    pub digests: Vec<String>,
    /// Live count of catalogue entries carrying any owned digest.
    pub entry_count: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Author (`subject_as_uuid()`), resolvable via `user_profiles`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_by: Option<Uuid>,
    /// Last mutator (`subject_as_uuid()`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_by: Option<Uuid>,
}

/// Promote payload: name a schema digest.
#[derive(Debug, Deserialize, ToSchema)]
pub struct DataTypePromote {
    /// Schema-fingerprint digest (hex16) — must be carried by at least one
    /// catalogue entry (the exemplar).
    pub digest: String,
    pub name: String,
    pub description: Option<String>,
}

/// Patch payload — every field optional; only provided fields are applied.
#[derive(Debug, Deserialize, ToSchema)]
pub struct DataTypeUpdate {
    pub name: Option<String>,
    pub description: Option<String>,
    /// Digests to attach. Each must resolve + verify against a live exemplar
    /// (columns are NOT required to match — attaching schema variants under
    /// one name is the point). An already-owned digest is a conflict.
    pub attach_digests: Option<Vec<String>>,
    /// Digests to detach (unconditional; unknown digests are no-ops).
    pub detach_digests: Option<Vec<String>>,
}

/// Promote/attach failure modes, mapped to HTTP by the handlers:
/// [`NoExemplar`](PromoteError::NoExemplar) → 404,
/// [`Unparseable`](PromoteError::Unparseable) /
/// [`FingerprintMismatch`](PromoteError::FingerprintMismatch) → 422,
/// a unique violation inside [`Database`](PromoteError::Database) → 409.
#[derive(Debug, thiserror::Error)]
pub enum PromoteError {
    #[error("no catalogue entry carries schema digest {0:?}")]
    NoExemplar(String),
    #[error("no exemplar for digest {digest:?} deserializes as fmeta FileMetadata: {reason}")]
    Unparseable { digest: String, reason: String },
    #[error(
        "recomputed fingerprint {recomputed} for digest {digest:?} does not match — \
         fmeta algorithm drift; refusing to register"
    )]
    FingerprintMismatch { digest: String, recomputed: String },
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
}

impl PromoteError {
    fn into_api_error(self, context: &str) -> ApiError {
        match self {
            PromoteError::NoExemplar(_) => ApiError::not_found(self.to_string()),
            PromoteError::Unparseable { .. } | PromoteError::FingerprintMismatch { .. } => {
                ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, self.to_string())
            }
            PromoteError::Database(ref e) if is_unique_violation(e) => ApiError::conflict(
                "name already taken or digest already owned by a data type".to_string(),
            ),
            PromoteError::Database(e) => ApiError::internal(format!("{context}: {e}")),
        }
    }
}

// ── Exemplar resolution ──────────────────────────────────────────────────────

/// Parse one candidate exemplar's `file_metadata` and verify its digest by
/// clearing + recomputing the fingerprint over the typed columns. Pure —
/// unit-tested offline with handmade fixtures.
fn verify_exemplar(
    digest: &str,
    raw: serde_json::Value,
) -> Result<Vec<DataTypeColumn>, PromoteError> {
    let mut fm: FileMetadata =
        serde_json::from_value(raw).map_err(|e| PromoteError::Unparseable {
            digest: digest.to_string(),
            reason: e.to_string(),
        })?;
    fm.schema_fingerprint = None;
    compute_schema_fingerprint(&mut fm);
    let recomputed = fm
        .schema_fingerprint
        .as_ref()
        .map(|fp| fp.digest.clone())
        .unwrap_or_default();
    if recomputed != digest {
        return Err(PromoteError::FingerprintMismatch {
            digest: digest.to_string(),
            recomputed,
        });
    }
    Ok(fm
        .columns
        .iter()
        .map(|c| DataTypeColumn {
            name: c.name.clone(),
            data_type: humanize_data_type(&c.data_type),
            nullable: c.nullable,
        })
        .collect())
}

/// Resolve + verify an exemplar entry for `digest` (rides
/// `idx_cat_fmeta_schema`). The newest few candidates are tried; the first
/// that PARSES as fmeta wins and is then fingerprint-verified.
async fn resolve_exemplar(
    pool: &sqlx::PgPool,
    digest: &str,
) -> Result<Vec<DataTypeColumn>, PromoteError> {
    let rows: Vec<(serde_json::Value,)> = sqlx::query_as(
        "SELECT file_metadata FROM catalogue_entries \
         WHERE (file_metadata->'schema_fingerprint'->>'digest') = $1 \
         ORDER BY catalogued_at DESC LIMIT 5",
    )
    .bind(digest)
    .fetch_all(pool)
    .await?;

    if rows.is_empty() {
        return Err(PromoteError::NoExemplar(digest.to_string()));
    }
    let mut last_err = None;
    for (raw,) in rows {
        match verify_exemplar(digest, raw) {
            Ok(columns) => return Ok(columns),
            // A row that doesn't parse may be a stale/foreign producer — the
            // next candidate may still be a valid exemplar. A fingerprint
            // mismatch on a PARSED row is terminal (algorithm drift).
            Err(e @ PromoteError::Unparseable { .. }) => last_err = Some(e),
            Err(e) => return Err(e),
        }
    }
    Err(last_err.expect("non-empty rows with no success leave an error"))
}

// ── Query layer (shared by the HTTP handlers and integration tests) ─────────

/// Shared SELECT head: the type row + an `array_agg` digest lateral + a LIVE
/// entry-count lateral (entries carrying any owned digest, via the same
/// expression `idx_cat_fmeta_schema` indexes). The count lateral is per-row —
/// fine while registered types number in the hundreds; revisit with a
/// materialized count if that grows.
const DATA_TYPE_SELECT: &str = "SELECT t.id, t.name, t.description, t.columns, \
       d.digests, c.entry_count, \
       t.created_at, t.updated_at, t.created_by, t.updated_by \
     FROM catalogue_data_types t \
     LEFT JOIN LATERAL ( \
       SELECT coalesce(array_agg(digest ORDER BY created_at, digest), '{}') AS digests \
       FROM catalogue_data_type_digests WHERE data_type_id = t.id \
     ) d ON TRUE \
     LEFT JOIN LATERAL ( \
       SELECT count(*)::bigint AS entry_count FROM catalogue_entries e \
       WHERE (e.file_metadata->'schema_fingerprint'->>'digest') IN ( \
         SELECT digest FROM catalogue_data_type_digests WHERE data_type_id = t.id \
       ) \
     ) c ON TRUE";

pub async fn list(pool: &sqlx::PgPool) -> Result<Vec<CatalogueDataType>, sqlx::Error> {
    sqlx::query_as(&format!("{DATA_TYPE_SELECT} ORDER BY t.created_at DESC"))
        .fetch_all(pool)
        .await
}

pub async fn get(pool: &sqlx::PgPool, id: Uuid) -> Result<Option<CatalogueDataType>, sqlx::Error> {
    sqlx::query_as(&format!("{DATA_TYPE_SELECT} WHERE t.id = $1"))
        .bind(id)
        .fetch_optional(pool)
        .await
}

pub async fn promote(
    pool: &sqlx::PgPool,
    body: &DataTypePromote,
    created_by: Uuid,
) -> Result<CatalogueDataType, PromoteError> {
    let columns = resolve_exemplar(pool, &body.digest).await?;
    let columns_json =
        serde_json::to_value(&columns).expect("DataTypeColumn serialization is infallible");

    let mut tx = pool.begin().await?;
    let (id,): (Uuid,) = sqlx::query_as(
        "INSERT INTO catalogue_data_types (name, description, columns, created_by, updated_by) \
         VALUES ($1, $2, $3, $4, $4) RETURNING id",
    )
    .bind(&body.name)
    .bind(&body.description)
    .bind(columns_json)
    .bind(created_by)
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO catalogue_data_type_digests (digest, data_type_id, created_by) \
         VALUES ($1, $2, $3)",
    )
    .bind(&body.digest)
    .bind(id)
    .bind(created_by)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    get(pool, id)
        .await?
        .ok_or_else(|| PromoteError::Database(sqlx::Error::RowNotFound))
}

pub async fn update(
    pool: &sqlx::PgPool,
    id: Uuid,
    body: &DataTypeUpdate,
    updated_by: Uuid,
) -> Result<Option<CatalogueDataType>, PromoteError> {
    // Every attach digest must resolve + verify against a live exemplar
    // BEFORE anything mutates (404/422 win over partial writes).
    if let Some(ref digests) = body.attach_digests {
        for digest in digests {
            resolve_exemplar(pool, digest).await?;
        }
    }

    let mut tx = pool.begin().await?;
    let touched: Option<(Uuid,)> = sqlx::query_as(
        "UPDATE catalogue_data_types SET \
           name = COALESCE($2, name), \
           description = COALESCE($3, description), \
           updated_at = now(), \
           updated_by = $4 \
         WHERE id = $1 RETURNING id",
    )
    .bind(id)
    .bind(&body.name)
    .bind(&body.description)
    .bind(updated_by)
    .fetch_optional(&mut *tx)
    .await?;
    if touched.is_none() {
        return Ok(None);
    }
    if let Some(ref digests) = body.attach_digests {
        for digest in digests {
            // Digest PK conflict (owned by ANY type, incl. this one) → 409.
            sqlx::query(
                "INSERT INTO catalogue_data_type_digests (digest, data_type_id, created_by) \
                 VALUES ($1, $2, $3)",
            )
            .bind(digest)
            .bind(id)
            .bind(updated_by)
            .execute(&mut *tx)
            .await?;
        }
    }
    if let Some(ref digests) = body.detach_digests {
        sqlx::query(
            "DELETE FROM catalogue_data_type_digests WHERE data_type_id = $1 AND digest = ANY($2)",
        )
        .bind(id)
        .bind(digests)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;

    Ok(get(pool, id).await?)
}

/// Returns whether a row was deleted (digest rows cascade).
pub async fn delete(pool: &sqlx::PgPool, id: Uuid) -> Result<bool, sqlx::Error> {
    let res = sqlx::query("DELETE FROM catalogue_data_types WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(res.rows_affected() > 0)
}

// ── HTTP handlers ────────────────────────────────────────────────────────────

/// GET /api/v1/catalogue/data-types — list registered data types, newest first.
#[utoipa::path(
    get,
    path = "/api/v1/catalogue/data-types",
    responses(
        (status = 200, description = "Registered data types, newest first", body = Vec<CatalogueDataType>),
        (status = 500, description = "Server error", body = ErrorResponse),
    ),
    tag = "catalogue",
)]
pub async fn list_data_types(
    State(state): State<AppState>,
    _user: AuthUser,
) -> Result<Json<Vec<CatalogueDataType>>, ApiError> {
    let rows = list(&state.db)
        .await
        .map_err(|e| ApiError::internal(format!("data types list: {e}")))?;
    Ok(Json(rows))
}

/// POST /api/v1/catalogue/data-types — promote a schema digest to a named
/// data type. The server derives the canonical columns from an exemplar entry.
#[utoipa::path(
    post,
    path = "/api/v1/catalogue/data-types",
    request_body = DataTypePromote,
    responses(
        (status = 201, description = "Created", body = CatalogueDataType),
        (status = 404, description = "No catalogue entry carries the digest", body = ErrorResponse),
        (status = 409, description = "Duplicate name, or digest already owned", body = ErrorResponse),
        (status = 422, description = "Exemplar unparseable or fingerprint drift", body = ErrorResponse),
        (status = 500, description = "Server error", body = ErrorResponse),
    ),
    tag = "catalogue",
)]
pub async fn promote_data_type(
    State(state): State<AppState>,
    user: AuthUser,
    Json(body): Json<DataTypePromote>,
) -> Result<(StatusCode, Json<CatalogueDataType>), ApiError> {
    let row = promote(&state.db, &body, user.subject_as_uuid())
        .await
        .map_err(|e| e.into_api_error("data type promote"))?;
    Ok((StatusCode::CREATED, Json(row)))
}

/// GET /api/v1/catalogue/data-types/{id}.
#[utoipa::path(
    get,
    path = "/api/v1/catalogue/data-types/{id}",
    params(("id" = Uuid, Path, description = "Data type id")),
    responses(
        (status = 200, description = "The data type", body = CatalogueDataType),
        (status = 404, description = "Not found", body = ErrorResponse),
        (status = 500, description = "Server error", body = ErrorResponse),
    ),
    tag = "catalogue",
)]
pub async fn get_data_type(
    State(state): State<AppState>,
    _user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<CatalogueDataType>, ApiError> {
    let row = get(&state.db, id)
        .await
        .map_err(|e| ApiError::internal(format!("data type get: {e}")))?;
    row.map(Json)
        .ok_or_else(|| ApiError::not_found(format!("data type {id} not found")))
}

/// PATCH /api/v1/catalogue/data-types/{id} — rename/redescribe and/or
/// attach/detach schema digests.
#[utoipa::path(
    patch,
    path = "/api/v1/catalogue/data-types/{id}",
    params(("id" = Uuid, Path, description = "Data type id")),
    request_body = DataTypeUpdate,
    responses(
        (status = 200, description = "Updated", body = CatalogueDataType),
        (status = 404, description = "Type not found, or an attach digest has no exemplar", body = ErrorResponse),
        (status = 409, description = "Duplicate name, or digest already owned", body = ErrorResponse),
        (status = 422, description = "Attach exemplar unparseable or fingerprint drift", body = ErrorResponse),
        (status = 500, description = "Server error", body = ErrorResponse),
    ),
    tag = "catalogue",
)]
pub async fn update_data_type(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<DataTypeUpdate>,
) -> Result<Json<CatalogueDataType>, ApiError> {
    let row = update(&state.db, id, &body, user.subject_as_uuid())
        .await
        .map_err(|e| e.into_api_error("data type update"))?;
    row.map(Json)
        .ok_or_else(|| ApiError::not_found(format!("data type {id} not found")))
}

/// DELETE /api/v1/catalogue/data-types/{id} — digest rows cascade.
#[utoipa::path(
    delete,
    path = "/api/v1/catalogue/data-types/{id}",
    params(("id" = Uuid, Path, description = "Data type id")),
    responses(
        (status = 204, description = "Deleted"),
        (status = 404, description = "Not found", body = ErrorResponse),
        (status = 500, description = "Server error", body = ErrorResponse),
    ),
    tag = "catalogue",
)]
pub async fn delete_data_type(
    State(state): State<AppState>,
    _user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let deleted = delete(&state.db, id)
        .await
        .map_err(|e| ApiError::internal(format!("data type delete: {e}")))?;
    if !deleted {
        return Err(ApiError::not_found(format!("data type {id} not found")));
    }
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aithericon_file_metadata::data_type::DataType;
    use aithericon_file_metadata::format::FileFormat;
    use aithericon_file_metadata::types::ColumnInfo;
    use std::collections::HashMap;

    fn col(name: &str, dt: DataType, nullable: bool) -> ColumnInfo {
        ColumnInfo {
            name: name.into(),
            data_type: dt,
            nullable,
            metadata: HashMap::new(),
            statistics: None,
            classifications: vec![],
        }
    }

    /// Handmade exemplar with a REAL computed fingerprint, exercising the
    /// nested `Timestamp { timezone }` shape (humanizes to `timestamp<UTC>`).
    fn exemplar() -> FileMetadata {
        let mut fm = FileMetadata {
            format: FileFormat::Parquet,
            mime_type: None,
            num_rows: Some(1000),
            num_columns: Some(3),
            file_size_bytes: None,
            file_name: None,
            modified_at: None,
            created_at: None,
            readonly: false,
            unix_mode: None,
            column_names: vec!["id".into(), "ts".into(), "score".into()],
            dimensions: vec![],
            columns: vec![
                col("id", DataType::Int64, false),
                col(
                    "ts",
                    DataType::Timestamp {
                        timezone: Some("UTC".into()),
                    },
                    false,
                ),
                col("score", DataType::Float64, true),
            ],
            attributes: HashMap::new(),
            format_specific: None,
            preview: None,
            encrypted: None,
            checksum: None,
            schema_fingerprint: None,
            data_quality: None,
            extracted_at: chrono::Utc::now(),
        };
        compute_schema_fingerprint(&mut fm);
        fm
    }

    /// Happy path: the stored digest verifies and the columns project through
    /// `humanize_data_type` (incl. the nested timestamp variant).
    #[test]
    fn verify_exemplar_projects_humanized_columns() {
        let fm = exemplar();
        let digest = fm.schema_fingerprint.as_ref().unwrap().digest.clone();
        let raw = serde_json::to_value(&fm).expect("serialize exemplar");

        let columns = verify_exemplar(&digest, raw).expect("verify");
        let view: Vec<(&str, &str, bool)> = columns
            .iter()
            .map(|c| (c.name.as_str(), c.data_type.as_str(), c.nullable))
            .collect();
        assert_eq!(
            view,
            [
                ("id", "int64", false),
                ("ts", "timestamp<UTC>", false),
                ("score", "float64", true),
            ]
        );
    }

    /// A digest that doesn't match the recomputed fingerprint is refused —
    /// the stored `columns` projection must never be born from a drifted hash.
    #[test]
    fn verify_exemplar_detects_fingerprint_mismatch() {
        let fm = exemplar();
        let real = fm.schema_fingerprint.as_ref().unwrap().digest.clone();
        let raw = serde_json::to_value(&fm).expect("serialize exemplar");

        let err = verify_exemplar("0000000000000000", raw).unwrap_err();
        match err {
            PromoteError::FingerprintMismatch { digest, recomputed } => {
                assert_eq!(digest, "0000000000000000");
                assert_eq!(recomputed, real, "recomputes the true digest");
            }
            other => panic!("expected FingerprintMismatch, got {other}"),
        }
    }

    /// Non-fmeta JSON → Unparseable (422), not a panic or a silent accept.
    #[test]
    fn verify_exemplar_rejects_unparseable_blob() {
        let err = verify_exemplar(
            "0123456789abcdef",
            serde_json::json!({"format": 42, "columns": "nope"}),
        )
        .unwrap_err();
        assert!(
            matches!(err, PromoteError::Unparseable { ref digest, .. } if digest == "0123456789abcdef"),
            "expected Unparseable: {err}"
        );
    }
}
