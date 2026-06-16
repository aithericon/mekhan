//! Catalogue facet aggregation — group-by buckets over the SAME scope as the
//! list endpoint (filter DSL incl. `meta.*` specs, search, JSONB containment).
//!
//! SECURITY POSTURE: mirrors `analytics/queries.rs` — every `key_expr` is a
//! server-side constant chosen by a Rust `match` on [`CatalogueDimension`]; an
//! unknown `group_by` is rejected before any SQL. User values travel
//! exclusively as binds (through `catalogue::queries::append_where`); the only
//! inlined number (`limit`) is a clamped integer.

use serde::Serialize;
use sqlx::{PgPool, Postgres, QueryBuilder};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::query::builder::QueryError;
use crate::query::extractor::QueryParams;

use super::queries::append_where;

// ── Clamps ───────────────────────────────────────────────────────────────────

pub const DEFAULT_FACET_LIMIT: i64 = 30;
pub const MAX_FACET_LIMIT: i64 = 200;

pub fn clamp_limit(limit: Option<i64>) -> i64 {
    limit
        .unwrap_or(DEFAULT_FACET_LIMIT)
        .clamp(1, MAX_FACET_LIMIT)
}

// ── Dimensions ───────────────────────────────────────────────────────────────

/// The facet dimensions. Parsing is the ONLY place a request string maps into
/// SQL-shaping behavior — an unknown value is rejected before any query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatalogueDimension {
    /// Probed file format (`file_metadata->>'format'`).
    Format,
    /// Catalogue category column.
    Category,
    /// MIME type column.
    MimeType,
    /// Producing net.
    SourceNet,
    /// Producing process step.
    ProcessStep,
    /// Schema-fingerprint digest (`file_metadata->'schema_fingerprint'->>'digest'`,
    /// the `meta.schema` virtual field). Registered data-type names are joined
    /// client-side against `GET /catalogue/data-types`.
    Schema,
    /// Tabular column NAMES (lateral unnest of `column_names`; an entry's
    /// column list has unique names, so count = entries having that column).
    Column,
    /// Classification categories (per-entry DISTINCT over
    /// `columns[].classifications[].category`; count = entries containing it).
    Classification,
}

impl CatalogueDimension {
    pub const ALL: &'static [CatalogueDimension] = &[
        Self::Format,
        Self::Category,
        Self::MimeType,
        Self::SourceNet,
        Self::ProcessStep,
        Self::Schema,
        Self::Column,
        Self::Classification,
    ];

    pub fn parse(s: &str) -> Result<Self, QueryError> {
        match s {
            "format" => Ok(Self::Format),
            "category" => Ok(Self::Category),
            "mime_type" => Ok(Self::MimeType),
            "source_net" => Ok(Self::SourceNet),
            "process_step" => Ok(Self::ProcessStep),
            "schema" => Ok(Self::Schema),
            "column" => Ok(Self::Column),
            "classification" => Ok(Self::Classification),
            other => Err(QueryError::InvalidValue {
                field: "group_by".to_string(),
                reason: format!(
                    "unknown dimension {other:?} (allowed: format, category, mime_type, \
                     source_net, process_step, schema, column, classification)"
                ),
            }),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Format => "format",
            Self::Category => "category",
            Self::MimeType => "mime_type",
            Self::SourceNet => "source_net",
            Self::ProcessStep => "process_step",
            Self::Schema => "schema",
            Self::Column => "column",
            Self::Classification => "classification",
        }
    }

    /// Plain-column dimensions: the constant key expression. The lateral
    /// dimensions (`Column` / `Classification`) build their FROM clause in
    /// [`facets`] instead.
    fn key_expr(self) -> Option<&'static str> {
        match self {
            // `FileFormat::Unknown("fasta")` serializes as the externally
            // tagged object `{"unknown":"fasta"}`; collapse it to its inner
            // name so the facet lists `fasta` like every typed format rather
            // than the raw `{"unknown":…}` envelope (mirrors `format_name` in
            // metadata_view.rs).
            Self::Format => Some(
                "coalesce(\
                 CASE jsonb_typeof(file_metadata->'format') \
                   WHEN 'object' THEN file_metadata->'format'->>'unknown' \
                   ELSE file_metadata->>'format' END, \
                 'unknown')",
            ),
            Self::Category => Some("coalesce(nullif(category, ''), 'uncategorized')"),
            Self::MimeType => Some("coalesce(mime_type, 'unknown')"),
            Self::SourceNet => Some("coalesce(source_net, 'none')"),
            Self::ProcessStep => Some("coalesce(process_step, 'none')"),
            // Rides idx_cat_fmeta_schema; 'none' placeholder per the
            // SourceNet/ProcessStep convention.
            Self::Schema => {
                Some("coalesce(file_metadata->'schema_fingerprint'->>'digest', 'none')")
            }
            Self::Column | Self::Classification => None,
        }
    }
}

/// The lateral FROM-clause tail for the unnesting dimensions. The CASE on
/// `jsonb_typeof` makes a missing/non-array key yield ZERO lateral rows
/// instead of an error, so entries without probe data simply don't bucket.
fn lateral_from(dim: CatalogueDimension) -> &'static str {
    match dim {
        CatalogueDimension::Column => {
            ", LATERAL jsonb_array_elements_text(\
               CASE WHEN jsonb_typeof(file_metadata->'column_names') = 'array' \
                    THEN file_metadata->'column_names' END) AS facet(key)"
        }
        CatalogueDimension::Classification => {
            ", LATERAL (\
               SELECT DISTINCT cls->>'category' AS key \
               FROM jsonb_array_elements(\
                 CASE WHEN jsonb_typeof(file_metadata->'columns') = 'array' \
                      THEN file_metadata->'columns' END) c, \
                    jsonb_array_elements(\
                 CASE WHEN jsonb_typeof(c->'classifications') = 'array' \
                      THEN c->'classifications' END) cls\
               ) AS facet"
        }
        _ => unreachable!("lateral_from only called for unnesting dimensions"),
    }
}

