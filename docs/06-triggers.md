# Trigger Nodes — Workflow Instantiation & Signal Sources

Status: Proposal
Author: handoff doc — continuation of [`05-typed-ports.md`](./05-typed-ports.md), unblocked now that Phases 1–4 of typed ports have landed.
Related: `service/src/models/template.rs`, `service/src/handlers/instances.rs`, `service/src/petri/instance.rs`, `service/src/catalogue/subscriptions.rs`, `service/src/lifecycle.rs`

## 1. Problem

Workflows can only be instantiated by a human or external system calling `POST /api/instances`. Everything else either doesn't exist or is half-built:

- **No scheduling.** No cron, no time-based triggers anywhere in the codebase.
- **No completion chaining.** When an instance terminates, `lifecycle.rs` updates the DB row and that's it. There's no mechanism to start template B because template A's instance just finished.
- **No webhooks.** Nothing exposes a stable URL that external systems can POST to in order to start a workflow.
- **Catalog subscriptions exist but only for in-flight nets.** `CatalogueSubscription` (`service/src/catalogue/subscriptions.rs:25`) targets a running `net_id` + `signal_place`. It can't spawn new instances, and it's registered via a separate side-channel API rather than declared on the template.

The result: every recurring or event-driven workflow is operated by a human pressing a button (or a separate ad-hoc cron job calling the API). That doesn't scale, and the catalog-subscription side-channel means trigger logic isn't versioned with the template it belongs to.

Typed ports unblock a clean fix. A trigger now has a concrete target to bind to: a Start block's `initial: Port` (`service/src/models/template.rs:92`). The payload-mapping problem reduces to "map event fields to declared port fields," which is exactly what the editor already does for human-task forms.

## 2. Goals & Non-Goals

**Goals**

- A new `Trigger` block kind that lives in the graph and connects via an edge to a target input port.
- One model covers both **spawn triggers** (edge → Start port → new instance) and **in-flight signal triggers** (edge → non-Start port → signal into a running net). The latter subsumes today's separate `CatalogueSubscription` side-channel.
- Trigger config travels with the template's `graph_json` and is versioned alongside everything else (`base_template_id`, `parent_id`, `version`).
- A `TriggerDispatcher` task running inside mekhan owns: cron scheduling, NATS subscriptions, webhook routing, and the funnel into `POST /api/instances` or `petri.signal.{net_id}.{place}`.
- Editor surfaces triggers as a "rail" of nodes attached to the canvas — same xyflow primitives as every other block, no special mode.

**Non-goals**

- No new runtime/AIR concept. Triggers are a pre-compile editor concern + a runtime dispatcher that fires the existing instance and signal APIs.
- No meta-net implementation. The completion-chain trigger (§4.3) uses the lifecycle event stream directly; if users later need conditional chaining ("only fire Y if X succeeded AND Z's instance is also done"), that's a separate proposal.
- No replacement of NATS as the event substrate. Triggers fire on top of the existing `petri.events.*`, `catalogue.*`, and signal subjects.
- No standalone trigger-execution service. The dispatcher lives in mekhan's process because that's where templates and the instance API are.

## 3. Design Overview

### 3.1 Trigger node

New variant on `WorkflowNodeData` (`service/src/models/template.rs:80`):

```rust
#[serde(rename = "trigger")]
Trigger {
    label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,

    /// What event source fires this trigger. Tagged enum below.
    source: TriggerSource,

    /// Optional concurrency / dedup policy. Defaults to `Allow` (every fire
    /// creates an instance / sends a signal, no coordination).
    #[serde(default)]
    concurrency: ConcurrencyPolicy,

    /// Payload→token field mapping. Resolved at fire time against the target
    /// port's schema (see §3.2). Empty when target port has no fields.
    #[serde(default)]
    payload_mapping: Vec<FieldMapping>,

    /// True once the user explicitly enables it. Disabled triggers are
    /// stored but the dispatcher ignores them.
    #[serde(default)]
    enabled: bool,
}
```

Trigger nodes only have **output ports** — they emit a token into the downstream edge. They are never edge *targets*; the editor refuses to draw an edge into a Trigger node.

The output port is derived: it has the same field shape as the target input port the edge leads to. This is what makes payload mapping concrete — the trigger node "wears the shape" of whatever it's wired to.

### 3.2 Wiring & target resolution

