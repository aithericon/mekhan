//! Resource state machine builder for external resource pools.
//!
//! Resources are external entities (workers, jobs, GPUs) that can be in different states.
//! Each resource type defines its own state machine. The scenario defines transitions
//! between states. Adapters react to state changes.
//!
//! # Example
//!
//! ```ignore
//! // Define a worker resource with its states
//! let workers = ctx.resource_def("workers")
//!     .state("available", |s| s.shared())           // External injects here
//!     .state("reserving", |s| s)                    // Optional 2PC state
//!     .state("leased", |s| s)                       // Leased by workflow
//!     .state("draining", |s| s)                     // Graceful shutdown
//!     .on_signal(&sig_worker_event)                // Where to route signals
//!     .build();
//!
//! // Use the states in transitions
//! ctx.transition("claim_worker")
//!     .input(&workers.available)
//!     .output(&workers.leased)
//!     .build();
//! ```

use std::marker::PhantomData;

use crate::context::Context;
use crate::place::PlaceHandle;
use crate::Token;

/// Builder for defining a resource state machine.
pub struct ResourceBuilder<'ctx, T: Token> {
    ctx: &'ctx mut Context,
    resource_type: String,
    states: Vec<ResourceStateBuilder>,
    signal_place: Option<String>,
    _phantom: PhantomData<T>,
}

/// Builder for a single state in the resource state machine.
pub struct ResourceStateBuilder {
    name: String,
    place_type: String,
}

impl ResourceStateBuilder {
    fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            place_type: "state".into(),
        }
    }

    /// Mark this as a signal state (receives external triggers).
    pub fn signal(mut self) -> Self {
        self.place_type = "signal".into();
        self
    }
}

/// A built resource with handles to its state places.
pub struct Resource<T: Token> {
    /// The resource type name
    pub resource_type: String,
    /// Map of state name to place handle
    states: std::collections::HashMap<String, PlaceHandle<T>>,
    /// Signal place for lifecycle events
    pub signal_place: Option<PlaceHandle<T>>,
}

impl<T: Token> Resource<T> {
    /// Get a state's place handle by name.
    pub fn state(&self, name: &str) -> &PlaceHandle<T> {
        self.states.get(name).unwrap_or_else(|| {
            panic!(
                "Unknown state '{}' for resource '{}'",
                name, self.resource_type
            )
        })
    }
}

// Allow accessing states with dot notation via Deref-like pattern
// We'll implement common states as fields through a macro or manual implementation

impl<'ctx, T: Token> ResourceBuilder<'ctx, T> {
    /// Create a new resource builder.
    pub(crate) fn new(ctx: &'ctx mut Context, resource_type: impl Into<String>) -> Self {
        Self {
            ctx,
            resource_type: resource_type.into(),
            states: vec![],
            signal_place: None,
            _phantom: PhantomData,
        }
    }

    /// Add a state to this resource's state machine.
    ///
    /// The closure receives a `ResourceStateBuilder` to configure the state.
    ///
    /// # Example
    /// ```ignore
    /// .state("available", |s| s.shared())
    /// .state("leased", |s| s)
    /// ```
    pub fn state<F>(mut self, name: impl Into<String>, configure: F) -> Self
    where
        F: FnOnce(ResourceStateBuilder) -> ResourceStateBuilder,
    {
        let builder = ResourceStateBuilder::new(name);
        self.states.push(configure(builder));
        self
    }

    /// Set the signal place for lifecycle events (updated, deleted, stale).
    ///
    /// When a lifecycle event occurs on a resource in a claimed state,
    /// the engine will inject a signal into this place for the claiming workflow.
    ///
    /// The signal place can hold any token type (typically a signal type
    /// that carries resource event information).
    pub fn on_signal<S: Token>(mut self, signal_place: &PlaceHandle<S>) -> Self {
        self.signal_place = Some(signal_place.id.clone());
        self
    }

    /// Build the resource and create all state places.
    ///
    /// Returns a `Resource<T>` with handles to all state places.
    pub fn build(self) -> Resource<T> {
        let mut states = std::collections::HashMap::new();

        // Create a place for each state
        for state_builder in &self.states {
            let place_id = format!("{}/{}", self.resource_type, state_builder.name);
            let place_name = format!("{} ({})", self.resource_type, state_builder.name);

            let handle = self.ctx.create_resource_state_place::<T>(
                &place_id,
                &place_name,
                &state_builder.place_type,
            );

            states.insert(state_builder.name.clone(), handle);
        }

        // Get signal place handle if configured
        let signal_place = self.signal_place.map(|id| PlaceHandle::new(id));

        Resource {
            resource_type: self.resource_type,
            states,
            signal_place,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::token;

    #[token]
    struct Worker {
        id: String,
    }

    #[test]
    fn test_resource_builder_creates_states() {
        let mut ctx = Context::new("test");
        let sig = ctx.signal::<Worker>("sig_worker", "Worker Signal");

        let workers = ctx
            .resource_def::<Worker>("workers")
            .state("available", |s| s.signal())
            .state("leased", |s| s)
            .on_signal(&sig)
            .build();

        // Check states were created
        assert_eq!(workers.resource_type, "workers");
        assert!(workers.states.contains_key("available"));
        assert!(workers.states.contains_key("leased"));

        // Check places were created with correct IDs
        assert_eq!(workers.state("available").id, "workers/available");
        assert_eq!(workers.state("leased").id, "workers/leased");

        // Check places exist in context
        assert!(ctx.places.iter().any(|p| p.id == "workers/available"));
        assert!(ctx.places.iter().any(|p| p.id == "workers/leased"));
    }
}
