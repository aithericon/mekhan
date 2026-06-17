//! Breakdown + timeseries aggregation queries.
//!
//! SECURITY POSTURE: every `key_expr` is a server-side constant chosen by a
//! Rust `match` on [`Dimension`] — no user input is ever interpolated into the
//! SQL text. User values travel exclusively as binds (the filter DSL via
//! `build_where_conditions`, `search`, and the LIKE-escaped `under` prefix);
//! the only inlined numbers (`depth`, `limit`, the `under` char offset) are
//! clamped/derived integers.

use sqlx::{PgPool, Postgres, QueryBuilder};
use uuid::Uuid;

use crate::inventory::queries::ALLOWED_FILTER_FIELDS;
use crate::query::builder::{self, QueryError};
use crate::query::extractor::QueryParams;

use super::model::{BreakdownBucket, BreakdownResponse, SnapshotPoint};

// ── Clamps ───────────────────────────────────────────────────────────────────

pub const DEFAULT_LIMIT: i64 = 100;
pub const MAX_LIMIT: i64 = 500;
pub const MIN_DEPTH: i64 = 1;
pub const MAX_DEPTH: i64 = 8;

pub fn clamp_limit(limit: Option<i64>) -> i64 {
    limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT)
}

pub fn clamp_depth(depth: Option<i64>) -> i64 {
    depth.unwrap_or(MIN_DEPTH).clamp(MIN_DEPTH, MAX_DEPTH)
}

// ── Dimensions ───────────────────────────────────────────────────────────────

/// The breakdown dimensions. Parsing is the ONLY place a request string maps
/// into SQL-shaping behavior — an unknown value is rejected before any query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dimension {
    Server,
    Extension,
    SizeClass,
    /// Cohorts on `first_seen` (when the platform first observed the copy).
    Age,
    /// Cohorts on `mtime` (the file's own modification time).
    MtimeAge,
    Owner,
    Directory,
}

impl Dimension {
    pub fn parse(s: &str) -> Result<Self, QueryError> {
        match s {
            "server" => Ok(Self::Server),
            "extension" => Ok(Self::Extension),
            "size_class" => Ok(Self::SizeClass),
            "age" => Ok(Self::Age),
            "mtime_age" => Ok(Self::MtimeAge),
            "owner" => Ok(Self::Owner),
            "directory" => Ok(Self::Directory),
            other => Err(QueryError::InvalidValue {
                field: "group_by".to_string(),
                reason: format!(
                    "unknown dimension {other:?} (allowed: server, extension, size_class, \
                     age, mtime_age, owner, directory)"
                ),
            }),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Server => "server",
            Self::Extension => "extension",
            Self::SizeClass => "size_class",
            Self::Age => "age",
            Self::MtimeAge => "mtime_age",
            Self::Owner => "owner",
            Self::Directory => "directory",
        }
    }
}

// ── Size classes (log buckets) ───────────────────────────────────────────────

/// `(label, exclusive upper bound in bytes)` — ascending. Sizes at or above
/// the last bound fall into [`SIZE_CLASS_TOP`]; NULL sizes into
/// [`SIZE_CLASS_UNKNOWN`]. The SQL CASE and [`size_class_label`] are both
/// generated from this one table so they cannot drift.
pub(crate) const SIZE_CLASSES: &[(&str, i64)] = &[
    ("<1 KiB", 1 << 10),
    ("1 KiB-1 MiB", 1 << 20),
    ("1-16 MiB", 16 << 20),
    ("16-256 MiB", 256 << 20),
    ("256 MiB-1 GiB", 1 << 30),
    ("1-4 GiB", 4_i64 << 30),
];
pub(crate) const SIZE_CLASS_TOP: &str = ">=4 GiB";
pub(crate) const SIZE_CLASS_UNKNOWN: &str = "unknown";

/// Rust mirror of the SQL CASE — the unit-testable boundary spec.
pub(crate) fn size_class_label(size_bytes: i64) -> &'static str {
    for (label, bound) in SIZE_CLASSES {
        if size_bytes < *bound {
            return label;
        }
    }
    SIZE_CLASS_TOP
}

fn size_class_case_expr() -> String {
    let mut expr = format!("CASE WHEN size_bytes IS NULL THEN '{SIZE_CLASS_UNKNOWN}'");
    for (label, bound) in SIZE_CLASSES {
        expr.push_str(&format!(" WHEN size_bytes < {bound} THEN '{label}'"));
    }
    expr.push_str(&format!(" ELSE '{SIZE_CLASS_TOP}' END"));
    expr
}