A trigger has exactly one outgoing edge. The compiler resolves the trigger's *effective target*:

- **Spawn target**: edge lands on a Start block's input port (`Start.initial`). The dispatcher must call `POST /api/instances` when this trigger fires, with a `start_tokens` entry seeding that Start.
- **In-flight target**: edge lands on any other input port (`AutomatedStep.input`, `HumanTask` derived port, `End.terminal`, etc.). The dispatcher must publish to `petri.signal.{net_id}.{target_place}` for every currently-running instance of this template. The target place id is computed from the target node's id following the same convention `catalogue/subscriptions.rs` already uses.

A single trigger node has one target — split by adding more trigger nodes. Multiple triggers can target the same input port (e.g. cron + webhook both feeding the same Start).

### 3.3 Field mapping

```rust
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct FieldMapping {
    /// Field name in the target port. Must exist in the resolved target's
    /// `Port.fields`.
    pub target_field: String,

    /// Expression evaluated against the trigger source's event payload.
    /// Source-specific scope (see §4); compiler validates the references.
    pub expression: String,
}
```

The expression language is the same Rhai dialect already used for Decision guards (`service/src/compiler/compile.rs:601`). Each trigger source provides a scope of identifiers (e.g. `payload.headers.x_user`, `fire_time`, `catalogue_entry.category`); the editor shows them in a field picker. Compiler validates every reference and asserts the expression's inferred kind matches the `target_field`'s `FieldKind`.

### 3.4 Concurrency & dedup

```rust
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ConcurrencyPolicy {
    /// Default. Every fire produces an event.
    Allow,
    /// At most one instance / signal in-flight at a time for this trigger.
    /// Subsequent fires are dropped.
    Skip,
    /// As `Skip` but holds the fire and replays it once the previous one
    /// completes. Backed by a per-trigger NATS-KV queue.
    Queue,
    /// Idempotent: the dispatcher computes a dedup key from a configured
    /// expression and skips fires whose key has been seen within a window.
    DedupKey { expression: String, window_secs: u32 },
}
```

`Skip` and `Queue` are operational guards (don't overwhelm a slow workflow). `DedupKey` is correctness (don't double-process the same catalog event on dispatcher restart) — catalog subscriptions already embed a `dedup_id` in their signal payloads, and this surfaces that contract to the trigger model.

## 4. Trigger Sources

```rust
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TriggerSource {
    Cron(CronTrigger),
    Catalog(CatalogTrigger),
    NetCompletion(NetCompletionTrigger),
    Webhook(WebhookTrigger),
    Manual(ManualTrigger),
}
```

Ship in this order. The first two unblock 80% of the use cases.

### 4.1 `Cron` — ship first

```rust
pub struct CronTrigger {
    pub schedule: String,            // "0 9 * * MON-FRI"
    pub timezone: String,            // IANA tz, e.g. "Europe/Berlin"
    #[serde(default)]
    pub jitter_secs: u32,            // randomize fire time within window
    #[serde(default = "default_catchup")]
    pub catchup: CronCatchup,        // FireMissed | SkipMissed
}
```

Expression scope: `fire_time: Timestamp`, `scheduled_time: Timestamp`. Most cron triggers just need `fire_time` mapped into an audit field on the token.

Implementation: `tokio_cron_scheduler` or hand-rolled around `cron` crate; the dispatcher owns one scheduler instance.

### 4.2 `Catalog` — ship first; subsumes existing subscriptions

```rust
pub struct CatalogTrigger {
    /// Same filter shape as `CatalogueSubscription.filters`:
    /// field name → (operator → value).
    pub filters: HashMap<String, HashMap<String, String>>,
    #[serde(default)]
    pub backfill: bool,  // process existing matching entries on publish
}
```

Expression scope: every field on `CatalogueEntry` (`service/src/catalogue/model.rs:7`) plus `catalogue_entry` as a `Json` blob for escape-hatch access.

**Reconciliation with the existing subscription manager**: the catalog trigger is an *additional* authoring surface, not a replacement. The engine's `catalogue_subscribe` effect handler (`engine/core-engine/crates/application/src/catalogue_handlers.rs:434`, ADR 17) lets a running net dynamically register subscriptions whose filters depend on runtime token data — e.g. "subscribe to future revisions of *this specific* document I just received." Static trigger nodes can't express that case and don't try to. Both surfaces write through the same `SubscriptionManager`:

