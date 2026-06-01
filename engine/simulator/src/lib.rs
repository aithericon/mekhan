//! In-memory test simulator for SDK-defined Petri net scenarios.
//!
//! Provides a lightweight way to test workflows without running the engine or NATS.
//! Built on top of `petri-test-harness` internals.
//!
//! # Example
//!
//! ```ignore
//! use petri_simulator::Simulator;
//! use aithericon_sdk::prelude::*;
//! use serde_json::json;
//!
//! #[token]
//! struct Task { id: String }
//!
//! #[tokio::test]
//! async fn test_workflow() {
//!     let mut ctx = Context::new("test");
//!     let tasks = ctx.state::<Task>("tasks", "Tasks");
//!     let done = ctx.state::<Task>("done", "Done");
//!     ctx.transition("process", "Process")
//!         .auto_input("t", &tasks)
//!         .auto_output("d", &done)
//!         .logic(r#"#{ d: t }"#);
//!     ctx.seed_one(&tasks, Task { id: "1".into() });
//!
//!     let sim = Simulator::from_sdk(ctx.build()).await;
//!     sim.evaluate().await.unwrap();
//!     assert_eq!(sim.tokens_at("Done").len(), 1);
//! }
//! ```

use std::sync::Arc;

use petri_application::{EvaluateFinalState as AppFinalState, PetriNetService};
use petri_domain::{Marking, PlaceId};
use petri_test_harness::doubles::{
    MockEventRepository, MockStateProjection, MockTopologyRepository,
};
use petri_test_harness::fixtures::{TestContext, TestScenario};

pub use petri_application::EffectHandler;

/// Result of an evaluation run.
pub struct EvaluateResult {
    /// Number of transition firings executed.
    pub steps: usize,
    /// Whether the net reached quiescence or hit the step limit.
    pub final_state: FinalState,
}

/// Terminal state of evaluation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinalState {
    Quiescent,
    LimitReached,
}

/// In-memory simulator for SDK scenarios.
///
/// Wraps a `PetriNetService` with mock stores. Call [`evaluate`](Self::evaluate)
/// to fire all enabled transitions, then inspect the marking with
/// [`tokens_at`](Self::tokens_at).
pub struct Simulator {
    ctx: TestContext<MockEventRepository, MockTopologyRepository, MockStateProjection>,
    scenario: TestScenario,
}

impl Simulator {
    /// Create a simulator from an SDK `ScenarioDefinition`.
    ///
    /// Parses the scenario, loads the topology, and creates initial seed tokens.
    pub async fn from_sdk(definition: aithericon_sdk::ScenarioDefinition) -> Self {
        let scenario = TestScenario::from_sdk(definition);
        let ctx = TestContext::builder()
            .with_scenario(scenario.clone())
            .build()
            .await;
        Self { ctx, scenario }
    }

    /// Fire all enabled transitions until quiescence or `max_steps`.
    pub async fn evaluate(&self) -> Result<EvaluateResult, SimulatorError> {
        self.evaluate_with_limit(1000).await
    }

    /// Fire all enabled transitions with a custom step limit.
    pub async fn evaluate_with_limit(
        &self,
        max_steps: usize,
    ) -> Result<EvaluateResult, SimulatorError> {
        let result = self
            .ctx
            .service
            .evaluate_until_quiescent(max_steps)
            .await
            .map_err(|e| SimulatorError::Evaluation(e.to_string()))?;

        Ok(EvaluateResult {
            steps: result.steps_executed,
            final_state: match result.final_state {
                AppFinalState::Quiescent => FinalState::Quiescent,
                AppFinalState::LimitReached => FinalState::LimitReached,
            },
        })
    }

    /// Get tokens at a place, looked up by **name** (not ID).
    ///
    /// # Panics
    /// Panics if the place name is not found in the scenario.
    pub async fn tokens_at(&self, place_name: &str) -> Vec<serde_json::Value> {
        let place_id = self.resolve_place(place_name);
        let marking = self.ctx.service.get_marking().await;
        marking
            .tokens_at(&place_id)
            .iter()
            .map(|t| petri_application::token_color_to_json(&t.color))
            .collect()
    }

    /// Get the count of tokens at a place by name.
    pub async fn token_count(&self, place_name: &str) -> usize {
        self.tokens_at(place_name).await.len()
    }

    /// Inject a token at a place by **name**.
    ///
    /// # Panics
    /// Panics if the place name is not found.
    pub async fn inject(
        &self,
        place_name: &str,
        data: serde_json::Value,
    ) -> Result<(), SimulatorError> {
        let place_id = self.resolve_place(place_name);
        let color = petri_application::json_to_token_color(&data);
        self.ctx
            .service
            .create_token(place_id, color)
            .await
            .map(|_| ())
            .map_err(|e| SimulatorError::TokenCreation(e.to_string()))
    }