// ── Age cohorts ──────────────────────────────────────────────────────────────

/// `(label, postgres interval)` — ascending recency cutoffs. A timestamp newer
/// than the cutoff lands in that cohort; older than all of them → [`AGE_TOP`];
/// NULL → [`SIZE_CLASS_UNKNOWN`].
pub(crate) const AGE_COHORTS: &[(&str, &str)] = &[
    ("<7d", "7 days"),
    ("7-30d", "30 days"),
    ("30-90d", "90 days"),
    ("90d-1y", "1 year"),
    ("1-2y", "2 years"),
];
pub(crate) const AGE_TOP: &str = ">2y";

/// `col` is a module-chosen column name (`first_seen` / `mtime`), never input.
fn age_case_expr(col: &'static str) -> String {
    let mut expr = format!("CASE WHEN {col} IS NULL THEN '{SIZE_CLASS_UNKNOWN}'");
    for (label, interval) in AGE_COHORTS {
        expr.push_str(&format!(
            " WHEN {col} >= now() - interval '{interval}' THEN '{label}'"
        ));
    }
    expr.push_str(&format!(" ELSE '{AGE_TOP}' END"));
    expr
}

// ── Directory grouping ───────────────────────────────────────────────────────

/// Escape LIKE metacharacters for use with `ESCAPE '\'`.
pub(crate) fn escape_like(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

/// Normalize an `under` prefix: strip slashes at both ends so descent keys
/// compose as `under + '/' + key` regardless of how callers quote them. All
/// directory math runs over `ltrim(path, '/')` so absolute and relative
/// inventory paths group consistently.
pub(crate) fn normalize_under(under: Option<&str>) -> Option<String> {
    under
        .map(|u| u.trim_matches('/').to_string())
        .filter(|u| !u.is_empty())
}

/// The path remainder the directory key is computed over. With an `under`
/// prefix the LIKE scope guarantees `ltrim(path,'/')` starts with
/// `under + '/'`, so the remainder starts at `chars(under) + 2` (1-based,
/// past the separator).
fn directory_rest_expr(under: Option<&str>) -> String {
    match under {
        None => "ltrim(path, '/')".to_string(),
        Some(u) => format!("substring(ltrim(path, '/') from {})", u.chars().count() + 2),
    }
}

fn directory_key_expr(rest: &str, depth: i64) -> String {
    format!("array_to_string((string_to_array({rest}, '/'))[1:{depth}], '/')")
}

// ── Scope (shared WHERE) ─────────────────────────────────────────────────────

/// Append the shared WHERE: the (always-on) workspace tenant scope + the
/// optional filter DSL + free-text search (same fields as the inventory list) +
/// the optional LIKE-escaped `under` directory prefix.
///
/// The `workspace_id` predicate is UNCONDITIONAL — analytics must never
/// aggregate `file_inventory` across tenants (the handler derives it from the
/// authenticated session, never from request input), so every breakdown/totals
/// query is anchored to one workspace before any optional clause ANDs onto it.
fn append_scope(
    qb: &mut QueryBuilder<'_, Postgres>,
    workspace_id: Uuid,
    params: &QueryParams,
    under: Option<&str>,
) -> Result<(), QueryError> {
    qb.push(" WHERE workspace_id = ");
    qb.push_bind(workspace_id);

    if let Some(ref filter) = params.filter {
        if !filter.is_empty() {
            qb.push(" AND ");
            builder::build_where_conditions(qb, filter, ALLOWED_FILTER_FIELDS)?;
        }
    }

    if let Some(ref search) = params.search {
        qb.push(" AND path ILIKE ");
        qb.push_bind(format!("%{search}%"));
    }

    if let Some(under) = under {
        qb.push(" AND ltrim(path, '/') LIKE ");
        qb.push_bind(format!("{}/%", escape_like(under)));
        qb.push(" ESCAPE '\\'");
    }

    Ok(())
}

// ── Breakdown ────────────────────────────────────────────────────────────────

/// Group-by aggregation over `file_inventory`. `under`/`depth` shape the
/// `directory` dimension's lazy descent (and `under` scopes every dimension);
/// `params` carries the filter DSL + search. `depth`/`limit` must already be
/// clamped ([`clamp_depth`] / [`clamp_limit`]).
pub async fn breakdown(
    pool: &PgPool,
    workspace_id: Uuid,
    params: &QueryParams,
    dimension: Dimension,
    under: Option<&str>,
    depth: i64,
    limit: i64,
) -> Result<BreakdownResponse, QueryError> {
    let under = normalize_under(under);
    let under = under.as_deref();

    let key_expr = match dimension {
        Dimension::Server => "file_server_id".to_string(),
        Dimension::Extension => "coalesce(extension, 'none')".to_string(),
        Dimension::SizeClass => size_class_case_expr(),
        Dimension::Age => age_case_expr("first_seen"),
        Dimension::MtimeAge => age_case_expr("mtime"),
        Dimension::Owner => "coalesce(uid::text, 'unknown')".to_string(),
        Dimension::Directory => directory_key_expr(&directory_rest_expr(under), depth),
    };

    // -- buckets --
    let buckets = {
        let mut qb = QueryBuilder::<Postgres>::new(format!(
            "SELECT {key_expr} AS key, \
             count(*)::bigint AS count, \
             coalesce(sum(size_bytes), 0)::bigint AS bytes"
        ));
        if dimension == Dimension::Directory {
            // A bucket is a leaf when NO path inside it descends past `depth`
            // components — the frontend stops offering drill-down there.
            let rest = directory_rest_expr(under);
            qb.push(format!(
                ", bool_and(coalesce(array_length(string_to_array({rest}, '/'), 1), 1) <= {depth}) AS is_leaf"
            ));
        }
        qb.push(" FROM file_inventory");
        append_scope(&mut qb, workspace_id, params, under)?;
        qb.push(format!(
            " GROUP BY 1 ORDER BY bytes DESC, key ASC LIMIT {limit}"
        ));
        qb.build_query_as::<BreakdownBucket>()
            .fetch_all(pool)
            .await?
    };

    // -- totals over the same scope (NOT just the returned buckets) --
    let (total_count, total_bytes) = {
        let mut qb = QueryBuilder::<Postgres>::new(
            "SELECT count(*)::bigint, coalesce(sum(size_bytes), 0)::bigint FROM file_inventory",
        );
        append_scope(&mut qb, workspace_id, params, under)?;
        let row: (i64, i64) = qb.build_query_as().fetch_one(pool).await?;
        row
    };

    Ok(BreakdownResponse {
        group_by: dimension.as_str().to_string(),
        buckets,
        total_count,
        total_bytes,
    })
}

// ── Timeseries ───────────────────────────────────────────────────────────────

/// Deduped growth points over `inventory_snapshots`: `time_bucket` per
/// `(server, dim, key)`, keeping only the LATEST capture inside each bucket
/// (`rn = 1`) — a manual trigger landing next to the hourly job is harmless.
/// `time_bucket` is TimescaleDB (same posture as `inference_timeseries`).
pub async fn timeseries(
    pool: &PgPool,
    workspace_id: Uuid,
    dim: &str,
    key: Option<&str>,
    file_server_id: Option<&str>,
    bucket_secs: i64,
    window_secs: i64,
) -> Result<Vec<SnapshotPoint>, sqlx::Error> {
    sqlx::query_as(
        "SELECT bucket, file_server_id, dim, key, file_count, total_bytes FROM ( \
           SELECT time_bucket(make_interval(secs => $1), snapped_at) AS bucket, \
                  file_server_id, dim, key, file_count, total_bytes, \
                  row_number() OVER ( \
                      PARTITION BY time_bucket(make_interval(secs => $1), snapped_at), \
                                   file_server_id, dim, key \
                      ORDER BY snapped_at DESC \
                  ) AS rn \
           FROM inventory_snapshots \
           WHERE snapped_at >= now() - make_interval(secs => $2) \
             AND workspace_id = $6 \
             AND dim = $3 \
             AND ($4::text IS NULL OR key = $4) \
             AND ($5::text IS NULL OR file_server_id = $5) \
         ) t WHERE rn = 1 \
         ORDER BY bucket ASC, file_server_id ASC, key ASC",
    )
    .bind(bucket_secs as f64)
    .bind(window_secs as f64)
    .bind(dim)
    .bind(key)
    .bind(file_server_id)
    .bind(workspace_id)
    .fetch_all(pool)
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dimension_parses_all_known_and_rejects_unknown() {
        for s in [
            "server",
            "extension",
            "size_class",
            "age",
            "mtime_age",
            "owner",
            "directory",
        ] {
            let d = Dimension::parse(s).expect("known dimension");
            assert_eq!(d.as_str(), s, "round-trip");
        }
        let err = Dimension::parse("path; DROP TABLE file_inventory").unwrap_err();
        assert!(
            matches!(err, QueryError::InvalidValue { ref field, .. } if field == "group_by"),
            "unknown dimension must be rejected before any SQL: {err}"
        );
    }

    #[test]
    fn size_class_boundaries_match_const_table() {
        // Exact boundary values: a size equal to a bound belongs to the NEXT class.
        assert_eq!(size_class_label(0), "<1 KiB");
        assert_eq!(size_class_label(1023), "<1 KiB");
        assert_eq!(size_class_label(1024), "1 KiB-1 MiB");
        assert_eq!(size_class_label((1 << 20) - 1), "1 KiB-1 MiB");
        assert_eq!(size_class_label(1 << 20), "1-16 MiB");
        assert_eq!(size_class_label(16 << 20), "16-256 MiB");
        assert_eq!(size_class_label(256 << 20), "256 MiB-1 GiB");
        assert_eq!(size_class_label(1 << 30), "1-4 GiB");
        assert_eq!(size_class_label((4_i64 << 30) - 1), "1-4 GiB");
        assert_eq!(size_class_label(4_i64 << 30), SIZE_CLASS_TOP);
        // The SQL CASE is generated from the same table — every label + bound
        // must appear in it.
        let case = size_class_case_expr();
        for (label, bound) in SIZE_CLASSES {
            assert!(case.contains(label), "case has label {label}");
            assert!(case.contains(&bound.to_string()), "case has bound {bound}");
        }
        assert!(case.contains(SIZE_CLASS_TOP));
        assert!(case.contains(SIZE_CLASS_UNKNOWN));
    }

    #[test]
    fn under_like_escaping() {
        assert_eq!(escape_like("plain/dir"), "plain/dir");
        assert_eq!(escape_like("a_b"), "a\\_b");
        assert_eq!(escape_like("100%done"), "100\\%done");
        assert_eq!(escape_like("back\\slash"), "back\\\\slash");
        // Escape backslashes FIRST — an injected `\%` must not survive as a
        // bare wildcard escape.
        assert_eq!(escape_like("\\%"), "\\\\\\%");
    }

    #[test]
    fn normalize_under_strips_slashes() {
        assert_eq!(normalize_under(Some("/data/raw/")), Some("data/raw".into()));
        assert_eq!(normalize_under(Some("data")), Some("data".into()));
        assert_eq!(normalize_under(Some("/")), None);
        assert_eq!(normalize_under(Some("")), None);
        assert_eq!(normalize_under(None), None);
    }

    #[test]
    fn depth_and_limit_clamps() {
        assert_eq!(clamp_depth(None), 1);
        assert_eq!(clamp_depth(Some(0)), 1);
        assert_eq!(clamp_depth(Some(-5)), 1);
        assert_eq!(clamp_depth(Some(3)), 3);
        assert_eq!(clamp_depth(Some(99)), MAX_DEPTH);
        assert_eq!(clamp_limit(None), DEFAULT_LIMIT);
        assert_eq!(clamp_limit(Some(0)), 1);
        assert_eq!(clamp_limit(Some(50)), 50);
        assert_eq!(clamp_limit(Some(10_000)), MAX_LIMIT);
    }

    #[test]
    fn key_expr_selection_per_dimension() {
        // Server/owner/extension are plain column exprs.
        assert_eq!(directory_rest_expr(None), "ltrim(path, '/')");
        // `under = "data/raw"` (8 chars) → remainder starts at char 10.
        assert_eq!(
            directory_rest_expr(Some("data/raw")),
            "substring(ltrim(path, '/') from 10)"
        );
        let key = directory_key_expr(&directory_rest_expr(Some("data/raw")), 2);
        assert_eq!(
            key,
            "array_to_string((string_to_array(substring(ltrim(path, '/') from 10), '/'))[1:2], '/')"
        );
        // Age cohorts target the right column per dimension.
        assert!(age_case_expr("first_seen").contains("first_seen >= now() - interval '7 days'"));
        assert!(age_case_expr("mtime").contains("mtime IS NULL"));
        assert!(age_case_expr("mtime").ends_with(&format!("ELSE '{AGE_TOP}' END")));
    }
}
