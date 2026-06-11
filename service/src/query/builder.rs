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

/// A registered filter/sort field backed by a server-side SQL expression.
///
/// SECURITY POSTURE: `expr` is ALWAYS a `'static` server-side constant
/// declared in a Rust registry (e.g. `catalogue::queries::CATALOGUE_FIELD_SPECS`)
/// and selected by exact name lookup — no user input is ever interpolated into
/// the SQL text. User values travel exclusively as bound parameters. This is
/// the same posture as `analytics/queries.rs` key_exprs.
#[derive(Debug, Clone, Copy)]
pub struct FieldSpec {
    /// The wire name clients use (`filter[<name>][op]=` / `sort=<name>`).
    /// Virtual fields may contain dots (e.g. `meta.num_rows`).
    pub name: &'static str,
    /// The SQL left-hand side emitted verbatim (server-side constant — any
    /// cast lives here, e.g. `((file_metadata->>'num_rows')::bigint)`).
    pub expr: &'static str,
    /// String values bound against this field get a `::timestamptz` cast.
    /// Replaces the `_at`-suffix heuristic of the allowlist path.
    pub timestamp: bool,
    /// DISCOVERY METADATA ONLY: the snake_case fmeta `FileFormat` wire strings
    /// (the values of `file_metadata->>'format'`) this field is meaningful for;
    /// empty = universal. Never read by `build_where_conditions_specs` /
    /// `build_order_by_specs` — the static-`'static`-expr security invariant
    /// above is unchanged. Surfaced verbatim by registry endpoints so field
    /// pickers can scope per-format fields.
    pub applies_to: &'static [&'static str],
}

/// Look up a spec by (normalized) wire name. `camel_to_snake_case` passes
/// dots through unchanged, so dotted virtual names (`meta.numRows` →
/// `meta.num_rows`) normalize like everything else.
fn find_spec<'a>(field: &str, specs: &'a [FieldSpec]) -> Result<&'a FieldSpec, QueryError> {
    let normalized = camel_to_snake_case(field);
    specs
        .iter()
        .find(|s| s.name == normalized)
        .ok_or_else(|| QueryError::InvalidField(field.to_string(), spec_names(specs)))
}

fn spec_names(specs: &[FieldSpec]) -> String {
    specs.iter().map(|s| s.name).collect::<Vec<_>>().join(", ")
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
        // `_at` is the repo-wide timestamp suffix convention; `mtime`
        // (file_inventory's promoted modification time) is the one column that
        // predates it on the wire and can't be renamed.
        let is_timestamp = field.ends_with("_at") || field == "mtime";
        let col = format!("{prefix}{field}");
        push_condition(qb, &col, condition, &field, is_timestamp)?;
    }

    Ok(())
}

/// Spec-based sibling of [`build_where_conditions`]: the SQL left-hand side is
/// `spec.expr` (a server-side constant — see [`FieldSpec`]) and the
/// `::timestamptz` value cast is driven by `spec.timestamp` instead of the
/// `_at`-suffix heuristic. Unknown field → [`QueryError::InvalidField`].
pub fn build_where_conditions_specs(
    qb: &mut QueryBuilder<'_, Postgres>,
    filter: &Filter,
    specs: &[FieldSpec],
) -> Result<(), QueryError> {
    for (i, condition) in filter.conditions.iter().enumerate() {
        if i > 0 {
            qb.push(" AND ");
        }
        let spec = find_spec(&condition.field, specs)?;
        push_condition(qb, spec.expr, condition, spec.name, spec.timestamp)?;
    }
    Ok(())
}