- **In-flight catalog trigger** (target = non-Start port): the dispatcher calls `SubscriptionManager::create_subscription` when an instance starts, with the trigger's literal filter values. Cleaned up on instance completion by the existing `cleanup_net_subscriptions` path.
- **Spawn catalog trigger** (target = Start port): no `net_id` exists yet, so the dispatcher subscribes directly to catalog events (no KV row) and calls `POST /api/instances` on match.
- **Dynamic `catalogue_subscribe` effect**: unchanged. Runtime token-driven subscriptions stay; the engine effect handler and the dispatcher both write to the same KV bucket and the subscription manager is agnostic to who created a row.

Static triggers are essentially sugar for "subscribe on instance start with literal filters" — they pull their weight when the filter is known at authoring time, without forcing the author to model a startup transition with a `catalogue_subscribe` effect.

### 4.3 `NetCompletion` — ship second

```rust
pub struct NetCompletionTrigger {
    pub source_template_id: Uuid,
    #[serde(default)]
    pub source_version: Option<i32>,  // None = any published version
    pub on: CompletionStatus,         // Success | Failure | Cancelled | Any
}
```

The dispatcher subscribes to `petri.events.mekhan-*.net.completed|cancelled|failed` — same stream `lifecycle.rs:38` already consumes — and matches by joining against the instance table on `net_id` to recover the source template.

Expression scope: `source_instance_id`, `source_template_id`, `completion_time`, `completion_status`, and `final_token: Json` (the terminal token of the completed instance, available from the lifecycle event payload). This lets a downstream workflow consume the upstream's result.

### 4.4 `Webhook` — ship third

```rust
pub struct WebhookTrigger {
    /// Slug appended to /api/triggers/webhook/{slug}. Globally unique;
    /// editor reserves at publish.
    pub slug: String,
    pub auth: WebhookAuth,            // None | SharedSecret | SignedHmac
    #[serde(default)]
    pub require_method: Option<HttpMethod>,
}
```

Expression scope: `payload: Json` (request body), `headers: Json`, `query: Json`, `fire_time: Timestamp`. The slug must be stable across template versions so external systems' configured URLs keep working.

### 4.5 `Manual` — ship fourth

```rust
pub struct ManualTrigger {
    /// Form schema for the "Run with parameters" dialog in the UI. Reuses
    /// the existing `TaskFieldConfig` form-builder.
    #[serde(default)]
    pub form: Vec<TaskFieldConfig>,
}
```

Expression scope: every field name declared in `form`. This is "the existing manual instance creation, but with a typed form instead of a JSON textarea." Useful for human-fired workflows that take parameters.

### 4.6 Not shipping yet

- **Generic NATS subject** — easy to build (`pattern: String`, `expression scope: subject: String, payload: Json`) but premature without a concrete use case. Add when asked.
- **Database CDC**, **file system watchers**, **calendar-aware schedules** — implementation leaks or vendor-specific. Defer indefinitely.

## 5. Dispatcher

New module `service/src/triggers/` containing `dispatcher.rs`, `sources/cron.rs`, `sources/catalog.rs`, etc.

Structure mirrors `lifecycle.rs`: a `start_trigger_dispatcher(state, ...)` entry point spawned at app boot, listening on multiple inputs and emitting actions.

```
                              ┌─ cron scheduler ─────┐
   triggers (from publish) ───┼─ catalog event sub ──┼─→ fire decision ─→ action
                              ├─ lifecycle event sub ┤        │              │
                              └─ webhook receiver ───┘        │              ▼
                                                     concurrency check    ┌── spawn:
                                                              │           │   POST /api/instances
                                                              └─ dedup ───┤   { start_tokens }
                                                                          │
                                                                          └── signal:
                                                                              publish to
                                                                              petri.signal.{net_id}.{place}
```

**Registration**. On startup the dispatcher scans every `published = true` template, walks each graph for `Trigger` nodes, and registers them. On every subsequent template publish, it diffs the new published graph against the previously-registered set for that template and adds/removes accordingly. Unpublish/version-supersede tears down old registrations.

