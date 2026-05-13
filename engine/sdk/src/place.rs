//! Type-safe place handles.
//!
//! `PlaceHandle<T>` tracks the token type at compile time, ensuring
//! you can only wire places to ports expecting the same type.

use std::marker::PhantomData;

use crate::Token;

/// Type-safe handle to a place in the Petri net.
///
/// The type parameter `T` tracks what kind of token this place holds.
/// This enables compile-time verification that wiring is type-correct.
///
/// # Example
/// ```ignore
/// let tasks: PlaceHandle<Task> = ctx.state("tasks", "Task Queue");
/// let workers: PlaceHandle<Worker> = ctx.state("workers", "Workers");
///
/// // This would fail to compile:
/// // t.wire_input(&tasks, &worker_port);  // Type mismatch!
/// ```
#[derive(Clone)]
pub struct PlaceHandle<T: Token> {
    pub(crate) id: String,
    pub(crate) _marker: PhantomData<T>,
}

impl<T: Token> PlaceHandle<T> {
    /// Create a new place handle (internal use)
    pub(crate) fn new(id: String) -> Self {
        Self {
            id,
            _marker: PhantomData,
        }
    }

    /// Get the place ID
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Re-type this handle to a different token type.
    ///
    /// Useful when a generic API (e.g. `SpawnChildIO`) returns
    /// `PlaceHandle<DynamicToken>` but the consumer needs a concrete type.
    /// The underlying place ID is preserved; only the compile-time phantom
    /// marker changes.
    pub fn retyped<U: Token>(self) -> PlaceHandle<U> {
        PlaceHandle {
            id: self.id,
            _marker: PhantomData,
        }
    }

    /// Create a handle to an externally-defined place (by ID).
    ///
    /// Used by components to reference places that exist outside their scope
    /// (e.g., the user's job queue).
    pub(crate) fn external(id: String) -> Self {
        Self {
            id,
            _marker: PhantomData,
        }
    }
}

impl<T: Token> std::fmt::Debug for PlaceHandle<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PlaceHandle")
            .field("id", &self.id)
            .field("type", &T::type_name())
            .finish()
    }
}

/// Marker type for step outputs that target existing places.
///
/// Used in `#[step]` functions to indicate "wire to this existing place"
/// instead of "create a new place". This enables cyclic workflows where
/// outputs need to flow back to input queues (e.g., retry loops).
///
/// # Example
///
/// ```ignore
/// // Instead of creating a new place, output to existing job_queue
/// #[step("retry", "Retry Job")]
/// #[guard("job.retries < job.max_retries")]
/// fn retry(job: Job, sig: FailureSignal, queue: Target<Job>) {
///     Job {
///         id: job.id,
///         retries: job.retries + 1,
///         max_retries: job.max_retries,
///     }
/// }
///
/// // Usage: pass the existing handle as the target
/// retry(ctx, &failed_jobs, &sig_fail, &job_queue);
/// ```
pub struct Target<T: Token>(PhantomData<T>);

impl<T: Token> Target<T> {
    /// Create a new target marker (only used by macro)
    #[doc(hidden)]
    pub fn new() -> Self {
        Target(PhantomData)
    }
}

impl<T: Token> Default for Target<T> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::token::UnitToken;

    #[test]
    fn test_place_handle_new() {
        let handle: PlaceHandle<UnitToken> = PlaceHandle::new("test_place".to_string());
        assert_eq!(handle.id(), "test_place");
    }

    #[test]
    fn test_place_handle_clone() {
        let handle: PlaceHandle<UnitToken> = PlaceHandle::new("original".to_string());
        let cloned = handle.clone();
        assert_eq!(handle.id(), cloned.id());
    }

    #[test]
    fn test_place_handle_debug() {
        let handle: PlaceHandle<UnitToken> = PlaceHandle::new("debug_test".to_string());
        let debug_str = format!("{:?}", handle);
        assert!(debug_str.contains("PlaceHandle"));
        assert!(debug_str.contains("debug_test"));
        assert!(debug_str.contains("UnitToken"));
    }

    #[test]
    fn test_target_default() {
        let target: Target<UnitToken> = Target::default();
        // Target is just a marker type - make sure it exists
        let _ = target;
    }

    #[test]
    fn test_target_new() {
        let target: Target<UnitToken> = Target::new();
        // Target is a marker type with no state to check
        let _ = target;
    }
}