// ── DTOs ─────────────────────────────────────────────────────────────────────

/// One facet bucket: key + entry count + summed entry bytes.
#[derive(Debug, Serialize, sqlx::FromRow, ToSchema)]
pub struct FacetBucket {
    pub key: String,
    pub count: i64,
    pub bytes: i64,
}

/// Facet buckets + totals over the SAME scope (totals cover the whole scope,
/// not just the returned buckets).
#[derive(Debug, Serialize, ToSchema)]
pub struct FacetsResponse {
    pub group_by: String,
    pub buckets: Vec<FacetBucket>,
    pub total_count: i64,
    pub total_bytes: i64,
}

// ── Query ────────────────────────────────────────────────────────────────────

/// Facet aggregation over `catalogue_entries`. `params` carries the full list
/// scope (filter DSL + search + containment); `limit` must already be clamped
/// ([`clamp_limit`]).
pub async fn facets(
    pool: &PgPool,
    workspace_id: Uuid,
    params: &QueryParams,
    dimension: CatalogueDimension,
    limit: i64,
) -> Result<FacetsResponse, QueryError> {
    // -- buckets --
    let buckets = {
        let mut qb = QueryBuilder::<Postgres>::new(match dimension.key_expr() {
            Some(key_expr) => format!(
                "SELECT {key_expr} AS key, \
                 count(*)::bigint AS count, \
                 coalesce(sum(size_bytes), 0)::bigint AS bytes \
                 FROM catalogue_entries"
            ),
            None => format!(
                "SELECT facet.key AS key, \
                 count(*)::bigint AS count, \
                 coalesce(sum(size_bytes), 0)::bigint AS bytes \
                 FROM catalogue_entries{}",
                lateral_from(dimension)
            ),
        });
        let wrote_where = append_where(&mut qb, workspace_id, params)?;
        if dimension == CatalogueDimension::Classification {
            // Cheap pre-filter (the lateral CASE already guards correctness).
            qb.push(if wrote_where { " AND " } else { " WHERE " });
            qb.push("file_metadata ? 'columns'");
        }
        qb.push(format!(
            " GROUP BY 1 ORDER BY count DESC, key ASC LIMIT {limit}"
        ));
        qb.build_query_as::<FacetBucket>().fetch_all(pool).await?
    };

    // -- totals over the same scope (base table — no lateral multiplication) --
    let (total_count, total_bytes) = {
        let mut qb = QueryBuilder::<Postgres>::new(
            "SELECT count(*)::bigint, coalesce(sum(size_bytes), 0)::bigint FROM catalogue_entries",
        );
        append_where(&mut qb, workspace_id, params)?;
        let row: (i64, i64) = qb.build_query_as().fetch_one(pool).await?;
        row
    };

    Ok(FacetsResponse {
        group_by: dimension.as_str().to_string(),
        buckets,
        total_count,
        total_bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dimension_parses_all_known_and_rejects_unknown() {
        for d in CatalogueDimension::ALL {
            assert_eq!(
                CatalogueDimension::parse(d.as_str()).expect("known dimension"),
                *d,
                "round-trip"
            );
        }
        let err = CatalogueDimension::parse("category; DROP TABLE catalogue_entries").unwrap_err();
        assert!(
            matches!(err, QueryError::InvalidValue { ref field, .. } if field == "group_by"),
            "unknown dimension must be rejected before any SQL: {err}"
        );
    }

    #[test]
    fn key_expr_selection_per_dimension() {
        assert_eq!(
            CatalogueDimension::Format.key_expr(),
            Some(
                "coalesce(\
                 CASE jsonb_typeof(file_metadata->'format') \
                   WHEN 'object' THEN file_metadata->'format'->>'unknown' \
                   ELSE file_metadata->>'format' END, \
                 'unknown')"
            )
        );
        assert_eq!(
            CatalogueDimension::Category.key_expr(),
            Some("coalesce(nullif(category, ''), 'uncategorized')")
        );
        assert_eq!(
            CatalogueDimension::MimeType.key_expr(),
            Some("coalesce(mime_type, 'unknown')")
        );
        assert_eq!(
            CatalogueDimension::SourceNet.key_expr(),
            Some("coalesce(source_net, 'none')")
        );
        assert_eq!(
            CatalogueDimension::ProcessStep.key_expr(),
            Some("coalesce(process_step, 'none')")
        );
        assert_eq!(
            CatalogueDimension::Schema.key_expr(),
            Some("coalesce(file_metadata->'schema_fingerprint'->>'digest', 'none')")
        );
        // Unnesting dimensions have no plain key_expr — they go lateral.
        assert_eq!(CatalogueDimension::Column.key_expr(), None);
        assert_eq!(CatalogueDimension::Classification.key_expr(), None);
        assert!(lateral_from(CatalogueDimension::Column).contains("jsonb_array_elements_text"));
        let cls = lateral_from(CatalogueDimension::Classification);
        assert!(cls.contains("SELECT DISTINCT cls->>'category'"));
        assert!(cls.contains("c->'classifications'"));
    }

    #[test]
    fn limit_clamps() {
        assert_eq!(clamp_limit(None), DEFAULT_FACET_LIMIT);
        assert_eq!(clamp_limit(Some(0)), 1);
        assert_eq!(clamp_limit(Some(-3)), 1);
        assert_eq!(clamp_limit(Some(42)), 42);
        assert_eq!(clamp_limit(Some(10_000)), MAX_FACET_LIMIT);
    }
}
