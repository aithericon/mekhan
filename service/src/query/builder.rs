//! SQL query builder: generates parameterized WHERE and ORDER BY clauses
//! from typed filter/sort structures using sqlx's `QueryBuilder`.

use sqlx::{Postgres, QueryBuilder};

use super::filter::*;
use super::pagination::*;

/// Error from query building (invalid field, bad filter value, etc.)
#[derive(Debug, thiserror::Error)]
pub enum QueryError {
    #[error("invalid filter field: {0} (allowed: {1})")]
    InvalidField(String, String),
    #[error("invalid sort field: {0} (allowed: {1})")]
    InvalidSortField(String, String),
    #[error("invalid filter value for {field}: {reason}")]
    InvalidValue { field: String, reason: String },
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
}

/// Validate that a field name is in the allowed whitelist.
/// Normalizes camelCase to snake_case before checking.
pub fn validate_field(field: &str, allowed: &[&str]) -> Result<String, QueryError> {
    let normalized = camel_to_snake_case(field);
    if allowed.contains(&normalized.as_str()) {
        Ok(normalized)
    } else {
        Err(QueryError::InvalidField(
            field.to_string(),
            allowed.join(", "),
        ))
    }
}

/// Append WHERE conditions to a `QueryBuilder` from a `Filter`.
///
/// All conditions are AND-combined. Field names are validated against the
/// allowed whitelist. Values are always bound as parameters (no interpolation).
pub fn build_where_conditions(
    qb: &mut QueryBuilder<'_, Postgres>,
    filter: &Filter,
    allowed_fields: &[&str],
) -> Result<(), QueryError> {
    build_where_conditions_with_prefix(qb, filter, allowed_fields, None)
}

/// Same as `build_where_conditions` but supports a table prefix (e.g., "c." for JOINs).
pub fn build_where_conditions_with_prefix(
    qb: &mut QueryBuilder<'_, Postgres>,
    filter: &Filter,
    allowed_fields: &[&str],
    table_prefix: Option<&str>,
) -> Result<(), QueryError> {
    let prefix = table_prefix.unwrap_or("");

    for (i, condition) in filter.conditions.iter().enumerate() {
        if i > 0 {
            qb.push(" AND ");
        }

        let field = validate_field(&condition.field, allowed_fields)?;
        let col = format!("{prefix}{field}");

        match condition.operator {
            FilterOperator::Eq => {
                qb.push(format!("{col} = "));
                push_value(qb, &condition.value, &field)?;
            }
            FilterOperator::Ne => {
                qb.push(format!("{col} != "));
                push_value(qb, &condition.value, &field)?;
            }
            FilterOperator::Gt => {
                qb.push(format!("{col} > "));
                push_value(qb, &condition.value, &field)?;
            }
            FilterOperator::Gte => {
                qb.push(format!("{col} >= "));
                push_value(qb, &condition.value, &field)?;
            }
            FilterOperator::Lt => {
                qb.push(format!("{col} < "));
                push_value(qb, &condition.value, &field)?;
            }
            FilterOperator::Lte => {
                qb.push(format!("{col} <= "));
                push_value(qb, &condition.value, &field)?;
            }
            FilterOperator::Contains => {
                qb.push(format!("{col} ILIKE "));
                if let FilterValue::String(s) = &condition.value {
                    qb.push_bind(format!("%{s}%"));
                } else {
                    return Err(QueryError::InvalidValue {
                        field,
                        reason: "contains requires a string value".into(),
                    });
                }
            }
            FilterOperator::StartsWith => {
                qb.push(format!("{col} ILIKE "));
                if let FilterValue::String(s) = &condition.value {
                    qb.push_bind(format!("{s}%"));
                } else {
                    return Err(QueryError::InvalidValue {
                        field,
                        reason: "starts_with requires a string value".into(),
                    });
                }
            }
            FilterOperator::EndsWith => {
                qb.push(format!("{col} ILIKE "));
                if let FilterValue::String(s) = &condition.value {
                    qb.push_bind(format!("%{s}"));
                } else {
                    return Err(QueryError::InvalidValue {
                        field,
                        reason: "ends_with requires a string value".into(),
                    });
                }
            }
            FilterOperator::In => {
                if let FilterValue::StringList(list) = &condition.value {
                    qb.push(format!("{col} = ANY("));
                    qb.push_bind(list.clone());
                    qb.push(")");
                } else if let FilterValue::String(s) = &condition.value {
                    // Comma-separated string → list
                    let list: Vec<String> = s.split(',').map(|v| v.trim().to_string()).collect();
                    qb.push(format!("{col} = ANY("));
                    qb.push_bind(list);
                    qb.push(")");
                } else {
                    return Err(QueryError::InvalidValue {
                        field,
                        reason: "in requires a string list".into(),
                    });
                }
            }
            FilterOperator::NotIn => {
                if let FilterValue::StringList(list) = &condition.value {
                    qb.push(format!("{col} != ALL("));
                    qb.push_bind(list.clone());
                    qb.push(")");
                } else if let FilterValue::String(s) = &condition.value {
                    let list: Vec<String> = s.split(',').map(|v| v.trim().to_string()).collect();
                    qb.push(format!("{col} != ALL("));
                    qb.push_bind(list);
                    qb.push(")");
                } else {
                    return Err(QueryError::InvalidValue {
                        field,
                        reason: "not_in requires a string list".into(),
                    });
                }
            }
            FilterOperator::IsNull => {
                qb.push(format!("{col} IS NULL"));
            }
            FilterOperator::IsNotNull => {
                qb.push(format!("{col} IS NOT NULL"));
            }
        }
    }

    Ok(())
}

