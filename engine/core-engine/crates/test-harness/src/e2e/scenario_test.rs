//! Fluent builder for scenario-based tests.

use crate::doubles::{MockEventRepository, MockStateProjection, MockTopologyRepository};
use crate::e2e::MarkingAssertions;
use crate::fixtures::{TestContext, TestScenario};
use petri_application::EvaluateFinalState;

/// Fluent builder for scenario-based e2e tests.
///
/// Combines setup, execution, and assertions into a single fluent API.
///
/// # Example
///
/// ```ignore
/// use petri_test_harness::prelude::*;
///
/// ScenarioTest::new(TestScenario::resource_allocation())
///     .expect_quiescent()
///     .expect_empty("Tasks")
///     .expect_tokens("Completed", 3)
///     .run().await;
/// ```
pub struct ScenarioTest {
    scenario: TestScenario,
    max_steps: usize,
    expected_quiescent: bool,
    expected_limit_reached: bool,
    expected_tokens: Vec<(String, usize)>,
    expected_empty: Vec<String>,
    expected_at_least: Vec<(String, usize)>,
}

impl ScenarioTest {
    /// Create a new scenario test from a TestScenario.
    pub fn new(scenario: TestScenario) -> Self {
        Self {
            scenario,
            max_steps: 100,
            expected_quiescent: false,
            expected_limit_reached: false,
            expected_tokens: vec![],
            expected_empty: vec![],
            expected_at_least: vec![],
        }
    }

    /// Set maximum evaluation steps (default: 100).
    pub fn max_steps(mut self, steps: usize) -> Self {
        self.max_steps = steps;
        self
    }

    /// Expect the scenario to reach quiescent state (no more transitions can fire).
    pub fn expect_quiescent(mut self) -> Self {
        self.expected_quiescent = true;
        self.expected_limit_reached = false;
        self
    }

    /// Expect the scenario to hit the step limit.
    pub fn expect_limit_reached(mut self) -> Self {
        self.expected_limit_reached = true;
        self.expected_quiescent = false;
        self
    }

    /// Expect exact token count in a named place.
    ///
    /// # Panics
    ///
    /// Panics during `run()` if the place name is not found in the scenario.
    pub fn expect_tokens(mut self, place_name: &str, count: usize) -> Self {
        self.expected_tokens.push((place_name.to_string(), count));
        self
    }

    /// Expect a named place to be empty.
    pub fn expect_empty(mut self, place_name: &str) -> Self {
        self.expected_empty.push(place_name.to_string());
        self
    }

    /// Expect at least N tokens in a named place.
    pub fn expect_at_least(mut self, place_name: &str, min: usize) -> Self {
        self.expected_at_least.push((place_name.to_string(), min));
        self
    }

    /// Run the scenario test and verify all assertions.
    ///
    /// # Panics
    ///
    /// Panics if any assertion fails.
    pub async fn run(self) {
        // Build context with mocks
        let ctx: TestContext<MockEventRepository, MockTopologyRepository, MockStateProjection> =
            TestContext::builder()
                .with_scenario(self.scenario.clone())
                .build()
                .await;

        // Evaluate until quiescent or limit
        let result = ctx
            .service
            .evaluate_until_quiescent(self.max_steps)
            .await
            .expect("evaluate_until_quiescent should not fail");

        // Assert final state
        if self.expected_quiescent {
            assert!(
                matches!(result.final_state, EvaluateFinalState::Quiescent),
                "Expected quiescent state, got {:?}. Steps executed: {}, transitions fired: {:?}",
                result.final_state,
                result.steps_executed,
                result.transitions_fired.len()
            );
        }

        if self.expected_limit_reached {
            assert!(
                matches!(result.final_state, EvaluateFinalState::LimitReached),
                "Expected limit reached, got {:?}. Steps executed: {}",
                result.final_state,
                result.steps_executed
            );
        }

        // Get final marking
        let marking = ctx.service.get_marking().await;

        // Assert token counts
        for (place_name, expected_count) in &self.expected_tokens {
            let place_id = self.scenario.places.get(place_name).unwrap_or_else(|| {
                panic!(
                    "Unknown place: '{}'. Available: {:?}",
                    place_name,
                    self.scenario.places.keys().collect::<Vec<_>>()
                )
            });
            marking.assert_token_count(place_id, *expected_count);
        }

        // Assert empty places
        for place_name in &self.expected_empty {
            let place_id = self.scenario.places.get(place_name).unwrap_or_else(|| {
                panic!(
                    "Unknown place: '{}'. Available: {:?}",
                    place_name,
                    self.scenario.places.keys().collect::<Vec<_>>()
                )
            });
            marking.assert_empty(place_id);
        }

        // Assert at least N tokens
        for (place_name, min_count) in &self.expected_at_least {
            let place_id = self.scenario.places.get(place_name).unwrap_or_else(|| {
                panic!(
                    "Unknown place: '{}'. Available: {:?}",
                    place_name,
                    self.scenario.places.keys().collect::<Vec<_>>()
                )
            });
            marking.assert_at_least(place_id, *min_count);
        }
    }

    /// Get the scenario reference (for debugging).
    pub fn scenario(&self) -> &TestScenario {
        &self.scenario
    }
}
