//! Wire types for NATS catalogue request-reply protocol.
//!
//! Subjects follow the pattern `catalogue.query.<operation>`:
//! - `catalogue.query.list`         — paginated listing with filters
//! - `catalogue.query.get`          — single entry by composite key
//! - `catalogue.query.lineage`      — lineage grouped by step
//! - `catalogue.query.stats`        — aggregate statistics
//! - `catalogue.query.stats-by-net` — per-net breakdown
//! - `catalogue.query.distinct`     — distinct column values
//! - `catalogue.query.distinct-jsonb` — distinct JSONB key values

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::query::extractor::QueryParams;
use crate::query::filter::{Filter, FilterCondition, FilterValue};
use crate::query::pagination::{PageQuery, Sort};

/// Resolve an optional wire `workspace_id` string into the scoping `Uuid`.
///
/// Job nets carry their owning workspace as a string in every catalogue query
/// request (mirrors the engine's `LoadScenarioRequest.workspace_id`). An
/// absent / unparseable value falls back to the nil workspace, matching the
/// `DEFAULT '00000000-...'` backfill on the catalogue columns — so a legacy
/// caller that doesn't yet send a workspace still resolves to the shared
/// default tenant rather than erroring.
pub(crate) fn resolve_workspace(raw: Option<&str>) -> uuid::Uuid {
    raw.and_then(|s| uuid::Uuid::parse_str(s.trim()).ok())
        .unwrap_or_else(uuid::Uuid::nil)
}

/// Request for `catalogue.query.list` and `catalogue.query.stats`.
#[derive(Debug, Serialize, Deserialize)]
pub struct CatalogueQueryRequest {
    /// Owning workspace of the requesting job net (string form of the scoping
    /// `Uuid`). Server-enforced — absent resolves to the nil workspace.
    #[serde(default)]
    pub workspace_id: Option<String>,
    #[serde(default)]
    pub filters: Option<HashMap<String, HashMap<String, String>>>,
    #[serde(default)]
    pub page: Option<i64>,
    #[serde(default)]
    pub page_size: Option<i64>,
    #[serde(default)]
    pub sort: Option<String>,
    #[serde(default)]
    pub search: Option<String>,
    #[serde(default)]
    pub metadata: Option<String>,
    #[serde(default)]
    pub file_metadata: Option<String>,
}

/// Request for `catalogue.query.get`.
#[derive(Debug, Serialize, Deserialize)]
pub struct CatalogueGetRequest {
    #[serde(default)]
    pub workspace_id: Option<String>,
    pub execution_id: String,
    pub id: String,
}

/// Request for `catalogue.query.lineage`.
#[derive(Debug, Serialize, Deserialize)]
pub struct CatalogueLineageRequest {
    #[serde(default)]
    pub workspace_id: Option<String>,
    pub process_id: String,
}

/// Request for `catalogue.query.stats-by-net` (no body fields besides the
/// scope).
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct CatalogueStatsByNetRequest {
    #[serde(default)]
    pub workspace_id: Option<String>,
}

/// Request for `catalogue.query.distinct`.
#[derive(Debug, Serialize, Deserialize)]
pub struct CatalogueDistinctRequest {
    #[serde(default)]
    pub workspace_id: Option<String>,
    pub column: String,
}

/// Request for `catalogue.query.distinct-jsonb`.
#[derive(Debug, Serialize, Deserialize)]
pub struct CatalogueDistinctJsonbRequest {
    #[serde(default)]
    pub workspace_id: Option<String>,
    pub column: String,
    pub key: String,
}

/// Request for `catalogue.subscribe`.
#[derive(Debug, Serialize, Deserialize)]
pub struct SubscribeRequest {
    pub net_id: String,
    pub signal_place: String,
    /// Owning tenant. Server-enforced scope for the backfill query; `None`
    /// (legacy/SDK/dev) resolves to the nil workspace.
    #[serde(default)]
    pub workspace_id: Option<String>,
    /// Catalogue query DSL string (same grammar as the data browser and catalog
    /// triggers). Compiled server-side at evaluation time. Empty = match all.
    #[serde(default)]
    pub query: String,
    #[serde(default)]
    pub backfill: bool,
}

