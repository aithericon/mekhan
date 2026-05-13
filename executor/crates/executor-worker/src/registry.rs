use std::sync::Arc;
use std::time::Duration;

use aithericon_executor_backend::ExecutionBackend;
use aithericon_executor_domain::ExecutionSpec;

/// Registry of execution backends. Dispatches jobs to the first backend
/// that supports the given spec.
pub struct BackendRegistry {
    backends: Vec<Arc<dyn ExecutionBackend>>,
    default_timeout: Duration,
}

impl BackendRegistry {
    pub fn new(default_timeout: Duration) -> Self {
        Self {
            backends: Vec::new(),
            default_timeout,
        }
    }

    /// Register a backend. Order matters — first match wins.
    pub fn register<B: ExecutionBackend>(mut self, backend: B) -> Self {
        self.backends.push(Arc::new(backend));
        self
    }

    /// Register an already-wrapped `Arc<dyn ExecutionBackend>`.
    pub fn register_arc(mut self, backend: Arc<dyn ExecutionBackend>) -> Self {
        self.backends.push(backend);
        self
    }

    /// Find the first backend that supports the given spec.
    pub fn find(&self, spec: &ExecutionSpec) -> Option<Arc<dyn ExecutionBackend>> {
        self.backends.iter().find(|b| b.supports(spec)).cloned()
    }

    pub fn default_timeout(&self) -> Duration {
        self.default_timeout
    }
}