    /// Register a mock effect handler.
    pub fn register_effect(
        &self,
        handler_id: impl Into<String>,
        handler: Arc<dyn EffectHandler>,
    ) -> Result<(), SimulatorError> {
        self.ctx
            .service
            .register_effect_handler(handler_id, handler)
            .map_err(|e| SimulatorError::EffectRegistration(e.to_string()))
    }

    /// Get the full marking (for advanced assertions).
    pub async fn marking(&self) -> Marking {
        self.ctx.service.get_marking().await
    }

    /// Resolve a place name to its PlaceId, panicking with a helpful message if not found.
    fn resolve_place(&self, place_name: &str) -> PlaceId {
        self.scenario
            .places
            .get(place_name)
            .unwrap_or_else(|| {
                panic!(
                    "Unknown place: '{}'. Available: {:?}",
                    place_name,
                    self.scenario.places.keys().collect::<Vec<_>>()
                )
            })
            .clone()
    }

    /// Get a reference to the underlying service for advanced usage.
    pub fn service(
        &self,
    ) -> &Arc<PetriNetService<MockEventRepository, MockTopologyRepository, MockStateProjection>>
    {
        &self.ctx.service
    }

    /// Get place ID by name.
    pub fn place_id(&self, name: &str) -> Option<&PlaceId> {
        self.scenario.places.get(name)
    }
}

/// Errors from simulator operations.
#[derive(Debug, thiserror::Error)]
pub enum SimulatorError {
    #[error("Evaluation failed: {0}")]
    Evaluation(String),

    #[error("Token creation failed: {0}")]
    TokenCreation(String),

    #[error("Effect handler registration failed: {0}")]
    EffectRegistration(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use aithericon_sdk::prelude::*;
    use serde_json::json;

    #[tokio::test]
    async fn test_simple_passthrough() {
        let mut ctx = Context::new("test");
        let a = ctx.state::<UnitToken>("a", "A");
        let b = ctx.state::<UnitToken>("b", "B");
        ctx.transition("pass", "Pass")
            .auto_input("inp", &a)
            .auto_output("out", &b)
            .logic(r#"#{ out: inp }"#);
        ctx.seed(&a, vec![UnitToken]);

        let sim = Simulator::from_sdk(ctx.build()).await;
        let result = sim.evaluate().await.unwrap();

        assert_eq!(result.final_state, FinalState::Quiescent);
        assert!(result.steps >= 1);
        assert_eq!(sim.token_count("A").await, 0);
        assert_eq!(sim.token_count("B").await, 1);
    }

    #[tokio::test]
    async fn test_inject_and_evaluate() {
        let mut ctx = Context::new("test");
        let a = ctx.state::<DynamicToken>("a", "A");
        let b = ctx.state::<DynamicToken>("b", "B");
        ctx.transition("pass", "Pass")
            .auto_input("inp", &a)
            .auto_output("out", &b)
            .logic(r#"#{ out: inp }"#);

        let sim = Simulator::from_sdk(ctx.build()).await;
        sim.inject("A", json!({"id": "test-1"})).await.unwrap();
        sim.evaluate().await.unwrap();

        let tokens = sim.tokens_at("B").await;
        assert_eq!(tokens.len(), 1);
        assert_eq!(tokens[0]["id"], "test-1");
    }

    #[tokio::test]
    async fn test_guard_routing() {
        let mut ctx = Context::new("test");
        let input = ctx.state::<DynamicToken>("input", "Input");
        let high = ctx.state::<DynamicToken>("high", "High");
        let low = ctx.state::<DynamicToken>("low", "Low");

        ctx.transition("route_high", "Route High")
            .auto_input("x", &input)
            .guard(r#"x.value > 10"#)
            .auto_output("out", &high)
            .logic(r#"#{ out: x }"#);

        ctx.transition("route_low", "Route Low")
            .auto_input("x", &input)
            .guard(r#"x.value <= 10"#)
            .auto_output("out", &low)
            .logic(r#"#{ out: x }"#);

        ctx.seed_one(&input, DynamicToken::new(json!({"value": 5})));
        ctx.seed_one(&input, DynamicToken::new(json!({"value": 20})));

        let sim = Simulator::from_sdk(ctx.build()).await;
        sim.evaluate().await.unwrap();

        assert_eq!(sim.token_count("High").await, 1);
        assert_eq!(sim.token_count("Low").await, 1);
        assert_eq!(sim.token_count("Input").await, 0);
    }
}
