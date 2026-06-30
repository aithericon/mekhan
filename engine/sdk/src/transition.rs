//! Transition builder with type-safe port wiring.
//!
//! # Example
//! ```ignore
//! let (t, task_in) = ctx.transition("allocate", "Allocate Task")
//!     .input::<Task>("task", Cardinality::Single);
//! let (t, worker_in) = t.input::<Worker>("worker", Cardinality::Single);
//! let (t, assign_out) = t.output::<Assignment>("assignment");
//!
//! t.wire_input(&tasks, &task_in)
//!  .wire_input(&workers, &worker_in)
//!  .wire_output(&assign_out, &in_progress)
//!  .logic(r#"#{ assignment: #{ task_id: task.id, worker_id: worker.id } }"#)
//!  .done();
//! ```

use std::marker::PhantomData;

use petri_domain::effects::{self, EffectDescriptor};

use crate::context::Context;
use crate::contracts::{
    ExecutorCancel, ExecutorSubmit, HumanTaskCancel, HumanTaskSubmit, ProcessComplete,
    ProcessStart, SchedulerCancel, SchedulerSubmit, TimerCancel, TimerSchedule,
};
use crate::place::PlaceHandle;
use crate::port::{Cardinality, InputPort, OutputPort};
use crate::scenario::{
    ScenarioArc, ScenarioPort, ScenarioTransition, TransitionGuard, TransitionLogic,
    TransitionPriority,
};
use crate::validation::validate_script_inline;
use crate::Token;

/// Builder for defining transitions with type-safe ports.
pub struct TransitionBuilder<'ctx> {
    ctx: &'ctx mut Context,
    id: String,
    name: String,
    input_ports: Vec<ScenarioPort>,
    output_ports: Vec<ScenarioPort>,
    inputs: Vec<ScenarioArc>,
    outputs: Vec<ScenarioArc>,
    guard: Option<TransitionGuard>,
    priority: Option<TransitionPriority>,
    /// Finalizer flag — fires only during the engine's post-failure drain.
    finalizer: bool,
    logic: Option<TransitionLogic>,
    /// Collected input type names for schema composition
    input_types: Vec<String>,
    /// Collected output type names for schema composition
    output_types: Vec<String>,
    /// Collected caused signal names
    caused_signals: Vec<String>,
    /// Process step key: publish "step_started" after this transition fires
    process_step_started: Option<String>,
    /// Process step key: publish "step_completed" after this transition fires
    process_step_completed: Option<String>,
    /// Per-transition Rhai constants (only prepended to THIS transition's script).
    local_rhai_constants: Vec<(String, String)>,
    /// Per-transition Rhai variables (only prepended to THIS transition's script).
    local_rhai_variables: Vec<(String, String)>,
}

impl<'ctx> TransitionBuilder<'ctx> {
    /// Create a new transition builder (internal use).
    pub(crate) fn new(ctx: &'ctx mut Context, id: String, name: String) -> Self {
        Self {
            ctx,
            id,
            name,
            input_ports: vec![],
            output_ports: vec![],
            inputs: vec![],
            outputs: vec![],
            guard: None,
            priority: None,
            finalizer: false,
            logic: None,
            input_types: vec![],
            output_types: vec![],
            caused_signals: vec![],
            process_step_started: None,
            process_step_completed: None,
            local_rhai_constants: vec![],
            local_rhai_variables: vec![],
        }
    }

    /// Define an input port with type tracking.
    ///
    /// Returns `(self, InputPort<T>)` for chaining.
    pub fn input<T: Token>(
        mut self,
        name: impl Into<String>,
        cardinality: Cardinality,
    ) -> (Self, InputPort<T>) {
        self.ctx.register_schema::<T>();
        let name = name.into();
        let schema_ref = T::schema_ref();
        self.input_types.push(T::type_name().to_string());
        self.input_ports.push(ScenarioPort {
            name: name.clone(),
            cardinality: match cardinality {
                Cardinality::Single => "single".into(),
                Cardinality::Batch => "batch".into(),
            },
            schema_ref: Some(schema_ref),
        });
        let port = InputPort {
            name,
            cardinality,
            _marker: PhantomData,
        };
        (self, port)
    }

    /// Define an output port with type tracking.
    ///
    /// Returns `(self, OutputPort<T>)` for chaining.
    pub fn output<T: Token>(mut self, name: impl Into<String>) -> (Self, OutputPort<T>) {
        self.ctx.register_schema::<T>();
        let name = name.into();
        let schema_ref = T::schema_ref();
        self.output_types.push(T::type_name().to_string());
        self.output_ports.push(ScenarioPort {
            name: name.clone(),
            cardinality: "single".into(),
            schema_ref: Some(schema_ref),
        });
        let port = OutputPort {
            name,
            _marker: PhantomData,
        };
        (self, port)
    }

    /// Wire an input arc (place → port).
    ///
    /// **Type-checked at compile time!** The place and port must have the same token type.
    pub fn wire_input<T: Token>(mut self, from: &PlaceHandle<T>, to: &InputPort<T>) -> Self {
        self.inputs.push(ScenarioArc {
            place: from.id.clone(),
            port: to.name.clone(),
            weight: 1,
            read: false,
            count_from: None,
            correlate_on: None,
            reset_reply_routing: false,
            drain_max: None,
        });
        self
    }

    /// Wire an input arc with weight.
    pub fn wire_input_weight<T: Token>(
        mut self,
        from: &PlaceHandle<T>,
        to: &InputPort<T>,
        weight: usize,
    ) -> Self {
        self.inputs.push(ScenarioArc {
            place: from.id.clone(),
            port: to.name.clone(),
            weight,
            read: false,
            count_from: None,
            correlate_on: None,
            reset_reply_routing: false,
            drain_max: None,
        });
        self
    }

    /// Wire an output arc (port → place).
    ///
    /// **Type-checked at compile time!** The port and place must have the same token type.
    pub fn wire_output<T: Token>(mut self, from: &OutputPort<T>, to: &PlaceHandle<T>) -> Self {
        self.outputs.push(ScenarioArc {
            place: to.id.clone(),
            port: from.name.clone(),
            weight: 1,
            read: false,
            count_from: None,
            correlate_on: None,
            reset_reply_routing: false,
            drain_max: None,
        });
        self
    }

    /// Wire an output arc with weight.
    pub fn wire_output_weight<T: Token>(
        mut self,
        from: &OutputPort<T>,
        to: &PlaceHandle<T>,
        weight: usize,
    ) -> Self {
        self.outputs.push(ScenarioArc {
            place: to.id.clone(),
            port: from.name.clone(),
            weight,
            read: false,
            count_from: None,
            correlate_on: None,
            reset_reply_routing: false,
            drain_max: None,
        });
        self
    }

    // =========================================================================
    // Fluent Wiring API (auto_* methods)
    // =========================================================================
    // These methods combine port creation AND arc wiring in one call.
    // Type is inferred from the PlaceHandle, making the API more concise.

    /// Fluent input: creates port AND wires arc from place.
    ///
    /// Type is inferred from the place handle. Uses Single cardinality and weight 1.
    /// This is the recommended API for simple cases.
    ///
    /// # Example
    /// ```ignore
    /// ctx.transition("allocate", "Allocate Task")
    ///     .auto_input("task", &tasks)
    ///     .auto_input("worker", &workers)
    ///     .auto_output("result", &results)
    ///     .logic(r#"#{ result: ... }"#);
    /// ```
    pub fn auto_input<T: Token>(
        mut self,
        port_name: impl Into<String>,
        place: &PlaceHandle<T>,
    ) -> Self {
        let name = port_name.into();

        // Register schema
        self.ctx.register_schema::<T>();
        self.input_types.push(T::type_name().to_string());

        // Create port
        self.input_ports.push(ScenarioPort {
            name: name.clone(),
            cardinality: Cardinality::Single.as_str().into(),
            schema_ref: Some(T::schema_ref()),
        });

        // Create arc (place → transition port)
        self.inputs.push(ScenarioArc {
            place: place.id.clone(),
            port: name,
            weight: 1,
            read: false,
            count_from: None,
            correlate_on: None,
            reset_reply_routing: false,
            drain_max: None,
        });

        self
    }

