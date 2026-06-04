//! Compile-time error type and its editor-facing view.

#[derive(Debug, thiserror::Error)]
pub enum CompileError {
    #[error("validation error: {0}")]
    Validation(String),
    #[error("compilation error: {0}")]
    Compilation(String),

    // --- Typed-ports edge errors (Phase 2). Carry the offending edge_id (and
    //     sometimes a node_id / handle) so the editor can highlight inline.
    #[error("edge '{edge_id}' is missing a target_handle (required at publish time)")]
    MissingTargetHandle { edge_id: String },

    #[error(
        "edge '{edge_id}': source handle '{handle}' is not a declared output port on node '{node_id}'"
    )]
    UnknownSourcePort {
        edge_id: String,
        node_id: String,
        handle: String,
    },

    #[error(
        "edge '{edge_id}': target handle '{handle}' is not a declared input port on node '{node_id}'"
    )]
    UnknownTargetPort {
        edge_id: String,
        node_id: String,
        handle: String,
    },

    #[error(
        "edge '{edge_id}': source port fields {expected:?} don't match target port fields {found:?}"
    )]
    EdgeTypeMismatch {
        edge_id: String,
        expected: Vec<String>,
        found: Vec<String>,
    },

    // --- Typed-ports guard errors (Phase 3). Decision/Loop guards are Rhai
    //     expressions; we syntax-check them and resolve each
    //     `<upstream_node>.<field>` reference against the topological scope at
    //     that node. The editor consumes these via `to_view()` and highlights
    //     the offending node.
    /// Two nodes resolve to the same author-facing `slug` (the
    /// `<slug>.<field>` guard namespace must be unique within a graph). Only
    /// explicit, user-set slugs can conflict — derived defaults are
    /// collision-suffixed deterministically and never reach here.
    #[error(
        "nodes '{node_a}' and '{node_b}' both use slug '{slug}' — slugs must be unique within a graph"
    )]
    SlugConflict {
        slug: String,
        node_a: String,
        node_b: String,
    },

    #[error("guard on node '{node_id}' has a Rhai syntax error: {message}")]
    GuardSyntax { node_id: String, message: String },

    #[error(
        "guard on node '{node_id}' references unknown identifier '{identifier}' (in-scope upstream identifiers: {available:?})"
    )]
    GuardUnresolved {
        node_id: String,
        identifier: String,
        available: Vec<String>,
    },

    // --- Trigger node errors (Phase 5a). Triggers connect to a target input
    //     port via one outgoing edge and supply a payload_mapping. The
    //     compiler enforces:
    //       - Trigger has exactly one outgoing edge.
    //       - Trigger is never an edge target.
    //       - payload_mapping.target_field exists on the resolved target port.
    //       - payload_mapping.expression parses as Rhai.
    #[error("trigger '{node_id}' must have exactly one outgoing edge (found {found})")]
    TriggerEdgeCardinality { node_id: String, found: usize },

    #[error("trigger '{node_id}' cannot be the target of edge '{edge_id}'")]
    TriggerIsEdgeTarget { node_id: String, edge_id: String },

    #[error(
        "trigger '{node_id}': payload mapping references unknown target field '{field}' (available: {available:?})"
    )]
    TriggerUnknownTargetField {
        node_id: String,
        field: String,
        available: Vec<String>,
    },

    #[error(
        "trigger '{node_id}': payload mapping for field '{field}' has a Rhai syntax error: {message}"
    )]
    TriggerMappingSyntax {
        node_id: String,
        field: String,
        message: String,
    },

    /// Phase 5b: invalid cron schedule (bad cron string or unknown IANA tz).
    #[error("trigger '{node_id}': invalid cron schedule: {message}")]
    TriggerCronInvalid { node_id: String, message: String },

    /// A payload-mapping expression references a `<root>.<field>` whose root
    /// isn't a declared scope identifier for the trigger's source kind (e.g.
    /// referencing `catalogue_entry` from a cron trigger). Mirrors
    /// `GuardUnresolved`; identifier-resolution only (no kind inference).
    #[error(
        "trigger '{node_id}': payload mapping for field '{field}' references unknown identifier '{identifier}' (in-scope for this source: {available:?})"
    )]
    TriggerUnresolvedRef {
        node_id: String,
        field: String,
        identifier: String,
        available: Vec<String>,
    },

    /// The trigger has an empty `payload_mapping` but its resolved target port
    /// declares required field(s). An empty mapping forwards the source payload
    /// verbatim, which can't satisfy a typed port — fail at publish, not at
    /// first fire.
    #[error(
        "trigger '{node_id}': empty payload mapping but the target port requires field(s): {missing:?}"
    )]
    TriggerEmptyMappingRequiredFields {
        node_id: String,
        missing: Vec<String>,
    },

    // --- Sub-workflow errors (call/return composition). Resolution + cycle
    //     detection run at the parent's publish time (they need DB access to
    //     resolve the child template version chain); `Unresolved` also fires
    //     in the compiler when a `SubWorkflow` node reaches lowering with no
    //     pre-resolved child AIR. All carry the offending node_id so the
    //     editor canvas can ring it.
    #[error(
        "sub-workflow node '{node_id}' references template '{template_id}' which is not published / not found"
    )]
    SubWorkflowUnresolved {
        node_id: String,
        template_id: String,
    },

    #[error("sub-workflow cycle detected: {chain:?}")]
    SubWorkflowCycle { chain: Vec<String> },

    #[error(
        "sub-workflow node '{node_id}' references private template '{template_id}' owned by a different workflow"
    )]
    SubWorkflowPrivateOwnershipViolation {
        node_id: String,
        template_id: String,
    },

    #[error("sub-workflow nesting too deep (limit {limit}) at node '{node_id}'")]
    SubWorkflowDepthExceeded { node_id: String, limit: usize },

    /// Loop has no body — no child node has `parent_id == loop.id`. An empty
    /// Loop is a config error (an iterating-counter-with-no-work workflow is
    /// not a useful primitive; use a dedicated Delay node if/when needed).
    #[error("loop '{node_id}' has no body — add at least one node inside the loop container")]
    LoopEmpty { node_id: String },

    /// LeaseScope has no body — no child node has `parent_id == lease_scope.id`.
    /// A LeaseScope with nothing inside holds an allocation that no step runs
    /// on; that is pointless. Wire at least one node into the scope's interior.
    #[error("lease scope '{node_id}' has no body — add at least one node inside the lease-scope container")]
    LeaseScopeEmpty { node_id: String },

    /// A node INSIDE a loop body reads an upstream business field off the
    /// *control token* (`input.<field>` / `token.<field>`), but that field only
    /// rides the token on the FIRST iteration. The loop's `t_continue` rebuilds
    /// the token every pass (`#{ body: <body_out>, data: … }`) — an
    /// envelope-stripping body (any AutomatedStep) drops the field, so the read
    /// returns `undefined`/`AttributeError` on iteration 1+. The fix is always
    /// the parked-borrow form `<producer_slug>.<field>`: a non-consuming read-arc
    /// into the producer's write-once `p_<id>_data` place, which survives every
    /// iteration (this is how `lp.iteration`, `bo.observations`, etc. work). See
    /// docs/10 (control-data token model) and docs/17 (lease scope).
    #[error(
        "node '{node_label}' inside loop '{loop_label}' reads `{referenced}` off the control \
         token, but that field only rides the token on the FIRST iteration — the loop rebuilds \
         the token each pass and drops it. Use the parked-borrow form `{suggested}` instead (a \
         non-consuming read-arc that survives every iteration)."
    )]
    LoopBodyStaleControlRef {
        node_id: String,
        node_label: String,
        loop_label: String,
        /// The exact reference text the author wrote (`input.job_name`).
        referenced: String,
        /// The suggested parked-borrow replacement (`start.job_name`).
        suggested: String,
    },

    /// A node borrows `<scope>.lease.<field>` where `<field>` is not part of the
    /// typed lease the scope's resolved datacenter flavor produces. The lease is
    /// a typed core (`alloc_id`, `node`, `expiry`, `executor_namespace`) plus a
    /// per-flavor `scheduler` detail (`scheduler.flavor` + that flavor's fields);
    /// borrowing anything else would resolve to a silent runtime `null`. The
    /// classic trip is `lease.gpu_uuid` — that placeholder was removed (no
    /// allocator reports device UUIDs). Surface flavor-specific data the engine
    /// doesn't type via the cluster runtime view instead.
    #[error(
        "node '{node_label}' borrows `{referenced}`, but the lease held by lease scope \
         '{scope_label}' (flavor '{flavor}') has no such field. Available: {allowed}."
    )]
    LeaseFieldUnknown {
        node_id: String,
        node_label: String,
        scope_label: String,
        flavor: String,
        /// The exact reference text the author wrote (`gpu_lease.lease.gpu_uuid`).
        referenced: String,
        /// Human-readable list of the borrowable lease fields for this flavor.
        allowed: String,
    },

    /// Map has no body — no child node has `parent_id == map.id`. A Map with
    /// nothing to run per element is a config error; wire at least one node
    /// inside the map container.
    #[error("map '{node_id}' has no body — add at least one node inside the map container")]
    MapEmpty { node_id: String },

    /// A `<map_slug>.<field>` reference omits the required `[*]` collection
    /// boundary. A Map parks a gathered ARRAY at `p_<id>_data`; downstream
    /// borrows must iterate it as `<map_slug>[*].<field>`. A bare
    /// `<map_slug>.<field>` would address a scalar that doesn't exist.
    #[error(
        "node '{node_id}': reference '{ref_value}' borrows map producer '{map_slug}' \
         without the required `[*]` collection boundary — use `{map_slug}[*].<field>`"
    )]
    MapRefMissingStar {
        node_id: String,
        map_slug: String,
        ref_value: String,
    },

    /// A Map's `resultVar` is not a valid Rhai identifier
    /// (`[A-Za-z_][A-Za-z0-9_]*`). Required because the gather transition
    /// projects each body token's `<resultVar>` field into the result
    /// collection (`#{ value: body.<resultVar>, … }`); a malformed name
    /// would produce a Rhai syntax error deep in the emitted logic.
    #[error(
        "map '{node_id}': resultVar '{result_var}' is not a valid Rhai identifier ([A-Za-z_][A-Za-z0-9_]*)"
    )]
    MapResultVarInvalid { node_id: String, result_var: String },

    /// A Map node is nested inside another Map (its `parent_id` chain reaches
    /// a Map ancestor). v1 forbids nested map-reduce: the gather barrier's
    /// `__map_id` correlation key and the namespace-on-token item injection
    /// assume a single scatter scope, and the `<slug>[*].<field>` borrow
    /// surface only describes one level of collection. Use a SubWorkflow as
    /// the body if you need a second fan-out.
    #[error(
        "map '{node_id}' is nested inside map '{outer_id}' — nested map-reduce is not supported in v1"
    )]
    MapNested { node_id: String, outer_id: String },

    /// A `StreamFold` is missing exactly one inbound `stream` or `control`
    /// handle edge. It needs the producer's data Signal place (`stream`) and
    /// its EOS/completion token (`control`) — one of each.
    #[error("node {node_id}: stream fold is missing exactly one inbound `{handle}` handle edge")]
    StreamFoldMissingHandle {
        node_id: String,
        handle: &'static str,
    },

    /// A `StreamFold`'s `Custom` reduce expression is not a parseable Rhai
    /// expression (it is embedded verbatim into the gather transition's logic).
    #[error("node {node_id}: invalid stream-fold reduce expression `{expr}`: {detail}")]
    StreamFoldInvalidReduce {
        node_id: String,
        expr: String,
        detail: String,
    },

    /// A `streamInput` AutomatedStep (streaming reducer) is mis-wired or
    /// mis-configured: it must have exactly one inbound `stream` edge from a
    /// `streamOutput` producer's `stream` handle plus exactly one control `in`
    /// edge from that same producer, and it cannot run under a pooled/leased or
    /// scheduled deployment model (the inline executor lifecycle is the only
    /// path that plumbs the IPC chunk feed).
    #[error("node {node_id}: invalid streamInput reducer: {detail}")]
    StreamInputInvalid { node_id: String, detail: String },

    /// A Map body terminal is a node kind that cannot produce the
    /// `detail.outputs.<resultVar>` envelope the gather requires (engine-effect
    /// backends like CatalogueQuery, Scheduled AutomatedSteps whose scheduler
    /// round-trip is unverified, and pure pass-through nodes like PhaseUpdate /
    /// Decision / Join that forward the raw control token with no parked
    /// output). The Map terminal must be a parked-producer kind (inline
    /// AutomatedStep, Agent, or SubWorkflow).
    #[error(
        "map '{map_id}': body terminal '{node_id}' ({kind}) cannot be a Map body \
         terminal — it produces no `detail.outputs` envelope for the gather. Use \
         an inline AutomatedStep, Agent, or SubWorkflow as the terminal node."
    )]
    MapBodyUnsupported {
        map_id: String,
        node_id: String,
        kind: String,
    },

    /// A Map's `itemsRef` parses + resolves to a known producer field, but
    /// that field's declared shape is not an array — the scatter can only
    /// fan out over a collection. Mirrors `RepeaterItemsRefNotArray`;
    /// `Any`/`Opaque`/`Json` are accepted (deferred to runtime).
    #[error(
        "map '{node_id}': itemsRef '{ref_value}' resolves to {actual_kind}, not an array — map can only scatter a collection"
    )]
    MapItemsRefNotArray {
        node_id: String,
        ref_value: String,
        actual_kind: String,
    },

    /// A Map's `itemsRef` either doesn't parse as `<slug>.<field>…`, names a
    /// `<slug>` that isn't a parked producer in the graph, or the field path
    /// doesn't land on the producer's outbound shape. Mirrors
    /// `RepeaterRefUnresolved`.
    #[error(
        "map '{node_id}': itemsRef '{ref_value}' is unresolved (slug: '{slug}', candidates: {available:?})"
    )]
    MapItemsRefUnresolved {
        node_id: String,
        ref_value: String,
        slug: String,
        available: Vec<String>,
    },

    /// A node wired as an agent tool (target of an edge with
    /// `source_handle == "tools"`) has an incoming `WorkflowEdge` from
    /// somewhere other than the agent's tools handle. Tools are dispatched
    /// by the agent compiler via the tools-handle edge index — any other
    /// incoming edge would let the tool fire outside the agent's control
    /// loop. This catches the case where an author drags an extra sequence
    /// edge into a tool node by mistake.
    #[error(
        "node '{child_id}' is a tool of agent '{agent_id}' and must not have incoming edges \
         from anywhere except the agent's tools handle (offending edge: '{edge_id}')"
    )]
    ToolChildHasIncomingEdge {
        agent_id: String,
        child_id: String,
        edge_id: String,
    },

    // --- Python AutomatedStep output-field guards (sibling of the
    //     direct-slug-access input borrows). Declared output.fields[].name is
    //     swept from Python globals after exec() — if the name collides with a
    //     reserved runner global or an upstream slug borrowed by this node,
    //     the runtime would either silently lose the assignment or
    //     accidentally re-emit borrowed input as output. Reject at compile.
    /// Declared output field name matches a reserved runner global (`token`,
    /// `input`, `set_output`, etc — mirror of the runner.rs `_RESERVED_GLOBALS`
    /// set). Rename the field.
    #[error(
        "node '{node_id}': output field '{field_name}' shadows a reserved runner global — rename the field"
    )]
    OutputFieldShadowsReserved { node_id: String, field_name: String },

    /// Declared output field name matches a slug bound as a Python global on
    /// this node (an upstream producer the user's source borrows as
    /// `<slug>.<attr>`). Without the guard the input global would silently
    /// re-export as this step's output.
    #[error(
        "node '{node_id}': output field '{field_name}' collides with borrowed input '{upstream_slug}' from upstream node '{upstream_node_id}' — rename the output field"
    )]
    OutputFieldShadowsInput {
        node_id: String,
        field_name: String,
        upstream_slug: String,
        upstream_node_id: String,
    },

    // --- LLM / Kreuzberg upstream-producer refs (sibling of Python direct
    //     slug access and HumanTask placeholders). The `{{}}` syntax is
    //     unambiguous so unlike Python's silent-ignore semantics, an
    //     unknown slug or field is a typo — hard-reject at compile.
    /// `{{<slug>.<field>}}` references an unknown slug, or `<field>` is
    /// not declared on the producer's output port. `backend` is `"llm"` or
    /// `"kreuzberg"`; `site` names the offending config field (e.g.
    /// `"prompt"`, `"system_prompt"`, `"file"`, `"images[0].path"`).
    #[error(
        "node '{node_id}' ({backend}): {site} references unknown {kind} '{name}' in `{{{{{slug}.{field}}}}}` (available {kind}s: {available:?})"
    )]
    BackendRefUnresolved {
        node_id: String,
        backend: String,
        site: String,
        slug: String,
        field: String,
        /// `"slug"` when the head doesn't match any graph slug; `"field"`
        /// when the head is known but the attr isn't on its output port.
        kind: String,
        /// The unknown name (== `slug` when kind="slug", == `field` when
        /// kind="field"). Surfaced separately so the editor can highlight
        /// just the failing part of the path.
        name: String,
        /// Candidate names the author might have meant — slugs in the
        /// graph (kind="slug") or fields on the producer (kind="field").
        available: Vec<String>,
    },

    /// `{{<slug>.<field>}}` references a producer that lives downstream of
    /// (or at) the consumer in the graph topology — a borrow cycle. The
    /// `{{}}` syntax pre-binds the field at compile time, so circular
    /// references aren't physically realizable.
    #[error(
        "node '{node_id}' ({backend}): {site} borrows '{{{{{slug}.{field}}}}}' from producer '{producer_node_id}' which is not strictly upstream"
    )]
    BackendRefNotUpstream {
        node_id: String,
        backend: String,
        site: String,
        slug: String,
        field: String,
        producer_node_id: String,
    },

    /// Malformed `{{...}}` placeholder body — not a dotted-identifier path.
    /// Surfaces early from `validate_and_transform` so the author sees a
    /// precise syntax error instead of a downstream "unresolved input".
    #[error(
        "node '{node_id}' ({backend}): {site} contains malformed placeholder '{{{{{body}}}}}' — expected `<slug>.<field>`"
    )]
    BackendPlaceholderSyntax {
        node_id: String,
        backend: String,
        site: String,
        body: String,
    },

    /// LLM `images[i].path` references an upstream producer field whose
    /// declared kind is not `file`. Unlike Kreuzberg (which can stage text
    /// as a temp file), LLM vision needs actual image bytes.
    #[error(
        "node '{node_id}' (llm): {site} requires a file-kind upstream field; '{{{{{slug}.{field}}}}}' resolves to kind '{actual_kind}'"
    )]
    LlmImageRefNotFileKind {
        node_id: String,
        site: String,
        slug: String,
        field: String,
        actual_kind: String,
    },

    // --- Direct-mode resource validation. The compiler scans Python
    //     source for `<head>.<field>` patterns and validates each `<head>`
    //     against the workspace's resource list at publish time. The
    //     variants below are kept under their original "alias" names for
    //     the editor's error-discriminant wire format (the picker still
    //     reads them); semantically `alias` now means "resource path
    //     authored as a Python identifier".
    /// A resource whose `path` references a type that isn't registered in
    /// the `aithericon_resources` registry. Usually a database state
    /// mismatch — a resource row exists with a type the server build
    /// doesn't know about (e.g. older binary running against a newer DB).
    #[error(
        "resource '{alias}' references unknown resource type '{type_name}' — \
         expected one of the built-in types in `aithericon_resources`"
    )]
    ResourceTypeUnknown { alias: String, type_name: String },

    /// A resource path equals a step's explicit slug — `<path>.<field>`
    /// would be ambiguous between the staged resource envelope and the
    /// producer's parked envelope. Rename either the resource or the slug.
    #[error(
        "resource '{alias}' collides with a step slug of the same name — \
         rename the resource or the step"
    )]
    ResourceAliasCollidesWithSlug { alias: String },

    /// A resource path collides with a reserved control-token field
    /// (`_instance_id`, `_template_id`, …). At runtime the system field
    /// would shadow the resource binding (or vice versa) silently.
    #[error(
        "resource '{alias}' collides with a reserved control-token field — \
         pick a non-underscore-prefixed path"
    )]
    ResourceAliasCollidesWithToken { alias: String },

    /// A step explicitly declared `resource_alias: "<alias>"` (via the
    /// backend's `resource_alias_paths`), but the workspace has no
    /// resource at that path. Without this hard fail at publish time the
    /// AIR would still build — minus the resource borrow — and the SMTP /
    /// LLM / FileOps backend would crash at run time with "compiler must
    /// emit a ResourceEnvelope borrow". This variant points the operator
    /// at the right fix (create the resource at `/resources`).
    #[error(
        "node '{node_id}': resource_alias '{alias}' is not defined in this workspace — \
         create it at /resources before publishing"
    )]
    WorkspaceResourceUnknown { node_id: String, alias: String },

    /// A node declared an asset binding (`asset_bindings[].ref_key`) that does
    /// not resolve to any asset visible from the template's scope (docs/20 §5).
    /// Symmetric with [`Self::WorkspaceResourceUnknown`] — hard-fail at publish
    /// so the AIR can't build minus the staged asset. The fix is to create the
    /// asset (or correct the ref-key) before publishing.
    #[error(
        "node '{node_id}': asset binding '{ref_key}' is not defined in any scope visible to this \
         template — create it at /assets before publishing"
    )]
    AssetBindingUnknown { node_id: String, ref_key: String },

    /// An asset `ref_key` resolved ambiguously: two equally-specific scopes
    /// (e.g. two sibling projects containing this template) both define it, so
    /// picking one would be a silent guess (docs/20 §2). Same posture as
    /// `SlugConflict` — ambiguity is an error, not a guess.
    #[error(
        "node '{node_id}': asset binding '{ref_key}' is ambiguous — {detail}"
    )]
    AssetBindingAmbiguous {
        node_id: String,
        ref_key: String,
        detail: String,
    },

    /// An `Executor.capacity.alias` resolved to a resource whose dispatch backend
    /// is not a token/presence admission pool. Executor.capacity admission is
    /// in-net-pool-only: a `scheduler` backend (a `datacenter`) is a lease
    /// resource (bind it under `Scheduled`/`LeaseScope`); a `queue` backend (a
    /// worker-pool `capacity`) competes directly with no admission net; and a
    /// plain credential (`postgres`, …) is no pool at all. `backend` is the human
    /// label of the resolved [`crate::models::capacity::CapacityBackend`]
    /// (`scheduler` / `queue` / `deferred` / `non-pool`).
    #[error(
        "node '{node_id}': Executor.capacity alias '{alias}' resolves to a {backend} capacity, \
         not a token/presence pool — {}",
        if backend == "scheduler" {
            "a scheduler capacity is a lease resource; bind it under a Scheduled/LeaseScope deployment model"
        } else {
            "bind it to a capacity whose liveness is `seeded` (a concurrency limit) or `presence` (an instrument group)"
        }
    )]
    ResourcePoolNotAPool {
        node_id: String,
        alias: String,
        backend: String,
    },

    /// A `Scheduled.scheduler` (or `LeaseScope.lease`) alias resolved to a
    /// resource whose dispatch backend is not the scheduler lease. The scheduler
    /// binding is `datacenter`-only (docs/13): a token/presence `capacity` is
    /// executor-pool admission (bind it under `Executor.capacity`), and a plain
    /// credential (`postgres`, …) is no scheduler at all. `backend` is the human
    /// label of the resolved [`crate::models::capacity::CapacityBackend`]
    /// (`tokens` / `presence` / `queue` / `deferred` / `non-pool`).
    #[error(
        "node '{node_id}': scheduler alias '{alias}' resolves to a {backend} capacity, \
         not a scheduler capacity — {}",
        if backend == "tokens" || backend == "presence" || backend == "queue" {
            "this is executor-pool admission; bind it under Executor.capacity"
        } else {
            "point it at a datacenter resource"
        }
    )]
    SchedulerNotADatacenter {
        node_id: String,
        alias: String,
        backend: String,
    },

    /// A `datacenter` resource declares `scheduler_flavor = "<flavor>"` but is
    /// missing a connection field that flavor requires (slurm needs
    /// `ssh_host` + `ssh_user` + `template_dir`; nomad needs `nomad_addr`;
    /// http needs `allocator_url`). Hard-fail at publish so a half-configured
    /// cluster can't reach a fire.
    #[error(
        "datacenter resource '{alias}' (flavor '{flavor}') is missing required \
         connection field(s): {missing:?}"
    )]
    DatacenterConnectionIncomplete {
        node_id: String,
        alias: String,
        flavor: String,
        missing: Vec<String>,
    },

    /// `resourcePool.request` failed validation against the pool kind's
    /// `claim_schema`. `message` carries the first jsonschema error.
    #[error("node '{node_id}': resource pool request for '{alias}' is invalid: {message}")]
    ResourcePoolRequestInvalid {
        node_id: String,
        alias: String,
        message: String,
    },

    /// A `Scheduled`/leased step resolved to NO datacenter through the whole
    /// selection chain (`node.scheduler ?? template.default_scheduler ??
    /// workspace.default_datacenter`; see `docs/16-multi-cluster-scheduling.md`
    /// §6). Hard-fail at publish — there is no implicit env fallback for
    /// multi-cluster selection. Fires for `operation: Lease` unconditionally
    /// (a lease REQUIRES a concrete cluster); for `operation: Submit` only when
    /// the dev-bootstrap env path is also absent (so `just dev scheduler-up`'s
    /// env-global submit still resolves). Mirrors `WorkspaceResourceUnknown` /
    /// `SchedulerNotADatacenter`.
    #[error(
        "node '{node_id}': Scheduled/leased step has no datacenter — set a scheduler on \
         the node, a template default_scheduler, or a workspace default_datacenter"
    )]
    SchedulerUnresolved { node_id: String },

    /// A `Scheduled` step carries a `job_template_ref` (Phase 3, B-model) whose
    /// `(template_id, version)` couldn't be loaded from the workspace's
    /// `job_templates` / `job_template_versions` tables — unknown id,
    /// soft-deleted template, or no row at that version. Hard-fail at publish so
    /// a dangling reference can't reach lowering. `template_ref` is a
    /// human-readable `"<template_id>@v<version>"` for the diagnostic.
    #[error(
        "node '{node_id}': job template reference '{template_ref}' is unresolved — \
         no such job template/version in this workspace"
    )]
    JobTemplateUnresolved {
        node_id: String,
        template_ref: String,
    },

    /// A `Scheduled` step's referenced job template (Phase 3, B-model) has a
    /// flavor (`slurm` | `nomad` | …) that doesn't match the step's RESOLVED
    /// cluster's scheduler flavor. A Slurm template can't stage onto a Nomad
    /// datacenter and vice-versa — hard-fail at publish.
    #[error(
        "node '{node_id}': job template flavor '{template_flavor}' does not match \
         the resolved cluster flavor '{cluster_flavor}'"
    )]
    JobTemplateFlavorMismatch {
        node_id: String,
        template_flavor: String,
        cluster_flavor: String,
    },

    /// `executionSpec.config` (or a nested value) carries a
    /// `{"$ref": "#/definitions/<name>"}` that the workflow-level
    /// `definitions` map can't resolve — unknown name, cycle,
    /// unsupported pointer shape, etc. Surfaced before lowering by the
    /// `validate_schema_refs` pass so the editor can highlight the node.
    /// `path` is the JSON pointer to the offending `$ref` inside the
    /// node's `executionSpec.config`.
    #[error("node '{node_id}': schema ref at config{path}: {message}")]
    SchemaRefUnresolved {
        node_id: String,
        path: String,
        message: String,
    },

    // --- Phase 4: step placement Requirements validated against the workspace
    //     capability registry. These run at the PUBLISH compile path (where the
    //     `KnownCapabilities` map is loaded from the DB) — the pure
    //     `compile_to_air` has no DB, so they cannot live in a `validate` hook.
    /// A step Constraint names a `capability` that is not a defined
    /// `capability_type` in the workspace. Point the author at the registry.
    #[error(
        "node '{node_id}': requirement references capability '{capability}' which is not a \
         defined capability type in this workspace — define it at /capability-types"
    )]
    UndefinedRequirementCapability { node_id: String, capability: String },

    /// A step Constraint names a `field` that is not declared on the referenced
    /// (defined) capability's typed schema.
    #[error(
        "node '{node_id}': requirement references field '{field}' on capability \
         '{capability}', but that capability has no such field"
    )]
    UnknownRequirementField {
        node_id: String,
        capability: String,
        field: String,
    },

    /// A step Constraint's `op`/`value` is incompatible with the referenced
    /// field's declared [`crate::models::template::FieldKind`] — e.g. a numeric
    /// comparison (`gt`/`lt`) on a `text` field, or an `value` whose JSON type
    /// the field's kind rejects.
    #[error(
        "node '{node_id}': requirement on '{capability}.{field}' is type-incompatible: {message}"
    )]
    RequirementTypeMismatch {
        node_id: String,
        capability: String,
        field: String,
        message: String,
    },

    // --- Repeater block validation (Feature B). A HumanTask
    //     `TaskBlockConfig::Repeater` carries a structured `<slug>.<field>[*]…`
    //     reference into an upstream array. The compiler validates the ref
    //     syntax, resolution, array shape, and the Repeater's own
    //     `output_slug` at publish time.
    /// `items_ref` (or `item_label_ref`) does not parse as
    /// `<slug>.<field>[*]…` with exactly one `[*]` iteration boundary.
    /// Covers nested `[*]` (v1 rejects with `NestedIterationUnsupported`
    /// wording) and missing boundaries.
    #[error("human task '{node_id}': Repeater {site} '{ref_value}' is malformed: {message}")]
    RepeaterRefMalformed {
        node_id: String,
        site: String,
        ref_value: String,
        message: String,
    },

    /// `items_ref` parses cleanly but its `<slug>` doesn't match any
    /// graph slug, OR the pre-`[*]` path doesn't land on a field on the
    /// resolved producer's outbound shape.
    #[error(
        "human task '{node_id}': Repeater items_ref '{ref_value}' is unresolved (slug: '{slug}', candidates: {available:?})"
    )]
    RepeaterRefUnresolved {
        node_id: String,
        ref_value: String,
        slug: String,
        available: Vec<String>,
    },

    /// The pre-`[*]` path resolves to a non-array shape on the upstream
    /// producer. `[*]` only makes sense over an Array — a Scalar/Object
    /// is a hard reject. `Any`/`Opaque` are accepted (deferred to runtime).
    #[error(
        "human task '{node_id}': Repeater items_ref '{ref_value}' resolves to {actual_kind}, not an array — iteration boundary `[*]` requires an array"
    )]
    RepeaterItemsRefNotArray {
        node_id: String,
        ref_value: String,
        actual_kind: String,
    },

    /// `output_slug` is missing/empty, or not a valid Rhai identifier
    /// (`[A-Za-z_][A-Za-z0-9_]*`). Required because downstream consumers
    /// address the Repeater's typed array as `<human_task_slug>.<output_slug>`.
    #[error(
        "human task '{node_id}': Repeater output_slug '{output_slug}' is invalid — must be a non-empty Rhai identifier ([A-Za-z_][A-Za-z0-9_]*)"
    )]
    RepeaterOutputSlugInvalid {
        node_id: String,
        output_slug: String,
    },

    /// A `Repeater` block was found inside another Repeater's `blocks`.
    /// v1 forbids nested iteration — the typed array output schema can
    /// only describe one level of `[*]`, and the runtime renderer assumes
    /// a single row-iteration scope per Repeater.
    #[error(
        "human task '{node_id}': Repeater '{output_slug}' nests another Repeater — nested iteration is not supported"
    )]
    RepeaterNested {
        node_id: String,
        output_slug: String,
    },

    // --- Dynamic-form `stepsRef` (opt-in runtime-sourced form blocks). The
    //     HumanTask sources its whole `steps` list at runtime from a
    //     producer-namespaced `<slug>.<field>` ref instead of static authoring.
    //     The ref rides the same read-arc borrow machinery as Repeater/Map, so
    //     two of the three failure modes are ALREADY covered and need no
    //     dedicated error here: (1) an unknown producer/field is hard-failed by
    //     the guard/borrow net as `GuardUnresolved` (the ref is surfaced as a
    //     borrow site), and (2) the SHAPE of the runtime-produced blocks is
    //     enforced by the colored-token `SchemaRegistry` when the producer's
    //     output field is typed. The two variants below cover the gaps those
    //     don't: a malformed ref STRING (which would otherwise silently degrade
    //     to an empty static form with NO authoring signal), and a publish-time
    //     non-array shape check (cheaper/earlier than waiting for the runtime
    //     schema gate).
    /// `stepsRef` is not a plain `<slug>.<field>[.<more>…]` dotted path: it is
    /// empty, carries a `[*]` wildcard (steps are sourced whole, not iterated),
    /// or has fewer than two non-empty segments. This is the one failure mode
    /// nothing else catches — a malformed ref is skipped by the borrow planner,
    /// so without this it would silently render an empty form.
    #[error(
        "human task '{node_id}': stepsRef '{ref_value}' is malformed — expected a producer-namespaced `<slug>.<field>` path with no `[*]` wildcard"
    )]
    HumanTaskStepsRefMalformed { node_id: String, ref_value: String },

    /// `stepsRef` resolves to a producer field whose declared shape is not an
    /// array — the dynamic form needs a list of step/block objects. Mirrors
    /// `MapItemsRefNotArray`; `Any`/`Opaque`/`Json` are accepted (the producer
    /// declared arbitrary JSON, so the strict shape is deferred to the runtime
    /// `SchemaRegistry`). Unresolved producers are NOT reported here — they fall
    /// through to the guard pass's `GuardUnresolved`, avoiding a redundant error.
    #[error(
        "human task '{node_id}': stepsRef '{ref_value}' resolves to {actual_kind}, not an array — the dynamic form needs a list of step blocks"
    )]
    HumanTaskStepsRefNotArray {
        node_id: String,
        ref_value: String,
        actual_kind: String,
    },

    // --- Loop accumulator (fold/scan state) guards. Each accumulator var
    //     becomes a parked field in the loop's `p_<id>_data` envelope and is
    //     addressed downstream as `<loop_slug>.<var>`; the init/merge_expr are
    //     emitted verbatim into the enter/continue transition logic. Reject
    //     malformed declarations at publish so the editor can ring the loop.
    /// Accumulator `var` is not a valid Rhai identifier (`[A-Za-z_][A-Za-z0-9_]*`).
    #[error(
        "loop '{node_id}': accumulator var '{var}' is not a valid Rhai identifier ([A-Za-z_][A-Za-z0-9_]*)"
    )]
    LoopAccumulatorVarInvalid { node_id: String, var: String },

    /// Accumulator `var` is `iteration` — reserved for the loop's own counter.
    #[error("loop '{node_id}': accumulator var '{var}' is reserved (the loop iteration counter)")]
    LoopAccumulatorVarReserved { node_id: String, var: String },

    /// Two accumulators on the same loop declare the same `var`.
    #[error("loop '{node_id}': duplicate accumulator var '{var}'")]
    LoopAccumulatorDuplicateVar { node_id: String, var: String },

    /// An accumulator's `init` or `merge_expr` does not parse as Rhai.
    #[error("loop '{node_id}': accumulator '{var}' has an unparseable expression: {error}")]
    LoopAccumulatorExprUnparseable {
        node_id: String,
        var: String,
        error: String,
    },

    /// An `Executor` step declares BOTH a `capacity` binding and a `group`
    /// (docs/23/24). The two are mutually exclusive: `capacity` is the
    /// presence-PUSH admission handshake (claim/grant/register/release on a
    /// backing net, R3), `group` is a plain PULL routing coordinate
    /// (`executor-<wire>/<group>`, competing consumers). A grouped step stays on
    /// the plain pull path and must NOT enter the pooled lowering. Author one or
    /// the other.
    #[error(
        "automated step '{node_id}' sets both `capacity` and `group` on its executor \
         deployment — these are mutually exclusive (capacity is presence-push admission, \
         group is a pull routing coordinate). Use one or the other."
    )]
    CapacityGroupConflict { node_id: String },

    /// An `Executor` step's `group` is not a single safe NATS subject token
    /// (`[A-Za-z0-9_-]`, non-empty). The group is interpolated verbatim into the
    /// pull namespace `executor-<wire>/<group>` → a NATS subject segment, so a
    /// `.`, wildcard, or whitespace would broaden/break routing.
    #[error(
        "automated step '{node_id}': group '{group}' is not a valid routing token \
         (allowed: non-empty [A-Za-z0-9_-])"
    )]
    GroupTokenInvalid { node_id: String, group: String },
}