**Spawn fire path**. Resolve the target Start block id. Evaluate `payload_mapping` against the event scope. Build a `StartToken { start_block_id, token }`. Call the existing `POST /api/instances` handler in-process (don't go through HTTP — cheaper and avoids self-auth dance). Tag the resulting instance with a `triggered_by: trigger_node_id` audit field in `metadata`.

**Signal fire path**. Query the instance table for `template_id` = trigger's template, `status = 'running'`. For each, publish `ExternalSignal` to `petri.signal.{net_id}.{place}` exactly the same way `catalogue/subscriptions.rs:334` does today. Include a `signal_key` so receivers can dedup across dispatcher restarts.

**Crash recovery**. Cron uses NATS KV to persist last-fire timestamps per trigger. On boot, replay any missed fires for triggers with `catchup: FireMissed`. Catalog uses its existing `dedup_id` mechanism. Webhook is stateless — external retries are the source of truth. NetCompletion replays from the JetStream consumer offset.

## 6. API Surface

New endpoints under `/api/triggers`:

| Method | Path                                   | Purpose                                                                                       |
|--------|----------------------------------------|-----------------------------------------------------------------------------------------------|
| GET    | `/api/triggers`                        | List all active triggers across templates (admin / debug).                                    |
| GET    | `/api/templates/{id}/triggers`         | List triggers for a specific template (derived from `graph_json`; not a separate store).      |
| POST   | `/api/triggers/webhook/{slug}`         | Webhook receiver. Authenticates per the trigger's `WebhookAuth`, fires.                       |
| POST   | `/api/triggers/{node_id}/fire`         | Manual fire path (for the `Manual` trigger source and admin testing of any other source).     |
| GET    | `/api/triggers/{node_id}/history`      | Last N fires with status, payload digest, resulting instance id.                              |

Existing instance API (`POST /api/instances`) is unchanged — the dispatcher uses the same handler path that humans do.

Existing standalone subscription API (whatever `service/src/catalogue/handlers.rs` exposes for direct `CatalogueSubscription` CRUD) gets a deprecation header and one release of overlap.

## 7. Editor

Lives in `app/src/lib/editor/`. xyflow handles the visuals; Yjs the collab state.

- **Trigger node component**. Renders as a chevron-shaped node visually distinct from workflow blocks. Source-kind picker (Cron / Catalog / NetCompletion / Webhook / Manual) selects the inner config form.
- **Trigger rail**. Trigger nodes are positioned by the editor along the left edge of the canvas (or in a dedicated "triggers" lane above the start row). They cannot be dragged into the body of the workflow. Visually clear: triggers are *inputs to the workflow*, not part of it.
- **Edge handles**. A Trigger node has one output handle; valid drop targets are any input port on the current canvas. Type-check happens on drop, same flow as Phase 2 of typed ports.
- **Payload-mapper panel**. Selected trigger shows a per-target-field form: one row per `Port.fields[i]` in the resolved target, with an expression input and a field picker scoped to the trigger source. Reuses Phase 3's GuardEditor component shell.
- **Enabled toggle**. Prominent on the trigger node body, also reflected in the side panel. Disabled triggers render at half opacity.
- **History tab**. New tab on the template detail page showing fire history per trigger (last N fires, status, resulting instance link). Backed by the `history` endpoint.

The canvas layout work is the most visually risky part — get a designer to sketch the "rail" before building it.

## 8. Phased Delivery

Each sub-phase is shippable on its own. Order matters: 5a is the spine, then sources land independently.

**Phase 5a — Trigger node model + dispatcher skeleton.** ~1 week.

- Add `WorkflowNodeData::Trigger`, `TriggerSource`, `FieldMapping`, `ConcurrencyPolicy`.
- Editor renders trigger nodes (visual only — no kinds yet, no firing).
- Compiler walks them, validates edges target a real input port, validates `payload_mapping` against the resolved port's schema.
- New `service/src/triggers/` module with the dispatcher entry point spawned from `main.rs`, plus a `Manual` source that fires via the `/api/triggers/{node_id}/fire` endpoint.
- *Unblocks*: end-to-end manual firing through the trigger model, including in-flight signals replacing one-off direct signal injection.

**Phase 5b — Cron source.** ~3 days.

- Add `CronTrigger`, schedule registration, missed-fire replay.
- Editor: schedule input with human-readable preview ("next fires: 2026-05-15 09:00 CEST").

**Phase 5c — Catalog source.** ~3 days.

- Add `CatalogTrigger`. For in-flight targets, register with `SubscriptionManager`. For spawn targets, subscribe directly to catalog events.
- Editor: filter builder matching the existing catalog query UI.
- The engine's `catalogue_subscribe` effect handler and any standalone subscription API stay as-is — they serve runtime/dynamic subscriptions that static triggers can't express (see §4.2).

**Phase 5d — NetCompletion source.** ~3 days.

- Add `NetCompletionTrigger`. Subscribe to the same lifecycle event stream `lifecycle.rs` already uses; share the consumer.
- Editor: template picker (constrained to published templates the user has access to) + status filter.

**Phase 5e — Webhook + manual form sources.** ~3 days.

- Add `WebhookTrigger`, `ManualTrigger`. Webhook receiver endpoint, auth (`SharedSecret` first, `SignedHmac` second).
- Editor: form builder reuses the existing `TaskFieldConfig` field component.

**Phase 5f — History + observability.** ~3 days.

- `/api/triggers/{node_id}/history` endpoint, history tab in editor.
- Metrics: fire counts per source kind, dedup hits, dispatcher lag.

## 9. Open Questions

These are deferred decisions, not blockers. Flag them when an owner picks this up.

1. **Where do trigger registrations live?** Two options: (a) walk `graph_json` on every dispatcher start (simple, slow on large fleets); (b) materialize a `template_trigger_registrations` table on publish (faster lookups, more moving parts). Recommendation: start with (a); switch to (b) when scan times exceed ~1s.
2. **Trigger nodes inside `Scope` blocks.** Allowed or not? Recommendation: forbid for now — triggers live at the template's top level. Revisit when `Scope` blocks become invokable sub-templates with their own boundaries.
3. **Authorization for spawn fires.** Whose identity does a triggered instance get attributed to? Recommendation: a synthetic `trigger:{node_id}` principal stored in `created_by`, with the original trigger metadata in `metadata.triggered_by` for audit. Don't try to forge a user identity.
4. **Webhook slug rebinding on version supersede.** When a new template version supersedes the old, does the old version's webhook URL keep working? Recommendation: yes — slugs are template-scoped, not version-scoped. The dispatcher always routes to the latest published version's trigger config.
5. **Backfill semantics for catalog spawn triggers.** Should `backfill: true` on a spawn trigger really spawn N instances retroactively when the trigger is first published, or should it only apply to in-flight targets? Recommendation: opt-in per trigger, with a hard limit (e.g. 100) to prevent accidental fan-out.
6. **Pause vs. delete.** Need a "pause this trigger without unpublishing the template" affordance? The `enabled` field covers it, but the editor needs a place to toggle it without dragging the user into a full edit/publish cycle. Recommendation: pause = edit-publish a single-field change; if that's too heavy, add a separate `PATCH /api/triggers/{node_id}/enabled` that updates the live graph_json without bumping version (the only such fast-path on the template).

## 10. Out of Scope (future work)

- **Composite trigger logic** ("fire Y only if X and Z completed"). Becomes the meta-net argument from the original discussion; revisit if users hit the ceiling of single-source triggers.
- **Replay / rerun UI.** Re-firing an old trigger event against an updated workflow is a separate feature.
- **Trigger throttling across templates.** Global rate limits, fairness between tenants, etc.
- **Inbound event bus abstraction.** If the trigger sources balloon to 8+ kinds, factor out a `EventSource` trait so adding new ones doesn't touch the dispatcher core. Premature now.

## 11. Acceptance Criteria

**Phase 5a ships when**:
- A `Trigger` node can be created in the editor, wired to a Start port with `payload_mapping` against that port's fields, published, and manually fired via `/api/triggers/{node_id}/fire` to create an instance with the correct `start_tokens`.
- A trigger wired to a non-Start port fires by publishing to `petri.signal.{net_id}.{place}` for all running instances of the template.
- Compile errors for invalid payload mappings (missing target field, kind mismatch, unresolved expression identifier) surface in the editor like Phase 3 guard errors do.

**Full proposal ships when**:
- All five trigger sources work end-to-end with editor UX, history, and at least one e2e test per source.
- Static catalog triggers and dynamic `catalogue_subscribe` effects coexist cleanly — both write through `SubscriptionManager`, both are cleaned up by `cleanup_net_subscriptions` on instance completion.
- A published template carrying a cron trigger survives mekhan restart with no missed fires (modulo `SkipMissed` triggers configured to skip).
- Trigger registration scan + dispatcher boot time is under 1s for a fleet of 1000 published templates with 5 triggers each, or the materialized-table path (Q9.1) is built.