    /// Fluent input with full configuration: cardinality and weight.
    ///
    /// Use this when you need custom cardinality or arc weight.
    ///
    /// # Example
    /// ```ignore
    /// ctx.transition("process", "Process Batch")
    ///     .auto_input_with("items", &items, Cardinality::Batch, 3)
    ///     .auto_output("result", &results)
    ///     .logic(r#"#{ result: items }"#);
    /// ```
    pub fn auto_input_with<T: Token>(
        mut self,
        port_name: impl Into<String>,
        place: &PlaceHandle<T>,
        cardinality: Cardinality,
        weight: usize,
    ) -> Self {
        let name = port_name.into();

        self.ctx.register_schema::<T>();
        self.input_types.push(T::type_name().to_string());

        self.input_ports.push(ScenarioPort {
            name: name.clone(),
            cardinality: cardinality.as_str().into(),
            schema_ref: Some(T::schema_ref()),
        });

        self.inputs.push(ScenarioArc {
            place: place.id.clone(),
            port: name,
            weight,
            read: false,
            count_from: None,
            correlate_on: None,
            reset_reply_routing: false,
            drain_max: None,
        });

        self
    }

    /// Fluent batch input: creates batch port AND wires arc from place.
    ///
    /// Type is inferred from the place handle. Uses Batch cardinality and weight 1.
    pub fn auto_input_batch<T: Token>(
        mut self,
        port_name: impl Into<String>,
        place: &PlaceHandle<T>,
    ) -> Self {
        let name = port_name.into();

        self.ctx.register_schema::<T>();
        self.input_types.push(T::type_name().to_string());

        self.input_ports.push(ScenarioPort {
            name: name.clone(),
            cardinality: Cardinality::Batch.as_str().into(),
            schema_ref: Some(T::schema_ref()),
        });

        self.inputs.push(ScenarioArc {
            place: place.id.clone(),
            port: name,
            weight: 1,
            read: false,
            count_from: None,
            correlate_on: None,
            reset_reply_routing: false,
            drain_max: None,
        });

        self
    }

    /// Fluent greedy-drain input: a Batch port whose arc consumes **up to**
    /// `max` tokens per firing (firing on **≥1** token). The script/effect sees
    /// a JSON array of exactly the drained tokens.
    ///
    /// Use for high-volume record-and-discard sinks (telemetry drains): instead
    /// of one firing per token — which makes the eval loop re-fold the marking
    /// O(B²) to drain a backlog of B tokens — the drain swallows the backlog in
    /// `ceil(B/max)` firings, so the quadratic never materializes. Unlike a
    /// fixed `weight`, a drain never strands the `<max` tail (it fires on ≥1)
    /// and never forces accumulation (it consumes whatever is present, up to
    /// `max`). See engine `Arc::drain_max`.
    pub fn auto_input_drain<T: Token>(
        mut self,
        port_name: impl Into<String>,
        place: &PlaceHandle<T>,
        max: usize,
    ) -> Self {
        let name = port_name.into();

        self.ctx.register_schema::<T>();
        self.input_types.push(T::type_name().to_string());

        self.input_ports.push(ScenarioPort {
            name: name.clone(),
            cardinality: Cardinality::Batch.as_str().into(),
            schema_ref: Some(T::schema_ref()),
        });

        self.inputs.push(ScenarioArc {
            place: place.id.clone(),
            port: name,
            weight: 1,
            read: false,
            count_from: None,
            correlate_on: None,
            reset_reply_routing: false,
            drain_max: Some(max),
        });

        self
    }

    /// Fluent read input: creates port AND wires a read arc from place.
    ///
    /// A read arc borrows a token for evaluation: the token is consumed for the
    /// transition's execution but automatically produced back to the same place.
    /// The Rhai script sees it as a regular input variable but doesn't need to
    /// include it in its output.
    ///
    /// # Example
    /// ```ignore
    /// ctx.transition("analyze", "Analyze")
    ///     .auto_input("data", &pending)
    ///     .read_input("config", &shared_config)  // config is borrowed, not consumed
    ///     .auto_output("result", &analyzed)
    ///     .logic(r#"#{ result: #{ value: data.value, threshold: config.threshold } }"#);
    /// ```
    pub fn read_input<T: Token>(
        mut self,
        port_name: impl Into<String>,
        place: &PlaceHandle<T>,
    ) -> Self {
        let name = port_name.into();

        self.ctx.register_schema::<T>();
        self.input_types.push(T::type_name().to_string());

        self.input_ports.push(ScenarioPort {
            name: name.clone(),
            cardinality: Cardinality::Single.as_str().into(),
            schema_ref: Some(T::schema_ref()),
        });

        self.inputs.push(ScenarioArc {
            place: place.id.clone(),
            port: name,
            weight: 1,
            read: true,
            count_from: None,
            correlate_on: None,
            reset_reply_routing: false,
            drain_max: None,
        });

        self
    }

    /// Fluent batch read input: borrows ALL tokens from a place without consuming them.
    ///
    /// Combines batch cardinality (receives an array of all tokens at the place)
    /// with read semantics (tokens are returned to the place after the transition fires).
    /// Useful for collector/accumulator places where tokens build up over time
    /// and a transition needs to read the full history.
    ///
    /// # Example
    /// ```ignore
    /// // observation_log accumulates one token per BO iteration.
    /// // The surrogate reads all observations to fit the GP, without consuming them.
    /// ctx.transition("fit_gp", "Fit GP Model")
    ///     .auto_input("trigger", &train_trigger)
    ///     .read_input_batch("observations", &observation_log)
    ///     .auto_output("model", &model_ready)
    ///     .logic(r#"#{ model: #{ n_obs: observations.len() } }"#);
    /// ```
    pub fn read_input_batch<T: Token>(
        mut self,
        port_name: impl Into<String>,
        place: &PlaceHandle<T>,
    ) -> Self {
        let name = port_name.into();

        self.ctx.register_schema::<T>();
        self.input_types.push(T::type_name().to_string());

        self.input_ports.push(ScenarioPort {
            name: name.clone(),
            cardinality: Cardinality::Batch.as_str().into(),
            schema_ref: Some(T::schema_ref()),
        });

        self.inputs.push(ScenarioArc {
            place: place.id.clone(),
            port: name,
            weight: 1,
            read: true,
            count_from: None,
            correlate_on: None,
            reset_reply_routing: false,
            drain_max: None,
        });

        self
    }

