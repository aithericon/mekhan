//! Axum extractor that parses query parameters in bracket notation:
//!
//! ```text
//! ?page=0&page_size=20&filter[category][eq]=model&filter[name][contains]=gp&sort=-created_at
//! ```
//!
//! Uses `serde_qs` for bracket-notation deserialization.

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use std::collections::HashMap;

use super::filter::*;
use super::pagination::*;

/// Parsed query parameters: pagination + filter + sort + optional JSONB search.
#[derive(Debug, Clone)]
pub struct QueryParams {
    pub page: PageQuery,
    pub filter: Option<Filter>,
    pub sort: Option<Sort>,
    /// Raw JSONB containment query for user_metadata.
    pub metadata: Option<String>,
    /// Raw JSONB containment query for file_metadata.
    pub file_metadata: Option<String>,
    /// Free-text search (applied by the handler to whichever fields make sense).
    pub search: Option<String>,
}

/// Raw deserialization target (serde_qs handles bracket notation).
#[derive(Debug, Deserialize, Default)]
struct RawQueryParams {
    #[serde(default)]
    page: Option<i64>,
    #[serde(default)]
    page_size: Option<i64>,
    #[serde(default)]
    sort: Option<String>,
    #[serde(default)]
    search: Option<String>,
    #[serde(default)]
    metadata: Option<String>,
    #[serde(default)]
    file_metadata: Option<String>,
    /// Bracket-notation filters: `filter[field][operator] = value`
    #[serde(default)]
    filter: Option<HashMap<String, HashMap<String, String>>>,
}

impl QueryParams {
    /// Parse from a raw query string.
    pub fn from_query_str(query: &str) -> Result<Self, String> {
        let raw: RawQueryParams =
            serde_qs::from_str(query).map_err(|e| format!("invalid query params: {e}"))?;
        Ok(Self::from_raw(raw))
    }

    fn from_raw(raw: RawQueryParams) -> Self {
        let page = PageQuery {
            page: raw.page.unwrap_or(0),
            page_size: raw.page_size.unwrap_or(20),
        };

        let sort = raw.sort.as_deref().map(Sort::parse);

        let filter = raw.filter.map(|filter_map| {
            let conditions = filter_map
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
            metadata: raw.metadata,
            file_metadata: raw.file_metadata,
            search: raw.search,
        }
    }
}

fn parse_operator(s: &str) -> Option<FilterOperator> {
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
        _ => {
            tracing::warn!("unknown filter operator: {s}");
            None
        }
    }
}

fn parse_value(s: &str, operator: FilterOperator) -> FilterValue {
    // Null operators don't need a value
    if matches!(operator, FilterOperator::IsNull | FilterOperator::IsNotNull) {
        return FilterValue::Null;
    }

    // IN/NOT_IN: comma-separated list
    if matches!(operator, FilterOperator::In | FilterOperator::NotIn) {
        return FilterValue::StringList(s.split(',').map(|v| v.trim().to_string()).collect());
    }

    // String operators always stay as strings
    if matches!(
        operator,
        FilterOperator::Contains | FilterOperator::StartsWith | FilterOperator::EndsWith
    ) {
        return FilterValue::String(s.to_string());
    }

    // Try type coercion: bool → i64 → f64 → string
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

/// Axum extractor implementation: reads query string and parses via serde_qs.
impl<S: Send + Sync> FromRequestParts<S> for QueryParams {
    type Rejection = QueryParamsRejection;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let query = parts.uri.query().unwrap_or("");
        let decoded =
            urlencoding::decode(query).map_err(|e| QueryParamsRejection(format!("URL decode error: {e}")))?;
        QueryParams::from_query_str(&decoded).map_err(QueryParamsRejection)
    }
}

/// Rejection type for query param parsing failures.
#[derive(Debug)]
pub struct QueryParamsRejection(pub String);

impl IntoResponse for QueryParamsRejection {
    fn into_response(self) -> Response {
        crate::models::error::ApiError::bad_request(self.0).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_filters() {
        let params =
            QueryParams::from_query_str("filter[category][eq]=model&filter[name][contains]=gp")
                .unwrap();
        let filter = params.filter.unwrap();
        assert_eq!(filter.conditions.len(), 2);
    }

    #[test]
    fn parse_sort_desc() {
        let params = QueryParams::from_query_str("sort=-created_at").unwrap();
        let sort = params.sort.unwrap();
        assert_eq!(sort.field, "created_at");
        assert_eq!(sort.direction, SortDirection::Desc);
    }

    #[test]
    fn parse_pagination() {
        let params = QueryParams::from_query_str("page=2&page_size=10").unwrap();
        assert_eq!(params.page.page, 2);
        assert_eq!(params.page.page_size, 10);
        assert_eq!(params.page.offset(), 20);
    }

    #[test]
    fn parse_in_operator() {
        let params =
            QueryParams::from_query_str("filter[category][in]=model,dataset,plot").unwrap();
        let filter = params.filter.unwrap();
        assert_eq!(filter.conditions.len(), 1);
        assert_eq!(filter.conditions[0].operator, FilterOperator::In);
        if let FilterValue::StringList(list) = &filter.conditions[0].value {
            assert_eq!(list, &["model", "dataset", "plot"]);
        } else {
            panic!("expected StringList");
        }
    }

    #[test]
    fn parse_metadata_and_search() {
        let params = QueryParams::from_query_str(
            r#"search=gp_model&metadata={"kernel":"rbf"}"#,
        )
        .unwrap();
        assert_eq!(params.search.as_deref(), Some("gp_model"));
        assert_eq!(params.metadata.as_deref(), Some(r#"{"kernel":"rbf"}"#));
    }
}