/// Emit one `<col> <op> <bound value>` condition. `col` is ALWAYS a
/// server-side constant (validated column name or [`FieldSpec::expr`]);
/// `field_label` only feeds error messages.
fn push_condition(
    qb: &mut QueryBuilder<'_, Postgres>,
    col: &str,
    condition: &FilterCondition,
    field_label: &str,
    is_timestamp: bool,
) -> Result<(), QueryError> {
    let field = field_label.to_string();
    {
        match condition.operator {
            FilterOperator::Eq => {
                qb.push(format!("{col} = "));
                push_value(qb, &condition.value, &field, is_timestamp)?;
            }
            FilterOperator::Ne => {
                qb.push(format!("{col} != "));
                push_value(qb, &condition.value, &field, is_timestamp)?;
            }
            FilterOperator::Gt => {
                qb.push(format!("{col} > "));
                push_value(qb, &condition.value, &field, is_timestamp)?;
            }
            FilterOperator::Gte => {
                qb.push(format!("{col} >= "));
                push_value(qb, &condition.value, &field, is_timestamp)?;
            }
            FilterOperator::Lt => {
                qb.push(format!("{col} < "));
                push_value(qb, &condition.value, &field, is_timestamp)?;
            }
            FilterOperator::Lte => {
                qb.push(format!("{col} <= "));
                push_value(qb, &condition.value, &field, is_timestamp)?;
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

/// Push a typed value as a bound parameter, with explicit timestamp casting.
/// `field` only feeds error messages; the caller decides `is_timestamp`
/// (suffix heuristic on the allowlist path, [`FieldSpec::timestamp`] on the
/// spec path).
fn push_value(
    qb: &mut QueryBuilder<'_, Postgres>,
    value: &FilterValue,
    field: &str,
    is_timestamp: bool,
) -> Result<(), QueryError> {
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
    qb.push(format!(
        " ORDER BY {prefix}{field} {}",
        sort.sql_direction()
    ));
    Ok(())
}

/// Spec-based sibling of [`build_order_by`]: the sorted expression is
/// `spec.expr` (server-side constant). Unknown field →
/// [`QueryError::InvalidSortField`] listing the spec names.
///
/// Always `NULLS LAST`: virtual `meta.*` projections are sparse (a PNG has no
/// `num_rows`), and Postgres's `DESC` default of `NULLS FIRST` would float
/// every metadata-less entry above the values actually being sorted on.
pub fn build_order_by_specs(
    qb: &mut QueryBuilder<'_, Postgres>,
    sort: &Sort,
    specs: &[FieldSpec],
) -> Result<(), QueryError> {
    let normalized = camel_to_snake_case(&sort.field);
    let spec = specs
        .iter()
        .find(|s| s.name == normalized)
        .ok_or_else(|| QueryError::InvalidSortField(sort.field.clone(), spec_names(specs)))?;
    qb.push(format!(
        " ORDER BY {} {} NULLS LAST",
        spec.expr,
        sort.sql_direction()
    ));
    Ok(())
}

/// Append LIMIT and OFFSET.
pub fn build_pagination(qb: &mut QueryBuilder<'_, Postgres>, page: &PageQuery) {
    qb.push(format!(" LIMIT {} OFFSET {}", page.limit(), page.offset()));
}

/// JSONB containment filter: `column @> $N::jsonb`.
///
/// Use this for user_metadata / file_metadata queries.
pub fn push_jsonb_contains(qb: &mut QueryBuilder<'_, Postgres>, column: &str, json_str: &str) {
    qb.push(format!("{column} @> "));
    qb.push_bind(json_str.to_string());
    qb.push("::jsonb");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sql_for(field: &str, value: FilterValue) -> String {
        let mut qb = QueryBuilder::<Postgres>::new("SELECT 1 WHERE ");
        let filter = Filter::new(vec![FilterCondition {
            field: field.to_string(),
            operator: FilterOperator::Gte,
            value,
        }]);
        build_where_conditions(&mut qb, &filter, &[field]).expect("build where");
        qb.sql().to_string()
    }

    /// `mtime` is a timestamptz column with no `_at` suffix — a string filter
    /// value must still get the timestamp cast or Postgres compares text.
    #[test]
    fn mtime_string_value_gets_timestamptz_cast() {
        let sql = sql_for("mtime", FilterValue::String("2026-01-01T00:00:00Z".into()));
        assert!(
            sql.contains("::timestamptz"),
            "mtime filter must cast: {sql}"
        );
    }

    #[test]
    fn at_suffix_string_value_gets_timestamptz_cast() {
        let sql = sql_for("updated_at", FilterValue::String("2026-01-01".into()));
        assert!(sql.contains("::timestamptz"), "_at filter must cast: {sql}");
    }

    /// A non-timestamp field must NOT be cast (path comparisons stay textual).
    #[test]
    fn non_timestamp_string_value_is_not_cast() {
        let sql = sql_for("path", FilterValue::String("/data".into()));
        assert!(!sql.contains("::timestamptz"), "path must not cast: {sql}");
    }

    // ── FieldSpec path ───────────────────────────────────────────────────────

    const SPECS: &[FieldSpec] = &[
        FieldSpec {
            name: "name",
            expr: "name",
            timestamp: false,
            applies_to: &[],
        },
        FieldSpec {
            name: "meta.num_rows",
            expr: "((file_metadata->>'num_rows')::bigint)",
            timestamp: false,
            applies_to: &[],
        },
        FieldSpec {
            name: "ingested",
            expr: "ingested",
            timestamp: true,
            applies_to: &[],
        },
    ];

    fn spec_sql(field: &str, op: FilterOperator, value: FilterValue) -> Result<String, QueryError> {
        let mut qb = QueryBuilder::<Postgres>::new("SELECT 1 WHERE ");
        let filter = Filter::new(vec![FilterCondition {
            field: field.to_string(),
            operator: op,
            value,
        }]);
        build_where_conditions_specs(&mut qb, &filter, SPECS)?;
        Ok(qb.sql().to_string())
    }

    /// The spec's `expr` (incl. the cast) is emitted verbatim as the SQL LHS;
    /// the value is a bound parameter, never inlined.
    #[test]
    fn spec_expr_emitted_verbatim_value_bound() {
        let sql = spec_sql("meta.num_rows", FilterOperator::Gte, FilterValue::Int(100)).unwrap();
        assert!(
            sql.contains("((file_metadata->>'num_rows')::bigint) >= $1"),
            "expr verbatim + bind: {sql}"
        );
        assert!(
            !sql.contains("100"),
            "value must be bound, not inlined: {sql}"
        );
    }

    /// Dotted virtual names survive normalization (dots pass through
    /// `camel_to_snake_case`); camelCase segments still normalize.
    #[test]
    fn spec_dotted_names_normalize_and_resolve() {
        let sql = spec_sql("meta.numRows", FilterOperator::Eq, FilterValue::Int(5)).unwrap();
        assert!(sql.contains("((file_metadata->>'num_rows')::bigint) = "));
    }

    /// `timestamp: true` drives the cast — no `_at` suffix involved.
    #[test]
    fn spec_timestamp_flag_drives_cast() {
        let sql = spec_sql(
            "ingested",
            FilterOperator::Gte,
            FilterValue::String("2026-01-01".into()),
        )
        .unwrap();
        assert!(
            sql.contains("::timestamptz"),
            "flagged spec must cast: {sql}"
        );
        let sql = spec_sql(
            "name",
            FilterOperator::Eq,
            FilterValue::String("a_at".into()),
        )
        .unwrap();
        assert!(
            !sql.contains("::timestamptz"),
            "unflagged spec must NOT cast even if value looks dated: {sql}"
        );
    }

    /// Unknown field is rejected before any SQL, listing the spec names.
    #[test]
    fn spec_unknown_field_rejected() {
        let err = spec_sql(
            "nope; DROP TABLE catalogue_entries",
            FilterOperator::Eq,
            FilterValue::Int(1),
        )
        .unwrap_err();
        match err {
            QueryError::InvalidField(field, allowed) => {
                assert!(field.starts_with("nope"));
                assert!(allowed.contains("meta.num_rows"));
            }
            other => panic!("expected InvalidField, got {other}"),
        }
    }

    /// ORDER BY uses the spec expr verbatim; unknown sort field is rejected.
    #[test]
    fn spec_order_by() {
        let mut qb = QueryBuilder::<Postgres>::new("SELECT 1");
        build_order_by_specs(&mut qb, &Sort::desc("meta.num_rows"), SPECS).unwrap();
        assert_eq!(
            qb.sql(),
            "SELECT 1 ORDER BY ((file_metadata->>'num_rows')::bigint) DESC NULLS LAST"
        );

        let mut qb = QueryBuilder::<Postgres>::new("SELECT 1");
        let err = build_order_by_specs(&mut qb, &Sort::asc("bogus"), SPECS).unwrap_err();
        assert!(matches!(err, QueryError::InvalidSortField(..)));
    }

    /// `applies_to` is discovery metadata only: two specs differing ONLY in
    /// `applies_to` must compile to byte-identical WHERE and ORDER BY SQL.
    #[test]
    fn applies_to_has_no_sql_effect() {
        const UNIVERSAL: &[FieldSpec] = &[FieldSpec {
            name: "meta.fps",
            expr: "((file_metadata->'format_specific'->'details'->>'fps')::float8)",
            timestamp: false,
            applies_to: &[],
        }];
        const SCOPED: &[FieldSpec] = &[FieldSpec {
            name: "meta.fps",
            expr: "((file_metadata->'format_specific'->'details'->>'fps')::float8)",
            timestamp: false,
            applies_to: &["mp4", "mkv", "avi", "web_m"],
        }];

        let compile = |specs: &[FieldSpec]| {
            let mut qb = QueryBuilder::<Postgres>::new("SELECT 1 WHERE ");
            let filter = Filter::new(vec![FilterCondition {
                field: "meta.fps".to_string(),
                operator: FilterOperator::Gte,
                value: FilterValue::Float(24.0),
            }]);
            build_where_conditions_specs(&mut qb, &filter, specs).expect("build where");
            build_order_by_specs(&mut qb, &Sort::desc("meta.fps"), specs).expect("order by");
            qb.sql().to_string()
        };
        assert_eq!(compile(UNIVERSAL), compile(SCOPED));
    }
}