    /// Fluent gather (reduce) input: a count-gated Batch input arc (gather barrier).
    ///
    /// Creates a [`Cardinality::Batch`] input port and wires a Batch input arc that
    /// carries the gather-barrier fields:
    ///
    /// - `count_from` — a producer-namespaced reference (e.g. `"expected.k"`) to a
    ///   field on a bound coordinator token that supplies the count `K` of result
    ///   tokens this arc must accumulate before the transition fires. The arc's
    ///   `weight` is ignored on a gather arc — `K` comes from the coordinator.
    /// - `correlate_on` — an optional field name read from the coordinator token and
    ///   matched against the same-named field on result tokens, so only tokens from
    ///   one gather group (e.g. one loop iteration's `"iteration_id"`) are consumed.
    ///   `None` makes every token in the place eligible.
    ///
    /// The transition fires only once `K` matching result tokens are present, and
    /// consumes exactly those `K`. Pair it with a [`read_input`](Self::read_input)
    /// (or [`auto_input`](Self::auto_input)) for the coordinator token referenced by
    /// `count_from`, so the coordinator is bound before `K` is read.
    ///
    /// # Example
    /// ```ignore
    /// ctx.transition("gather", "Reduce Results")
    ///     .read_input("expected", &coordinator)         // carries expected.k (+ iteration_id)
    ///     .gather_input("results", &result_inbox, "expected.k", Some("iteration_id"))
    ///     .auto_output("reduced", &done)
    ///     .logic(r#"#{ reduced: #{ n: results.len() } }"#);
    /// ```
    pub fn gather_input<T: Token>(
        mut self,
        port_name: impl Into<String>,
        place: &PlaceHandle<T>,
        count_from: &str,
        correlate_on: Option<&str>,
    ) -> Self {
        let name = port_name.into();

        self.ctx.register_schema::<T>();
        self.input_types.push(T::type_name().to_string());

        self.input_ports.push(ScenarioPort {
            name: name.clone(),
            cardinality: Cardinality::Batch.as_str().into(),
            schema_ref: Some(T::schema_ref()),
        });

        self.inputs.push(ScenarioArc {
            place: place.id.clone(),
            port: name,
            weight: 1,
            read: false,
            count_from: Some(count_from.to_string()),
            correlate_on: correlate_on.map(|s| s.to_string()),
            reset_reply_routing: false,
            drain_max: None,
        });

        self
    }

    /// Fluent output: creates port AND wires arc to place.
    ///
    /// Type is inferred from the place handle. Uses weight 1.
    pub fn auto_output<T: Token>(
        mut self,
        port_name: impl Into<String>,
        place: &PlaceHandle<T>,
    ) -> Self {
        let name = port_name.into();

        self.ctx.register_schema::<T>();
        self.output_types.push(T::type_name().to_string());

        self.output_ports.push(ScenarioPort {
            name: name.clone(),
            cardinality: Cardinality::Single.as_str().into(),
            schema_ref: Some(T::schema_ref()),
        });

        self.outputs.push(ScenarioArc {
            place: place.id.clone(),
            port: name,
            weight: 1,
            read: false,
            count_from: None,
            correlate_on: None,
            reset_reply_routing: false,
            drain_max: None,
        });

        self
    }

    /// Mark an already-wired OUTPUT arc (by port name) so its produced token is
    /// emitted WITHOUT inheriting the firing's consumed reply-routing (it starts
    /// routing-less). Call after the matching `auto_output(...)`. Use for a
    /// recycled resource token that must stay re-grantable — e.g. a presence
    /// pool's `t_release` returning a freed unit. No-op if the port has no
    /// output arc. See engine `Arc::reset_reply_routing`.
    pub fn reset_reply_routing_on(mut self, port_name: impl Into<String>) -> Self {
        let name = port_name.into();
        for arc in self.outputs.iter_mut() {
            if arc.port == name {
                arc.reset_reply_routing = true;
            }
        }
        self
    }

    /// Fluent batch (scatter) output: creates a Batch output port AND wires arc to place.
    ///
    /// Mirror of [`auto_output`](Self::auto_output) but the output port is declared
    /// with [`Cardinality::Batch`]. When the transition's script returns a JSON
    /// array on this port, the engine emits ONE token per array element (scatter);
    /// a non-array value on a Batch output port is a permanent error.
    ///
    /// # Example
    /// ```ignore
    /// ctx.transition("scatter", "Fan Out")
    ///     .auto_input("batch", &pending)
    ///     .auto_output_batch("items", &items)  // each element of `items` becomes a token
    ///     .logic(r#"#{ items: batch.parts }"#);
    /// ```
    pub fn auto_output_batch<T: Token>(
        mut self,
        port_name: impl Into<String>,
        place: &PlaceHandle<T>,
    ) -> Self {
        let name = port_name.into();

        self.ctx.register_schema::<T>();
        self.output_types.push(T::type_name().to_string());

        self.output_ports.push(ScenarioPort {
            name: name.clone(),
            cardinality: Cardinality::Batch.as_str().into(),
            schema_ref: Some(T::schema_ref()),
        });

        self.outputs.push(ScenarioArc {
            place: place.id.clone(),
            port: name,
            weight: 1,
            read: false,
            count_from: None,
            correlate_on: None,
            reset_reply_routing: false,
            drain_max: None,
        });

        self
    }

    /// Fluent output with weight configuration.
    ///
    /// Use this when you need a custom arc weight.
    ///
    /// # Example
    /// ```ignore
    /// ctx.transition("split", "Split Output")
    ///     .auto_input("item", &items)
    ///     .auto_output_with("copies", &copies, 3)  // Produces 3 tokens
    ///     .logic(r#"#{ copies: item }"#);
    /// ```
    pub fn auto_output_with<T: Token>(
        mut self,
        port_name: impl Into<String>,
        place: &PlaceHandle<T>,
        weight: usize,
    ) -> Self {
        let name = port_name.into();

        self.ctx.register_schema::<T>();
        self.output_types.push(T::type_name().to_string());

        self.output_ports.push(ScenarioPort {
            name: name.clone(),
            cardinality: Cardinality::Single.as_str().into(),
            schema_ref: Some(T::schema_ref()),
        });

        self.outputs.push(ScenarioArc {
            place: place.id.clone(),
            port: name,
            weight,
            read: false,
            count_from: None,
            correlate_on: None,
            reset_reply_routing: false,
            drain_max: None,
        });

        self
    }

    /// Wire an `_error` output port to a `DynamicToken` place.
    ///
    /// Effect transitions can route errors to an `_error` port. The error token
    /// contains `{ error, handler_id, transition_id, inputs, retryable }` which
    /// enables retry and dead-letter patterns.
    ///
    /// # Example
    /// ```ignore
    /// let errors = ctx.state::<DynamicToken>("errors", "Error Queue");
    ///
    /// ctx.transition("call_api", "Call API")
    ///     .auto_input("request", &requests)
    ///     .auto_output("response", &responses)
    ///     .error_output(&errors)
    ///     .effect("http_handler");
    /// ```
    pub fn error_output<T: Token>(mut self, error_place: &PlaceHandle<T>) -> Self {
        self.ctx.register_schema::<T>();
        self.output_types.push(T::type_name().to_string());
        self.output_ports.push(ScenarioPort {
            name: "_error".to_string(),
            cardinality: Cardinality::Single.as_str().into(),
            schema_ref: Some(T::schema_ref()),
        });
        self.outputs.push(ScenarioArc {
            place: error_place.id.clone(),
            port: "_error".to_string(),
            weight: 1,
            read: false,
            count_from: None,
            correlate_on: None,
            reset_reply_routing: false,
            drain_max: None,
        });
        self
    }

    // =========================================================================
    // Guard and Logic
    // =========================================================================

    /// Set Rhai guard script (returns bool).
    pub fn guard_rhai(mut self, script: impl Into<String>) -> Self {
        self.guard = Some(TransitionGuard::rhai(script));
        self
    }

    /// Set Rhai logic script (returns #{port: value, ...}).
    pub fn logic_rhai(mut self, script: impl Into<String>) -> Self {
        self.logic = Some(TransitionLogic::rhai(script));
        self
    }

    /// Set Wasm logic (future - for compiled Rust modules).
    pub fn logic_wasm(mut self, module: impl Into<String>, function: impl Into<String>) -> Self {
        self.logic = Some(TransitionLogic::wasm(module, function));
        self
    }