impl CompileError {
    /// Stable discriminant for the editor's error map. Keeps the wire format
    /// independent of Rust enum variant names.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Validation(_) => "validation",
            Self::Compilation(_) => "compilation",
            Self::MissingTargetHandle { .. } => "missing_target_handle",
            Self::UnknownSourcePort { .. } => "unknown_source_port",
            Self::UnknownTargetPort { .. } => "unknown_target_port",
            Self::EdgeTypeMismatch { .. } => "edge_type_mismatch",
            Self::SlugConflict { .. } => "slug_conflict",
            Self::GuardSyntax { .. } => "guard_syntax",
            Self::GuardUnresolved { .. } => "guard_unresolved",
            Self::TriggerEdgeCardinality { .. } => "trigger_edge_cardinality",
            Self::TriggerIsEdgeTarget { .. } => "trigger_is_edge_target",
            Self::TriggerUnknownTargetField { .. } => "trigger_unknown_target_field",
            Self::TriggerMappingSyntax { .. } => "trigger_mapping_syntax",
            Self::TriggerCronInvalid { .. } => "trigger_cron_invalid",
            Self::TriggerUnresolvedRef { .. } => "trigger_unresolved_ref",
            Self::TriggerEmptyMappingRequiredFields { .. } => {
                "trigger_empty_mapping_required_fields"
            }
            Self::SubWorkflowUnresolved { .. } => "subworkflow_unresolved",
            Self::SubWorkflowCycle { .. } => "subworkflow_cycle",
            Self::SubWorkflowPrivateOwnershipViolation { .. } => {
                "subworkflow_private_ownership_violation"
            }
            Self::SubWorkflowDepthExceeded { .. } => "subworkflow_depth_exceeded",
            Self::LoopEmpty { .. } => "loop_empty",
            Self::LeaseScopeEmpty { .. } => "lease_scope_empty",
            Self::LoopBodyStaleControlRef { .. } => "loop_body_stale_control_ref",
            Self::LeaseFieldUnknown { .. } => "lease_field_unknown",
            Self::MapEmpty { .. } => "map_empty",
            Self::MapRefMissingStar { .. } => "map_ref_missing_star",
            Self::MapResultVarInvalid { .. } => "map_result_var_invalid",
            Self::MapNested { .. } => "map_nested",
            Self::StreamFoldMissingHandle { .. } => "stream_fold_missing_handle",
            Self::StreamFoldInvalidReduce { .. } => "stream_fold_invalid_reduce",
            Self::StreamInputInvalid { .. } => "stream_input_invalid",
            Self::MapBodyUnsupported { .. } => "map_body_unsupported",
            Self::MapItemsRefNotArray { .. } => "map_items_ref_not_array",
            Self::MapItemsRefUnresolved { .. } => "map_items_ref_unresolved",
            Self::ToolChildHasIncomingEdge { .. } => "tool_child_has_incoming_edge",
            Self::OutputFieldShadowsReserved { .. } => "output_field_shadows_reserved",
            Self::OutputFieldShadowsInput { .. } => "output_field_shadows_input",
            Self::BackendRefUnresolved { .. } => "backend_ref_unresolved",
            Self::BackendRefNotUpstream { .. } => "backend_ref_not_upstream",
            Self::BackendPlaceholderSyntax { .. } => "backend_placeholder_syntax",
            Self::LlmImageRefNotFileKind { .. } => "llm_image_ref_not_file_kind",
            Self::ResourceTypeUnknown { .. } => "resource_type_unknown",
            Self::ResourceAliasCollidesWithSlug { .. } => "resource_alias_collides_with_slug",
            Self::ResourceAliasCollidesWithToken { .. } => "resource_alias_collides_with_token",
            Self::WorkspaceResourceUnknown { .. } => "workspace_resource_unknown",
            Self::AssetBindingUnknown { .. } => "asset_binding_unknown",
            Self::AssetBindingAmbiguous { .. } => "asset_binding_ambiguous",
            Self::ResourcePoolNotAPool { .. } => "resource_pool_not_a_pool",
            Self::SchedulerNotADatacenter { .. } => "scheduler_not_a_datacenter",
            Self::DatacenterConnectionIncomplete { .. } => "datacenter_connection_incomplete",
            Self::SchedulerUnresolved { .. } => "scheduler_unresolved",
            Self::JobTemplateUnresolved { .. } => "job_template_unresolved",
            Self::JobTemplateFlavorMismatch { .. } => "job_template_flavor_mismatch",
            Self::ResourcePoolRequestInvalid { .. } => "resource_pool_request_invalid",
            Self::SchemaRefUnresolved { .. } => "schema_ref_unresolved",
            Self::UndefinedRequirementCapability { .. } => "undefined_requirement_capability",
            Self::UnknownRequirementField { .. } => "unknown_requirement_field",
            Self::RequirementTypeMismatch { .. } => "requirement_type_mismatch",
            Self::RepeaterRefMalformed { .. } => "repeater_ref_malformed",
            Self::RepeaterRefUnresolved { .. } => "repeater_ref_unresolved",
            Self::RepeaterItemsRefNotArray { .. } => "repeater_items_ref_not_array",
            Self::RepeaterOutputSlugInvalid { .. } => "repeater_output_slug_invalid",
            Self::RepeaterNested { .. } => "repeater_nested",
            Self::HumanTaskStepsRefMalformed { .. } => "human_task_steps_ref_malformed",
            Self::HumanTaskStepsRefNotArray { .. } => "human_task_steps_ref_not_array",
            Self::LoopAccumulatorVarInvalid { .. } => "loop_accumulator_var_invalid",
            Self::LoopAccumulatorVarReserved { .. } => "loop_accumulator_var_reserved",
            Self::LoopAccumulatorDuplicateVar { .. } => "loop_accumulator_duplicate_var",
            Self::LoopAccumulatorExprUnparseable { .. } => "loop_accumulator_expr_unparseable",
            Self::CapacityGroupConflict { .. } => "capacity_group_conflict",
            Self::GroupTokenInvalid { .. } => "group_token_invalid",
        }
    }

    pub fn edge_id(&self) -> Option<&str> {
        match self {
            Self::MissingTargetHandle { edge_id }
            | Self::UnknownSourcePort { edge_id, .. }
            | Self::UnknownTargetPort { edge_id, .. }
            | Self::EdgeTypeMismatch { edge_id, .. } => Some(edge_id),
            Self::TriggerIsEdgeTarget { edge_id, .. } => Some(edge_id),
            Self::ToolChildHasIncomingEdge { edge_id, .. } => Some(edge_id),
            _ => None,
        }
    }

    pub fn node_id(&self) -> Option<&str> {
        match self {
            Self::UnknownSourcePort { node_id, .. } | Self::UnknownTargetPort { node_id, .. } => {
                Some(node_id)
            }
            Self::GuardSyntax { node_id, .. } | Self::GuardUnresolved { node_id, .. } => {
                Some(node_id)
            }
            Self::SlugConflict { node_a, .. } => Some(node_a),
            Self::TriggerEdgeCardinality { node_id, .. }
            | Self::TriggerIsEdgeTarget { node_id, .. }
            | Self::TriggerUnknownTargetField { node_id, .. }
            | Self::TriggerMappingSyntax { node_id, .. }
            | Self::TriggerCronInvalid { node_id, .. }
            | Self::TriggerUnresolvedRef { node_id, .. }
            | Self::TriggerEmptyMappingRequiredFields { node_id, .. }
            | Self::SubWorkflowUnresolved { node_id, .. }
            | Self::SubWorkflowPrivateOwnershipViolation { node_id, .. }
            | Self::SubWorkflowDepthExceeded { node_id, .. }
            | Self::LoopEmpty { node_id }
            | Self::LeaseScopeEmpty { node_id }
            | Self::LoopBodyStaleControlRef { node_id, .. }
            | Self::LeaseFieldUnknown { node_id, .. }
            | Self::MapEmpty { node_id }
            | Self::MapRefMissingStar { node_id, .. }
            | Self::MapResultVarInvalid { node_id, .. }
            | Self::MapNested { node_id, .. }
            | Self::StreamFoldMissingHandle { node_id, .. }
            | Self::StreamFoldInvalidReduce { node_id, .. }
            | Self::StreamInputInvalid { node_id, .. }
            | Self::MapBodyUnsupported { node_id, .. }
            | Self::MapItemsRefNotArray { node_id, .. }
            | Self::MapItemsRefUnresolved { node_id, .. }
            | Self::ToolChildHasIncomingEdge {
                child_id: node_id, ..
            }
            | Self::OutputFieldShadowsReserved { node_id, .. }
            | Self::OutputFieldShadowsInput { node_id, .. }
            | Self::BackendRefUnresolved { node_id, .. }
            | Self::BackendRefNotUpstream { node_id, .. }
            | Self::BackendPlaceholderSyntax { node_id, .. }
            | Self::LlmImageRefNotFileKind { node_id, .. }
            | Self::SchemaRefUnresolved { node_id, .. }
            | Self::UndefinedRequirementCapability { node_id, .. }
            | Self::UnknownRequirementField { node_id, .. }
            | Self::RequirementTypeMismatch { node_id, .. }
            | Self::RepeaterRefMalformed { node_id, .. }
            | Self::RepeaterRefUnresolved { node_id, .. }
            | Self::RepeaterItemsRefNotArray { node_id, .. }
            | Self::RepeaterOutputSlugInvalid { node_id, .. }
            | Self::RepeaterNested { node_id, .. }
            | Self::HumanTaskStepsRefMalformed { node_id, .. }
            | Self::HumanTaskStepsRefNotArray { node_id, .. }
            | Self::LoopAccumulatorVarInvalid { node_id, .. }
            | Self::LoopAccumulatorVarReserved { node_id, .. }
            | Self::LoopAccumulatorDuplicateVar { node_id, .. }
            | Self::LoopAccumulatorExprUnparseable { node_id, .. }
            | Self::CapacityGroupConflict { node_id }
            | Self::GroupTokenInvalid { node_id, .. }
            | Self::WorkspaceResourceUnknown { node_id, .. }
            | Self::AssetBindingUnknown { node_id, .. }
            | Self::AssetBindingAmbiguous { node_id, .. }
            | Self::ResourcePoolNotAPool { node_id, .. }
            | Self::SchedulerNotADatacenter { node_id, .. }
            | Self::DatacenterConnectionIncomplete { node_id, .. }
            | Self::SchedulerUnresolved { node_id }
            | Self::JobTemplateUnresolved { node_id, .. }
            | Self::JobTemplateFlavorMismatch { node_id, .. }
            | Self::ResourcePoolRequestInvalid { node_id, .. } => Some(node_id),
            _ => None,
        }
    }

    pub fn to_view(&self) -> CompileErrorView {
        CompileErrorView {
            kind: self.kind().to_string(),
            message: self.to_string(),
            edge_id: self.edge_id().map(str::to_string),
            node_id: self.node_id().map(str::to_string),
        }
    }
}

/// Structured payload of a compile error for the editor. Returned as part of
/// the publish API response so the frontend can highlight the offending
/// node/edge inline instead of just showing a flat error string.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub struct CompileErrorView {
    pub kind: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edge_id: Option<String>,
}
