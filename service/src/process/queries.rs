use sqlx::{PgPool, Postgres, QueryBuilder};

use crate::catalogue::model::CatalogueEntry;
use crate::query::builder::{self, QueryError};
use crate::query::extractor::QueryParams;
use crate::query::pagination::Paginated;

use super::model::*;

/// Allowed filter fields for hpi_processes (whitelist).
const PROCESS_FILTER_FIELDS: &[&str] = &[
    "status",
    "kind",
    "owner",
    "name",
    "process_id",
    "created_at",
    "updated_at",
];

/// Allowed sort fields for hpi_processes (whitelist).
const PROCESS_SORT_FIELDS: &[&str] = &[
    "created_at",
    "updated_at",
    "name",
    "status",
];

/// Allowed filter fields for hpi_tasks (whitelist).
const TASK_FILTER_FIELDS: &[&str] = &[
    "process_id",
    "status",
    "assignee",
    "title",
    "id",
    "created_at",
    "completed_at",
];

/// Allowed sort fields for hpi_tasks (whitelist).
const TASK_SORT_FIELDS: &[&str] = &[
    "created_at",
    "completed_at",
    "title",
    "status",
];

/// Allowed filter fields for hpi_logs (whitelist).
const LOG_FILTER_FIELDS: &[&str] = &[
    "process_id",
    "level",
    "source",
    "timestamp",
];

/// Allowed sort fields for hpi_logs (whitelist).
const LOG_SORT_FIELDS: &[&str] = &[
    "timestamp",
    "level",
    "source",
];

/// List processes with full filter/sort/pagination support.
pub async fn list_processes(
    pool: &PgPool,
    params: &QueryParams,
) -> Result<Paginated<HpiProcess>, QueryError> {
    // -- COUNT query --
    let count = {
        let mut qb = QueryBuilder::<Postgres>::new("SELECT COUNT(*)::bigint FROM hpi_processes");
        append_process_where(&mut qb, params)?;
        let row: (i64,) = qb.build_query_as().fetch_one(pool).await?;
        row.0
    };

    // -- SELECT query --
    let entries = {
        let mut qb = QueryBuilder::<Postgres>::new("SELECT * FROM hpi_processes");
        append_process_where(&mut qb, params)?;

        if let Some(ref sort) = params.sort {
            builder::build_order_by(&mut qb, sort, PROCESS_SORT_FIELDS)?;
        } else {
            qb.push(" ORDER BY created_at DESC");
        }

        builder::build_pagination(&mut qb, &params.page);

        qb.build_query_as::<HpiProcess>()
            .fetch_all(pool)
            .await?
    };

    Ok(Paginated::new(entries, count, &params.page))
}

/// Append a WHERE clause for process queries.
fn append_process_where(
    qb: &mut QueryBuilder<'_, Postgres>,
    params: &QueryParams,
) -> Result<(), QueryError> {
    let has_filter = params
        .filter
        .as_ref()
        .map(|f| !f.is_empty())
        .unwrap_or(false);
    let has_search = params.search.is_some();

    if !has_filter && !has_search {
        return Ok(());
    }

    qb.push(" WHERE ");
    let mut need_and = false;

    if let Some(ref filter) = params.filter {
        if !filter.is_empty() {
            builder::build_where_conditions(qb, filter, PROCESS_FILTER_FIELDS)?;
            need_and = true;
        }
    }

    // Free-text search across name and kind
    if let Some(ref search) = params.search {
        if need_and {
            qb.push(" AND ");
        }
        let pattern = format!("%{search}%");
        qb.push("(name ILIKE ");
        qb.push_bind(pattern.clone());
        qb.push(" OR kind ILIKE ");
        qb.push_bind(pattern.clone());
        qb.push(" OR process_id ILIKE ");
        qb.push_bind(pattern);
        qb.push(")");
    }

    Ok(())
}

/// Get a single process by process_id.
pub async fn get_process(
    pool: &PgPool,
    process_id: &str,
) -> Result<Option<HpiProcess>, sqlx::Error> {
    sqlx::query_as::<_, HpiProcess>(
        "SELECT * FROM hpi_processes WHERE process_id = $1",
    )
    .bind(process_id)
    .fetch_optional(pool)
    .await
}