    /// Register a Rhai constant scoped to this transition only.
    ///
    /// Unlike [`Context::rhai_const()`], this constant is only prepended to
    /// *this* transition's script, not every transition in the net.
    /// Use for large data (candidate grids, config maps) that only one transition needs.
    ///
    /// # Example
    /// ```ignore
    /// ctx.transition("select", "Select Candidate")
    ///     .rhai_const("GRID", r#"[[0.1, 0.2], [0.3, 0.4]]"#)
    ///     .auto_input("trigger", &trigger)
    ///     .auto_output("result", &results)
    ///     .logic(r#"#{ result: GRID[0] }"#);
    /// ```
    pub fn rhai_const(mut self, name: impl Into<String>, rhai_expr: impl Into<String>) -> Self {
        self.local_rhai_constants
            .push((name.into(), rhai_expr.into()));
        self
    }

    /// Register a Rhai string variable scoped to this transition only.
    ///
    /// Unlike [`Context::rhai_var()`], this variable is only prepended to
    /// *this* transition's script. The value is JSON-escaped and quoted.
    pub fn rhai_var(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        let json_str = serde_json::to_string(&value.into()).unwrap();
        self.local_rhai_variables.push((name.into(), json_str));
        self
    }

    /// Generate the per-transition Rhai preamble.
    fn local_rhai_preamble(&self) -> String {
        let mut lines =
            Vec::with_capacity(self.local_rhai_constants.len() + self.local_rhai_variables.len());
        for (name, expr) in &self.local_rhai_constants {
            lines.push(format!("let {} = {};", name, expr));
        }
        for (name, json_str) in &self.local_rhai_variables {
            lines.push(format!("let {} = {};", name, json_str));
        }
        lines.join("\n")
    }

    /// Set Rhai logic and auto-finalize the transition.
    ///
    /// This is the terminal method - no need to call `.done()` afterwards.
    /// For most use cases, this is the recommended way to end a transition builder chain.
    ///
    /// **Validates the script at build time!** Panics if:
    /// - Rhai syntax is invalid
    /// - Script references variables that don't match input port names
    ///
    /// # Example
    /// ```ignore
    /// ctx.transition("allocate", "Allocate Task")
    ///     .auto_input("task", &tasks)
    ///     .auto_output("result", &results)
    ///     .logic(r#"#{ result: task }"#);  // Auto-finalizes
    /// ```
    ///
    /// # Panics
    /// Panics if the script has syntax errors or references undefined variables.
    pub fn logic(mut self, script: impl Into<String>) {
        let raw_script = script.into();

        // Prepend Rhai constants/variables from context (global) and transition (local)
        let global_preamble = self.ctx.rhai_preamble();
        let local_preamble = self.local_rhai_preamble();
        let script_str = match (global_preamble.is_empty(), local_preamble.is_empty()) {
            (true, true) => raw_script,
            (false, true) => format!("{}\n{}", global_preamble, raw_script),
            (true, false) => format!("{}\n{}", local_preamble, raw_script),
            (false, false) => format!("{}\n{}\n{}", global_preamble, local_preamble, raw_script),
        };

        // Inline validation - fail fast!
        let input_port_names: Vec<String> =
            self.input_ports.iter().map(|p| p.name.clone()).collect();
        let errors = validate_script_inline(&script_str, &input_port_names, &self.id);

        if !errors.is_empty() {
            panic!(
                "\n\n=== SCRIPT VALIDATION FAILED ===\n{}\n================================\n",
                errors.join("\n")
            );
        }

        self.logic = Some(TransitionLogic::rhai(script_str));
        self.finalize();
    }

    /// Like [`logic()`](Self::logic), but auto-wraps the script result in a
    /// spawn_request envelope targeting `"inbox"`.
    ///
    /// The script should return just the initial token fields (job_id, spec, etc.).
    /// This method wraps it as:
    /// ```rhai
    /// let __spawn_token = { <your_script> };
    /// #{ spawn_request: #{ initial_token: __spawn_token, target_place: "inbox" } }
    /// ```
    ///
    /// Use with [`SpawnHandles::prepare()`] for the full spawn pipeline pattern.
    pub fn spawn_logic(self, script: impl Into<String>) {
        self.spawn_logic_to(script, "inbox");
    }

    /// Like [`spawn_logic()`](Self::spawn_logic), but with a custom target place
    /// in the child net.
    pub fn spawn_logic_to(self, script: impl Into<String>, target_place: &str) {
        let inner = script.into();
        let wrapped = format!(
            "let __spawn_token = {{\n{}\n}};\n#{{ spawn_request: #{{ initial_token: __spawn_token, target_place: \"{}\" }} }}",
            inner, target_place
        );
        self.logic(wrapped);
    }

    /// Like [`spawn_logic()`](Self::spawn_logic), but with a Rhai expression
    /// for the child net's human-readable label.
    ///
    /// The `label_expr` is a Rhai expression evaluated in the same scope as the
    /// transition's input ports (e.g. `"OCR: " + entry.department`).
    pub fn spawn_logic_labeled(self, script: impl Into<String>, label_expr: &str) {
        self.spawn_logic_labeled_to(script, "inbox", label_expr);
    }

    /// Like [`spawn_logic_labeled()`](Self::spawn_logic_labeled), but with a
    /// custom target place in the child net.
    pub fn spawn_logic_labeled_to(
        self,
        script: impl Into<String>,
        target_place: &str,
        label_expr: &str,
    ) {
        let inner = script.into();
        // Pre-compute label in a let binding to avoid Rhai parsing ambiguity
        // when label_expr contains if/else blocks inside the map literal.
        let wrapped = format!(
            "let __spawn_token = {{\n{}\n}};\nlet __label = {{\n{}\n}};\n#{{ spawn_request: #{{ initial_token: __spawn_token, target_place: \"{}\", label: __label }} }}",
            inner, label_expr, target_place
        );
        self.logic(wrapped);
    }

    /// Set Rhai guard expression (returns bool).
    ///
    /// **Validates the script at build time!** Panics if:
    /// - Rhai syntax is invalid
    /// - Script references variables that don't match input port names
    ///
    /// # Panics
    /// Panics if the script has syntax errors or references undefined variables.
    pub fn guard(mut self, script: impl Into<String>) -> Self {
        let script_str = script.into();

        // Inline validation - fail fast!
        let input_port_names: Vec<String> =
            self.input_ports.iter().map(|p| p.name.clone()).collect();
        let errors = validate_script_inline(&script_str, &input_port_names, &self.id);

        if !errors.is_empty() {
            panic!(
                "\n\n=== GUARD VALIDATION FAILED ===\n{}\n================================\n",
                errors.join("\n")
            );
        }

        self.guard = Some(TransitionGuard::rhai(script_str));
        self
    }

    /// Set a priority expression for this transition.
    ///
    /// The expression receives input tokens as scope variables and must return a numeric value.
    /// Higher values = higher priority. Evaluated after enabling time and input count comparisons.
    ///
    /// **Validates the script at build time!** Panics if:
    /// - Rhai syntax is invalid
    /// - Script references variables that don't match input port names
    ///
    /// # Example
    /// ```ignore
    /// ctx.transition("process_task", "Process Task")
    ///     .auto_input("task", &tasks)
    ///     .auto_input("worker", &workers)
    ///     .priority("task.urgency")  // Higher urgency fires first
    ///     .auto_output("result", &results)
    ///     .logic(r#"#{ result: task }"#);
    /// ```
    ///
    /// # Priority Expression Examples
    /// - `task.urgency` - Simple field access
    /// - `task.urgency * 10 + task.value` - Weighted combination
    /// - `if task.vip { 100 } else { 0 }` - Conditional priority
    ///
    /// # Panics
    /// Panics if the script has syntax errors or references undefined variables.
    pub fn priority(mut self, expression: impl Into<String>) -> Self {
        let script_str = expression.into();

        // Inline validation - fail fast!
        let input_port_names: Vec<String> =
            self.input_ports.iter().map(|p| p.name.clone()).collect();
        let errors = validate_script_inline(&script_str, &input_port_names, &self.id);

        if !errors.is_empty() {
            panic!(
                "\n\n=== PRIORITY EXPRESSION VALIDATION FAILED ===\n{}\n================================\n",
                errors.join("\n")
            );
        }

        self.priority = Some(TransitionPriority::rhai(script_str));
        self
    }

