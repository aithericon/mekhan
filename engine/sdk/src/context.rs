//! Context builder - the main entry point for building topologies.
//!
//! # Example
//! ```ignore
//! let mut ctx = Context::new("my-workflow")
//!     .description("A sample workflow");
//!
//! let tasks = ctx.state::<Task>("tasks", "Task Queue");
//! let workers = ctx.state::<Worker>("workers", "Workers");
//!
//! // Use scope() to group related elements
//! ctx.scope("Worker Pool", |ctx| {
//!     let processing = ctx.state::<Task>("processing", "Processing");
//!     // ... transitions inside the group ...
//! });
//!
//! let scenario = ctx.build();
//! println!("{}", scenario.to_json().unwrap());
//! ```

use std::collections::{HashMap, HashSet};
use std::marker::PhantomData;

use petri_domain::effects::{self, EffectDescriptor, ServiceCategory, ServiceRequirement};

use crate::component::Component;
use crate::place::PlaceHandle;
use crate::resource::ResourceBuilder;
use crate::scenario::{
    AdapterLogic, BridgeSourceDto, BridgeTargetDto, MockAdapterConfig, ScenarioDefinition,
    ScenarioGroup, ScenarioPlace, ScenarioToken, ScenarioTransition,
};
use crate::token::DynamicToken;
use crate::transition::TransitionBuilder;
use crate::Token;

/// Pre-created I/O places for a spawned child net.
///
/// Passed to the child builder closure in [`Context::spawn()`]. The child wires
/// transitions between these places — bridging is handled automatically.
pub struct SpawnChildIO {
    /// Bridge-in place where the initial token arrives from the parent.
    pub inbox: PlaceHandle<DynamicToken>,
    /// Bridge-reply place: output here routes back to the parent's reply place
    /// via consumed `reply_routing.reply_to` (automatic correlation).
    pub reply: PlaceHandle<DynamicToken>,
    /// Bridge-out-param place: output here routes to the parent's failure place
    /// via `$params.parent_net_id` / `$params.failure_place`.
    pub failure: PlaceHandle<DynamicToken>,
}

/// Handles returned by [`Context::spawn()`] for wiring into the parent net.
///
/// The parent orchestrator wires a prepare transition to [`request`](Self::request),
/// then consumes results from [`reply`](Self::reply) and failures from
/// [`failure`](Self::failure).
pub struct SpawnHandles<TReply: Token> {
    /// Bridge-in place receiving the child's result token.
    pub reply: PlaceHandle<TReply>,
    /// Bridge-in place receiving the child's failure token.
    pub failure: PlaceHandle<DynamicToken>,
    /// State place for the spawn request token. Wire your prepare transition's
    /// output here. The token must contain at minimum:
    /// ```json
    /// { "initial_token": { ... }, "target_place": "inbox" }
    /// ```
    /// Optionally: `child_net_id` (custom ID) and `parameters` (runtime overrides).
    pub request: PlaceHandle<DynamicToken>,
    /// State place for the spawn confirmation token
    /// (`{ child_net_id, status: "spawned" }`).
    pub spawned: PlaceHandle<DynamicToken>,
    /// Bridge-out place for forwarding the initial token to the spawned child.
    /// Connected automatically by the spawn transition's "bridge" output port.
    pub outbox: PlaceHandle<DynamicToken>,
    /// The spawn name (used by `on_failure()` for transition ID generation).
    pub(crate) name: String,
    _marker: PhantomData<TReply>,
}

/// Handles returned by [`Context::timer_with_cancel`].
///
/// Provides access to the scheduled confirmation place (for correlation)
/// and the cancel-input place (for requesting cancellation).
pub struct TimerHandles {
    /// Place holding `TimerScheduled` tokens (contains `timer_correlation_id` for cancel).
    pub scheduled: PlaceHandle<crate::effect_tokens::TimerScheduled>,
    /// Place to inject `TimerCancelInput` tokens into to request cancellation.
    pub cancel_input: PlaceHandle<crate::effect_tokens::TimerCancelInput>,
}

impl<TReply: Token> SpawnHandles<TReply> {
    /// Create a standard failure-forwarding transition.
    ///
    /// Consumes tokens from the spawn's failure bridge and produces a
    /// `WorkflowFailed`-shaped token to `target` with the given `phase` label.
    ///
    /// This is a convenience method that replaces the common pattern:
    /// ```ignore
    /// ctx.transition("fail_ocr", "Fail OCR Step")
    ///     .auto_input("fail", &ocr.failure)
    ///     .auto_output("out", &workflow_failed)
    ///     .logic(r#"#{ out: #{ phase: "ocr", ... } }"#);
    /// ```
    /// With:
    /// ```ignore
    /// ocr.on_failure(ctx, &workflow_failed, "ocr");
    /// ```
    pub fn on_failure<T: Token>(&self, ctx: &mut Context, target: &PlaceHandle<T>, phase: &str) {
        ctx.transition(
            format!("fail_{}", self.name),
            format!("Fail {} Step", phase),
        )
        .auto_input("fail", &self.failure)
        .auto_output("out", target)
        .logic(format!(
            r#"#{{
                out: #{{
                    phase: "{}",
                    job_id: if fail.job_id != () {{ fail.job_id }} else {{ "unknown" }},
                    reason: if fail.reason != () {{ fail.reason }} else {{ "unknown" }}
                }}
            }}"#,
            phase
        ));
    }

    /// Create a pre-wired prepare transition for this spawn step.
    ///
    /// The transition ID is `prepare_{name}` and the output to `self.request`
    /// is already configured. Chain additional `.read_input()`, `.auto_input()`,
    /// `.guard()` calls, then finalize with `.spawn_logic()` or `.logic()`.
    ///
    /// # Example
    /// ```ignore
    /// ocr.prepare(ctx, "Prepare OCR Job")
    ///     .read_input("entry", &entry_data)
    ///     .auto_input("params", &invoice_params)
    ///     .guard(r#"params.document_type.starts_with("image")"#)
    ///     .spawn_logic(r#"
    ///         #{ job_id: "ocr:" + entry.dept, run: 0, retries: 0, max_retries: 2, spec: spec }
    ///     "#);
    /// ```
    pub fn prepare<'ctx>(&self, ctx: &'ctx mut Context, label: &str) -> TransitionBuilder<'ctx> {
        ctx.transition(format!("prepare_{}", self.name), label)
            .auto_output("spawn_request", &self.request)
    }

    /// Create a pre-wired join transition for this spawn step.
    ///
    /// The transition ID is `join_{name}` and the input from `self.reply`
    /// is already configured. Chain additional `.read_input()`, `.auto_output()`
    /// calls, then finalize with `.logic()`.
    ///
    /// # Example
    /// ```ignore
    /// ocr.join(ctx, "Join OCR Result")
    ///     .auto_output("invoice", &extracted_data)
    ///     .logic(r#"#{ invoice: #{ data: result.detail.outputs.response } }"#);
    /// ```
    pub fn join<'ctx>(&self, ctx: &'ctx mut Context, label: &str) -> TransitionBuilder<'ctx> {
        ctx.transition(format!("join_{}", self.name), label)
            .auto_input("result", &self.reply)
    }
}

/// The main context for building a Petri net topology.
///
/// Collects places, transitions, groups, and type schemas as you build.
/// Use `scope()` to create hierarchical groupings for visualization.
/// Use `use_component()` to instantiate reusable components with prefixed IDs.
pub struct Context {
    name: String,
    description: Option<String>,
    pub(crate) places: Vec<ScenarioPlace>,
    pub(crate) transitions: Vec<ScenarioTransition>,
    /// Groups for visualization (hierarchical components)
    pub(crate) groups: Vec<ScenarioGroup>,
    /// Mock adapters for frontend simulation
    pub(crate) mock_adapters: Vec<MockAdapterConfig>,
    /// Stack of group IDs for nested scope() calls
    scope_stack: Vec<String>,
    /// Counter for generating unique group IDs
    group_counter: usize,
    /// Stack of ID prefixes for component isolation (e.g., ["transcode_1", "validation"])
    id_prefix_stack: Vec<String>,
    /// Counter for generating unique component instance IDs
    component_counter: usize,
    /// Counters for generating unique step instance IDs (per step type)
    step_counters: HashMap<String, usize>,
    /// Collected JSON schemas from all token types
    pub(crate) definitions: HashMap<String, serde_json::Value>,
    /// Collected service requirements from typed effect methods
    service_requirements: HashMap<ServiceCategory, HashSet<String>>,
    /// Named Rhai expressions prepended to all Rhai scripts (e.g., shared config maps).
    rhai_constants: Vec<(String, String)>,
    /// Named Rust string values injected as Rhai string literals.
    rhai_variables: Vec<(String, String)>,
    /// Files to upload to the artifact store during net deployment.
    pub(crate) staged_files: Vec<StagedFile>,
}

/// A file to be uploaded to the artifact store during net deployment.
///
/// Content is read from the local filesystem at deploy time (not at
/// definition time), so the net binary doesn't need `include_str!()`.
#[derive(Clone, Debug)]
pub struct StagedFile {
    /// Storage path in the artifact store (e.g., `"scripts/fit_gp.py"`).
    pub storage_path: String,
    /// Local filesystem path to read at deploy time.
    pub local_path: std::path::PathBuf,
}

