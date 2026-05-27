//! Subworkflow cancel effect handler.
//!
//! Terminates a running child net by id. Used by the Timeout node's body
//! cancellation post-pass — when the timer wins, the Timeout emits one
//! `subworkflow_cancel` per SubWorkflow body child.

use std::collections::HashMap;
use std::sync::Arc;

use petri_domain::subworkflow::{
    SubWorkflowCancelRequest, SubWorkflowCancellor,
};

use crate::effect::{EffectError, EffectHandler, EffectInput, EffectOutput};

pub struct SubWorkflowCancelHandler {
    cancellor: Arc<dyn SubWorkflowCancellor>,
    input_port: String,
    output_port: String,
}

impl SubWorkflowCancelHandler {
    pub fn new(
        cancellor: Arc<dyn SubWorkflowCancellor>,
        input_port: impl Into<String>,
        output_port: impl Into<String>,
    ) -> Self {
        Self {
            cancellor,
            input_port: input_port.into(),
            output_port: output_port.into(),
        }
    }
}

#[async_trait::async_trait]
impl EffectHandler for SubWorkflowCancelHandler {
    async fn execute(&self, input: EffectInput) -> Result<EffectOutput, EffectError> {
        let cancel_data = input.inputs.get(&self.input_port).ok_or_else(|| {
            EffectError::Fatal(format!(
                "Missing input port '{}' in subworkflow cancel handler",
                self.input_port
            ))
        })?;

        let child_net_id = cancel_data
            .get("child_net_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                EffectError::Fatal("Missing child_net_id in cancel input".to_string())
            })?
            .to_string();

        let reason = cancel_data
            .get("reason")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let cancelled = self
            .cancellor
            .cancel(SubWorkflowCancelRequest {
                child_net_id: child_net_id.clone(),
                reason,
            })
            .await
            .map_err(|e| EffectError::ExecutionFailed(e.to_string()))?;

        let already_terminal = !cancelled;
        let mut tokens = HashMap::new();
        tokens.insert(
            self.output_port.clone(),
            serde_json::json!({
                "child_net_id": child_net_id,
                "already_terminal": already_terminal,
            }),
        );

        Ok(EffectOutput {
            tokens,
            result: serde_json::json!({
                "child_net_id": child_net_id,
                "already_terminal": already_terminal,
            }),
        })
    }

    fn name(&self) -> &str {
        "subworkflow_cancel"
    }

    fn port_schemas(&self) -> Option<crate::effect::EffectPortSchemas> {
        Some(crate::effect::EffectPortSchemas {
            inputs: HashMap::from([(
                self.input_port.clone(),
                "#/definitions/SubWorkflowCancelInput".into(),
            )]),
            outputs: HashMap::from([(
                self.output_port.clone(),
                "#/definitions/SubWorkflowCancelled".into(),
            )]),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use petri_domain::subworkflow::SubWorkflowCancelError;
    use petri_domain::TransitionId;
    use serde_json::{json, Value as JsonValue};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    struct MockCancellor {
        return_already_terminal: AtomicBool,
        return_error: AtomicBool,
        calls: AtomicUsize,
    }

    impl MockCancellor {
        fn new() -> Self {
            Self {
                return_already_terminal: AtomicBool::new(false),
                return_error: AtomicBool::new(false),
                calls: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait::async_trait]
    impl SubWorkflowCancellor for MockCancellor {
        async fn cancel(
            &self,
            _request: SubWorkflowCancelRequest,
        ) -> Result<bool, SubWorkflowCancelError> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            if self.return_error.load(Ordering::Relaxed) {
                return Err(SubWorkflowCancelError::CancellationFailed(
                    "mock failure".into(),
                ));
            }
            // `cancel` returns true when the net was active + got cancelled,
            // false when already-terminal. Invert for the handler's
            // `already_terminal` output flag.
            Ok(!self.return_already_terminal.load(Ordering::Relaxed))
        }
        fn name(&self) -> &str {
            "mock-cancellor"
        }
    }

    fn make_input(port: &str, data: JsonValue) -> EffectInput {
        let mut inputs = HashMap::new();
        inputs.insert(port.to_string(), data);
        EffectInput {
            transition_id: TransitionId::new(),
            inputs,
            config: None,
            read_inputs: HashMap::new(),
            process_step: None,
        }
    }

    #[tokio::test]
    async fn cancel_handler_terminates_running_net() {
        let cancellor = Arc::new(MockCancellor::new());
        let handler = SubWorkflowCancelHandler::new(cancellor.clone(), "cancel", "cancelled");

        let input = make_input(
            "cancel",
            json!({
                "child_net_id": "child-123",
                "reason": "timeout"
            }),
        );

        let out = handler.execute(input).await.unwrap();
        assert_eq!(cancellor.calls.load(Ordering::Relaxed), 1);
        let cancelled = out.tokens.get("cancelled").unwrap();
        assert_eq!(cancelled["child_net_id"], "child-123");
        assert_eq!(cancelled["already_terminal"], false);
    }

    #[tokio::test]
    async fn cancel_handler_idempotent_on_terminal_net() {
        let cancellor = Arc::new(MockCancellor::new());
        cancellor
            .return_already_terminal
            .store(true, Ordering::Relaxed);
        let handler = SubWorkflowCancelHandler::new(cancellor, "cancel", "cancelled");

        let input = make_input("cancel", json!({"child_net_id": "done-1"}));
        let out = handler.execute(input).await.unwrap();
        assert_eq!(out.tokens["cancelled"]["already_terminal"], true);
    }

    #[tokio::test]
    async fn cancel_handler_missing_child_net_id() {
        let cancellor = Arc::new(MockCancellor::new());
        let handler = SubWorkflowCancelHandler::new(cancellor, "cancel", "cancelled");

        let input = make_input("cancel", json!({"reason": "nope"}));
        let err = handler.execute(input).await.unwrap_err();
        assert!(matches!(err, EffectError::Fatal(_)));
    }

    #[tokio::test]
    async fn cancel_handler_propagates_client_failure() {
        let cancellor = Arc::new(MockCancellor::new());
        cancellor.return_error.store(true, Ordering::Relaxed);
        let handler = SubWorkflowCancelHandler::new(cancellor, "cancel", "cancelled");

        let input = make_input("cancel", json!({"child_net_id": "x"}));
        let err = handler.execute(input).await.unwrap_err();
        assert!(matches!(err, EffectError::ExecutionFailed(_)));
    }

    #[test]
    fn cancel_handler_port_schemas() {
        let cancellor = Arc::new(MockCancellor::new());
        let handler = SubWorkflowCancelHandler::new(cancellor, "cancel", "cancelled");
        let schemas = handler.port_schemas().expect("should declare port schemas");
        assert_eq!(
            schemas.inputs.get("cancel").unwrap(),
            "#/definitions/SubWorkflowCancelInput"
        );
        assert_eq!(
            schemas.outputs.get("cancelled").unwrap(),
            "#/definitions/SubWorkflowCancelled"
        );
    }
}
