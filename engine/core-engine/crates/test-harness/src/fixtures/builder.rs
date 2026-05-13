//! Fluent builder for test contexts.

use std::sync::Arc;

use petri_application::{EventRepository, PetriNetService, StateProjection, TopologyRepository};
use petri_domain::PersistedEvent;

use crate::doubles::{MockEventRepository, MockStateProjection, MockTopologyRepository};
use crate::fixtures::TestScenario;

/// A test context with service and repository references.
///
/// Provides access to both the service for operations and the underlying
/// repositories for assertions.
pub struct TestContext<E, T, S>
where
    E: EventRepository,
    T: TopologyRepository,
    S: StateProjection,
{
    /// The Petri net service.
    pub service: Arc<PetriNetService<E, T, S>>,
    /// Event repository reference for assertions.
    pub event_repo: Arc<E>,
    /// Topology repository reference for assertions.
    pub topology_repo: Arc<T>,
    /// State projection reference.
    pub projection: Arc<S>,
}

/// Builder for creating test contexts with mock implementations.
///
/// # Example
///
/// ```ignore
/// use petri_test_harness::prelude::*;
///
/// let ctx = TestContext::builder()
///     .with_scenario(TestScenario::resource_allocation())
///     .build();
///
/// // Use the service
/// let result = ctx.service.evaluate_until_quiescent(100);
///
/// // Make assertions on the event repo
/// assert!(ctx.event_repo.append_count() > 0);
/// ```
pub struct TestContextBuilder {
    scenario: Option<TestScenario>,
    custom_events: Vec<PersistedEvent>,
}

impl TestContextBuilder {
    /// Create a new builder.
    pub fn new() -> Self {
        Self {
            scenario: None,
            custom_events: Vec::new(),
        }
    }

    /// Use a pre-defined test scenario.
    pub fn with_scenario(mut self, scenario: TestScenario) -> Self {
        self.scenario = Some(scenario);
        self
    }

    /// Pre-populate event store with events.
    pub fn with_events(mut self, events: Vec<PersistedEvent>) -> Self {
        self.custom_events = events;
        self
    }

    /// Build the test context with mock implementations.
    pub async fn build(
        self,
    ) -> TestContext<MockEventRepository, MockTopologyRepository, MockStateProjection> {
        let event_repo = Arc::new(MockEventRepository::with_events(self.custom_events));
        let topology_repo = Arc::new(MockTopologyRepository::new());
        let projection = Arc::new(MockStateProjection::new());

        let service = Arc::new(PetriNetService::new(
            event_repo.clone(),
            topology_repo.clone(),
            projection.clone(),
        ));

        // Initialize with scenario if provided
        if let Some(scenario) = self.scenario {
            service.initialize(scenario.net).await.unwrap();
            for (place_id, token) in scenario.initial_tokens {
                let _ = service.create_token(place_id, token.color).await;
            }
        }

        TestContext {
            service,
            event_repo,
            topology_repo,
            projection,
        }
    }
}

impl Default for TestContextBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl TestContext<MockEventRepository, MockTopologyRepository, MockStateProjection> {
    /// Create a new builder.
    pub fn builder() -> TestContextBuilder {
        TestContextBuilder::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_builder_empty() {
        let ctx = TestContext::builder().build().await;
        assert!(!ctx.topology_repo.has_topology());
        assert_eq!(ctx.event_repo.append_count(), 0);
    }

    #[tokio::test]
    async fn test_builder_with_scenario() {
        let ctx = TestContext::builder()
            .with_scenario(TestScenario::simple_pass_through())
            .build()
            .await;

        assert!(ctx.topology_repo.has_topology());
        // 1 event for net init + 1 event for token creation
        assert!(ctx.event_repo.append_count() >= 2);
    }

    #[tokio::test]
    async fn test_builder_with_resource_allocation() {
        let ctx = TestContext::builder()
            .with_scenario(TestScenario::resource_allocation())
            .build()
            .await;

        assert!(ctx.topology_repo.has_topology());
        // 1 event for net init + 5 events for token creation
        assert_eq!(ctx.event_repo.append_count(), 6);
    }
}
