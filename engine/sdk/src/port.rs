//! Type-safe port definitions.
//!
//! Ports are the connection points on transitions. They define what types of
//! tokens a transition consumes and produces. Arcs connect places to ports.
//!
//! ## Port Types
//!
//! - **[`InputPort`]** - Receives tokens from places (consumed during firing)
//! - **[`OutputPort`]** - Sends tokens to places (produced during firing)
//!
//! ## Cardinality
//!
//! Ports have a [`Cardinality`] that determines how many tokens they handle:
//!
//! | Cardinality | Behavior |
//! |-------------|----------|
//! | [`Single`](Cardinality::Single) | Consumes/produces one token at a time |
//! | [`Batch`](Cardinality::Batch) | Consumes/produces all available tokens as an array |
//!
//! ## Creating Ports
//!
//! Ports are typically created via [`TransitionBuilder`](crate::TransitionBuilder):
//!
//! ```ignore
//! // Fluent API (recommended)
//! ctx.transition("process", "Process")
//!     .auto_input("task", &tasks)      // Single cardinality
//!     .auto_input_batch("items", &items) // Batch cardinality
//!     .auto_output("result", &results)
//!     .logic(r#"#{ result: task }"#);
//!
//! // Tuple API (for custom cardinality)
//! let (t, task_in) = ctx.transition("process", "Process")
//!     .input::<Task>("task", Cardinality::Single);
//! ```

use std::marker::PhantomData;

use crate::Token;

/// Port cardinality - how many tokens the port handles.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Cardinality {
    /// Single token - script receives the token as a direct object
    #[default]
    Single,
    /// Batch of tokens - script receives tokens as an array
    Batch,
}

impl Cardinality {
    /// Convert to string representation for JSON serialization.
    pub fn as_str(&self) -> &'static str {
        match self {
            Cardinality::Single => "single",
            Cardinality::Batch => "batch",
        }
    }
}

/// Typed input port reference.
///
/// Created via `TransitionBuilder::input()`. The type parameter
/// ensures only places with matching token types can be wired.
pub struct InputPort<T: Token> {
    pub(crate) name: String,
    pub(crate) cardinality: Cardinality,
    pub(crate) _marker: PhantomData<T>,
}

impl<T: Token> InputPort<T> {
    /// Get the port name
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Get the cardinality
    pub fn cardinality(&self) -> Cardinality {
        self.cardinality
    }
}

impl<T: Token> std::fmt::Debug for InputPort<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InputPort")
            .field("name", &self.name)
            .field("cardinality", &self.cardinality)
            .field("type", &T::type_name())
            .finish()
    }
}

/// Typed output port reference.
///
/// Created via `TransitionBuilder::output()`. The type parameter
/// ensures only places with matching token types can be wired.
pub struct OutputPort<T: Token> {
    pub(crate) name: String,
    pub(crate) _marker: PhantomData<T>,
}

impl<T: Token> OutputPort<T> {
    /// Get the port name
    pub fn name(&self) -> &str {
        &self.name
    }
}

impl<T: Token> std::fmt::Debug for OutputPort<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OutputPort")
            .field("name", &self.name)
            .field("type", &T::type_name())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::token::UnitToken;
    use std::marker::PhantomData;

    #[test]
    fn test_cardinality_default() {
        assert_eq!(Cardinality::default(), Cardinality::Single);
    }

    #[test]
    fn test_cardinality_as_str() {
        assert_eq!(Cardinality::Single.as_str(), "single");
        assert_eq!(Cardinality::Batch.as_str(), "batch");
    }

    #[test]
    fn test_cardinality_equality() {
        assert_eq!(Cardinality::Single, Cardinality::Single);
        assert_eq!(Cardinality::Batch, Cardinality::Batch);
        assert_ne!(Cardinality::Single, Cardinality::Batch);
    }

    #[test]
    fn test_cardinality_copy() {
        let c = Cardinality::Batch;
        let c2 = c; // Copy
        assert_eq!(c, c2);
    }

    #[test]
    fn test_input_port_name() {
        let port: InputPort<UnitToken> = InputPort {
            name: "task".into(),
            cardinality: Cardinality::Single,
            _marker: PhantomData,
        };
        assert_eq!(port.name(), "task");
    }

    #[test]
    fn test_input_port_cardinality() {
        let port: InputPort<UnitToken> = InputPort {
            name: "items".into(),
            cardinality: Cardinality::Batch,
            _marker: PhantomData,
        };
        assert_eq!(port.cardinality(), Cardinality::Batch);
    }

    #[test]
    fn test_input_port_debug() {
        let port: InputPort<UnitToken> = InputPort {
            name: "test".into(),
            cardinality: Cardinality::Single,
            _marker: PhantomData,
        };
        let debug_str = format!("{:?}", port);
        assert!(debug_str.contains("InputPort"));
        assert!(debug_str.contains("test"));
        assert!(debug_str.contains("Single"));
    }

    #[test]
    fn test_output_port_name() {
        let port: OutputPort<UnitToken> = OutputPort {
            name: "result".into(),
            _marker: PhantomData,
        };
        assert_eq!(port.name(), "result");
    }

    #[test]
    fn test_output_port_debug() {
        let port: OutputPort<UnitToken> = OutputPort {
            name: "output".into(),
            _marker: PhantomData,
        };
        let debug_str = format!("{:?}", port);
        assert!(debug_str.contains("OutputPort"));
        assert!(debug_str.contains("output"));
    }
}