/// Push a typed value as a bound parameter, with automatic timestamp casting.
fn push_value(
    qb: &mut QueryBuilder<'_, Postgres>,
    value: &FilterValue,
    field: &str,
) -> Result<(), QueryError> {
    let is_timestamp = field.ends_with("_at");

    match value {
        FilterValue::String(s) => {
            qb.push_bind(s.clone());
            if is_timestamp {
                qb.push("::timestamptz");
            }
        }
        FilterValue::Int(v) => {
            qb.push_bind(*v);
        }
        FilterValue::Float(v) => {
            qb.push_bind(*v);
        }
        FilterValue::Bool(v) => {
            qb.push_bind(*v);
        }
        FilterValue::Null => {
            qb.push("NULL");
        }
        FilterValue::StringList(_) => {
            return Err(QueryError::InvalidValue {
                field: field.to_string(),
                reason: "list values only valid for in/not_in operators".into(),
            });
        }
    }
    Ok(())
}

/// Append a validated ORDER BY clause.
pub fn build_order_by(
    qb: &mut QueryBuilder<'_, Postgres>,
    sort: &Sort,
    allowed_fields: &[&str],
) -> Result<(), QueryError> {
    build_order_by_with_prefix(qb, sort, allowed_fields, None)
}

/// Same as `build_order_by` but supports a table prefix (e.g., "t." for JOINs),
/// so the sorted column is unambiguous when the SELECT joins other tables.
pub fn build_order_by_with_prefix(
    qb: &mut QueryBuilder<'_, Postgres>,
    sort: &Sort,
    allowed_fields: &[&str],
    table_prefix: Option<&str>,
) -> Result<(), QueryError> {
    let field = camel_to_snake_case(&sort.field);
    if !allowed_fields.contains(&field.as_str()) {
        return Err(QueryError::InvalidSortField(
            sort.field.clone(),
            allowed_fields.join(", "),
        ));
    }
    let prefix = table_prefix.unwrap_or("");
    qb.push(format!(" ORDER BY {prefix}{field} {}", sort.sql_direction()));
    Ok(())
}

/// Append LIMIT and OFFSET.
pub fn build_pagination(qb: &mut QueryBuilder<'_, Postgres>, page: &PageQuery) {
    qb.push(format!(" LIMIT {} OFFSET {}", page.limit(), page.offset()));
}

/// JSONB containment filter: `column @> $N::jsonb`.
///
/// Use this for user_metadata / file_metadata queries.
pub fn push_jsonb_contains(
    qb: &mut QueryBuilder<'_, Postgres>,
    column: &str,
    json_str: &str,
) {
    qb.push(format!("{column} @> "));
    qb.push_bind(json_str.to_string());
    qb.push("::jsonb");
}