    /// Mark this transition as a **finalizer**: it is NEVER selected during
    /// normal evaluation and fires ONLY during the engine's post-failure
    /// finalizer drain (see engine `evaluate_until_quiescent`). Use it to
    /// release a resource the net still holds when it fails permanently — e.g.
    /// a LeaseScope's held lease, whose normal release is gated on body success
    /// and so can never fire on the failure path. The finalizer must be enabled
    /// purely by its own input arcs (e.g. consume the single held token); do
    /// NOT gate it on a guard that only the success path satisfies.
    pub fn finalizer(mut self) -> Self {
        self.finalizer = true;
        self
    }

    /// Annotate this transition as both starting and completing a process step.
    ///
    /// Convenience method that sets both `process_step_started` and `process_step_completed`
    /// to the same key. Use this for transitions that represent an instantaneous step
    /// (e.g., a single Rhai script that does validation).
    ///
    /// For multi-transition steps (prepare → dispatch → join), use
    /// `.process_step_started()` and `.process_step_completed()` separately.
    ///
    /// # Example
    /// ```ignore
    /// ctx.transition("validate", "Validate Data")
    ///     .process_step("validation")
    ///     .auto_input("data", &pending)
    ///     .read_input("process", &processes)
    ///     .auto_output("result", &validated)
    ///     .logic(r#"#{ result: data }"#);
    /// ```
    pub fn process_step(mut self, step_key: impl Into<String>) -> Self {
        let key = step_key.into();
        self.process_step_started = Some(key.clone());
        self.process_step_completed = Some(key);
        self
    }

    /// Annotate this transition as starting a process step.
    ///
    /// When this transition fires, the engine publishes a "step_started" event.
    /// Use with `.process_step_completed()` on a later transition to track
    /// multi-transition steps.
    pub fn process_step_started(mut self, step_key: impl Into<String>) -> Self {
        self.process_step_started = Some(step_key.into());
        self
    }

    /// Annotate this transition as completing a process step.
    ///
    /// When this transition fires, the engine publishes a "step_completed" event.
    /// Use with `.process_step_started()` on an earlier transition to track
    /// multi-transition steps.
    pub fn process_step_completed(mut self, step_key: impl Into<String>) -> Self {
        self.process_step_completed = Some(step_key.into());
        self
    }

    /// Declare that this transition causes/expects a signal in the given place.
    ///
    /// This creates a visual "Causation Arc" (dashed line) in the topology,
    /// indicating that the transition's effect will eventually trigger a signal
    /// in the specified place.
    ///
    /// # Example
    /// ```ignore
    /// let job_started = ctx.signal::<()>("job_started", "Job Started");
    ///
    /// ctx.transition("deploy", "Deploy Job")
    ///     .auto_input("job", &pending_jobs)
    ///     .effect("nomad_submit")
    ///     .causes(&job_started);
    /// ```
    pub fn causes<T: Token>(mut self, signal_place: &PlaceHandle<T>) -> Self {
        self.caused_signals.push(signal_place.id.clone());
        self
    }

    /// Internal: finalize and register the transition.
    fn finalize(self) {
        let prefixed_id = self.ctx.prefixed_id(&self.id);

        let logic = self
            .logic
            .expect("Transition must have logic - call .logic(script) before .done()");

        let effect_config = if let TransitionLogic::Effect { config, .. } = &logic {
            config.clone()
        } else {
            None
        };

        // Guards are standalone boolean filter expressions over token data.
        // Unlike logic bodies, they must NOT receive the rhai const/var
        // preamble: prepending `let NAME = <value>;` (e.g. a multi-KB script
        // blob registered via ctx.rhai_var()) ahead of every guard bloats the
        // AIR and can break the engine's guard parse (a JSON-escaped string is
        // not always a valid Rhai literal — e.g. JSON `\uXXXX` vs Rhai
        // `\u{XXXX}`). The logic-body preamble is applied separately in
        // `logic()`. Pass the guard through verbatim.
        let guard = self.guard;

        let transition = ScenarioTransition {
            id: prefixed_id,
            name: self.name,
            group_id: self.ctx.current_group(),
            input_ports: self.input_ports,
            output_ports: self.output_ports,
            inputs: self.inputs,
            outputs: self.outputs,
            guard,
            priority: self.priority,
            finalizer: self.finalizer,
            logic,
            effect_config,
            caused_signals: self.caused_signals,
            process_step_started: self.process_step_started,
            process_step_completed: self.process_step_completed,
            // Build composite schemas for Wasm validation
            input_schema: if self.input_types.is_empty() {
                None
            } else {
                Some(serde_json::json!({
                    "type": "object",
                    "properties": self.input_types.iter()
                        .map(|t| (t.clone(), serde_json::json!({"$ref": format!("#/definitions/{}", t)})))
                        .collect::<serde_json::Map<_, _>>()
                }))
            },
            output_schema: if self.output_types.is_empty() {
                None
            } else {
                Some(serde_json::json!({
                    "type": "object",
                    "properties": self.output_types.iter()
                        .map(|t| (t.clone(), serde_json::json!({"$ref": format!("#/definitions/{}", t)})))
                        .collect::<serde_json::Map<_, _>>()
                }))
            },
        };
        self.ctx.transitions.push(transition);
    }

    /// Set this transition as an effect transition (side effect via registered handler).
    ///
    /// This is a terminal method — no need to call `.done()` afterwards.
    /// Effect transitions don't need a Rhai script; the handler provides the logic.
    ///
    /// # Example
    /// ```ignore
    /// ctx.transition("call_api", "Call External API")
    ///     .auto_input("request", &requests)
    ///     .auto_output("response", &responses)
    ///     .effect("http_handler");  // Auto-finalizes
    /// ```
    pub fn effect(mut self, handler_id: impl Into<String>) {
        self.logic = Some(TransitionLogic::Effect {
            handler_id: handler_id.into(),
            config: None,
        });
        self.finalize();
    }

    /// Set this transition as an effect transition with static configuration.
    ///
    /// Configuration values can reference secrets using the `{{secret:KEY}}`
    /// syntax (or via [`crate::secret()`]). Secrets are resolved at runtime
    /// just before handler execution and never appear in the event log.
    ///
    /// ```ignore
    /// use aithericon_sdk::secret;
    ///
    /// ctx.transition("call_api", "Call API")
    ///     .auto_input("request", &requests)
    ///     .auto_output("response", &responses)
    ///     .effect_with_config("http_handler", serde_json::json!({
    ///         "url": "https://api.example.com",
    ///         "auth": { "token": secret("API_TOKEN") }
    ///     }));
    /// ```
    pub fn effect_with_config(mut self, handler_id: impl Into<String>, config: serde_json::Value) {
        self.logic = Some(TransitionLogic::Effect {
            handler_id: handler_id.into(),
            config: Some(config),
        });
        self.finalize();
    }

    // =========================================================================
    // Typed built-in effect methods
    // =========================================================================

    /// Use a built-in effect handler descriptor.
    ///
    /// Records the service requirement on the context and sets the handler ID.
    /// This is a terminal method — auto-finalizes the transition.
    pub fn builtin_effect(mut self, descriptor: &EffectDescriptor) {
        self.ctx.record_service_requirement(descriptor);
        self.logic = Some(TransitionLogic::Effect {
            handler_id: descriptor.handler_id.to_string(),
            config: None,
        });
        self.finalize();
    }

    /// Submit a job to the scheduler service (Nomad, Slurm, or Mock).
    ///
    /// Uses handler `"scheduler_submit"` with default ports `"job"` / `"submitted"`.
    /// This is a terminal method — auto-finalizes the transition.
    pub fn scheduler_submit(self) {
        self.builtin_effect(&effects::SCHEDULER_SUBMIT);
    }

