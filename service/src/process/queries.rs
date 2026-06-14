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
    "instance_id",
    "net_id",
    "created_at",
    "updated_at",
];

/// Allowed sort fields for hpi_processes (whitelist).
const PROCESS_SORT_FIELDS: &[&str] = &["created_at", "updated_at", "name", "status"];

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
const TASK_SORT_FIELDS: &[&str] = &["created_at", "completed_at", "title", "status"];

/// Allowed filter fields for hpi_logs (whitelist).
const LOG_FILTER_FIELDS: &[&str] = &["process_id", "level", "source", "timestamp"];

/// Allowed sort fields for hpi_logs (whitelist).
const LOG_SORT_FIELDS: &[&str] = &["timestamp", "level", "source"];

/// The nil workspace sentinel — `resolve_net_workspace` (causality ingest) maps
/// pool/infra/legacy nets with no linked instance/template to this value, and
/// the catalogue/inventory `workspace_id` columns default to it. We mirror that
/// here so a process whose net is unlinked is visible exactly to a
/// nil-workspace caller, consistent across the read surface.
const NIL_WS: &str = "'00000000-0000-0000-0000-000000000000'::uuid";

/// FROM + JOINs that resolve a process's owning workspace. A process carries no
/// `workspace_id` column; its tenant is its producing instance's template
/// workspace (`instance_id → workflow_instances → workflow_templates`), the same
/// path `causality::ingest::resolve_net_workspace` uses. LEFT JOINs so an
/// unlinked process (NULL `instance_id`, or an instance not yet linked to a
/// template) survives the join with NULL workspace → resolved to `NIL_WS`.
const PROCESS_JOIN: &str = " FROM hpi_processes p \
     LEFT JOIN workflow_instances wi ON wi.id = p.instance_id \
     LEFT JOIN workflow_templates wt ON wt.id = wi.template_id AND wt.version = wi.template_version";

/// True if `process_id` exists and is visible to a caller in `workspace_id`.
///
/// Scopes through [`PROCESS_JOIN`]: the process is visible when its resolved
/// workspace equals the caller's (unlinked → `NIL_WS`, so nil-workspace callers
/// see infra/pool/legacy processes). STRICT — no public-visibility escape: a
/// process is per-run execution data owned by one workspace, not a shareable
/// definition. A public template is read-only-discoverable cross-workspace, but
/// its *runs* (and their processes/causality) stay private to the workspace that
/// owns them — mirrors `list_instances`. Per-process read handlers call this and
/// 404 on `false`, so a tenant can neither read nor confirm the existence of
/// another tenant's process.
pub async fn process_in_workspace(
    pool: &PgPool,
    process_id: &str,
    workspace_id: uuid::Uuid,
) -> Result<bool, sqlx::Error> {
    let sql = format!(
        "SELECT EXISTS ( \
             SELECT 1{PROCESS_JOIN} \
             WHERE p.process_id = $1 \
               AND COALESCE(wt.workspace_id, {NIL_WS}) = $2 \
         )"
    );
    sqlx::query_scalar::<_, bool>(&sql)
        .bind(process_id)
        .bind(workspace_id)
        .fetch_one(pool)
        .await
}

/// List processes with full filter/sort/pagination support, scoped to one
/// workspace (+ public-template processes).
pub async fn list_processes(
    pool: &PgPool,
    params: &QueryParams,
    workspace_id: uuid::Uuid,
) -> Result<Paginated<HpiProcess>, QueryError> {
    // -- COUNT query --
    let count = {
        let mut qb =
            QueryBuilder::<Postgres>::new(format!("SELECT COUNT(*)::bigint{PROCESS_JOIN}"));
        append_process_where(&mut qb, params, workspace_id)?;
        let row: (i64,) = qb.build_query_as().fetch_one(pool).await?;
        row.0
    };

    // -- SELECT query --
    let entries = {
        let mut qb = QueryBuilder::<Postgres>::new(format!("SELECT p.*{PROCESS_JOIN}"));
        append_process_where(&mut qb, params, workspace_id)?;

        if let Some(ref sort) = params.sort {
            builder::build_order_by_with_prefix(&mut qb, sort, PROCESS_SORT_FIELDS, Some("p."))?;
        } else {
            qb.push(" ORDER BY p.created_at DESC");
        }

        builder::build_pagination(&mut qb, &params.page);

        qb.build_query_as::<HpiProcess>().fetch_all(pool).await?
    };

    Ok(Paginated::new(entries, count, &params.page))
}

