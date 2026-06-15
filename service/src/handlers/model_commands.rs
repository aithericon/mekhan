//! Operator load/unload action — the HTTP front for the model-pool command
//! publisher ([`crate::runner_commands::publish_model_command`]).
//!
//! `POST /api/v1/runners/{runner_id}/model-commands` lets the Control-Plane UI
//! place/evict a model on a specific runner's local engine. The node agent picks
//! the mechanism per its `[model_agent].backend` (vLLM admin surface, or the
//! Ollama Metal runtime). Fire-and-forget on the runner's CORE `runner.{id}.>`
//! grant; `202 Accepted`. Inference NEVER crosses this channel — control plane only.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::models::error::{ApiError, ErrorResponse};
use crate::runner_commands::{publish_model_command, ModelCommand};
use crate::AppState;

/// `POST /api/v1/runners/{runner_id}/model-commands` — publish a load/unload
/// command to a runner's model agent. Body is the wire [`ModelCommand`]
/// (`{kind, target:{Base|Lora}}`). `202`: accepted (fire-and-forget,
/// desired-state — the agent applies it and re-publishes its catalog).
#[utoipa::path(
    post,
    path = "/api/v1/runners/{runner_id}/model-commands",
    params(("runner_id" = Uuid, Path, description = "target runner id")),
    request_body = ModelCommand,
    responses(
        (status = 202, description = "Command published to the runner's model agent"),
        (status = 500, description = "NATS publish failed", body = ErrorResponse),
    ),
    tag = "models",
)]
pub async fn publish_runner_model_command(
    State(state): State<AppState>,
    _user: AuthUser,
    Path(runner_id): Path<Uuid>,
    Json(cmd): Json<ModelCommand>,
) -> Result<StatusCode, ApiError> {
    publish_model_command(&state.nats, runner_id, &cmd)
        .await
        .map_err(|e| {
            ApiError::internal(format!("publish model command to runner {runner_id}: {e}"))
        })?;
    Ok(StatusCode::ACCEPTED)
}