    /// Cancel a running scheduler job.
    ///
    /// Uses handler `"scheduler_cancel"` with default ports `"job"` / `"cancelled"`.
    /// This is a terminal method — auto-finalizes the transition.
    pub fn scheduler_cancel(self) {
        self.builtin_effect(&effects::SCHEDULER_CANCEL);
    }

    /// Submit an execution to the executor service.
    ///
    /// Uses handler `"executor_submit"` with default ports `"job"` / `"submitted"`.
    /// Does **not** set signal routing or causation arcs — prefer
    /// [`executor_submit_to`](Self::executor_submit_to) which handles both
    /// automatically from `PlaceHandle` references.
    ///
    /// This is a terminal method — auto-finalizes the transition.
    pub fn executor_submit(self) {
        self.builtin_effect(&effects::EXECUTOR_SUBMIT);
    }

    /// Submit an execution with fully typed contract.
    ///
    /// Wires the input/output/error ports, builds `effect_config.signal_routes`
    /// and `effect_config.event_routes` from `PlaceHandle` IDs, and registers
    /// causation arcs — all from a single [`ExecutorSubmit`] declaration.
    ///
    /// This is a terminal method — auto-finalizes the transition.
    pub fn executor_submit_to(self, ports: ExecutorSubmit<'_>) {
        // Wire the handler's I/O contract
        let mut builder = self
            .auto_input("job", ports.job)
            .auto_output("submitted", ports.submitted)
            .error_output(ports.errors);

        builder
            .ctx
            .record_service_requirement(&effects::EXECUTOR_SUBMIT);

        // Build signal_routes from handle IDs (type-safe, scoped)
        let mut config = serde_json::json!({
            "signal_routes": {
                "accepted": ports.accepted.id(),
                "running": ports.running.id(),
                "completed": ports.completed.id(),
                "failed": ports.failed.id(),
                "timed_out": ports.timed_out.id(),
                "cancelled": ports.cancelled.id(),
            }
        });

        // Event routes (optional)
        let mut event_routes = serde_json::Map::new();
        if let Some(p) = ports.progress {
            event_routes.insert("progress".into(), serde_json::json!(p.id()));
        }
        if let Some(a) = ports.artifact {
            event_routes.insert("artifact".into(), serde_json::json!(a.id()));
        }
        if let Some(m) = ports.metric {
            event_routes.insert("metric".into(), serde_json::json!(m.id()));
        }
        if let Some(p) = ports.phase {
            event_routes.insert("phase".into(), serde_json::json!(p.id()));
        }
        if let Some(o) = ports.output {
            event_routes.insert("output".into(), serde_json::json!(o.id()));
        }
        if let Some(l) = ports.log {
            event_routes.insert("log".into(), serde_json::json!(l.id()));
        }
        if let Some(c) = ports.control_in {
            event_routes.insert("control_emit".into(), serde_json::json!(c.id()));
        }
        if !event_routes.is_empty() {
            config["event_routes"] = serde_json::Value::Object(event_routes);
        }

        // Process context (optional)
        if let Some(pid) = ports.process_id {
            config["process_id"] = serde_json::json!(pid);
        }
        if let Some(step) = ports.process_step {
            config["process_step"] = serde_json::json!(step);
        }

        builder.logic = Some(TransitionLogic::Effect {
            handler_id: effects::EXECUTOR_SUBMIT.handler_id.to_string(),
            config: Some(config),
        });

        // Register causation for submit-triggered signals
        // (not cancelled — that's caused by the cancel transition)
        builder.caused_signals.push(ports.accepted.id().to_string());
        builder.caused_signals.push(ports.running.id().to_string());
        builder
            .caused_signals
            .push(ports.completed.id().to_string());
        builder.caused_signals.push(ports.failed.id().to_string());
        builder
            .caused_signals
            .push(ports.timed_out.id().to_string());

        builder.finalize();
    }

    /// Cancel a running execution.
    ///
    /// Uses handler `"executor_cancel"` with default ports `"job"` / `"cancelled"`.
    /// Does **not** register causation arcs — prefer
    /// [`executor_cancel_to`](Self::executor_cancel_to) which handles that
    /// automatically.
    ///
    /// This is a terminal method — auto-finalizes the transition.
    pub fn executor_cancel(self) {
        self.builtin_effect(&effects::EXECUTOR_CANCEL);
    }

    /// Cancel a running execution with fully typed contract.
    ///
    /// Wires input ports (job + cancel request with `execution_id` correlation),
    /// output port, error port, the `executor_cancel` effect, and causation —
    /// all from a single [`ExecutorCancel`] declaration.
    ///
    /// This is a terminal method — auto-finalizes the transition.
    pub fn executor_cancel_to(self, ports: ExecutorCancel<'_>) {
        let mut builder = self
            .auto_input("job", ports.job)
            .auto_input("sig", ports.cancel_request)
            .correlate("sig", "job", "execution_id")
            .auto_output("cancelled", ports.cancelling)
            .error_output(ports.errors);

        builder
            .ctx
            .record_service_requirement(&effects::EXECUTOR_CANCEL);
        builder.logic = Some(TransitionLogic::Effect {
            handler_id: effects::EXECUTOR_CANCEL.handler_id.to_string(),
            config: None,
        });
        builder
            .caused_signals
            .push(ports.cancelled_signal.id().to_string());
        builder.finalize();
    }

    /// Schedule a durable timer via Clockmaster.
    ///
    /// Uses handler `"timer_schedule"` with default ports `"timer"` / `"scheduled"`.
    /// This is a terminal method — auto-finalizes the transition.
    pub fn timer_schedule(self) {
        self.builtin_effect(&effects::TIMER_SCHEDULE);
    }

    /// Cancel a scheduled timer.
    ///
    /// Uses handler `"timer_cancel"` with default ports `"timer"` / `"cancelled"`.
    /// This is a terminal method — auto-finalizes the transition.
    pub fn timer_cancel(self) {
        self.builtin_effect(&effects::TIMER_CANCEL);
    }

    /// Submit a human-in-the-loop task.
    ///
    /// Uses handler `"human_task"` with default ports `"task"` / `"assigned"`.
    /// This is a terminal method — auto-finalizes the transition.
    pub fn human_task(self) {
        self.builtin_effect(&effects::HUMAN_TASK);
    }

    /// Cancel a human task.
    ///
    /// Uses handler `"human_cancel"` with default ports `"task"` / `"cancelled"`.
    /// This is a terminal method — auto-finalizes the transition.
    pub fn human_cancel(self) {
        self.builtin_effect(&effects::HUMAN_CANCEL);
    }

    /// Spawn a child net dynamically.
    ///
    /// Uses handler `"spawn_net"` with default ports `"spawn_request"` / `"spawned"`.
    /// The input token must contain the child net definition (scenario JSON,
    /// optional parameters, initial token, and target place).
    ///
    /// This is a terminal method — auto-finalizes the transition.
    pub fn spawn_net(self) {
        self.builtin_effect(&effects::SPAWN_NET);
    }

    /// Submit a human-in-the-loop task, routing responses to the given signal place.
    ///
    /// **Deprecated**: prefer [`human_task_to`](Self::human_task_to) with
    /// [`HumanTaskSubmit`] which enforces the full handler contract.
    ///
    /// This is a terminal method — auto-finalizes the transition.
    pub fn human_task_to_signal<T: Token>(mut self, signal_place: &PlaceHandle<T>) {
        self.ctx.record_service_requirement(&effects::HUMAN_TASK);
        self.logic = Some(TransitionLogic::Effect {
            handler_id: effects::HUMAN_TASK.handler_id.to_string(),
            config: Some(serde_json::json!({
                "place": signal_place.id()
            })),
        });
        self.caused_signals.push(signal_place.id().to_string());
        self.finalize();
    }