/// Append the WHERE clause for process queries: the always-on workspace gate
/// first (so it binds `$1`-then-`$2` via the JOIN), then any filter/search
/// conditions. Filter/sort/search columns are `p.`-prefixed to disambiguate
/// from the joined `workflow_instances`/`workflow_templates` columns.
fn append_process_where(
    qb: &mut QueryBuilder<'_, Postgres>,
    params: &QueryParams,
    workspace_id: uuid::Uuid,
) -> Result<(), QueryError> {
    qb.push(format!(" WHERE COALESCE(wt.workspace_id, {NIL_WS}) = "));
    qb.push_bind(workspace_id);

    if let Some(ref filter) = params.filter {
        if !filter.is_empty() {
            qb.push(" AND ");
            builder::build_where_conditions_with_prefix(
                qb,
                filter,
                PROCESS_FILTER_FIELDS,
                Some("p."),
            )?;
        }
    }

    // Free-text search across name, kind, process_id.
    if let Some(ref search) = params.search {
        let pattern = format!("%{search}%");
        qb.push(" AND (p.name ILIKE ");
        qb.push_bind(pattern.clone());
        qb.push(" OR p.kind ILIKE ");
        qb.push_bind(pattern.clone());
        qb.push(" OR p.process_id ILIKE ");
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
    sqlx::query_as::<_, HpiProcess>("SELECT * FROM hpi_processes WHERE process_id = $1")
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
               WHERE pt.process_id = $1) \
            OR content_hash IN (\
               SELECT cp.content_hash FROM catalogue_producers cp \
               WHERE cp.process_id = $1 \
                  OR cp.source_net = (SELECT net_id FROM hpi_processes WHERE process_id = $1))",
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

/// List tasks with full filter/sort/pagination support, scoped to one
/// workspace. `hpi_tasks` carries its own `workspace_id` (migr 20240157); legacy
/// NULL rows resolve to `NIL_WS` so they remain visible to a nil-workspace
/// caller, consistent with the process read surface.
pub async fn list_tasks(
    pool: &PgPool,
    params: &QueryParams,
    workspace_id: uuid::Uuid,
) -> Result<Paginated<HpiTask>, QueryError> {
    // -- COUNT query --
    let count = {
        let mut qb = QueryBuilder::<Postgres>::new("SELECT COUNT(*)::bigint FROM hpi_tasks");
        append_task_where(&mut qb, params, workspace_id)?;
        let row: (i64,) = qb.build_query_as().fetch_one(pool).await?;
        row.0
    };

    // -- SELECT query --
    let entries = {
        let mut qb = QueryBuilder::<Postgres>::new("SELECT * FROM hpi_tasks");
        append_task_where(&mut qb, params, workspace_id)?;

        if let Some(ref sort) = params.sort {
            builder::build_order_by(&mut qb, sort, TASK_SORT_FIELDS)?;
        } else {
            qb.push(" ORDER BY created_at DESC");
        }

        builder::build_pagination(&mut qb, &params.page);

        qb.build_query_as::<HpiTask>().fetch_all(pool).await?
    };

    Ok(Paginated::new(entries, count, &params.page))
}

/// Append the WHERE clause for task queries: the always-on workspace gate first,
/// then any filter/search conditions.
fn append_task_where(
    qb: &mut QueryBuilder<'_, Postgres>,
    params: &QueryParams,
    workspace_id: uuid::Uuid,
) -> Result<(), QueryError> {
    qb.push(format!(" WHERE COALESCE(workspace_id, {NIL_WS}) = "));
    qb.push_bind(workspace_id);

    if let Some(ref filter) = params.filter {
        if !filter.is_empty() {
            qb.push(" AND ");
            builder::build_where_conditions(qb, filter, TASK_FILTER_FIELDS)?;
        }
    }

    // Free-text search across title
    if let Some(ref search) = params.search {
        let pattern = format!("%{search}%");
        qb.push(" AND (title ILIKE ");
        qb.push_bind(pattern);
        qb.push(")");
    }

    Ok(())
}

/// Get a single task by id.
pub async fn get_task(pool: &PgPool, id: &str) -> Result<Option<HpiTask>, sqlx::Error> {
    sqlx::query_as::<_, HpiTask>("SELECT * FROM hpi_tasks WHERE id = $1")
        .bind(id)
        .fetch_optional(pool)
        .await
}

/// The caller's human-task INBOX (docs/33 P4): the offered tasks they are
/// *eligible* to claim plus the tasks they already hold, scoped to one workspace.
///
/// Eligibility v1 is membership: an `offered` task is shown only when its backing
/// human capacity (`detail->>'capacity_id'`) is one the caller is *enrolled in*
/// (a live `roster_members` row). That is the correct coarse filter — the offer
/// pool's `t_claim` guard would reject a non-member's claim anyway — with finer
/// caps-vs-`requirements` matching deferred (see the handler doc). `claimed` rows
/// are returned when `assignee` is the caller, so a member can find work in
/// flight. A third bucket surfaces UNPOOLED work (docs/33 surface unification):
/// `pending` tasks with no `capacity_id` and no `assignee` are "open to anyone" in
/// the workspace — claimable (a soft assign) by any member, no roster enrollment.
/// All three are workspace-scoped; ordered newest-first.
///
/// `member` is the caller's `subject_as_uuid()`; `assignee` is stored as that
/// id's string form (the offer net relays the member id verbatim as `runner_id`).
pub async fn inbox_tasks(
    pool: &PgPool,
    workspace_id: uuid::Uuid,
    member: uuid::Uuid,
) -> Result<Vec<HpiTask>, sqlx::Error> {
    sqlx::query_as::<_, HpiTask>(
        "SELECT * FROM hpi_tasks t \
         WHERE t.workspace_id = $1 AND ( \
             (t.status = 'offered' AND (t.detail->>'capacity_id')::uuid IN ( \
                 SELECT capacity_id FROM roster_members \
                 WHERE workspace_id = $1 AND member_user_id = $2 AND revoked_at IS NULL \
             )) \
             OR (t.status = 'claimed' AND t.assignee = $3) \
             OR (t.status = 'pending' AND t.assignee IS NULL \
                 AND (t.detail->>'capacity_id') IS NULL) \
         ) \
         ORDER BY t.created_at DESC",
    )
    .bind(workspace_id)
    .bind(member)
    .bind(member.to_string())
    .fetch_all(pool)
    .await
}

/// Soft-claim an UNPOOLED (`pending`, no capacity) task: set `assignee` + flip to
/// `claimed` so it leaves the "open to anyone" inbox bucket and lands in the
/// claimer's "in progress" bucket. Purely control-plane (no engine handshake — an
/// unpooled task has no offer pool); advisory only, since `/complete` still allows
/// `claimed` and the engine net just awaits the `human.completed` signal regardless
/// of who submits it. Guarded on `status='pending'` so a racing second claimer (or
/// a pooled task) is a no-op → `None`.
pub async fn soft_claim_task(
    pool: &PgPool,
    id: &str,
    member: &str,
) -> Result<Option<HpiTask>, sqlx::Error> {
    sqlx::query_as::<_, HpiTask>(
        "UPDATE hpi_tasks \
         SET status = 'claimed', assignee = $2, claimed_at = COALESCE(claimed_at, now()) \
         WHERE id = $1 AND status = 'pending' RETURNING *",
    )
    .bind(id)
    .bind(member)
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

/// Summarize metrics for a process: count, min, max, avg, last value per key.
pub async fn summarize_metrics(
    pool: &PgPool,
    process_id: &str,
) -> Result<Vec<HpiMetricSummary>, sqlx::Error> {
    sqlx::query_as::<_, HpiMetricSummary>(
        "SELECT key, \
               COUNT(*)::bigint as count, \
               MIN(value) as min_value, \
               MAX(value) as max_value, \
               AVG(value) as avg_value, \
               (SELECT m2.value FROM hpi_metrics m2 \
                WHERE m2.process_id = $1 AND m2.key = m.key \
                ORDER BY m2.timestamp DESC LIMIT 1) as last_value, \
               MAX(timestamp) as last_timestamp \
         FROM hpi_metrics m \
         WHERE process_id = $1 \
         GROUP BY key \
         ORDER BY key",
    )
    .bind(process_id)
    .fetch_all(pool)
    .await
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
        let mut qb = QueryBuilder::<Postgres>::new(
            "SELECT COUNT(*)::bigint FROM hpi_logs WHERE process_id = ",
        );
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

        qb.build_query_as::<HpiLog>().fetch_all(pool).await?
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

/// Aggregate process stats grouped by status, scoped strictly to one workspace
/// via [`PROCESS_JOIN`] (no public-visibility escape — runs are private).
pub async fn process_stats(
    pool: &PgPool,
    workspace_id: uuid::Uuid,
) -> Result<ProcessStats, sqlx::Error> {
    let sql = format!(
        "SELECT p.status, COUNT(*)::bigint{PROCESS_JOIN} \
         WHERE COALESCE(wt.workspace_id, {NIL_WS}) = $1 \
         GROUP BY p.status"
    );
    let rows: Vec<(String, i64)> = sqlx::query_as(&sql)
        .bind(workspace_id)
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
        "id",
        "name",
        "category",
        "filename",
        "mime_type",
        "created_at",
        "catalogued_at",
    ];
    const ARTIFACT_SORT_FIELDS: &[&str] = &[
        "name",
        "category",
        "size_bytes",
        "created_at",
        "catalogued_at",
    ];

    // -- COUNT query --
    let count = {
        let mut qb = QueryBuilder::<Postgres>::new(
            "SELECT COUNT(*)::bigint FROM catalogue_entries WHERE (process_id = ",
        );
        qb.push_bind(process_id.to_string());
        qb.push(
            " OR signal_key IN (\
            SELECT cl.signal_key FROM causality_cross_links cl \
            JOIN causality_event_tokens et ON et.net_id = cl.egress_net \
              AND et.event_seq = cl.egress_seq \
            JOIN causality_process_tags pt ON pt.token_id = et.token_id \
            WHERE pt.process_id = ",
        );
        qb.push_bind(process_id.to_string());
        // ...OR the content was produced by this process / its net (producer edge):
        // recovers re-runs whose catalogue row deduped on content_hash.
        qb.push(") OR content_hash IN (\
            SELECT cp.content_hash FROM catalogue_producers cp WHERE cp.process_id = ");
        qb.push_bind(process_id.to_string());
        qb.push(" OR cp.source_net = (SELECT net_id FROM hpi_processes WHERE process_id = ");
        qb.push_bind(process_id.to_string());
        qb.push(")))");
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
        let mut qb =
            QueryBuilder::<Postgres>::new("SELECT * FROM catalogue_entries WHERE (process_id = ");
        qb.push_bind(process_id.to_string());
        qb.push(
            " OR signal_key IN (\
            SELECT cl.signal_key FROM causality_cross_links cl \
            JOIN causality_event_tokens et ON et.net_id = cl.egress_net \
              AND et.event_seq = cl.egress_seq \
            JOIN causality_process_tags pt ON pt.token_id = et.token_id \
            WHERE pt.process_id = ",
        );
        qb.push_bind(process_id.to_string());
        // ...OR the content was produced by this process / its net (producer edge):
        // recovers re-runs whose catalogue row deduped on content_hash.
        qb.push(") OR content_hash IN (\
            SELECT cp.content_hash FROM catalogue_producers cp WHERE cp.process_id = ");
        qb.push_bind(process_id.to_string());
        qb.push(" OR cp.source_net = (SELECT net_id FROM hpi_processes WHERE process_id = ");
        qb.push_bind(process_id.to_string());
        qb.push(")))");
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
            .into_iter()
            .map(CatalogueEntry::hydrate_view)
            .collect()
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
