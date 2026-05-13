//! Integration test builder.

use thiserror::Error;

/// Stub builder - will be reimplemented for claim protocol.
pub struct IntegrationTest;

impl IntegrationTest {
    pub fn new(_scenario: crate::fixtures::TestScenario) -> Self {
        Self
    }
}

/// Errors that can occur when building integration tests.
#[derive(Debug, Error)]
pub enum IntegrationTestError {
    #[error("NATS connection failed: {0}")]
    NatsConnection(String),

    #[error("Stream setup failed: {0}")]
    StreamSetup(String),

    #[error("Adapter setup failed: {0}")]
    AdapterSetup(String),

    #[error("Engine setup failed: {0}")]
    EngineSetup(String),

    #[error("Test timeout")]
    Timeout,

    #[error("Execution error: {0}")]
    Execution(String),
}