    /// Submit a human-in-the-loop task with fully typed contract.
    ///
    /// Wires input/output/error ports, effect config with signal routing,
    /// and causation — all from a single [`HumanTaskSubmit`] declaration.
    ///
    /// This is a terminal method — auto-finalizes the transition.
    pub fn human_task_to(self, ports: HumanTaskSubmit<'_>) {
        let mut builder = self
            .auto_input("task", ports.task)
            .auto_output("assigned", ports.assigned)
            .error_output(ports.errors);

        builder.ctx.record_service_requirement(&effects::HUMAN_TASK);
        builder.logic = Some(TransitionLogic::Effect {
            handler_id: effects::HUMAN_TASK.handler_id.to_string(),
            config: Some(serde_json::json!({
                "place": ports.response_signal.id()
            })),
        });
        builder
            .caused_signals
            .push(ports.response_signal.id().to_string());
        builder.finalize();
    }

    /// Cancel a human task with fully typed contract.
    ///
    /// Wires input/output/error ports and the `human_cancel` effect —
    /// all from a single [`HumanTaskCancel`] declaration.
    ///
    /// This is a terminal method — auto-finalizes the transition.
    pub fn human_cancel_to(self, ports: HumanTaskCancel<'_>) {
        let mut builder = self
            .auto_input("task", ports.task)
            .auto_output("cancelled", ports.cancelled)
            .error_output(ports.errors);

        builder
            .ctx
            .record_service_requirement(&effects::HUMAN_CANCEL);
        builder.logic = Some(TransitionLogic::Effect {
            handler_id: effects::HUMAN_CANCEL.handler_id.to_string(),
            config: None,
        });
        builder.finalize();
    }

    /// Schedule a durable timer with fully typed contract.
    ///
    /// Wires input/output/error ports, the `timer_schedule` effect, and
    /// causation for the signal place — all from a single [`TimerSchedule`]
    /// declaration.
    ///
    /// This is a terminal method — auto-finalizes the transition.
    pub fn timer_schedule_to(self, ports: TimerSchedule<'_>) {
        let mut builder = self
            .auto_input("timer", ports.timer)
            .auto_output("scheduled", ports.scheduled)
            .error_output(ports.errors);

        builder
            .ctx
            .record_service_requirement(&effects::TIMER_SCHEDULE);
        builder.logic = Some(TransitionLogic::Effect {
            handler_id: effects::TIMER_SCHEDULE.handler_id.to_string(),
            config: None,
        });
        builder.caused_signals.push(ports.signal.id().to_string());
        builder.finalize();
    }

    /// Cancel a scheduled timer with fully typed contract.
    ///
    /// Wires input/output/error ports and the `timer_cancel` effect —
    /// all from a single [`TimerCancel`] declaration.
    ///
    /// This is a terminal method — auto-finalizes the transition.
    pub fn timer_cancel_to(self, ports: TimerCancel<'_>) {
        let mut builder = self
            .auto_input("timer", ports.timer)
            .auto_output("cancelled", ports.cancelled)
            .error_output(ports.errors);

        builder
            .ctx
            .record_service_requirement(&effects::TIMER_CANCEL);
        builder.logic = Some(TransitionLogic::Effect {
            handler_id: effects::TIMER_CANCEL.handler_id.to_string(),
            config: None,
        });
        builder.finalize();
    }

    /// Submit a job to the scheduler with fully typed contract.
    ///
    /// Wires input/output/error ports, the `scheduler_submit` effect, and
    /// causation for all status signal places — all from a single
    /// [`SchedulerSubmit`] declaration.
    ///
    /// This is a terminal method — auto-finalizes the transition.
    pub fn scheduler_submit_to(self, ports: SchedulerSubmit<'_>) {
        let mut builder = self
            .auto_input("job", ports.job)
            .auto_output("submitted", ports.submitted)
            .error_output(ports.errors);

        builder
            .ctx
            .record_service_requirement(&effects::SCHEDULER_SUBMIT);
        builder.logic = Some(TransitionLogic::Effect {
            handler_id: effects::SCHEDULER_SUBMIT.handler_id.to_string(),
            config: None,
        });
        builder.caused_signals.push(ports.running.id().to_string());
        builder
            .caused_signals
            .push(ports.completed.id().to_string());
        builder.caused_signals.push(ports.failed.id().to_string());
        if let Some(t) = ports.timed_out {
            builder.caused_signals.push(t.id().to_string());
        }
        builder.finalize();
    }

    /// Cancel a scheduler job with fully typed contract.
    ///
    /// Wires input ports (job + cancel request with `scheduler_job_id` correlation),
    /// output port, error port, and the `scheduler_cancel` effect —
    /// all from a single [`SchedulerCancel`] declaration.
    ///
    /// This is a terminal method — auto-finalizes the transition.
    pub fn scheduler_cancel_to(self, ports: SchedulerCancel<'_>) {
        let mut builder = self
            .auto_input("job", ports.job)
            .auto_input("sig", ports.cancel_request)
            .correlate("sig", "job", "scheduler_job_id")
            .auto_output("cancelled", ports.cancelled)
            .error_output(ports.errors);

        builder
            .ctx
            .record_service_requirement(&effects::SCHEDULER_CANCEL);
        builder.logic = Some(TransitionLogic::Effect {
            handler_id: effects::SCHEDULER_CANCEL.handler_id.to_string(),
            config: None,
        });
        builder.finalize();
    }

    // =========================================================================
    // Process lifecycle effects
    // =========================================================================

    /// Start a process lifecycle (free-form ports).
    ///
    /// **Deprecated**: prefer [`process_start_to`](Self::process_start_to) with
    /// [`ProcessStart`] which enforces the full handler contract.
    ///
    /// This is a terminal method — auto-finalizes the transition.
    pub fn process_start(mut self, config: serde_json::Value) {
        self.ctx.record_service_requirement(&effects::PROCESS_START);
        self.logic = Some(TransitionLogic::Effect {
            handler_id: effects::PROCESS_START.handler_id.to_string(),
            config: Some(config),
        });
        self.finalize();
    }

    /// Complete a process lifecycle (free-form ports).
    ///
    /// **Deprecated**: prefer [`process_complete_to`](Self::process_complete_to) with
    /// [`ProcessComplete`] which enforces the full handler contract.
    ///
    /// This is a terminal method — auto-finalizes the transition.
    pub fn process_complete(self) {
        self.builtin_effect(&effects::PROCESS_COMPLETE);
    }

    /// Fail a process lifecycle (free-form ports).
    ///
    /// Tolerant counterpart to [`process_complete`](Self::process_complete):
    /// passes the trigger token through and marks the owning process failed
    /// (resolved by the causality tag graph — no `process_id` required on the
    /// token). The net continues to its normal end.
    ///
    /// This is a terminal method — auto-finalizes the transition.
    pub fn process_fail(self) {
        self.builtin_effect(&effects::PROCESS_FAIL);
    }

    /// Start a process lifecycle with fully typed contract.
    ///
    /// Wires input/output ports, the `process_start` effect with config —
    /// all from a single [`ProcessStart`] declaration.
    ///
    /// This is a terminal method — auto-finalizes the transition.
    pub fn process_start_to(self, ports: ProcessStart<'_>) {
        let mut builder = self
            .auto_input("trigger", ports.trigger)
            .auto_output("process", ports.process);

        builder
            .ctx
            .record_service_requirement(&effects::PROCESS_START);
        builder.logic = Some(TransitionLogic::Effect {
            handler_id: effects::PROCESS_START.handler_id.to_string(),
            config: Some(
                serde_json::to_value(ports.config).expect("ProcessStartConfig serializes"),
            ),
        });
        builder.finalize();
    }