/// Get full process detail: process + tasks + recent metrics + recent logs + artifact count.
pub async fn get_process_detail(
    pool: &PgPool,
    process_id: &str,
) -> Result<Option<ProcessDetail>, sqlx::Error> {
    let process = match get_process(pool, process_id).await? {
        Some(p) => p,
        None => return Ok(None),
    };

    let tasks = sqlx::query_as::<_, HpiTask>(
        "SELECT * FROM hpi_tasks WHERE process_id = $1 ORDER BY created_at DESC",
    )
    .bind(process_id)
    .fetch_all(pool)
    .await?;

    let recent_metrics = sqlx::query_as::<_, HpiMetric>(
        "SELECT * FROM hpi_metrics WHERE process_id = $1 ORDER BY timestamp DESC LIMIT 100",
    )
    .bind(process_id)
    .fetch_all(pool)
    .await?;

    let recent_logs = sqlx::query_as::<_, HpiLog>(
        "SELECT * FROM hpi_logs WHERE process_id = $1 ORDER BY timestamp DESC LIMIT 50",
    )
    .bind(process_id)
    .fetch_all(pool)
    .await?;

    let artifact_count: (i64,) = sqlx::query_as(
        "SELECT COALESCE(COUNT(*), 0)::bigint FROM catalogue_entries \
         WHERE process_id = $1 \
            OR signal_key IN (\
               SELECT cl.signal_key FROM causality_cross_links cl \
               JOIN causality_event_tokens et ON et.net_id = cl.egress_net \
                 AND et.event_seq = cl.egress_seq \
               JOIN causality_process_tags pt ON pt.token_id = et.token_id \
               WHERE pt.process_id = $1)",
    )
    .bind(process_id)
    .fetch_one(pool)
    .await?;

    Ok(Some(ProcessDetail {
        process,
        tasks,
        recent_metrics,
        recent_logs,
        artifact_count: artifact_count.0,
    }))
}

/// List tasks with full filter/sort/pagination support.
pub async fn list_tasks(
    pool: &PgPool,
    params: &QueryParams,
) -> Result<Paginated<HpiTask>, QueryError> {
    // -- COUNT query --
    let count = {
        let mut qb = QueryBuilder::<Postgres>::new("SELECT COUNT(*)::bigint FROM hpi_tasks");
        append_task_where(&mut qb, params)?;
        let row: (i64,) = qb.build_query_as().fetch_one(pool).await?;
        row.0
    };

    // -- SELECT query --
    let entries = {
        let mut qb = QueryBuilder::<Postgres>::new("SELECT * FROM hpi_tasks");
        append_task_where(&mut qb, params)?;

        if let Some(ref sort) = params.sort {
            builder::build_order_by(&mut qb, sort, TASK_SORT_FIELDS)?;
        } else {
            qb.push(" ORDER BY created_at DESC");
        }

        builder::build_pagination(&mut qb, &params.page);

        qb.build_query_as::<HpiTask>()
            .fetch_all(pool)
            .await?
    };

    Ok(Paginated::new(entries, count, &params.page))
}

/// Append a WHERE clause for task queries.
fn append_task_where(
    qb: &mut QueryBuilder<'_, Postgres>,
    params: &QueryParams,
) -> Result<(), QueryError> {
    let has_filter = params
        .filter
        .as_ref()
        .map(|f| !f.is_empty())
        .unwrap_or(false);
    let has_search = params.search.is_some();

    if !has_filter && !has_search {
        return Ok(());
    }

    qb.push(" WHERE ");
    let mut need_and = false;

    if let Some(ref filter) = params.filter {
        if !filter.is_empty() {
            builder::build_where_conditions(qb, filter, TASK_FILTER_FIELDS)?;
            need_and = true;
        }
    }

    // Free-text search across title
    if let Some(ref search) = params.search {
        if need_and {
            qb.push(" AND ");
        }
        let pattern = format!("%{search}%");
        qb.push("(title ILIKE ");
        qb.push_bind(pattern);
        qb.push(")");
    }

    Ok(())
}