impl Context {
    /// Create a new context with the given scenario name.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: None,
            places: vec![],
            transitions: vec![],
            groups: vec![],
            mock_adapters: vec![],
            scope_stack: vec![],
            group_counter: 0,
            id_prefix_stack: vec![],
            component_counter: 0,
            step_counters: HashMap::new(),
            definitions: HashMap::new(),
            service_requirements: HashMap::new(),
            rhai_constants: vec![],
            rhai_variables: vec![],
            staged_files: vec![],
        }
    }

    /// Record a service requirement from a typed effect handler.
    pub(crate) fn record_service_requirement(&mut self, descriptor: &EffectDescriptor) {
        self.service_requirements
            .entry(descriptor.category.clone())
            .or_default()
            .insert(descriptor.handler_id.to_string());
    }

    /// Register a named Rhai constant expression available in all Rhai scripts.
    ///
    /// The expression is prepended as `let NAME = <expr>;` to every Rhai transition
    /// script in this context. Use this for shared configuration maps, schemas, or
    /// other Rhai literals that appear in multiple transitions.
    ///
    /// # Example
    /// ```ignore
    /// ctx.rhai_const("S3_STORAGE", r#"#{
    ///     backend: "s3",
    ///     endpoint: "http://localhost:9000",
    ///     bucket: "data"
    /// }"#);
    /// // Now scripts can reference S3_STORAGE directly
    /// ```
    pub fn rhai_const(&mut self, name: impl Into<String>, rhai_expr: impl Into<String>) {
        self.rhai_constants.push((name.into(), rhai_expr.into()));
    }

    /// Register a named Rhai string variable from a Rust string value.
    ///
    /// The value is JSON-escaped and quoted, then prepended as `let NAME = "...";`
    /// to every Rhai script. Use this to inject dynamic Rust content (e.g.,
    /// `include_str!()` content, serialized data) without Rust `format!()`
    /// double-brace escaping.
    ///
    /// # Example
    /// ```ignore
    /// let script = include_str!("my_script.py");
    /// ctx.rhai_var("SCRIPT_CONTENT", script);
    /// // Scripts can reference SCRIPT_CONTENT as a Rhai string
    /// ```
    pub fn rhai_var(&mut self, name: impl Into<String>, value: impl Into<String>) {
        let json_str = serde_json::to_string(&value.into()).unwrap();
        self.rhai_variables.push((name.into(), json_str));
    }

    /// Stage a local file for upload to the artifact store during deployment.
    ///
    /// The file is read from `local_path` at deploy time (when `--deploy` is
    /// passed), uploaded to the engine's artifact store at `storage_path`,
    /// and then referenced in the scenario via `JobInput::storage_path()`.
    ///
    /// # Example
    /// ```ignore
    /// ctx.stage_file("scripts/fit_gp.py", "./python/fit_gp.py");
    /// // Reference in a job input:
    /// JobInput::storage_path("fit_gp.py", r#""scripts/fit_gp.py""#)
    /// ```
    pub fn stage_file(
        &mut self,
        storage_path: impl Into<String>,
        local_path: impl Into<std::path::PathBuf>,
    ) -> String {
        let sp = storage_path.into();
        self.staged_files.push(StagedFile {
            storage_path: sp.clone(),
            local_path: local_path.into(),
        });
        sp
    }

    /// Generate the Rhai preamble (constant/variable definitions) for script prepending.
    pub(crate) fn rhai_preamble(&self) -> String {
        let mut lines = Vec::with_capacity(self.rhai_constants.len() + self.rhai_variables.len());
        for (name, expr) in &self.rhai_constants {
            lines.push(format!("let {} = {};", name, expr));
        }
        for (name, json_str) in &self.rhai_variables {
            lines.push(format!("let {} = {};", name, json_str));
        }
        lines.join("\n")
    }

    /// Generate a unique step instance ID for reusability.
    ///
    /// Each call with the same step_id returns an incrementing ID:
    /// - First call: "allocate_1"
    /// - Second call: "allocate_2"
    /// - etc.
    ///
    /// This is used by the `#[step]` macro to prevent naming collisions
    /// when the same step function is called multiple times.
    pub fn next_step_id(&mut self, step_id: &str) -> String {
        let counter = self.step_counters.entry(step_id.to_string()).or_insert(0);
        *counter += 1;
        format!("{}_{}", step_id, counter)
    }

    /// Set the scenario description.
    pub fn description(mut self, desc: impl Into<String>) -> Self {
        self.description = Some(desc.into());
        self
    }

    /// Register a token type's schema (called automatically).
    pub(crate) fn register_schema<T: Token>(&mut self) {
        let type_name = T::type_name().to_string();
        self.definitions
            .entry(type_name)
            .or_insert_with(T::extract_schema);
    }

    /// Get the current group ID (top of scope stack), if any.
    pub(crate) fn current_group(&self) -> Option<String> {
        self.scope_stack.last().cloned()
    }

    /// Get the current ID prefix path (e.g., "transcode_1/validation").
    fn current_prefix(&self) -> Option<String> {
        if self.id_prefix_stack.is_empty() {
            None
        } else {
            Some(self.id_prefix_stack.join("/"))
        }
    }

    /// Prefix an ID with the current component path.
    ///
    /// When inside a component, IDs are prefixed to avoid collisions.
    /// For example, "processing" becomes "transcode_1/processing".
    pub(crate) fn prefixed_id(&self, id: &str) -> String {
        match self.current_prefix() {
            Some(prefix) => format!("{}/{}", prefix, id),
            None => id.to_string(),
        }
    }

    /// Create a scoped group for visual organization.
    ///
    /// All places and transitions created inside the closure will be
    /// tagged with this group. Groups are metadata for visualization
    /// and don't affect execution.
    ///
    /// # Example
    /// ```ignore
    /// ctx.scope("Worker Pool", |ctx| {
    ///     let processing = ctx.state::<Job>("processing", "Processing");
    ///     ctx.transition("pick", "Pick Job")
    ///         .auto_input("job", &jobs)
    ///         .auto_output("picked", &processing)
    ///         .logic(r#"#{ picked: job }"#);
    /// });
    /// ```
    pub fn scope<F, R>(&mut self, name: impl Into<String>, f: F) -> R
    where
        F: FnOnce(&mut Context) -> R,
    {
        // Generate unique group ID
        self.group_counter += 1;
        let group_id = format!("group_{}", self.group_counter);

        // Register the group
        self.groups.push(ScenarioGroup {
            id: group_id.clone(),
            name: name.into(),
            parent_id: self.current_group(),
            metadata: None,
        });

        // Push onto scope stack
        self.scope_stack.push(group_id);

        // Execute the closure
        let result = f(self);

        // Pop from scope stack
        self.scope_stack.pop();

        result
    }

    /// Create a scoped group with metadata.
    ///
    /// Like `scope()` but allows attaching metadata to the group.
    ///
    /// # Example
    /// ```ignore
    /// ctx.scope_with_metadata("Nomad Task", json!({"image": "ffmpeg:latest"}), |ctx| {
    ///     // ...
    /// });
    /// ```
    pub fn scope_with_metadata<F, R>(
        &mut self,
        name: impl Into<String>,
        metadata: serde_json::Value,
        f: F,
    ) -> R
    where
        F: FnOnce(&mut Context) -> R,
    {
        self.group_counter += 1;
        let group_id = format!("group_{}", self.group_counter);

        self.groups.push(ScenarioGroup {
            id: group_id.clone(),
            name: name.into(),
            parent_id: self.current_group(),
            metadata: Some(metadata),
        });

        self.scope_stack.push(group_id);
        let result = f(self);
        self.scope_stack.pop();

        result
    }

    /// Instantiate a reusable component within a new scope.
    ///
    /// This method:
    /// 1. Generates a unique instance ID (e.g., "transcode_1")
    /// 2. Pushes the instance ID onto the prefix stack
    /// 3. Creates a visual group for the component
    /// 4. Calls the component's `instantiate()` method
    /// 5. Returns the component's output handles
    ///
    /// All places and transitions created inside the component will have
    /// prefixed IDs (e.g., "transcode_1/outbox", "transcode_1/success").
    ///
    /// # Example
    /// ```ignore
    /// let transcode = ctx.use_component(
    ///     AsyncWorker::new("Transcode", "ffmpeg:latest"),
    ///     job_queue
    /// );
    ///
    /// // Chain components together
    /// let notify = ctx.use_component(
    ///     AsyncWorker::new("Notify", "smtp:latest"),
    ///     transcode.success
    /// );
    /// ```
    pub fn use_component<C>(&mut self, component: C, input: C::Input) -> C::Output
    where
        C: Component,
    {
        let name = component.name();

        // Generate unique instance ID based on component name
        self.component_counter += 1;
        let instance_id = format!(
            "{}_{}",
            name.to_lowercase().replace(' ', "_"),
            self.component_counter
        );

        // 1. Push ID prefix (for collision-free internal IDs)
        self.id_prefix_stack.push(instance_id);

        // 2. Enter visual scope (for group box) and expand component
        let result = self.scope(&name, |ctx| component.instantiate(ctx, input));

        // 3. Pop ID prefix
        self.id_prefix_stack.pop();

        result
    }

    /// Create a scoped group with ID prefixing for collision avoidance.
    ///
    /// Combines [`scope()`](Self::scope) (visual grouping) with ID prefixing.
    /// All places and transitions created inside the closure will have IDs
    /// prefixed with `{prefix}/` and be tagged with the visual group.
    ///
    /// Use this when calling shared topology-building functions (like
    /// [`executor_lifecycle()`](crate::components::executor_lifecycle::executor_lifecycle))
    /// multiple times in the same context — the prefix ensures no ID collisions.
    ///
    /// # Example
    /// ```ignore
    /// let handles = ctx.scoped_prefix("step_1", "Step 1", |ctx| {
    ///     executor_lifecycle(ctx, bridges)
    ///     // Creates "step_1/submitted", "step_1/completed", etc.
    /// });
    /// ```
    pub fn scoped_prefix<F, R>(
        &mut self,
        prefix: impl Into<String>,
        label: impl Into<String>,
        f: F,
    ) -> R
    where
        F: FnOnce(&mut Context) -> R,
    {
        self.id_prefix_stack.push(prefix.into());
        let result = self.scope(label, f);
        self.id_prefix_stack.pop();
        result
    }

    /// Create a State place.
    ///
    /// State places represent steps in a process (e.g., "Order Placed", "In Progress").
    pub fn state<T: Token>(
        &mut self,
        id: impl Into<String>,
        name: impl Into<String>,
    ) -> PlaceHandle<T> {
        self.create_place::<T>(id, name, "state")
    }

    /// Create a Terminal place.
    ///
    /// Terminal places are sinks — tokens here signal net completion.
    /// When all tokens reach terminal places and no transitions are enabled,
    /// the engine emits a `NetCompleted` event.
    pub fn terminal<T: Token>(
        &mut self,
        id: impl Into<String>,
        name: impl Into<String>,
    ) -> PlaceHandle<T> {
        self.create_place::<T>(id, name, "terminal")
    }

    /// Create a Signal place.
    ///
    /// Signal places receive external triggers (e.g., "User Approval", "Timer Fired").
    pub fn signal<T: Token>(
        &mut self,
        id: impl Into<String>,
        name: impl Into<String>,
    ) -> PlaceHandle<T> {
        self.create_place::<T>(id, name, "signal")
    }

    /// Define a resource state machine.
    ///
    /// Resources are external entities (workers, jobs, GPUs) that can be in different states.
    /// Each resource type defines its own state machine. The scenario defines transitions
    /// between states. Adapters react to state changes.
    ///
    /// # Example
    /// ```ignore
    /// let sig = ctx.signal::<Worker>("sig_worker", "Worker Signal");
    /// let workers = ctx.resource_def::<Worker>("workers")
    ///     .state("available", |s| s.signal())          // External injects here
    ///     .state("leased", |s| s)                      // Leased by workflow
    ///     .on_signal(&sig)                             // Where to route signals
    ///     .build();
    ///
    /// // Use the states in transitions
    /// ctx.transition("claim_worker")
    ///     .auto_input("worker", workers.state("available"))
    ///     .auto_output("claimed", workers.state("leased"))
    ///     .logic(r#"#{ claimed: worker }"#);
    /// ```
    pub fn resource_def<T: Token>(
        &mut self,
        resource_type: impl Into<String>,
    ) -> ResourceBuilder<'_, T> {
        ResourceBuilder::new(self, resource_type)
    }

    /// Internal helper to create a resource state place.
    pub(crate) fn create_resource_state_place<T: Token>(
        &mut self,
        id: &str,
        name: &str,
        place_type: &str,
    ) -> PlaceHandle<T> {
        self.register_schema::<T>();
        let prefixed = self.prefixed_id(id);

        self.places.push(ScenarioPlace {
            id: prefixed.clone(),
            name: name.to_string(),
            place_type: place_type.to_string(),
            group_id: self.current_group(),
            capacity: None,
            initial_tokens: vec![],
            token_schema: Some(T::schema_ref()),
            bridge_out: None,
            bridge_reply: false,
            bridge_reply_channel: None,
            bridge_in: None,
        });
        PlaceHandle::new(prefixed)
    }

    /// Internal helper to create a place.
    fn create_place<T: Token>(
        &mut self,
        id: impl Into<String>,
        name: impl Into<String>,
        place_type: &str,
    ) -> PlaceHandle<T> {
        self.register_schema::<T>();
        let raw_id = id.into();
        let prefixed = self.prefixed_id(&raw_id);
        self.places.push(ScenarioPlace {
            id: prefixed.clone(),
            name: name.into(),
            place_type: place_type.into(),
            group_id: self.current_group(),
            capacity: None,
            initial_tokens: vec![],
            token_schema: Some(T::schema_ref()),
            bridge_out: None,
            bridge_reply: false,
            bridge_reply_channel: None,
            bridge_in: None,
        });
        PlaceHandle::new(prefixed)
    }

    /// Create a Bridge-In place.
    ///
    /// Bridge-in places receive tokens from another net's bridge-out.
    pub fn bridge_in<T: Token>(
        &mut self,
        id: impl Into<String>,
        name: impl Into<String>,
    ) -> PlaceHandle<T> {
        self.create_place::<T>(id, name, "bridge_in")
    }

    /// Create a Bridge-In place with source annotation.
    ///
    /// Like `bridge_in()`, but also declares which remote net sends tokens here.
    /// This is metadata for visualization (phantom nodes in the Lab UI) and
    /// does not affect execution.
    pub fn bridge_in_from<T: Token>(
        &mut self,
        id: impl Into<String>,
        name: impl Into<String>,
        source_net_id: impl Into<String>,
        source_place_name: impl Into<String>,
    ) -> PlaceHandle<T> {
        self.register_schema::<T>();
        let raw_id = id.into();
        let prefixed = self.prefixed_id(&raw_id);
        self.places.push(ScenarioPlace {
            id: prefixed.clone(),
            name: name.into(),
            place_type: "bridge_in".into(),
            group_id: self.current_group(),
            capacity: None,
            initial_tokens: vec![],
            token_schema: Some(T::schema_ref()),
            bridge_out: None,
            bridge_reply: false,
            bridge_reply_channel: None,
            bridge_in: Some(BridgeSourceDto {
                source_net_id: source_net_id.into(),
                source_place_name: source_place_name.into(),
            }),
        });
        PlaceHandle::new(prefixed)
    }

    /// Create a Bridge-Out place.
    ///
    /// Tokens deposited here are forwarded to `target_place_name` in `target_net_id`.
    pub fn bridge_out<T: Token>(
        &mut self,
        id: impl Into<String>,
        name: impl Into<String>,
        target_net_id: impl Into<String>,
        target_place_name: impl Into<String>,
    ) -> PlaceHandle<T> {
        self.register_schema::<T>();
        let raw_id = id.into();
        let prefixed = self.prefixed_id(&raw_id);
        self.places.push(ScenarioPlace {
            id: prefixed.clone(),
            name: name.into(),
            place_type: "bridge_out".into(),
            group_id: self.current_group(),
            capacity: None,
            initial_tokens: vec![],
            token_schema: Some(T::schema_ref()),
            bridge_out: Some(BridgeTargetDto {
                target_net_id: target_net_id.into(),
                target_place_name: target_place_name.into(),
                reply_to: None,
                reply_channels: None,
                label: None,
            }),
            bridge_reply: false,
            bridge_reply_channel: None,
            bridge_in: None,
        });
        PlaceHandle::new(prefixed)
    }

    /// Create a Bridge-Out-Reply place.
    ///
    /// Like `bridge_out`, but also specifies a `reply_to` place in this net
    /// where the remote net should send its reply.
    pub fn bridge_out_reply<T: Token>(
        &mut self,
        id: impl Into<String>,
        name: impl Into<String>,
        target_net_id: impl Into<String>,
        target_place_name: impl Into<String>,
        reply_to: impl Into<String>,
    ) -> PlaceHandle<T> {
        self.register_schema::<T>();
        let raw_id = id.into();
        let prefixed = self.prefixed_id(&raw_id);
        self.places.push(ScenarioPlace {
            id: prefixed.clone(),
            name: name.into(),
            place_type: "bridge_out".into(),
            group_id: self.current_group(),
            capacity: None,
            initial_tokens: vec![],
            token_schema: Some(T::schema_ref()),
            bridge_out: Some(BridgeTargetDto {
                target_net_id: target_net_id.into(),
                target_place_name: target_place_name.into(),
                reply_to: Some(reply_to.into()),
                reply_channels: None,
                label: None,
            }),
            bridge_reply: false,
            bridge_reply_channel: None,
            bridge_in: None,
        });
        PlaceHandle::new(prefixed)
    }

    /// Create a Bridge-Out place with parametric target resolution.
    ///
    /// Like `bridge_out()`, but target values reference net parameters via `$params.`.
    /// At runtime, the engine resolves `$params.key` from the net's parameters.
    ///
    /// This is designed for child nets spawned with parameters — the child's
    /// reply bridge targets the parent net dynamically.
    ///
    /// # Example
    /// ```ignore
    /// // In a child net definition:
    /// // Parameters will include { parent_net_id: "...", reply_place: "..." }
    /// let reply_out = ctx.bridge_out_param::<Result>(
    ///     "reply_out",
    ///     "Reply to Parent",
    ///     "parent_net_id",    // resolves from $params.parent_net_id
    ///     "reply_place",      // resolves from $params.reply_place
    /// );
    /// ```
    pub fn bridge_out_param<T: Token>(
        &mut self,
        id: impl Into<String>,
        name: impl Into<String>,
        target_net_id_param: impl Into<String>,
        target_place_param: impl Into<String>,
    ) -> PlaceHandle<T> {
        let net_ref = format!("$params.{}", target_net_id_param.into());
        let place_ref = format!("$params.{}", target_place_param.into());
        self.bridge_out::<T>(id, name, net_ref, place_ref)
    }

    /// Create a Bridge-Out place with a display label for UI grouping.
    ///
    /// Like `bridge_out()`, but adds a `label` field used by the UI for
    /// RemoteNetNode grouping instead of the raw `target_net_id`.
    /// Useful when `target_net_id` is a dynamic reference like `$result.child_net_id`.
    pub fn bridge_out_labeled<T: Token>(
        &mut self,
        id: impl Into<String>,
        name: impl Into<String>,
        target_net_id: impl Into<String>,
        target_place_name: impl Into<String>,
        reply_to: Option<String>,
        label: impl Into<String>,
    ) -> PlaceHandle<T> {
        self.register_schema::<T>();
        let raw_id = id.into();
        let prefixed = self.prefixed_id(&raw_id);
        self.places.push(ScenarioPlace {
            id: prefixed.clone(),
            name: name.into(),
            place_type: "bridge_out".into(),
            group_id: self.current_group(),
            capacity: None,
            initial_tokens: vec![],
            token_schema: Some(T::schema_ref()),
            bridge_out: Some(BridgeTargetDto {
                target_net_id: target_net_id.into(),
                target_place_name: target_place_name.into(),
                reply_to,
                reply_channels: None,
                label: Some(label.into()),
            }),
            bridge_reply: false,
            bridge_reply_channel: None,
            bridge_in: None,
        });
        PlaceHandle::new(prefixed)
    }

    /// Create a Bridge-Reply place.
    ///
    /// This place receives reply tokens from a remote net's bridge-out-reply.
    pub fn bridge_reply<T: Token>(
        &mut self,
        id: impl Into<String>,
        name: impl Into<String>,
    ) -> PlaceHandle<T> {
        self.register_schema::<T>();
        let raw_id = id.into();
        let prefixed = self.prefixed_id(&raw_id);
        self.places.push(ScenarioPlace {
            id: prefixed.clone(),
            name: name.into(),
            place_type: "state".into(),
            group_id: self.current_group(),
            capacity: None,
            initial_tokens: vec![],
            token_schema: Some(T::schema_ref()),
            bridge_out: None,
            bridge_reply: true,
            bridge_reply_channel: None,
            bridge_in: None,
        });
        PlaceHandle::new(prefixed)
    }

    /// Create a Bridge-Reply place that reads from a named reply channel.
    ///
    /// When a transition produces to this place, it looks up `channel` in the
    /// consumed token's `reply_routing.reply_channels` map to determine the reply
    /// address. Use this with `bridge_out_reply_channels` on the sender side.
    pub fn bridge_reply_channel<T: Token>(
        &mut self,
        id: impl Into<String>,
        name: impl Into<String>,
        channel: impl Into<String>,
    ) -> PlaceHandle<T> {
        self.register_schema::<T>();
        let raw_id = id.into();
        let prefixed = self.prefixed_id(&raw_id);
        self.places.push(ScenarioPlace {
            id: prefixed.clone(),
            name: name.into(),
            place_type: "state".into(),
            group_id: self.current_group(),
            capacity: None,
            initial_tokens: vec![],
            token_schema: Some(T::schema_ref()),
            bridge_out: None,
            bridge_reply: true,
            bridge_reply_channel: Some(channel.into()),
            bridge_in: None,
        });
        PlaceHandle::new(prefixed)
    }

    /// Create a Bridge-Out place with named reply channels.
    ///
    /// Like `bridge_out_reply`, but embeds multiple named reply addresses in
    /// the outgoing token's bridge metadata. The remote net's `bridge_reply_channel`
    /// places read their channel by name.
    ///
    /// `channels` is a slice of `(channel_name, local_place_name)` pairs.
    pub fn bridge_out_reply_channels<T: Token>(
        &mut self,
        id: impl Into<String>,
        name: impl Into<String>,
        target_net_id: impl Into<String>,
        target_place_name: impl Into<String>,
        channels: &[(&str, &str)],
    ) -> PlaceHandle<T> {
        self.register_schema::<T>();
        let raw_id = id.into();
        let prefixed = self.prefixed_id(&raw_id);
        let channel_map: std::collections::HashMap<String, String> = channels
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        self.places.push(ScenarioPlace {
            id: prefixed.clone(),
            name: name.into(),
            place_type: "bridge_out".into(),
            group_id: self.current_group(),
            capacity: None,
            initial_tokens: vec![],
            token_schema: Some(T::schema_ref()),
            bridge_out: Some(BridgeTargetDto {
                target_net_id: target_net_id.into(),
                target_place_name: target_place_name.into(),
                reply_to: None,
                reply_channels: Some(channel_map),
                label: None,
            }),
            bridge_reply: false,
            bridge_reply_channel: None,
            bridge_in: None,
        });
        PlaceHandle::new(prefixed)
    }

    // ── Typed bridge address convenience methods ─────────────────────────

    /// Create a Bridge-Out place using a typed address.
    ///
    /// Like [`bridge_out`](Self::bridge_out), but accepts a [`BridgeTarget`](crate::bridge::BridgeTarget)
    /// instead of separate string arguments.
    ///
    /// ```ignore
    /// const JOB_QUEUE: BridgeAddress = BridgeAddress::new("job-net", "job_queue");
    /// let to_jobs = ctx.bridge_out_to::<Job>("to_jobs", "To Jobs", &JOB_QUEUE);
    /// ```
    pub fn bridge_out_to<T: Token>(
        &mut self,
        id: impl Into<String>,
        name: impl Into<String>,
        target: impl Into<crate::bridge::BridgeTarget>,
    ) -> PlaceHandle<T> {
        let t = target.into();
        self.bridge_out::<T>(id, name, t.net_id, t.place_name)
    }

    /// Create a Bridge-In place with a typed source annotation.
    ///
    /// Like [`bridge_in_from`](Self::bridge_in_from), but accepts a [`BridgeSource`](crate::bridge::BridgeSource).
    pub fn bridge_in_at<T: Token>(
        &mut self,
        id: impl Into<String>,
        name: impl Into<String>,
        source: impl Into<crate::bridge::BridgeSource>,
    ) -> PlaceHandle<T> {
        let s = source.into();
        self.bridge_in_from::<T>(id, name, s.net_id, s.place_name)
    }

    /// Create a Bridge-Out-Reply place using a typed address.
    ///
    /// Like [`bridge_out_reply`](Self::bridge_out_reply), but accepts a [`BridgeTarget`](crate::bridge::BridgeTarget).
    pub fn bridge_out_reply_to<T: Token>(
        &mut self,
        id: impl Into<String>,
        name: impl Into<String>,
        target: impl Into<crate::bridge::BridgeTarget>,
        reply_to: impl Into<String>,
    ) -> PlaceHandle<T> {
        let t = target.into();
        self.bridge_out_reply::<T>(id, name, t.net_id, t.place_name, reply_to)
    }

    /// Create a Bridge-Out place with named reply channels using a typed address.
    ///
    /// Like [`bridge_out_reply_channels`](Self::bridge_out_reply_channels), but accepts a
    /// [`BridgeTarget`](crate::bridge::BridgeTarget).
    pub fn bridge_out_reply_channels_to<T: Token>(
        &mut self,
        id: impl Into<String>,
        name: impl Into<String>,
        target: impl Into<crate::bridge::BridgeTarget>,
        channels: &[(&str, &str)],
    ) -> PlaceHandle<T> {
        let t = target.into();
        self.bridge_out_reply_channels::<T>(id, name, t.net_id, t.place_name, channels)
    }

    /// Create a bidirectional bridge channel (send + receive) to a remote net.
    ///
    /// Returns `(send_place, receive_place)` where:
    /// - `send_place` is a bridge-out routing to `remote_send_place` in `remote_net`
    /// - `receive_place` is a bridge-in receiving from `remote_recv_place` in `remote_net`
    ///
    /// # Example
    /// ```ignore
    /// let (to_scheduler, from_scheduler) = ctx.bridge_channel::<Request, Response>(
    ///     "scheduler",
    ///     "scheduler-relay", // example net id — any deployed scheduler relay net
    ///     "job_inbox",
    ///     "result_outbox",
    /// );
    /// ```
    pub fn bridge_channel<TSend: Token, TRecv: Token>(
        &mut self,
        id_prefix: impl Into<String>,
        remote_net: impl Into<String>,
        remote_send_place: impl Into<String>,
        remote_recv_place: impl Into<String>,
    ) -> (PlaceHandle<TSend>, PlaceHandle<TRecv>) {
        let prefix = id_prefix.into();
        let net_id = remote_net.into();
        let send_place = remote_send_place.into();
        let recv_place = remote_recv_place.into();

        let send = self.bridge_out::<TSend>(
            format!("{}_out", prefix),
            format!("{} (Out)", prefix),
            &net_id,
            &send_place,
        );

        let recv = self.bridge_in_from::<TRecv>(
            format!("{}_in", prefix),
            format!("{} (In)", prefix),
            &net_id,
            &recv_place,
        );

        (send, recv)
    }

    /// Set capacity on a place.
    pub fn set_capacity(&mut self, place_id: &str, capacity: usize) {
        if let Some(p) = self.places.iter_mut().find(|p| p.id == place_id) {
            p.capacity = Some(capacity);
        }
    }

    /// Add initial tokens to a place.
    ///
    /// Tokens are serialized to JSON. Use this to seed the initial state.
    pub fn seed<T: Token>(&mut self, place: &PlaceHandle<T>, tokens: Vec<T>) {
        if let Some(p) = self.places.iter_mut().find(|p| p.id == place.id) {
            for token in tokens {
                if let Ok(value) = serde_json::to_value(&token) {
                    p.initial_tokens.push(ScenarioToken::Data(value));
                }
            }
        }
    }

    /// Add a single initial token to a place.
    pub fn seed_one<T: Token>(&mut self, place: &PlaceHandle<T>, token: T) {
        self.seed(place, vec![token]);
    }

    /// Wire a place to a terminal (exit point).
    ///
    /// This is a convenience method for the functional step pattern.
    /// It creates a terminal place and a pass-through transition to wire the output.
    ///
    /// # Example
    /// ```ignore
    /// let result = process(ctx, &input);
    /// ctx.wire_terminal(&result, "completed");
    /// ```
    pub fn wire_terminal<T: Token>(&mut self, source: &PlaceHandle<T>, id: impl Into<String>) {
        let id = id.into();
        let terminal_id = format!("{}_terminal", id);
        let terminal_name = format!("{} (Terminal)", id);
        let transition_id = format!("{}_to_terminal", id);
        let transition_name = format!("→ {}", id);

        // Create terminal place
        let terminal = self.terminal::<T>(&terminal_id, &terminal_name);

        // Create pass-through transition
        self.transition(&transition_id, &transition_name)
            .auto_input("input", source)
            .auto_output("output", &terminal)
            .logic(r#"#{ output: input }"#);
    }

    /// Add a durable timer transition.
    ///
    /// This creates a transition that schedules a signal to be injected
    /// after the given delay.
    pub fn auto_timer<T: Token>(
        &mut self,
        id: impl Into<String>,
        name: impl Into<String>,
        input: &PlaceHandle<impl Token>,
        delay_ms: u64,
        signal_place: &PlaceHandle<T>,
    ) {
        let id = id.into();
        let name = name.into();

        // 1. Logic to prepare timer data
        let logic = format!(
            r#"#{{ timer: #{{ delay_ms: {}, target_place_id: "{}", payload: input }} }}"#,
            delay_ms,
            signal_place.id()
        );

        // 2. Intermediate place for the effect
        let timer_data_place =
            self.state::<crate::DynamicToken>(format!("{}_data", id), format!("{} (Data)", name));

        // 3. Preparation transition
        self.transition(format!("{}_prep", id), format!("{} (Prep)", name))
            .auto_input("input", input)
            .auto_output("timer", &timer_data_place)
            .logic(logic);

        // 4. Effect transition
        self.record_service_requirement(&effects::TIMER_SCHEDULE);
        self.transition(id, name)
            .auto_input("timer", &timer_data_place)
            .effect(effects::TIMER_SCHEDULE.handler_id);
    }

    /// Create a delay that schedules a timer and returns a "scheduled" place for cancellation.
    ///
    /// Like [`auto_timer`](Self::auto_timer) but also provides a "scheduled" output place
    /// containing the timer metadata (including `timer_correlation_id`) that can be used
    /// for timer cancellation.
    ///
    /// # Returns
    /// A `PlaceHandle<DynamicToken>` for the "scheduled" place.
    ///
    /// # Example
    /// ```ignore
    /// let sig_timeout = ctx.signal::<DynamicToken>("sig_timeout", "Timeout");
    /// let scheduled = ctx.delay("sla_timer", &pending, 300_000, &sig_timeout);
    ///
    /// // Use `scheduled` for cancellation:
    /// ctx.transition("cancel_timer", "Cancel Timer")
    ///     .auto_input("timer", &scheduled)
    ///     .auto_output("cancelled", &timer_cancelled)
    ///     .timer_cancel();
    /// ```
    pub fn delay(
        &mut self,
        id_prefix: impl Into<String>,
        input_place: &PlaceHandle<impl Token>,
        delay_ms: u64,
        signal_place: &PlaceHandle<impl Token>,
    ) -> PlaceHandle<DynamicToken> {
        let prefix = id_prefix.into();

        // 1. Logic to prepare timer data
        let logic = format!(
            r#"#{{ timer: #{{ delay_ms: {}, target_place_id: "{}", payload: input }} }}"#,
            delay_ms,
            signal_place.id()
        );

        // 2. Intermediate place for the effect input
        let timer_data_place =
            self.state::<DynamicToken>(format!("{}_data", prefix), format!("{} (Data)", prefix));

        // 3. Output place for the scheduled timer metadata
        let scheduled_place = self.state::<DynamicToken>(
            format!("{}_scheduled", prefix),
            format!("{} (Scheduled)", prefix),
        );

        // 4. Preparation transition
        self.transition(format!("{}_prep", prefix), format!("{} (Prep)", prefix))
            .auto_input("input", input_place)
            .auto_output("timer", &timer_data_place)
            .logic(logic);

        // 5. Effect transition with scheduled output
        self.record_service_requirement(&effects::TIMER_SCHEDULE);
        self.transition(format!("{}_exec", prefix), format!("{} (Schedule)", prefix))
            .auto_input("timer", &timer_data_place)
            .auto_output("scheduled", &scheduled_place)
            .causes(signal_place)
            .timer_schedule();

        scheduled_place
    }

    /// Create retry and dead-letter transitions for effect errors.
    ///
    /// Effect errors have shape: `{ error, handler_id, transition_id, inputs, retryable }`.
    /// This generates two transitions:
    /// - **retry**: fires when `err.retryable == true`, extracts `err.inputs.<port_name>`
    /// - **dead-letter**: fires when `err.retryable != true`, routes full error to DLQ
    ///
    /// # Example
    /// ```ignore
    /// let errors = ctx.state::<DynamicToken>("errors", "Effect Errors");
    /// let retry_queue = ctx.state::<Job>("retry_queue", "Retry Queue");
    /// let dlq = ctx.state::<DynamicToken>("dlq", "Dead Letter");
    ///
    /// ctx.effect_error_handler(&errors, &retry_queue, &dlq, "job");
    /// ```
    pub fn effect_error_handler(
        &mut self,
        error_place: &PlaceHandle<DynamicToken>,
        retry_place: &PlaceHandle<impl Token>,
        dead_letter_place: &PlaceHandle<DynamicToken>,
        input_port_name: &str,
    ) {
        self.transition("retry_effect_err", "Retry Effect Error")
            .auto_input("err", error_place)
            .auto_output("retry", retry_place)
            .guard(r#"err.retryable == true"#)
            .logic(format!(r#"#{{ retry: err.inputs.{} }}"#, input_port_name));

        self.transition("dlq_effect_err", "Dead Letter Effect Error")
            .auto_input("err", error_place)
            .auto_output("dlq", dead_letter_place)
            .guard(r#"err.retryable != true"#)
            .logic(r#"#{ dlq: err }"#);
    }

    /// Create retry and dead-letter transitions for business failures with retry counters.
    ///
    /// Expects tokens at `failed_place` to have fields named by `retries_field` and
    /// `max_retries_field`. The retry transition increments the retry counter; the DLQ
    /// transition fires when retries are exhausted.
    ///
    /// # Example
    /// ```ignore
    /// let failed = ctx.state::<DynamicToken>("failed", "Failed");
    /// let retry_queue = ctx.state::<DynamicToken>("retry", "Retry Queue");
    /// let dlq = ctx.state::<DynamicToken>("dlq", "Dead Letter");
    ///
    /// ctx.retry_handler("job_retry", &failed, &retry_queue, &dlq, "retries", "max_retries");
    /// ```
    pub fn retry_handler(
        &mut self,
        id_prefix: impl Into<String>,
        failed_place: &PlaceHandle<impl Token>,
        retry_place: &PlaceHandle<impl Token>,
        dead_letter_place: &PlaceHandle<impl Token>,
        retries_field: &str,
        max_retries_field: &str,
    ) {
        let prefix = id_prefix.into();

        self.transition(format!("{}_retry", prefix), format!("{} (Retry)", prefix))
            .auto_input("failed", failed_place)
            .auto_output("retry", retry_place)
            .guard(format!(
                "failed.{} < failed.{}",
                retries_field, max_retries_field
            ))
            .logic(format!(
                r#"#{{ retry: failed + #{{ {}: failed.{} + 1, run: failed.run + 1 }} }}"#,
                retries_field, retries_field
            ));

        self.transition(format!("{}_dlq", prefix), format!("{} (DLQ)", prefix))
            .auto_input("failed", failed_place)
            .auto_output("dlq", dead_letter_place)
            .guard(format!(
                "failed.{} >= failed.{}",
                retries_field, max_retries_field
            ))
            .logic(r#"#{ dlq: failed }"#);
    }

    /// Create a scoped "Effect Error Recovery" group with retry + dead-letter transitions.
    ///
    /// This is a higher-level wrapper around [`effect_error_handler`](Self::effect_error_handler)
    /// that wraps the transitions in a named scope and uses a sensible default DLQ logic
    /// that extracts `job_id` and `reason` from the error.
    ///
    /// # Default behavior
    ///
    /// - **Retry**: fires when `err.retryable == true`, re-injects `err.inputs.job` to `retry_to`
    /// - **DLQ**: fires when `err.retryable != true`, extracts `{ job_id, reason }` to `dead_letter`
    ///
    /// # Example
    /// ```ignore
    /// let effect_errors = ctx.state::<EffectError>("effect_errors", "Effect Errors");
    /// let dead_letter = ctx.state::<DeadLetter>("dead_letter", "Dead Letter");
    ///
    /// ctx.effect_error_recovery(&effect_errors, &job_inbox, &dead_letter);
    /// ```
    pub fn effect_error_recovery(
        &mut self,
        errors: &PlaceHandle<impl Token>,
        retry_to: &PlaceHandle<impl Token>,
        dead_letter: &PlaceHandle<impl Token>,
    ) {
        self.effect_error_recovery_with(
            errors,
            retry_to,
            dead_letter,
            r#"#{ dead: #{ job_id: err.inputs.job.job_id, reason: err.error } }"#,
        );
    }

    /// Like [`effect_error_recovery`](Self::effect_error_recovery) but with custom DLQ logic.
    ///
    /// The `dlq_logic` Rhai script receives `err` (an `EffectError` with `.error`,
    /// `.handler_id`, `.transition_id`, `.inputs`, `.retryable` fields) and must
    /// return a map with a `dead` key.
    ///
    /// # Example
    /// ```ignore
    /// ctx.effect_error_recovery_with(
    ///     &effect_errors, &job_inbox, &dead_letter,
    ///     r#"#{ dead: #{ job_id: err.inputs.job.job_id, task_name: err.inputs.job.task_name, reason: err.error } }"#,
    /// );
    /// ```
    pub fn effect_error_recovery_with(
        &mut self,
        errors: &PlaceHandle<impl Token>,
        retry_to: &PlaceHandle<impl Token>,
        dead_letter: &PlaceHandle<impl Token>,
        dlq_logic: &str,
    ) {
        self.scope("Effect Error Recovery", |ctx| {
            // Retry if retryable AND the token has retries remaining.
            // Tokens should carry `retries` and `max_retries` fields.
            // If the token lacks these fields, fall through to the DLQ transitions below.
            ctx.transition("retry_effect_err", "Retry Effect Error")
                .auto_input("err", errors)
                .guard(
                    r#"err.retryable == true && {
                    let job = err.inputs.job;
                    let retries = if job.contains("retries") { job.retries } else { 0 };
                    let max_retries = if job.contains("max_retries") { job.max_retries } else { 3 };
                    retries < max_retries
                }"#,
                )
                .auto_output("job", retry_to)
                .logic(
                    r#"{
                    let job = err.inputs.job;
                    let retries = if job.contains("retries") { job.retries } else { 0 };
                    job.retries = retries + 1;
                    #{ job: job }
                }"#,
                );

            // DLQ: non-retryable errors OR retries exhausted
            ctx.transition("dlq_effect_err", "Dead Letter Effect Error")
                .auto_input("err", errors)
                .guard(
                    r#"err.retryable != true || {
                    let job = err.inputs.job;
                    let retries = if job.contains("retries") { job.retries } else { 0 };
                    let max_retries = if job.contains("max_retries") { job.max_retries } else { 3 };
                    retries >= max_retries
                }"#,
                )
                .auto_output("dead", dead_letter)
                .logic(dlq_logic);
        });
    }

    /// Create a cancellable timer with schedule + cancel effect transitions.
    ///
    /// Extends [`delay`](Self::delay) by also creating the cancel effect transition
    /// and all intermediate places. Returns handles to the scheduled and cancel-input
    /// places for wiring into the broader workflow.
    ///
    /// # Created elements
    ///
    /// - `{prefix}_data` place — intermediate `TimerInput`
    /// - `{prefix}_prep` transition — prepares timer request from input
    /// - `{prefix}_exec` transition — `timer_schedule` effect
    /// - `{prefix}_scheduled` place — holds `TimerScheduled` (with `timer_correlation_id`)
    /// - `{prefix}_cancel_input` place — inject `TimerCancelInput` here to cancel
    /// - `{prefix}_cancel` transition — `timer_cancel` effect
    /// - `{prefix}_cancelled` place — receives cancellation confirmation
    ///
    /// # Example
    /// ```ignore
    /// let sig_timeout = ctx.signal::<DynamicToken>("sig_timeout", "Timeout");
    /// let handles = ctx.timer_with_cancel("sla", &pending_task, 300_000, &sig_timeout, &errors);
    ///
    /// // Use handles.scheduled for correlation, handles.cancel_input to request cancellation
    /// ```
    pub fn timer_with_cancel(
        &mut self,
        id_prefix: impl Into<String>,
        input_place: &PlaceHandle<impl Token>,
        delay_ms: u64,
        signal_place: &PlaceHandle<impl Token>,
        errors: &PlaceHandle<impl Token>,
    ) -> TimerHandles {
        let prefix = id_prefix.into();

        // 1. Intermediate places
        let timer_data = self.state::<crate::effect_tokens::TimerInput>(
            format!("{}_data", prefix),
            format!("{} (Data)", prefix),
        );
        let scheduled = self.state::<crate::effect_tokens::TimerScheduled>(
            format!("{}_scheduled", prefix),
            format!("{} (Scheduled)", prefix),
        );
        let cancel_input = self.state::<crate::effect_tokens::TimerCancelInput>(
            format!("{}_cancel_input", prefix),
            format!("{} (Cancel Input)", prefix),
        );
        let cancelled = self.state::<crate::effect_tokens::TimerCancelled>(
            format!("{}_cancelled", prefix),
            format!("{} (Cancelled)", prefix),
        );

        // 2. Prep transition — builds TimerInput from the input token
        let logic = format!(
            r#"#{{ timer: #{{ delay_ms: {}, target_place_id: "{}", payload: input }} }}"#,
            delay_ms,
            signal_place.id()
        );
        self.transition(format!("{}_prep", prefix), format!("{} (Prep)", prefix))
            .auto_input("input", input_place)
            .auto_output("timer", &timer_data)
            .logic(logic);

        // 3. Schedule effect transition
        self.record_service_requirement(&effects::TIMER_SCHEDULE);
        self.transition(format!("{}_exec", prefix), format!("{} (Schedule)", prefix))
            .auto_input("timer", &timer_data)
            .auto_output("scheduled", &scheduled)
            .error_output(errors)
            .causes(signal_place)
            .timer_schedule();

        // 4. Cancel effect transition
        self.record_service_requirement(&effects::TIMER_CANCEL);
        self.transition(format!("{}_cancel", prefix), format!("{} (Cancel)", prefix))
            .auto_input("timer", &cancel_input)
            .auto_output("cancelled", &cancelled)
            .error_output(errors)
            .timer_cancel();

        TimerHandles {
            scheduled,
            cancel_input,
        }
    }

    /// Create a pair of join transitions (success + failure) that correlate
    /// bridge-in results with a pending place.
    ///
    /// This encapsulates the common "dispatch + join" pattern where a pending token
    /// is held while waiting for an async result, then consumed on arrival.
    ///
    /// # Arguments
    ///
    /// - `prefix` — ID prefix for generated transitions (`join_{prefix}`, `fail_{prefix}`)
    /// - `label` — Human-readable label prefix
    /// - `pending` — Place holding the pending token (consumed on join)
    /// - `result_in` — Bridge-in place for success results
    /// - `success_out` — Output place for joined success tokens
    /// - `success_logic` — Rhai script for success join (receives `result` + `pending`)
    /// - `failure_in` — Bridge-in place for failure results
    /// - `failure_out` — Output place for joined failure tokens
    /// - `failure_logic` — Rhai script for failure join (receives `fail` + `pending`)
    /// - `correlate_fields` — Field(s) to correlate on between result/fail and pending
    ///
    /// # Example
    /// ```ignore
    /// ctx.join_pair(
    ///     "A", "Preprocess A",
    ///     &a_pending,
    ///     &result_inbox, &a_done, r#"#{ out: result }"#,
    ///     &failure_inbox, &a_failed, r#"#{ out: fail }"#,
    ///     &["job_id"],
    /// );
    /// ```
    pub fn join_pair(
        &mut self,
        prefix: &str,
        label: &str,
        pending: &PlaceHandle<impl Token>,
        result_in: &PlaceHandle<impl Token>,
        success_out: &PlaceHandle<impl Token>,
        success_logic: &str,
        failure_in: &PlaceHandle<impl Token>,
        failure_out: &PlaceHandle<impl Token>,
        failure_logic: &str,
        correlate_fields: &[&str],
    ) {
        self.transition(format!("join_{}", prefix), format!("Join {} Result", label))
            .auto_input("result", result_in)
            .auto_input("pending", pending)
            .correlate_on("result", "pending", correlate_fields)
            .auto_output("out", success_out)
            .logic(success_logic);

        self.transition(format!("fail_{}", prefix), format!("Fail {}", label))
            .auto_input("fail", failure_in)
            .auto_input("pending", pending)
            .correlate_on("fail", "pending", correlate_fields)
            .auto_output("out", failure_out)
            .logic(failure_logic);
    }

    /// Start building a transition.
    ///
    /// Returns a `TransitionBuilder` for fluent API construction.
    pub fn transition(
        &mut self,
        id: impl Into<String>,
        name: impl Into<String>,
    ) -> TransitionBuilder<'_> {
        TransitionBuilder::new(self, id.into(), name.into())
    }

    /// Add a mock adapter with Rhai logic for engine-side simulation.
    ///
    /// Mock adapters simulate external services (e.g., APIs, schedulers).
    /// The Rhai script receives `token` (the triggering token's data) and must return:
    /// ```rhai
    /// #{ target_place: "place_id", data: { ... } }
    /// ```
    ///
    /// # Example
    /// ```ignore
    /// // Inside a component's instantiate():
    /// let reserved = ctx.state::<Reservation>("reserved", "Reserved");
    ///
    /// ctx.mock_adapter(&reserved, "Resource Scheduler", 500, r#"
    ///     // token contains the triggering token's color data
    ///     let rand = timestamp() % 100;  // Simple pseudo-random
    ///     if rand < 80 {
    ///         #{ target_place: "video_processor_1/sig_ack", data: #{ correlation_id: token.job_id } }
    ///     } else {
    ///         #{ target_place: "video_processor_1/sig_nack", data: #{ correlation_id: token.job_id, error: "Busy" } }
    ///     }
    /// "#);
    /// ```
    pub fn mock_adapter<T: Token>(
        &mut self,
        trigger_place: &PlaceHandle<T>,
        name: impl Into<String>,
        latency_ms: u64,
        logic_rhai: impl Into<String>,
    ) {
        self.mock_adapters.push(MockAdapterConfig {
            name: name.into(),
            trigger_place_id: trigger_place.id.clone(), // Already prefixed!
            latency_ms,
            logic: AdapterLogic::rhai(logic_rhai),
            check_token_exists: false,
        });
    }

    /// Add a timeout adapter for SLA patterns.
    ///
    /// Similar to `mock_adapter()` but with `check_token_exists: true`.
    /// The adapter will verify the triggering token still exists in the place
    /// before executing the logic. This enables timeout patterns where:
    /// - Token arrives in a place
    /// - After SLA latency, adapter checks if token is still there
    /// - If token moved on (was consumed by another transition), adapter does nothing
    /// - If token still there, adapter fires (e.g., emits timeout signal)
    ///
    /// # Example
    /// ```ignore
    /// // SLA timeout: if patient waits more than 10s for a doctor, emit timeout signal
    /// ctx.timeout_adapter(
    ///     &confirmed_appointments,
    ///     "SLA Timeout Monitor",
    ///     10000,  // 10 second SLA
    ///     format!(
    ///         r#"#{{ target_place: "{}", data: #{{ patient_id: token.patient_id, waited_ms: timestamp() - token_created_at, sla_ms: 10000 }} }}"#,
    ///         sig_sla_timeout.id()
    ///     ),
    /// );
    /// ```
    pub fn timeout_adapter<T: Token>(
        &mut self,
        trigger_place: &PlaceHandle<T>,
        name: impl Into<String>,
        latency_ms: u64,
        logic_rhai: impl Into<String>,
    ) {
        self.mock_adapters.push(MockAdapterConfig {
            name: name.into(),
            trigger_place_id: trigger_place.id.clone(),
            latency_ms,
            logic: AdapterLogic::rhai(logic_rhai),
            check_token_exists: true,
        });
    }

    /// Add a mock adapter with a raw place ID (for advanced use cases).
    ///
    /// Unlike `mock_adapter()`, this method takes a string ID directly.
    /// Use this when you need to reference places by ID without a handle.
    /// The ID will be prefixed with the current component scope.
    pub fn mock_adapter_raw(
        &mut self,
        trigger_place_id: impl Into<String>,
        name: impl Into<String>,
        latency_ms: u64,
        logic_rhai: impl Into<String>,
    ) {
        let prefixed = self.prefixed_id(&trigger_place_id.into());
        self.mock_adapters.push(MockAdapterConfig {
            name: name.into(),
            trigger_place_id: prefixed,
            latency_ms,
            logic: AdapterLogic::rhai(logic_rhai),
            check_token_exists: false,
        });
    }

    /// Spawn a child net as a dynamic sub-workflow.
    ///
    /// Builds a child scenario from the provided builder closure, serializes it
    /// into the spawn effect's `effect_config`, and creates all parent-side
    /// plumbing inside a visual group:
    ///
    /// - `{name}_request` — state place: wire your prepare transition output here.
    ///   The token must contain `{ initial_token, target_place }` and optionally
    ///   `child_net_id` and `parameters`.
    /// - `{name}_reply` — bridge_in place: receives the child's reply token.
    /// - `{name}_failure` — bridge_in place: receives the child's failure token.
    /// - `{name}_spawned` — state place: spawn confirmation.
    /// - `{name}_do_spawn` — effect transition consuming from request, producing to spawned.
    ///
    /// The child builder receives a [`SpawnChildIO`] with pre-created I/O places:
    /// - `io.inbox` — bridge_in: receives the initial token from the parent.
    /// - `io.reply` — bridge_reply: output here auto-routes back to the parent's
    ///   reply place via `ReplyRouting` correlation (no `$params` needed).
    /// - `io.failure` — bridge_out_param: routes to parent's failure place via
    ///   `$params.parent_net_id` / `$params.failure_place`.
    ///
    /// # Example
    /// ```ignore
    /// let ocr = ctx.spawn::<DynamicToken>("ocr", |child, io| {
    ///     child.transition("process", "Process")
    ///         .auto_input("job", &io.inbox)
    ///         .auto_output("out", &io.reply)
    ///         .logic(r#"#{ out: #{ result: job.data } }"#);
    /// });
    ///
    /// // Wire a prepare transition to ocr.request
    /// ocr.prepare(ctx, "Prepare OCR")
    ///     .auto_input("params", &invoice_params)
    ///     .spawn_logic(r#"#{ job_id: params.id, spec: params }"#);
    /// ```
    pub fn spawn<TReply: Token>(
        &mut self,
        name: &str,
        child_builder: impl FnOnce(&mut Context, SpawnChildIO),
    ) -> SpawnHandles<TReply> {
        let child_scenario_name = format!("{}_child", name);

        // 1. Build the child scenario in a fresh context with pre-created I/O places
        let mut child_ctx = Context::new(&child_scenario_name);

        let inbox = child_ctx.bridge_in::<DynamicToken>("inbox", "Inbox");
        let reply_out = child_ctx.bridge_reply::<DynamicToken>("reply_out", "Reply");
        let fail_out = child_ctx.bridge_out_param::<DynamicToken>(
            "fail_out",
            "Fail",
            "parent_net_id",
            "failure_place",
        );

        child_builder(
            &mut child_ctx,
            SpawnChildIO {
                inbox,
                reply: reply_out,
                failure: fail_out,
            },
        );

        let child_scenario = child_ctx.build();
        let child_json =
            serde_json::to_value(&child_scenario).expect("Failed to serialize child scenario");

        // 2. Merge child definitions into parent (so schemas validate across bridge)
        for (key, schema) in child_scenario.definitions {
            self.definitions.entry(key).or_insert(schema);
        }

        // 3. Create parent-side places (no spawn group — uses real bridge topology)
        let reply_place_id = format!("{}_reply", name);
        let failure_place_id = format!("{}_failure", name);

        // Request state place (input to spawn effect)
        let request =
            self.state::<DynamicToken>(format!("{}_request", name), format!("{} (Request)", name));

        // Bridge-in places with source annotation for RemoteNetNode grouping.
        // source_net_id = child_scenario_name matches the bridge_out label below.
        let reply = self.bridge_in_from::<TReply>(
            &reply_place_id,
            format!("{} (Reply)", name),
            &child_scenario_name,
            "reply_out",
        );
        let failure = self.bridge_in_from::<DynamicToken>(
            &failure_place_id,
            format!("{} (Failure)", name),
            &child_scenario_name,
            "fail_out",
        );

        // Confirmation state place (spawn metadata)
        let spawned =
            self.state::<DynamicToken>(format!("{}_spawned", name), format!("{} (Spawned)", name));

        // Bridge-out place: forwards initial token to spawned child.
        // target_net_id uses $result.child_net_id (resolved at firing time),
        // label = child_scenario_name (for UI grouping).
        let outbox = self.bridge_out_labeled::<DynamicToken>(
            format!("{}_outbox", name),
            format!("{} (Outbox)", name),
            "$result.child_net_id",
            "inbox",
            Some(reply_place_id.clone()),
            &child_scenario_name,
        );

        // Build effect_config with scenario + parameter template
        let effect_config = serde_json::json!({
            "scenario": child_json,
            "parameters": {
                "reply_place": reply_place_id,
                "failure_place": failure_place_id,
            }
        });

        // Create the spawn effect transition with TWO output arcs:
        // - "spawned" → confirmation state place
        // - "bridge" → bridge_out place (forwards initial token to child)
        self.record_service_requirement(&effects::SPAWN_NET);
        self.transition(format!("{}_do_spawn", name), format!("{} (Spawn)", name))
            .auto_input(effects::SPAWN_NET.default_input_port, &request)
            .auto_output(effects::SPAWN_NET.default_output_port, &spawned)
            .auto_output("bridge", &outbox)
            .effect_with_config(effects::SPAWN_NET.handler_id, effect_config);

        SpawnHandles {
            reply,
            failure,
            request,
            spawned,
            outbox,
            name: name.to_string(),
            _marker: PhantomData,
        }
    }

    /// Collect service requirements into a sorted Vec for deterministic output.
    fn build_requirements(
        requirements: &HashMap<ServiceCategory, HashSet<String>>,
    ) -> Vec<ServiceRequirement> {
        let mut reqs: Vec<ServiceRequirement> = requirements
            .iter()
            .map(|(category, handler_ids)| {
                let mut ids: Vec<String> = handler_ids.iter().cloned().collect();
                ids.sort();
                ServiceRequirement {
                    category: category.clone(),
                    handler_ids: ids,
                }
            })
            .collect();
        reqs.sort_by(|a, b| a.category.as_str().cmp(b.category.as_str()));
        reqs
    }

    /// Build the final ScenarioDefinition with embedded schemas.
    pub fn build(self) -> ScenarioDefinition {
        let requirements = Self::build_requirements(&self.service_requirements);
        ScenarioDefinition {
            name: self.name,
            description: self.description,
            places: self.places,
            transitions: self.transitions,
            groups: self.groups,
            mock_adapters: self.mock_adapters,
            definitions: self.definitions,
            requirements,
        }
    }

    /// Build and serialize to pretty JSON.
    pub fn to_json(&self) -> String {
        let requirements = Self::build_requirements(&self.service_requirements);
        let scenario = ScenarioDefinition {
            name: self.name.clone(),
            description: self.description.clone(),
            places: self.places.clone(),
            transitions: self.transitions.clone(),
            groups: self.groups.clone(),
            mock_adapters: self.mock_adapters.clone(),
            definitions: self.definitions.clone(),
            requirements,
        };
        serde_json::to_string_pretty(&scenario).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::token;

    #[token]
    struct TestToken {
        id: String,
    }

    #[test]
    fn test_scope_creates_group() {
        let mut ctx = Context::new("test");

        ctx.scope("My Group", |_ctx| {
            // Empty scope
        });

        assert_eq!(ctx.groups.len(), 1);
        assert_eq!(ctx.groups[0].name, "My Group");
        assert!(ctx.groups[0].parent_id.is_none());
    }

    #[test]
    fn test_nested_scopes_create_hierarchy() {
        let mut ctx = Context::new("test");

        ctx.scope("Outer", |ctx| {
            ctx.scope("Inner", |_ctx| {
                // Nested scope
            });
        });

        assert_eq!(ctx.groups.len(), 2);

        let outer = &ctx.groups[0];
        let inner = &ctx.groups[1];

        assert_eq!(outer.name, "Outer");
        assert!(outer.parent_id.is_none());

        assert_eq!(inner.name, "Inner");
        assert_eq!(inner.parent_id, Some(outer.id.clone()));
    }

    #[test]
    fn test_places_inside_scope_get_group_id() {
        let mut ctx = Context::new("test");

        // Place outside scope
        let _outside = ctx.state::<TestToken>("outside", "Outside Place");

        ctx.scope("My Group", |ctx| {
            let _inside = ctx.state::<TestToken>("inside", "Inside Place");
        });

        let outside_place = ctx.places.iter().find(|p| p.id == "outside").unwrap();
        let inside_place = ctx.places.iter().find(|p| p.id == "inside").unwrap();

        assert!(outside_place.group_id.is_none());
        assert!(inside_place.group_id.is_some());
        assert_eq!(inside_place.group_id, Some("group_1".to_string()));
    }

    #[test]
    fn test_transitions_inside_scope_get_group_id() {
        let mut ctx = Context::new("test");
        let input = ctx.state::<TestToken>("input", "Input");
        let output = ctx.state::<TestToken>("output", "Output");

        // Transition outside scope
        ctx.transition("outside_t", "Outside Transition")
            .auto_input("inp", &input)
            .auto_output("out", &output)
            .logic(r#"#{ out: inp }"#);

        ctx.scope("My Group", |ctx| {
            ctx.transition("inside_t", "Inside Transition")
                .auto_input("inp", &input)
                .auto_output("out", &output)
                .logic(r#"#{ out: inp }"#);
        });

        let outside_t = ctx
            .transitions
            .iter()
            .find(|t| t.id == "outside_t")
            .unwrap();
        let inside_t = ctx.transitions.iter().find(|t| t.id == "inside_t").unwrap();

        assert!(outside_t.group_id.is_none());
        assert!(inside_t.group_id.is_some());
        assert_eq!(inside_t.group_id, Some("group_1".to_string()));
    }

    #[test]
    fn test_scope_with_metadata() {
        let mut ctx = Context::new("test");

        ctx.scope_with_metadata(
            "Nomad Task",
            serde_json::json!({"image": "ffmpeg:latest", "cpu": 1000}),
            |_ctx| {},
        );

        assert_eq!(ctx.groups.len(), 1);
        assert_eq!(ctx.groups[0].name, "Nomad Task");

        let metadata = ctx.groups[0].metadata.as_ref().unwrap();
        assert_eq!(metadata["image"], "ffmpeg:latest");
        assert_eq!(metadata["cpu"], 1000);
    }

    #[test]
    fn test_deeply_nested_scopes() {
        let mut ctx = Context::new("test");

        ctx.scope("Level 1", |ctx| {
            ctx.scope("Level 2", |ctx| {
                ctx.scope("Level 3", |_ctx| {});
            });
        });

        assert_eq!(ctx.groups.len(), 3);

        let level1 = &ctx.groups[0];
        let level2 = &ctx.groups[1];
        let level3 = &ctx.groups[2];

        assert!(level1.parent_id.is_none());
        assert_eq!(level2.parent_id, Some(level1.id.clone()));
        assert_eq!(level3.parent_id, Some(level2.id.clone()));
    }

    #[test]
    fn test_scope_stack_pops_correctly() {
        let mut ctx = Context::new("test");

        ctx.scope("Group A", |ctx| {
            let _inside_a = ctx.state::<TestToken>("inside_a", "Inside A");
        });

        // After scope A, we should be at root level
        let _after_a = ctx.state::<TestToken>("after_a", "After A");

        ctx.scope("Group B", |ctx| {
            let _inside_b = ctx.state::<TestToken>("inside_b", "Inside B");
        });

        let inside_a = ctx.places.iter().find(|p| p.id == "inside_a").unwrap();
        let after_a = ctx.places.iter().find(|p| p.id == "after_a").unwrap();
        let inside_b = ctx.places.iter().find(|p| p.id == "inside_b").unwrap();

        assert_eq!(inside_a.group_id, Some("group_1".to_string()));
        assert!(after_a.group_id.is_none()); // Should be at root
        assert_eq!(inside_b.group_id, Some("group_2".to_string()));
    }

    #[test]
    fn test_groups_serialized_in_json() {
        let mut ctx = Context::new("test");

        ctx.scope("Worker Pool", |ctx| {
            let _state = ctx.state::<TestToken>("processing", "Processing");
        });

        let json = ctx.to_json();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        let groups = parsed["groups"].as_array().unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0]["name"], "Worker Pool");

        let places = parsed["places"].as_array().unwrap();
        let processing = places.iter().find(|p| p["id"] == "processing").unwrap();
        assert_eq!(processing["group_id"], "group_1");
    }

    // =========================================================================
    // Component ID Prefixing Tests
    // =========================================================================

    /// Simple test component for unit tests
    struct SimpleComponent {
        name: String,
    }

    impl SimpleComponent {
        fn new(name: &str) -> Self {
            Self {
                name: name.to_string(),
            }
        }
    }

    struct SimpleOutputs {
        success: PlaceHandle<TestToken>,
    }

    impl crate::component::Component for SimpleComponent {
        type Input = PlaceHandle<TestToken>;
        type Output = SimpleOutputs;

        fn name(&self) -> String {
            self.name.clone()
        }

        fn instantiate(self, ctx: &mut Context, input: Self::Input) -> Self::Output {
            // Create internal places (should be prefixed)
            let internal = ctx.state::<TestToken>("internal", "Internal");
            let success = ctx.state::<TestToken>("success", "Success");

            // Create internal transition (should be prefixed)
            ctx.transition("process", "Process")
                .auto_input("inp", &input)
                .auto_output("out", &internal)
                .logic(r#"#{ out: inp }"#);

            ctx.transition("complete", "Complete")
                .auto_input("inp", &internal)
                .auto_output("out", &success)
                .logic(r#"#{ out: inp }"#);

            SimpleOutputs { success }
        }
    }

    #[test]
    fn test_component_id_prefixing() {
        let mut ctx = Context::new("test");
        let input = ctx.state::<TestToken>("input", "Input Queue");

        let outputs = ctx.use_component(SimpleComponent::new("Worker"), input);

        // Internal places should have prefixed IDs
        assert!(ctx.places.iter().any(|p| p.id == "worker_1/internal"));
        assert!(ctx.places.iter().any(|p| p.id == "worker_1/success"));

        // Internal transitions should have prefixed IDs
        assert!(ctx.transitions.iter().any(|t| t.id == "worker_1/process"));
        assert!(ctx.transitions.iter().any(|t| t.id == "worker_1/complete"));

        // Output handle should have prefixed ID
        assert_eq!(outputs.success.id, "worker_1/success");
    }

    #[test]
    fn test_multiple_component_instances_no_collision() {
        let mut ctx = Context::new("test");
        let input1 = ctx.state::<TestToken>("input1", "Input 1");
        let input2 = ctx.state::<TestToken>("input2", "Input 2");

        let outputs1 = ctx.use_component(SimpleComponent::new("Worker"), input1);
        let outputs2 = ctx.use_component(SimpleComponent::new("Worker"), input2);

        // Each component instance has unique prefixed IDs
        assert!(ctx.places.iter().any(|p| p.id == "worker_1/success"));
        assert!(ctx.places.iter().any(|p| p.id == "worker_2/success"));

        // Output handles have different IDs
        assert_ne!(outputs1.success.id, outputs2.success.id);
        assert_eq!(outputs1.success.id, "worker_1/success");
        assert_eq!(outputs2.success.id, "worker_2/success");
    }

    #[test]
    fn test_nested_components_deep_prefix() {
        /// Wrapper component that uses SimpleComponent internally
        struct WrapperComponent {
            name: String,
        }

        impl crate::component::Component for WrapperComponent {
            type Input = PlaceHandle<TestToken>;
            type Output = SimpleOutputs;

            fn name(&self) -> String {
                self.name.clone()
            }

            fn instantiate(self, ctx: &mut Context, input: Self::Input) -> Self::Output {
                // Use nested component
                ctx.use_component(SimpleComponent::new("Inner"), input)
            }
        }

        let mut ctx = Context::new("test");
        let input = ctx.state::<TestToken>("input", "Input");

        let _outputs = ctx.use_component(
            WrapperComponent {
                name: "Outer".to_string(),
            },
            input,
        );

        // Nested component should have deeply prefixed IDs
        assert!(ctx
            .places
            .iter()
            .any(|p| p.id == "outer_1/inner_2/internal"));
        assert!(ctx.places.iter().any(|p| p.id == "outer_1/inner_2/success"));
    }

    #[test]
    fn test_component_creates_visual_group() {
        let mut ctx = Context::new("test");
        let input = ctx.state::<TestToken>("input", "Input");

        let _outputs = ctx.use_component(SimpleComponent::new("My Worker"), input);

        // Should create a visual group
        assert_eq!(ctx.groups.len(), 1);
        assert_eq!(ctx.groups[0].name, "My Worker");

        // Internal places should have the group_id
        let internal = ctx
            .places
            .iter()
            .find(|p| p.id == "my_worker_1/internal")
            .unwrap();
        assert!(internal.group_id.is_some());
        assert_eq!(internal.group_id, Some(ctx.groups[0].id.clone()));
    }

    #[test]
    fn test_places_outside_component_not_prefixed() {
        let mut ctx = Context::new("test");

        // Place before component
        let before = ctx.state::<TestToken>("before", "Before");

        let _outputs = ctx.use_component(SimpleComponent::new("Worker"), before);

        // Place after component
        let _after = ctx.state::<TestToken>("after", "After");

        // Only internal places should be prefixed
        assert!(ctx.places.iter().any(|p| p.id == "before")); // Not prefixed
        assert!(ctx.places.iter().any(|p| p.id == "after")); // Not prefixed
        assert!(ctx
            .places
            .iter()
            .any(|p| p.id == "my_worker_1/internal" || p.id == "worker_1/internal"));
    }

    #[test]
    fn test_bridge_out_param_prefixes_params() {
        let mut ctx = Context::new("test");

        let _place = ctx.bridge_out_param::<TestToken>(
            "reply_out",
            "Reply to Parent",
            "parent_net_id",
            "reply_place",
        );

        let place = ctx.places.iter().find(|p| p.id == "reply_out").unwrap();
        assert_eq!(place.place_type, "bridge_out");

        let bridge = place.bridge_out.as_ref().unwrap();
        assert_eq!(bridge.target_net_id, "$params.parent_net_id");
        assert_eq!(bridge.target_place_name, "$params.reply_place");
    }

    // =========================================================================
    // Spawn Tests
    // =========================================================================

    #[test]
    fn test_spawn_creates_places_and_transition() {
        let mut ctx = Context::new("parent");

        let handles = ctx.spawn::<TestToken>("ocr", |_child, _io| {
            // Child builder: io.inbox, io.reply, io.failure are pre-created
        });

        // Verify 5 parent places created (request, reply, failure, spawned, outbox)
        assert!(ctx.places.iter().any(|p| p.id == "ocr_request"));
        assert!(ctx.places.iter().any(|p| p.id == "ocr_reply"));
        assert!(ctx.places.iter().any(|p| p.id == "ocr_failure"));
        assert!(ctx.places.iter().any(|p| p.id == "ocr_spawned"));
        assert!(ctx.places.iter().any(|p| p.id == "ocr_outbox"));

        // Verify reply is bridge_in
        let reply_place = ctx.places.iter().find(|p| p.id == "ocr_reply").unwrap();
        assert_eq!(reply_place.place_type, "bridge_in");

        // Verify failure is bridge_in
        let fail_place = ctx.places.iter().find(|p| p.id == "ocr_failure").unwrap();
        assert_eq!(fail_place.place_type, "bridge_in");

        // Verify spawn transition created
        let spawn_t = ctx
            .transitions
            .iter()
            .find(|t| t.id == "ocr_do_spawn")
            .unwrap();
        assert_eq!(spawn_t.inputs.len(), 1);
        assert_eq!(spawn_t.inputs[0].place, "ocr_request");
        assert_eq!(spawn_t.inputs[0].port, "spawn_request");
        assert_eq!(spawn_t.outputs.len(), 2);
        // One output to the state place ("spawned"), one to the bridge_out place ("bridge")
        let spawned_arc = spawn_t
            .outputs
            .iter()
            .find(|a| a.port == "spawned")
            .unwrap();
        assert_eq!(spawned_arc.place, "ocr_spawned");
        let bridge_arc = spawn_t.outputs.iter().find(|a| a.port == "bridge").unwrap();
        assert_eq!(bridge_arc.place, "ocr_outbox");

        // Verify handles point to correct places
        assert_eq!(handles.request.id, "ocr_request");
        assert_eq!(handles.reply.id, "ocr_reply");
        assert_eq!(handles.failure.id, "ocr_failure");
        assert_eq!(handles.spawned.id, "ocr_spawned");
        assert_eq!(handles.outbox.id, "ocr_outbox");

        // Verify outbox is a bridge_out place
        let outbox_place = ctx.places.iter().find(|p| p.id == "ocr_outbox").unwrap();
        assert_eq!(outbox_place.place_type, "bridge_out");

        // Verify reply/failure bridge_in places have source annotation
        assert!(reply_place.bridge_in.is_some());
        assert!(fail_place.bridge_in.is_some());
    }

    #[test]
    fn test_spawn_effect_config_contains_scenario() {
        let mut ctx = Context::new("parent");

        ctx.spawn::<TestToken>("step", |child, io| {
            let output = child.state::<TestToken>("output", "Output");
            child
                .transition("process", "Process")
                .auto_input("inp", &io.inbox)
                .auto_output("out", &output)
                .logic(r#"#{ out: inp }"#);
        });

        let spawn_t = ctx
            .transitions
            .iter()
            .find(|t| t.id == "step_do_spawn")
            .unwrap();

        // Verify effect_config exists and contains scenario + parameters
        let config = spawn_t.effect_config.as_ref().unwrap();
        assert!(config.get("scenario").is_some());
        assert_eq!(config["parameters"]["reply_place"], "step_reply");
        assert_eq!(config["parameters"]["failure_place"], "step_failure");

        // Verify the child scenario has the expected structure
        let scenario = &config["scenario"];
        assert_eq!(scenario["name"], "step_child");
        let places = scenario["places"].as_array().unwrap();
        // inbox, reply_out, fail_out are auto-created by spawn()
        assert!(places.iter().any(|p| p["id"] == "inbox"));
        assert!(places.iter().any(|p| p["id"] == "reply_out"));
        assert!(places.iter().any(|p| p["id"] == "fail_out"));
    }

    #[test]
    fn test_spawn_merges_child_definitions() {
        let mut ctx = Context::new("parent");

        // Parent has its own token type
        let _p = ctx.state::<TestToken>("p", "P");

        ctx.spawn::<TestToken>("step", |child, _io| {
            // Child can create additional typed places
            let _extra = child.state::<TestToken>("extra", "Extra");
        });

        // Parent definitions should include TestToken (from both parent and child)
        let scenario = ctx.build();
        assert!(scenario.definitions.contains_key("TestToken"));
    }

    #[test]
    fn test_multiple_spawns_no_collision() {
        let mut ctx = Context::new("parent");

        let ocr = ctx.spawn::<TestToken>("ocr", |_child, _io| {});
        let validate = ctx.spawn::<TestToken>("validate", |_child, _io| {});

        // Different IDs
        assert_ne!(ocr.request.id, validate.request.id);
        assert_ne!(ocr.reply.id, validate.reply.id);
        assert_eq!(ocr.request.id, "ocr_request");
        assert_eq!(validate.request.id, "validate_request");

        // Both spawn transitions exist
        assert!(ctx.transitions.iter().any(|t| t.id == "ocr_do_spawn"));
        assert!(ctx.transitions.iter().any(|t| t.id == "validate_do_spawn"));
    }
}