/// Response for `catalogue.subscribe`.
#[derive(Debug, Serialize, Deserialize)]
pub struct SubscribeResponse {
    pub subscription_id: String,
}

/// Request for `catalogue.unsubscribe`.
#[derive(Debug, Serialize, Deserialize)]
pub struct UnsubscribeRequest {
    pub subscription_id: String,
}

/// Response for `catalogue.unsubscribe`.
#[derive(Debug, Serialize, Deserialize)]
pub struct UnsubscribeResponse {
    pub unsubscribed: bool,
}

/// Generic response envelope for NATS replies.
#[derive(Debug, Serialize, Deserialize)]
pub struct CatalogueResponse<T: Serialize> {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl<T: Serialize> CatalogueResponse<T> {
    pub fn ok(data: T) -> Self {
        Self {
            data: Some(data),
            error: None,
        }
    }
}

impl CatalogueResponse<()> {
    pub fn err(msg: impl Into<String>) -> Self {
        Self {
            data: None,
            error: Some(msg.into()),
        }
    }
}

/// Convert a NATS query request into the internal `QueryParams` used by the
/// repository / query builder layer.
impl From<CatalogueQueryRequest> for QueryParams {
    fn from(req: CatalogueQueryRequest) -> Self {
        let page = PageQuery {
            page: req.page.unwrap_or(0),
            page_size: req.page_size.unwrap_or(20),
        };

        let sort = req.sort.as_deref().map(Sort::parse);

        let filter = req.filters.map(|filter_map| {
            let conditions: Vec<FilterCondition> = filter_map
                .into_iter()
                .flat_map(|(field, ops)| {
                    ops.into_iter().filter_map(move |(op_str, value_str)| {
                        let operator = parse_operator(&op_str)?;
                        let value = parse_value(&value_str, operator);
                        Some(FilterCondition {
                            field: field.clone(),
                            operator,
                            value,
                        })
                    })
                })
                .collect();
            Filter::new(conditions)
        });

        Self {
            page,
            filter,
            sort,
            metadata: req.metadata,
            file_metadata: req.file_metadata,
            search: req.search,
        }
    }
}

/// Parse operator string into a `FilterOperator`.
///
/// Mirrors the logic in `query::extractor` — kept here to avoid coupling
/// the protocol module to Axum extractor internals.
fn parse_operator(s: &str) -> Option<crate::query::filter::FilterOperator> {
    use crate::query::filter::FilterOperator;
    match s {
        "eq" => Some(FilterOperator::Eq),
        "ne" => Some(FilterOperator::Ne),
        "gt" => Some(FilterOperator::Gt),
        "gte" => Some(FilterOperator::Gte),
        "lt" => Some(FilterOperator::Lt),
        "lte" => Some(FilterOperator::Lte),
        "contains" => Some(FilterOperator::Contains),
        "starts_with" | "startsWith" => Some(FilterOperator::StartsWith),
        "ends_with" | "endsWith" => Some(FilterOperator::EndsWith),
        "in" => Some(FilterOperator::In),
        "not_in" | "notIn" => Some(FilterOperator::NotIn),
        "is_null" | "isNull" => Some(FilterOperator::IsNull),
        "is_not_null" | "isNotNull" => Some(FilterOperator::IsNotNull),
        _ => None,
    }
}

fn parse_value(s: &str, operator: crate::query::filter::FilterOperator) -> FilterValue {
    use crate::query::filter::FilterOperator;

    if matches!(operator, FilterOperator::IsNull | FilterOperator::IsNotNull) {
        return FilterValue::Null;
    }

    if matches!(operator, FilterOperator::In | FilterOperator::NotIn) {
        return FilterValue::StringList(s.split(',').map(|v| v.trim().to_string()).collect());
    }

    if matches!(
        operator,
        FilterOperator::Contains | FilterOperator::StartsWith | FilterOperator::EndsWith
    ) {
        return FilterValue::String(s.to_string());
    }

    if let Ok(b) = s.parse::<bool>() {
        return FilterValue::Bool(b);
    }
    if let Ok(i) = s.parse::<i64>() {
        return FilterValue::Int(i);
    }
    if let Ok(f) = s.parse::<f64>() {
        return FilterValue::Float(f);
    }
    FilterValue::String(s.to_string())
}