/// Get a single task by id.
pub async fn get_task(
    pool: &PgPool,
    id: &str,
) -> Result<Option<HpiTask>, sqlx::Error> {
    sqlx::query_as::<_, HpiTask>(
        "SELECT * FROM hpi_tasks WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
}

/// Update a task's status and completed_at timestamp.
pub async fn update_task_status(
    pool: &PgPool,
    id: &str,
    status: &str,
    detail: Option<&serde_json::Value>,
) -> Result<Option<HpiTask>, sqlx::Error> {
    let completed_at = if status == "completed" || status == "cancelled" {
        Some(chrono::Utc::now())
    } else {
        None
    };

    // If detail is provided, merge it into the existing detail JSONB
    if let Some(extra) = detail {
        sqlx::query_as::<_, HpiTask>(
            "UPDATE hpi_tasks SET status = $2, completed_at = COALESCE($3, completed_at), \
             detail = detail || $4 \
             WHERE id = $1 RETURNING *",
        )
        .bind(id)
        .bind(status)
        .bind(completed_at)
        .bind(extra)
        .fetch_optional(pool)
        .await
    } else {
        sqlx::query_as::<_, HpiTask>(
            "UPDATE hpi_tasks SET status = $2, completed_at = COALESCE($3, completed_at) \
             WHERE id = $1 RETURNING *",
        )
        .bind(id)
        .bind(status)
        .bind(completed_at)
        .fetch_optional(pool)
        .await
    }
}

/// List metrics for a process, optionally filtered by key, time-ordered.
pub async fn list_metrics(
    pool: &PgPool,
    process_id: &str,
    key_filter: Option<&str>,
    limit: i64,
) -> Result<Vec<HpiMetric>, sqlx::Error> {
    if let Some(key) = key_filter {
        sqlx::query_as::<_, HpiMetric>(
            "SELECT * FROM hpi_metrics WHERE process_id = $1 AND key = $2 \
             ORDER BY timestamp ASC LIMIT $3",
        )
        .bind(process_id)
        .bind(key)
        .bind(limit)
        .fetch_all(pool)
        .await
    } else {
        sqlx::query_as::<_, HpiMetric>(
            "SELECT * FROM hpi_metrics WHERE process_id = $1 \
             ORDER BY timestamp ASC LIMIT $2",
        )
        .bind(process_id)
        .bind(limit)
        .fetch_all(pool)
        .await
    }
}

/// List logs for a process with full filter/sort/pagination support.
pub async fn list_logs(
    pool: &PgPool,
    process_id: &str,
    params: &QueryParams,
) -> Result<Paginated<HpiLog>, QueryError> {
    // -- COUNT query --
    let count = {
        let mut qb = QueryBuilder::<Postgres>::new("SELECT COUNT(*)::bigint FROM hpi_logs WHERE process_id = ");
        qb.push_bind(process_id.to_string());
        append_log_where(&mut qb, params)?;
        let row: (i64,) = qb.build_query_as().fetch_one(pool).await?;
        row.0
    };

    // -- SELECT query --
    let entries = {
        let mut qb = QueryBuilder::<Postgres>::new("SELECT * FROM hpi_logs WHERE process_id = ");
        qb.push_bind(process_id.to_string());
        append_log_where(&mut qb, params)?;

        if let Some(ref sort) = params.sort {
            builder::build_order_by(&mut qb, sort, LOG_SORT_FIELDS)?;
        } else {
            qb.push(" ORDER BY timestamp DESC");
        }

        builder::build_pagination(&mut qb, &params.page);

        qb.build_query_as::<HpiLog>()
            .fetch_all(pool)
            .await?
    };

    Ok(Paginated::new(entries, count, &params.page))
}

/// Append additional WHERE conditions for log queries (process_id is already bound).
fn append_log_where(
    qb: &mut QueryBuilder<'_, Postgres>,
    params: &QueryParams,
) -> Result<(), QueryError> {
    // Additional typed filters
    if let Some(ref filter) = params.filter {
        if !filter.is_empty() {
            qb.push(" AND ");
            builder::build_where_conditions(qb, filter, LOG_FILTER_FIELDS)?;
        }
    }

    // Free-text search on message
    if let Some(ref search) = params.search {
        let pattern = format!("%{search}%");
        qb.push(" AND message ILIKE ");
        qb.push_bind(pattern);
    }

    Ok(())
}

/// Aggregate process stats grouped by status.
pub async fn process_stats(pool: &PgPool) -> Result<ProcessStats, sqlx::Error> {
    let rows: Vec<(String, i64)> = sqlx::query_as(
        "SELECT status, COUNT(*)::bigint FROM hpi_processes GROUP BY status",
    )
    .fetch_all(pool)
    .await?;

    let mut stats = ProcessStats {
        total: 0,
        active: 0,
        completed: 0,
        failed: 0,
    };

    for (status, count) in &rows {
        stats.total += count;
        match status.as_str() {
            "active" | "running" | "started" => stats.active += count,
            "completed" => stats.completed += count,
            "failed" => stats.failed += count,
            _ => {} // other statuses just count toward total
        }
    }

    Ok(stats)
}

/// Partial update of a process.
pub async fn update_process(
    pool: &PgPool,
    process_id: &str,
    update: &ProcessUpdateRequest,
) -> Result<Option<HpiProcess>, sqlx::Error> {
    sqlx::query_as::<_, HpiProcess>(
        "UPDATE hpi_processes SET \
         name = COALESCE($2, name), \
         kind = COALESCE($3, kind), \
         status = COALESCE($4, status), \
         owner = COALESCE($5, owner), \
         updated_at = NOW() \
         WHERE process_id = $1 RETURNING *",
    )
    .bind(process_id)
    .bind(&update.name)
    .bind(&update.kind)
    .bind(&update.status)
    .bind(&update.owner)
    .fetch_optional(pool)
    .await
}

/// List catalogue entries (artifacts) for a process.
pub async fn list_process_artifacts(
    pool: &PgPool,
    process_id: &str,
    params: &QueryParams,
) -> Result<Paginated<CatalogueEntry>, QueryError> {
    // Reuse catalogue's allowed fields for filter/sort
    const ARTIFACT_FILTER_FIELDS: &[&str] = &[
        "id", "name", "category", "filename", "mime_type", "created_at", "catalogued_at",
    ];
    const ARTIFACT_SORT_FIELDS: &[&str] = &[
        "name", "category", "size_bytes", "created_at", "catalogued_at",
    ];

    // -- COUNT query --
    let count = {
        let mut qb = QueryBuilder::<Postgres>::new(
            "SELECT COUNT(*)::bigint FROM catalogue_entries WHERE (process_id = ",
        );
        qb.push_bind(process_id.to_string());
        qb.push(" OR signal_key IN (\
            SELECT cl.signal_key FROM causality_cross_links cl \
            JOIN causality_event_tokens et ON et.net_id = cl.egress_net \
              AND et.event_seq = cl.egress_seq \
            JOIN causality_process_tags pt ON pt.token_id = et.token_id \
            WHERE pt.process_id = ");
        qb.push_bind(process_id.to_string());
        qb.push("))");
        if let Some(ref filter) = params.filter {
            if !filter.is_empty() {
                qb.push(" AND ");
                builder::build_where_conditions(&mut qb, filter, ARTIFACT_FILTER_FIELDS)?;
            }
        }
        let row: (i64,) = qb.build_query_as().fetch_one(pool).await?;
        row.0
    };

    // -- SELECT query --
    let entries = {
        let mut qb = QueryBuilder::<Postgres>::new(
            "SELECT * FROM catalogue_entries WHERE (process_id = ",
        );
        qb.push_bind(process_id.to_string());
        qb.push(" OR signal_key IN (\
            SELECT cl.signal_key FROM causality_cross_links cl \
            JOIN causality_event_tokens et ON et.net_id = cl.egress_net \
              AND et.event_seq = cl.egress_seq \
            JOIN causality_process_tags pt ON pt.token_id = et.token_id \
            WHERE pt.process_id = ");
        qb.push_bind(process_id.to_string());
        qb.push("))");
        if let Some(ref filter) = params.filter {
            if !filter.is_empty() {
                qb.push(" AND ");
                builder::build_where_conditions(&mut qb, filter, ARTIFACT_FILTER_FIELDS)?;
            }
        }

        if let Some(ref sort) = params.sort {
            builder::build_order_by(&mut qb, sort, ARTIFACT_SORT_FIELDS)?;
        } else {
            qb.push(" ORDER BY created_at DESC");
        }

        builder::build_pagination(&mut qb, &params.page);

        qb.build_query_as::<CatalogueEntry>()
            .fetch_all(pool)
            .await?
    };

    Ok(Paginated::new(entries, count, &params.page))
}

/// List tasks for a specific process (by process_id).
pub async fn list_process_tasks(
    pool: &PgPool,
    process_id: &str,
) -> Result<Vec<HpiTask>, sqlx::Error> {
    sqlx::query_as::<_, HpiTask>(
        "SELECT * FROM hpi_tasks WHERE process_id = $1 ORDER BY created_at DESC",
    )
    .bind(process_id)
    .fetch_all(pool)
    .await
}
