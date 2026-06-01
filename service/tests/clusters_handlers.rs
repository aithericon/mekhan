//! Integration tests for the multi-cluster management endpoints.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

async fn body_json(body: Body) -> Value {
    let bytes = body.collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap_or(Value::Null)
}

async fn insert_datacenter(
    db: &PgPool,
    workspace_id: Uuid,
    path: &str,
    display_name: &str,
    flavor: &str,
    deleted: bool,
) -> Uuid {
    let id = Uuid::new_v4();
    let created_by = Uuid::nil();
    sqlx::query(
        "INSERT INTO resources \
            (id, workspace_id, path, resource_type, display_name, latest_version, created_by, deleted_at) \
         VALUES ($1, $2, $3, 'datacenter', $4, 1, $5, \
                 CASE WHEN $6 THEN NOW() ELSE NULL::timestamptz END)",
    )
    .bind(id)
    .bind(workspace_id)
    .bind(path)
    .bind(display_name)
    .bind(created_by)
    .bind(deleted)
    .execute(db)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO resource_versions \
            (resource_id, version, vault_path, public_config, created_by) \
         VALUES ($1, 1, $2, $3, $4)",
    )
    .bind(id)
    .bind(format!("aithericon/resources/{workspace_id}/{id}/v1"))
    .bind(json!({ "scheduler_flavor": flavor }))
    .bind(created_by)
    .execute(db)
    .await
    .unwrap();

    id
}

#[tokio::test]
async fn clusters_list_includes_registered_idle_datacenters_for_current_workspace_only() {
    let (app, db) =
        common::test_app_with_petri_url(&common::nats_url(), "http://127.0.0.1:1").await;
    let current_workspace = Uuid::nil();
    let other_workspace_slug = format!("clusters_{}", Uuid::new_v4().simple());
    let other_workspace =
        common::workspace_fixtures::seed_workspace(&db, &other_workspace_slug).await;

    let slurm_id = insert_datacenter(
        &db,
        current_workspace,
        "slurm_dc",
        "Slurm Dev",
        "slurm",
        false,
    )
    .await;
    insert_datacenter(
        &db,
        current_workspace,
        "deleted_dc",
        "Deleted DC",
        "nomad",
        true,
    )
    .await;
    insert_datacenter(
        &db,
        other_workspace,
        "other_dc",
        "Other Workspace DC",
        "nomad",
        false,
    )
    .await;

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/v1/clusters")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = body_json(resp.into_body()).await;
    let clusters = body["clusters"].as_array().unwrap();
    assert_eq!(clusters.len(), 1, "unexpected clusters response: {body}");

    let cluster = &clusters[0];
    assert_eq!(cluster["resource_id"], slurm_id.to_string());
    assert_eq!(cluster["resource_path"], "slurm_dc");
    assert_eq!(cluster["display_name"], "Slurm Dev");
    assert_eq!(cluster["flavor"], "slurm");
    assert_eq!(cluster["connection_health"], "idle");
    assert_eq!(cluster["watcher_state"], "idle");
    assert_eq!(cluster["active_lease_count"], 0);
}