    /// Complete a process lifecycle with fully typed contract.
    ///
    /// Wires read input (for `process_id`), consuming input, output port,
    /// and the `process_complete` effect — all from a single [`ProcessComplete`]
    /// declaration.
    ///
    /// This is a terminal method — auto-finalizes the transition.
    pub fn process_complete_to(self, ports: ProcessComplete<'_>) {
        let builder = self
            .read_input("process", ports.process)
            .auto_input("done", ports.done)
            .auto_output("completed", ports.completed);

        builder.builtin_effect(&effects::PROCESS_COMPLETE);
    }

    // =========================================================================
    // Correlation helpers
    // =========================================================================

    /// Add a correlation guard matching a single field across two input ports.
    ///
    /// Generates: `"port1.field == port2.field"`
    ///
    /// Can be chained with additional `.guard()` or `.correlate()` calls.
    ///
    /// # Example
    /// ```ignore
    /// ctx.transition("join", "Join Result")
    ///     .auto_input("job", &submitted)
    ///     .auto_input("sig", &sig_completed)
    ///     .correlate("sig", "job", "execution_id")
    ///     .auto_output("done", &completed)
    ///     .logic(r#"#{ done: job }"#);
    /// ```
    pub fn correlate(self, port1: &str, port2: &str, field: &str) -> Self {
        self.correlate_on(port1, port2, &[field])
    }

    /// Add a correlation guard matching multiple fields across two input ports.
    ///
    /// Generates: `"port1.f1 == port2.f1 && port1.f2 == port2.f2 && ..."`
    ///
    /// # Example
    /// ```ignore
    /// ctx.transition("join", "Join Result")
    ///     .auto_input("result", &result_inbox)
    ///     .auto_input("pending", &pending)
    ///     .correlate_on("result", "pending", &["job_id", "run"])
    ///     .auto_output("done", &completed)
    ///     .logic(r#"#{ done: result }"#);
    /// ```
    pub fn correlate_on(self, port1: &str, port2: &str, fields: &[&str]) -> Self {
        assert!(
            !fields.is_empty(),
            "correlate_on requires at least one field"
        );
        let guard_expr: String = fields
            .iter()
            .map(|f| format!("{}.{} == {}.{}", port1, f, port2, f))
            .collect::<Vec<_>>()
            .join(" && ");
        self.guard(guard_expr)
    }

    /// Configure this transition as a durable timer.
    ///
    /// Automatically sets the logic to `timer_schedule` effect and uses the
    /// provided delay and signal place.
    ///
    /// Note: The transition must have an input port named "timer" which
    /// provides the trigger.
    ///
    /// # Example
    /// ```ignore
    /// let sig_ready = ctx.signal::<()>("sig_ready", "Timer Fired");
    ///
    /// ctx.transition("wait_1h", "Wait 1 Hour")
    ///     .auto_input("timer", &pending)
    ///     .auto_output("scheduled", &confirm_place)
    ///     .timer(3600000, &sig_ready);
    /// ```
    pub fn timer<T: Token>(self, _delay_ms: u64, _signal_place: &PlaceHandle<T>) {
        self.builtin_effect(&effects::TIMER_SCHEDULE);
    }

    /// Explicit finalization (for advanced use cases).
    ///
    /// Usually you don't need this - `.logic()` auto-finalizes.
    /// Use this when using `.logic_rhai()` or `.logic_wasm()` instead of `.logic()`.
    ///
    /// # Panics
    /// Panics if logic was not set via `.logic()`, `.logic_rhai()`, `.logic_wasm()`, or `.effect()`.
    pub fn done(self) {
        self.finalize();
    }
}

#[cfg(test)]
mod tests {
    use crate::token;
    use crate::Context;

    #[token]
    struct Item {
        value: i64,
    }

    #[token]
    struct Coordinator {
        k: i64,
        iteration_id: String,
    }

    #[test]
    fn auto_output_batch_marks_output_port_cardinality_batch() {
        let mut ctx = Context::new("test");
        let pending = ctx.state::<Coordinator>("pending", "Pending");
        let items = ctx.state::<Item>("items", "Items");

        ctx.transition("scatter", "Fan Out")
            .auto_input("batch", &pending)
            .auto_output_batch("items", &items)
            .logic(r#"#{ items: [batch.k] }"#);

        let t = ctx.transitions.iter().find(|t| t.id == "scatter").unwrap();

        let out_port = t
            .output_ports
            .iter()
            .find(|p| p.name == "items")
            .expect("output port `items` exists");
        assert_eq!(out_port.cardinality, "batch");

        // The output arc itself carries no gather fields.
        let out_arc = t.outputs.iter().find(|a| a.port == "items").unwrap();
        assert!(out_arc.count_from.is_none());
        assert!(out_arc.correlate_on.is_none());
    }

    #[test]
    fn gather_input_sets_batch_port_and_carries_barrier_fields() {
        let mut ctx = Context::new("test");
        let coordinator = ctx.state::<Coordinator>("coordinator", "Coordinator");
        let results = ctx.state::<Item>("results", "Results");
        let done = ctx.state::<Item>("done", "Done");

        ctx.transition("gather", "Reduce Results")
            .read_input("expected", &coordinator)
            .gather_input("results", &results, "expected.k", Some("iteration_id"))
            .auto_output("reduced", &done)
            .logic(r#"#{ reduced: #{ value: results.len() } }"#);

        let t = ctx.transitions.iter().find(|t| t.id == "gather").unwrap();

        // The gather input port is Batch cardinality.
        let in_port = t
            .input_ports
            .iter()
            .find(|p| p.name == "results")
            .expect("input port `results` exists");
        assert_eq!(in_port.cardinality, "batch");

        // The gather input arc carries count_from + correlate_on.
        let gather_arc = t
            .inputs
            .iter()
            .find(|a| a.port == "results")
            .expect("input arc on `results` exists");
        assert_eq!(gather_arc.count_from.as_deref(), Some("expected.k"));
        assert_eq!(gather_arc.correlate_on.as_deref(), Some("iteration_id"));
        // A gather arc is a consuming Batch arc (not a read arc).
        assert!(!gather_arc.read);
    }

    #[test]
    fn gather_input_without_correlate_on_leaves_field_none() {
        let mut ctx = Context::new("test");
        let coordinator = ctx.state::<Coordinator>("coordinator", "Coordinator");
        let results = ctx.state::<Item>("results", "Results");
        let done = ctx.state::<Item>("done", "Done");

        ctx.transition("gather", "Reduce Results")
            .read_input("expected", &coordinator)
            .gather_input("results", &results, "expected.k", None)
            .auto_output("reduced", &done)
            .logic(r#"#{ reduced: #{ value: results.len() } }"#);

        let t = ctx.transitions.iter().find(|t| t.id == "gather").unwrap();
        let gather_arc = t.inputs.iter().find(|a| a.port == "results").unwrap();
        assert_eq!(gather_arc.count_from.as_deref(), Some("expected.k"));
        assert!(gather_arc.correlate_on.is_none());
    }

    #[test]
    fn non_gather_arcs_omit_barrier_fields_from_air_json() {
        // Byte-compat: an ordinary auto_input/auto_output transition must not emit
        // count_from/correlate_on keys, so existing AIR round-trips identically.
        let mut ctx = Context::new("test");
        let input = ctx.state::<Item>("input", "Input");
        let output = ctx.state::<Item>("output", "Output");

        ctx.transition("plain", "Plain")
            .auto_input("inp", &input)
            .auto_output("out", &output)
            .logic(r#"#{ out: inp }"#);

        let t = ctx.transitions.iter().find(|t| t.id == "plain").unwrap();
        let json = serde_json::to_string(&t.inputs).unwrap();
        assert!(!json.contains("count_from"), "json was: {json}");
        assert!(!json.contains("correlate_on"), "json was: {json}");
    }
}
