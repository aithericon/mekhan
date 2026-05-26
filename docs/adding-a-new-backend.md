# Adding a New Executor Backend

This is the end-to-end recipe for adding a new executor backend to the
Aithericon platform — Rust crate, declarative registry entry, frontend
config panel, instance-view renderer, failure handling, demo, and tests.

The **SMTP backend** (added at commit `5d595ee`, then ported to the
declarative registry in the Phase 2.a–2.h work) is the worked example
throughout — every section cites real file paths from the merged
implementation. If a step here looks abstract, open the SMTP file in the
same section to see exactly what it became in practice.

> **Read §13 (Gotchas) before §3.** The SMTP backend shipped with a
> latent regression — resource envelopes silently dropped on nodes
> that also had upstream-producer borrows. That fix landed alongside
> the original guide. The §13 footguns enumerate the failure modes that
> bit a previous author so you can avoid them rather than re-discover them.

This guide assumes you already know:

- Rust workspaces (the executor lives in its own workspace under
  `executor/`; mekhan-service is the umbrella's only direct member;
  `shared/backends` is a cross-workspace member both binaries depend on).
- The control-data token model (`docs/10-control-data-token-model.md`).
- The resource model (`shared/resources/` + `service/src/petri/resource_resolver.rs`).
  Optional — you can add a backend that doesn't consume a resource.

---

## 1. What a backend is

A backend has **three distinct surfaces**, one per crate:

1. **Cross-crate metadata** in `shared/backends/src/registry.rs`. A
   `BackendMeta` const with wire name, display name, icon, dispatch
   mode, schedulable, resource channel. The mekhan compiler and the
   executor both read this slice — it is the single source of truth
   for "what backends exist."

2. **Compile-time decl** in `service/src/backends/<name>.rs`. A
   `BackendDecl` static referencing the shared `BackendMeta`, plus
   compile-only fields: `validate` (editor JSON → executor JSON +
   staged inputs), `ref_scanner` (which placeholder surfaces carry
   `<head>.<attr>` references), `default_editor_config` (seed JSON the
   editor inserts when the user picks this backend), and a few
   compile-pipeline flags (`consumes_declared_outputs`,
   `pyi_introspection`, `borrow_shape`, `validate_ref_kind`).

3. **Runtime impl** in `executor/crates/executor-<name>/`. An
   `ExecutionBackend` trait impl with `prepare` + `execute` + `name` +
   `supports`:

   ```rust
   #[async_trait]
   pub trait ExecutionBackend: Send + Sync + 'static {
       async fn prepare(&self, _job: &ExecutionJob, ctx: RunContext) -> Result<RunContext, ExecutorError> { Ok(ctx) }
       async fn execute(&self, ctx: &RunContext, cb: StatusCallback,
                        events: Option<Arc<dyn EventStream>>, cancel: CancellationToken)
           -> Result<ExecutionResult, ExecutorError>;
       fn name(&self) -> &'static str;
       fn supports(&self, spec: &ExecutionSpec) -> bool;
   }
   ```

`build_executor` in `executor-service/src/main.rs` walks
`aithericon_backends::BACKENDS`, filters to `dispatch_mode ==
ExecutorJob`, and dispatches each entry to a feature-gated match arm
that constructs the backend. The match key is the `BackendMeta::wire_name`
string — `"python"`, `"http"`, `"smtp"`, …

Each backend lives in its own crate under `executor/crates/executor-<name>/`
so its dependency graph is isolated and the user can opt in via a feature
flag on `executor-service`.

---

## 2. Decide the surface

Before writing code, settle:

1. **Wire-name** — short, snake_case, written to the database and the
   OpenAPI spec. **Locked once shipped.** Renames require a data migration.
   SMTP: `"smtp"`.
2. **Display name** — what the editor's "Backend Type" dropdown shows.
   SMTP: `"SMTP (Email)"`.
3. **Icon** — lucide name; the frontend resolves to the component.
   SMTP: `"mail"`.
4. **Dispatch mode** — `DispatchMode::ExecutorJob` (normal — executor
   dispatches a job) or `DispatchMode::EngineEffect { handler: "…" }`
   (engine fires a builtin effect, no executor involvement —
   CatalogueQuery's `catalogue_lookup` is the only one today).
5. **Schedulable?** — does it make sense to let an author submit this
   step to a scheduler-net (Nomad/Slurm GPU)? Engine effects MUST be
   `schedulable: false`. Most ExecutorJob backends are `schedulable:
   true`.
6. **Resource channel** — `ResourceChannel::StagedFile` (SMTP-style —
   compiler emits a `ResourceEnvelope` borrow, the publisher stages
   `<alias>.json`, the backend reads the file), `ResourceChannel::ConfigOverlay`
   (LLM-style — backend's `prepare()` merges `<alias>.json` fields
   into the resolved config), or `ResourceChannel::None`.
7. **Resource binding?** — does the backend need credentials from a
   workspace-scoped `Resource` (the typed credential surface in
   `shared/resources/`)? If yes, the resource type must exist there
   before you start; the launcher's `ResourceResolver` is what hands
   you the resolved view at run time. SMTP consumes
   `shared/resources/src/types.rs::Smtp`.
8. **Authoring surface** — does the workflow author write *code/templates*
   (Python source, Tera templates, SQL) or just *config* (HTTP URL +
   method, Docker image)? If yes, decide whether the source lives in node
   files (Python-style: per-node Y.Map, IDE-editable, scanned at compile
   time for refs) or inline in the config (SMTP-style: embedded string
   plus a `label` for diagnostics).
9. **Output shape** — what does `ExecutionResult.outputs` look like? Pick
   field names the editor's port picker will surface. SMTP emits
   `outcome` (structured) + `subject`, `body_text_preview?`,
   `body_html_preview?`. The shape pins the wire contract — once a
   workflow downstream of this step references one of these fields,
   renaming it breaks the workflow.
10. **Failure model** — every error class that a workflow author or
    operator will want to filter on. Define them up front as an enum and
    commit to the names; the frontend instance-view renderer pattern-matches
    on them.

SMTP's failure model:
`Success | TemplateRender | InvalidAddress | InvalidConfig | ConnectFailed
| TlsFailed | AuthFailed | RecipientRejected | ServerError | Timeout |
AttachmentError`

---

## 3. Register the backend across the three crates

The platform's registry is split across three crates. Add entries in
this order:

### 3.1 `shared/backends` — `ExecutionBackendType` variant + `BackendMeta` const

Add a variant to the enum in `shared/backends/src/types.rs`:

```rust
pub enum ExecutionBackendType {
    Python, Process, Docker, Http, Llm, FileOps, Kreuzberg,
    Smtp,                  // ← new
    CatalogueQuery,
}
```

The `as_wire_str` method needs the new arm:

```rust
Self::Smtp => "smtp",
```

Add the `BackendMeta` const in `shared/backends/src/registry.rs`:

```rust
pub const SMTP_META: BackendMeta = BackendMeta {
    backend_type: ExecutionBackendType::Smtp,
    wire_name: "smtp",
    display_name: "SMTP (Email)",
    icon: "mail",
    dispatch_mode: DispatchMode::ExecutorJob,
    schedulable: true,
    resource_channel: ResourceChannel::StagedFile,
};
```

Add it to the `BACKENDS` slice:

```rust
pub static BACKENDS: &[&BackendMeta] = &[
    &PYTHON_META,
    …
    &SMTP_META,        // ← new
    &CATALOGUE_QUERY_META,
];
```

Re-export the const from `shared/backends/src/lib.rs` so callers can
write `aithericon_backends::SMTP_META`:

```rust
pub use registry::{
    …, SMTP_META, …
};
```

### 3.2 `service/src/backends/<name>.rs` — compile-time `BackendDecl`

Create `service/src/backends/smtp.rs` (use any existing file as a
template — SMTP is the canonical pilot). The decl is:

```rust
use super::{BackendDecl, DefaultPortField, RefSite, ScanCtx, ValidationCtx, SMTP_META};

const DEFAULT_OUTPUT_FIELDS: &[DefaultPortField] = &[
    DefaultPortField { name: "outcome", label: "Outcome", kind: FieldKind::Json },
    DefaultPortField { name: "subject", label: "Subject", kind: FieldKind::Text },
    DefaultPortField { name: "body_text_preview", label: "Body (text)", kind: FieldKind::Textarea },
    DefaultPortField { name: "body_html_preview", label: "Body (html)", kind: FieldKind::Textarea },
];

const RESOURCE_ALIAS_PATHS: &[&[&str]] = &[&["resource_alias"]];

pub static SMTP_DECL: BackendDecl = BackendDecl {
    meta: &SMTP_META,
    backend_type: ExecutionBackendType::Smtp,
    default_output_fields: DEFAULT_OUTPUT_FIELDS,
    default_editor_config: default_editor_config,
    validate: validate,
    ref_scanner: Some(ref_scanner),
    resource_alias_paths: RESOURCE_ALIAS_PATHS,
    consumes_declared_outputs: false,
    pyi_introspection: false,
    borrow_shape: super::BorrowShape::Envelope,
    validate_ref_kind: super::accept_any_ref_kind,
};

fn default_editor_config() -> Value { json!({ … }) }
fn validate(config: &Value, ctx: &ValidationCtx<'_>) -> Result<(Value, Vec<InputDeclaration>), CompileError> { … }
fn ref_scanner(ctx: &ScanCtx<'_>) -> Vec<RefSite> { … }
```

Then register the module + decl in `service/src/backends/mod.rs`:

```rust
pub mod smtp;
…
pub static BACKENDS: &[&BackendDecl] = &[
    …,
    &smtp::SMTP_DECL,        // ← new
    …,
];
```

`BackendDecl` fields you must set per backend (the rest read through
`meta` automatically):

| Field | What it does |
|---|---|
| `meta` | `&'static BackendMeta` from `shared/backends` (§3.1) |
| `backend_type` | Equals `meta.backend_type`; carried separately so dispatch sites can match without indirection |
| `default_output_fields` | Canonical port — the "Reset to default" button uses this |
| `default_editor_config` | Seed JSON for newly-created steps of this backend |
| `validate` | Editor JSON → executor JSON + staged inputs (§6) |
| `ref_scanner` | `Option<fn(&ScanCtx) -> Vec<RefSite>>` — surfaces `<head>.<attr>` refs (§6.1) |
| `resource_alias_paths` | Static JSON paths where the config carries a resource alias string (e.g. `&["resource_alias"]`) |
| `consumes_declared_outputs` | `true` if declared output fields drive a Rhai `outputs:` constant (Python, Kreuzberg, Llm today) |
| `pyi_introspection` | Python-only flag (generates `.pyi` stubs on publish) |
| `borrow_shape` | `Envelope` (Python/SMTP) — whole `<slug>.json` staged; `PerField` (Kreuzberg/LLM) — per-field staging with `{{input:NAME}}` rewrite |
| `validate_ref_kind` | Per-site kind constraint — most backends use `accept_any_ref_kind`; LLM enforces `images[].path → File` |

### 3.3 The conformance test catches bijection drift

`service/tests/backend_registry_coverage.rs` walks both the shared and
service-side registries on every test run, asserting every
`ExecutionBackendType` variant has a `BackendMeta` AND a `BackendDecl`.
If you forget a step in §3.1 or §3.2 it fails immediately.

The DSL-format parser in `service/src/bin/cli/formats/dsl.rs` may also
need a new arm. Search for `"expected one of: python, process, …"` and
add your wire-name to the union if you want CLI DSL support.

---

## 4. Add the wire-format config DTO

The executor and the mekhan compiler both deserialize the same JSON blob
out of `ExecutionSpec.config`. To keep the contract in lockstep without a
heavy dependency edge, the DTO lives in
`executor/crates/executor-backend-configs/src/<name>.rs` — a thin crate
that takes only `serde` + `aithericon-executor-domain`.

SMTP's DTO is at `executor/crates/executor-backend-configs/src/smtp.rs`:

```rust
pub struct SmtpConfig {
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub bcc: Vec<String>,
    pub from: Option<String>,
    pub subject: TemplateSource,
    pub body_text: Option<TemplateSource>,
    pub body_html: Option<TemplateSource>,
    pub attachments: Vec<AttachmentSpec>,
    pub resource_alias: Option<String>,
    pub dry_run: bool,
    pub vars: HashMap<String, String>,
}
pub struct TemplateSource { pub label: String, pub source: String }
pub struct AttachmentSpec { pub filename: String, pub input_name: String, pub mime: Option<String> }
```

Three rules:

1. **Defaults everywhere** (`#[serde(default)]` on every optional field).
   Drafts coming back from the editor are routinely partial; a missing
   field should round-trip without error.
2. **`from_spec` + `into_spec`** mirror the patterns the other configs use
   so the SDK / harness code can build one without boilerplate. See the
   end of `smtp.rs` for the template.
3. **`validate()`** — local invariants only (recipient count > 0, at
   least one body, attachment names non-empty, no duplicate input names).
   Anything that needs to look at the workflow graph belongs in the
   service-side `BackendDecl::validate` (§6).

If the backend consumes a typed resource, also define a "resolved view"
struct in this file mirroring the registered resource type's shape
(`shared/resources/src/types.rs::<YourResource>`). SMTP's is
`ResolvedSmtpResource`. The struct deserializes the envelope the
launcher's `ResourceResolver` produces — public fields inline +
`{{secret:...}}` templates for secret fields, which the staging
pipeline substitutes with real Vault values before the backend reads
them. **Where the envelope lives at runtime** is documented in §5.3 —
it is NOT the `resolved_config` side-channel for production traffic.

Provide two parsers: `from_resolved_value(&Value)` reads the envelope
from any JSON object (the canonical path, used when reading a staged
`<alias>.json`), and `from_resolved(&resolved_config_value)` is the
test-harness fallback that looks up a constant key (SMTP's is
`smtp_resource`) inside `RunContext.resolved_config`.

Finally, add the new module to `executor-backend-configs/src/lib.rs`:

```rust
pub mod smtp;
```

---

## 5. Implement the executor backend crate

Layout under `executor/crates/executor-<name>/`:

```
Cargo.toml
src/
├── lib.rs        — ExecutionBackend impl, dispatch entry point
├── transport.rs  — protocol-specific connection setup (HTTP for `http`,
│                   lettre for `smtp`, lettre for `slack-webhook`, …)
├── template.rs   — render input data into protocol fields (SMTP: Tera
│                   context → subject + body + recipients)
├── multipart.rs  — payload assembly (SMTP: MIME tree)
├── outcome.rs    — structured success/failure enum
└── tests.rs      — unit tests, in-process sink, no network
```

### 5.1 Cargo.toml

```toml
[package]
name = "aithericon-executor-smtp"
version.workspace = true
edition.workspace = true
license.workspace = true
authors.workspace = true

[dependencies]
aithericon-executor-domain = { workspace = true }
aithericon-executor-backend = { workspace = true }
aithericon-executor-backend-configs = { workspace = true }
tokio = { workspace = true }
tokio-util = { workspace = true }
async-trait = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
tracing = { workspace = true }
lettre = { version = "0.11", default-features = false, features = [
    "tokio1", "tokio1-rustls-tls", "smtp-transport", "builder",
] }
tera = { version = "1", default-features = false }
mime = "0.3"

[dev-dependencies]
tempfile = "3"
```

Then wire the crate into the executor workspace by editing
`executor/Cargo.toml` (add to `members` + `workspace.dependencies`).

### 5.2 The trait impl

The minimal `lib.rs` does six things:

1. Deserialize `SmtpConfig::from_spec(&ctx.spec)` and call `.validate()`.
2. Pull the resolved resource view. **For resource ENVELOPES** (multiple
   fields → one `<alias>.json` file) read from
   `ctx.staged_inputs["<resource_alias>.json"]`. **For inline secret
   templates in `spec.config`** (one-field substitutions like an HTTP
   URL containing `{{secret:API_KEY}}`) read `ctx.resolved_config`.
   Two channels, two purposes — see §5.3. If neither yields a usable
   resource and your backend needs one, fail with
   `<Backend>Outcome::InvalidConfig`.
3. Build the runtime context (Tera context, HTTP client, DB connection).
4. Render / dispatch / send.
5. Map results to `ExecutionResult` — `outcome` field +
   `outputs["outcome"]` carrying the structured detail. The
   `ExecutionOutcome` enum from `executor-domain` is intentionally
   coarse (`Success | BackendError | TimedOut | Cancelled`); the
   structured per-backend reason lives in the outputs map.
6. Respect the cancellation token (`tokio::select! { _ = cancel.cancelled() => …, … }`).

See `executor/crates/executor-smtp/src/lib.rs::resolve_resource` for the
canonical staged-input read pattern, with a `resolved_config` fallback
for unit-test harnesses.

### 5.3 Two secret-delivery channels — pick the right one

Secrets reach a backend through **two** distinct mechanisms. Picking
the wrong one is the most common backend-author mistake (it cost us a
post-merge regression; see §13).

**(a) `RunContext.resolved_config` — for inline `{{secret:KEY}}` in
`spec.config`.** Use this when your backend's config has *individual*
fields that take a secret directly — e.g. the HTTP backend's
`Authorization: Bearer {{secret:API_KEY}}` header value, or a URL with
`https://{{secret:HOST}}/path`. The `PlanSecretsHook` walks
`spec.config` at staging time, substitutes the patterns, and parks the
resolved JSON in `RunContext.resolved_config` (`#[serde(skip)]`).
HTTP backend reference: `executor/crates/executor-http/src/lib.rs::prepare`:

```rust
let mut config = match run_context.resolved_config.as_ref() {
    Some(resolved) => serde_json::from_value::<HttpConfig>(resolved.clone())?,
    None           => HttpConfig::from_spec(&run_context.spec)?,
};
```

**(b) Staged input files (`<alias>.json`) — for resource ENVELOPES.**
Use this when your backend binds a typed workspace resource (Postgres,
SMTP, OpenAI, …). The mekhan compiler emits a `BorrowResolution::ResourceEnvelope`
for each resource the workflow references; the unified planner in
`compiler/borrow.rs` reads `BackendDecl::resource_alias_paths` and
`ref_scanner` to find the alias name and stage it. The launcher's
resolver computes the envelope (public fields inline + `{{secret:...}}`
templates for secret fields); when the run fires, `PlanSecretsHook`
walks inline-source JSON values, resolves embedded secret patterns,
then `StageInputsHook` writes the resolved JSON to
`inputs/<alias>.json`. The backend reads it back:

```rust
let path = run_context
    .staged_inputs
    .get(&format!("{alias}.json"))
    .cloned()
    .unwrap_or_else(|| run_context.run_dir.inputs_dir.join(format!("{alias}.json")));
let bytes = std::fs::read(&path)?;
let resource: ResolvedResource = serde_json::from_slice(&bytes)?;
```

The SMTP backend's `resolve_resource` in `executor-smtp/src/lib.rs` is
the reference. **It is NOT correct to expect the launcher to populate
`resolved_config` with the resource envelope** — that channel is for
the inline-substitution case in (a).

**Never log a `resolved_*` field** (env, config, input_storage,
output_storage). `RunContext`'s `Debug` impl is hand-written to elide
them precisely so a stray `tracing::debug!(?ctx, …)` can't leak.
Resource envelopes read from staged files have the same hazard — by
the time you read `inputs/<alias>.json` it holds plaintext secrets.
Do not log its contents either.

### 5.4 A test seam for the network

Real-server conformance tests are slow and require Docker. For the unit
test lane, add a `MessageSink` trait (or equivalent) — a tiny shim that
intercepts the would-be-sent message and stashes it for assertion:

```rust
pub trait MessageSink: Send + Sync {
    fn accept(&self, msg: &lettre::Message);
}

impl SmtpBackend {
    pub fn with_sink(mut self, sink: Arc<dyn MessageSink>) -> Self {
        self.sink = Some(sink);
        self
    }
}
```

Production wiring never sets the sink. Tests use a `CapturingSink` and
assert against `msg.formatted()` byte-for-byte.

### 5.5 Register the backend on the executor

Add the optional dep + feature in `executor/crates/executor-service/Cargo.toml`:

```toml
[dependencies]
aithericon-executor-smtp = { workspace = true, optional = true }

[features]
smtp = ["dep:aithericon-executor-smtp"]
```

Then add a match arm in `executor-service/src/main.rs::register_executor_backend`
keyed on the wire name:

```rust
#[cfg(feature = "smtp")]
"smtp" => {
    info!("smtp backend registered");
    registry.register(SmtpBackend::new())
}
```

`build_executor` iterates `aithericon_backends::BACKENDS`, filters to
`dispatch_mode == ExecutorJob`, and dispatches each entry to this
match. Unknown wire-names log a skip line; feature-disabled wire-names
log a skip line. The conformance test catches drift before it ships.

Finally, add the feature to the dev recipe so `just dev` builds with it
on by default — see `just/dev.just::executor_features`.

---

## 6. Service-side `validate` body

The hard work for compile-time validation lives inside
`BackendDecl::validate` (the fn-pointer set in §3.2). It does:

1. Deserialize the editor's config JSON into your `<Backend>Config` DTO.
2. Call `.validate()` for the local invariants.
3. Walk every Tera template + recipient string + from-override through
   `placeholder_refs::validate_placeholders(…)` so a typo like `{{
   user.emial }}` fails at publish time, with the precise field name in
   the error.
4. Validate attachment input-name uniqueness (or other backend-specific
   structural rules).
5. Re-serialize the validated config (canonical shape) and return it
   alongside the staged `InputDeclaration` list.

There is **no** central `validate_and_transform` match arm to edit —
the function in `compiler/backend_configs.rs` is a 13-line trampoline
that looks the decl up and calls `decl.validate`:

```rust
pub fn validate_and_transform(
    backend_type: &ExecutionBackendType,
    config: &Value,
    node_files: &HashMap<String, InputSource>,
    node_id: &str,
) -> Result<(Value, Vec<InputDeclaration>), CompileError> {
    let decl = crate::backends::lookup(*backend_type).ok_or_else(|| {
        CompileError::Compilation(format!("backend {:?} has no registered decl", backend_type))
    })?;
    let ctx = crate::backends::ValidationCtx { node_id, node_files };
    (decl.validate)(config, &ctx)
}
```

If your `validate` errors with `CompileError::BackendPlaceholderSyntax {
site, … }`, the editor surfaces it under the field whose `site` matches.

See `service/src/backends/smtp.rs::validate` for a reference body.

### 6.1 Ref scanner

If your backend has author-written templates that reference
upstream-step data (`{{ intake.email }}` in SMTP, `intake.email` in
Python), set `ref_scanner: Some(...)` on the decl. The scanner
function returns `Vec<RefSite>` — one entry per `<head>.<attr>` access
discovered in the config.

The unified compiler (`compiler/borrow.rs`) consumes those entries to:

- synthesize read-arcs into the producer's parked place so the
  prepare transition gets the upstream data staged,
- surface upstream refs for the frontend's instance-view "borrowed
  reads" badges,
- and route resource-named heads to the workspace `resources` table for
  envelope resolution.

There is **no** per-backend match arm in `compile.rs`, `token_shape.rs`,
or `compiler/resource_binding.rs` anymore — the registry drives them
all.

The shared `placeholder_refs::scan_placeholders` scanner handles `{{ a.b }}`
(Tera, HumanTask markdown, LLM prompts) and is what most backends call
from their `ref_scanner` body. For Python, see `python_refs::extract_python_refs`
(bare `<slug>.<attr>` identifiers). For a new language with different
lexical rules, add a sibling `<lang>_refs.rs` module that returns
`Vec<RefSite>` and call it from your decl's `ref_scanner`. Don't write
a parallel scanner for any backend that uses the `{{ }}` grammar —
reuse the shared one.

### 6.2 The unified borrow planner

A single prepare transition can collect borrows from multiple sources:
upstream producers (`<slug>.json`), workspace resources
(`<alias>.json`), and backend-field stage rewrites (e.g. Kreuzberg's
file paths). All three flow through `compiler/borrow.rs::collect_borrows`
which dispatches by `BackendDecl::borrow_shape`:

- `Envelope` (Python/SMTP) — whole producer envelope staged; backend
  reads `<slug>.json`.
- `PerField` (Kreuzberg/LLM) — per-field staging with `{{input:NAME}}`
  rewrite; the config's field value is replaced with the staged path
  before the executor sees it.

The legacy `BORROW_MARKER` multi-arm dance is gone (Phase 3 cleanup).
You don't need to think about it for a new backend — set `borrow_shape`
correctly and the unified planner handles the rest. The earlier
regression class ("resource arm silently no-ops because Python's arm
ate the marker") is structurally impossible now: there's one planner,
not multiple.

### 6.3 Publish-time resource discovery

`compiler/resource_binding.rs::collect_resource_heads` walks the graph
to find every resource name the workflow references, then queries the
workspace's `resources` table. It reads two fields off your decl:

- `resource_alias_paths` — static JSON paths where your config stores
  the alias name. SMTP: `&[&["resource_alias"]]`. FileOps:
  `&[&["storage", "resource_alias"], &["source_storage", …], &["destination_storage", …]]`.
- `ref_scanner` — any `<head>.<attr>` heads it returns are tried as
  resource names too (so `{{ mail.from_address }}` discovers `mail`
  even if the user didn't declare `resource_alias: "mail"` explicitly).

There is no per-backend match arm in `discover_known_resources` —
declaring `resource_alias_paths` correctly is the entire integration.

---

## 7. Frontend — config panel

Mirror an existing panel under
`app/src/lib/components/editor/panels/property-sections/automated/`. The
SMTP panel is at `SmtpConfigPanel.svelte`.

Rules:

- **Reuse `InsertRefButton`** for any text field that should accept a
  Tera-style upstream ref. The button drops a `{{ qualified.path }}`
  snippet at the end of the field.
- **Resource binding** — fetch workspace resources filtered by type:
  ```ts
  const page = await listResources({ resource_type: 'smtp', perPage: 200 });
  ```
- **Min `text-sm`** for every label / input — the workspace convention.
  No smaller, ever.
- **Defaults survive partial drafts** — every field reads
  `(config.foo as T | undefined) ?? defaultValue` so a freshly created
  step renders cleanly.

Wire the panel in **one** place: add the import and the entry to
`app/src/lib/editor/backend-panels.ts`:

```ts
import SmtpConfigPanel from '$lib/components/editor/panels/property-sections/automated/SmtpConfigPanel.svelte';

export const BACKEND_PANELS: Record<ExecutionBackendType, Component<any>> = {
    …
    smtp: SmtpConfigPanel,
    …
};
```

Compile-time exhaustiveness via `Record<ExecutionBackendType, …>`
makes "added a backend but forgot the panel" a build error. The
editor's `AutomatedStepSection.svelte` reads everything else from
`GET /api/v1/backends`:

- Backend picker labels come from `BackendMeta::display_name`.
- The default seed config comes from `BackendDecl::default_editor_config`.
- The Scheduled-toggle visibility comes from `BackendMeta::schedulable`.

You don't edit `AutomatedStepSection.svelte`, `defaultConfigs`,
`backendLabels`, or any `<Select.Item>` list. Those were collapsed to
registry consumption in the Phase 2.end refactor.

---

## 8. Frontend — instance-view renderer

The instance view dispatches output values through a predicate registry
at `app/src/lib/components/instances/output-renderers/index.ts`. Each
predicate returns `true` for one specific output shape; the registry
runs them in declaration order and picks the first match.

To add a renderer for your backend's output:

1. Write `<Name>Envelope.svelte` next to the existing envelopes (see
   `SmtpEnvelope.svelte` for the SMTP example).
2. Add a `matches<Name>` predicate to `index.ts`. The predicate must be
   structurally safe — no throws on null/missing/unexpected shapes.
3. Insert the registration into `REGISTRY` **before** any more-general
   predicate that would also match (the generic `key-value` predicate
   matches almost any object, so put your renderer earlier).

The SMTP predicate hinges on `outcome.type` being one of the known
SmtpOutcome variants AND `subject` being a string — distinctive enough
not to collide with other backends.

For failure rendering, branch on `outcome.type` and show a tailored
detail block per reason. This is the payoff for declaring the structured
outcome enum back in section 2: the renderer can match `"template_render"`
and show `outcome.file` + `outcome.error`, or match `"recipient_rejected"`
and show `outcome.failed_recipients`. Operators get actionable
diagnostics; no parsing raw error strings.

---

## 9. Failure handling

The wire contract is: `ExecutionResult.outputs["outcome"]` is a tagged
union with one variant per failure mode, plus `Success`. The tag string
is stable — the frontend renderer pattern-matches on it.

Steps:

1. Define the enum in `outcome.rs` with `#[serde(tag = "type", rename_all = "snake_case")]`.
2. Expose `reason() -> &'static str` returning the wire-name for each
   variant. The frontend matches on this.
3. Implement `classify_<protocol>_error(&Error) -> SmtpOutcome` that
   maps the protocol library's opaque error type to your structured
   variants. Use `is_timeout()`, `is_tls()`, response-code parsing —
   whatever the library exposes. When in doubt, fall back to a generic
   "server_error" with the raw response so the operator can still see it.
4. Document each variant in the guide (this section, for SMTP).
5. **The variant names are wire-stable once shipped.** Renaming requires
   coordinated changes in the renderer + every downstream workflow that
   filters on the field.

SMTP variants and what each surfaces:

| Variant            | When                                | Renderer detail                    |
|--------------------|-------------------------------------|------------------------------------|
| `success`          | Server accepted the message         | `message_id`, `recipients`         |
| `template_render`  | Tera failed to render a template    | `file`, `error` (rendered location)|
| `invalid_address`  | Rendered string isn't RFC 5322      | `field`, `value`, `error`          |
| `invalid_config`   | Bad port, missing `from`, etc.      | `message`                          |
| `connect_failed`   | DNS / TCP / no server               | `host`, `port`, `error`            |
| `tls_failed`       | TLS handshake failed                | `error`                            |
| `auth_failed`      | SMTP AUTH rejected (typically 535)  | `server_response`                  |
| `recipient_rejected` | Server rejected RCPT (550)        | `failed_recipients`, `server_response` |
| `server_error`     | Any other 5xx                       | `code`, `server_response`          |
| `timeout`          | Send exceeded the run timeout       | —                                  |
| `attachment_error` | File missing / too large            | `filename`, `error`                |

---

## 10. Tests

Four lanes — pick the ones that apply to your backend:

### 10.1 Backend unit tests (`executor-<name>/src/tests.rs`)

The bulk of testing happens here. Use the `MessageSink` (or equivalent)
test seam so no network is needed:

- Happy path — template renders, MIME assembled, sink captures expected bytes.
- Each failure variant — template error, invalid address, missing
  resolved_config, etc. — produces the expected structured outcome.
- Edge cases — multipart selection, attachment loading, port→mode
  mapping. SMTP has ~10 unit tests covering these; reference them when
  scoping yours.

### 10.2 Conformance / parity tests (`service/tests/registry_parity.rs`)

Add a fixture under `service/tests/fixtures/backends/<name>/` and a
test entry in `registry_parity.rs` that exercises your `BackendDecl::validate`
through the canonical compile path. Existing tests (search for
`smtp_minimal_config_compiles_through_registry`) are the model.

This lane covers:

- Minimal valid config compiles.
- Missing required fields produce the right `CompileError::Validation`.
- Malformed `{{ … }}` in any template surface raises
  `CompileError::BackendPlaceholderSyntax` with the right `site`.

### 10.3 Compile e2e (`service/src/compiler/compile.rs::tests` or `service/tests/compiler_e2e.rs`)

A complete graph with the new step produces an AIR with the right
shape. **Two tests are required if the backend binds a resource**:

1. **Upstream-producer borrow** — uses `compile_to_scenario` (no
   `KnownResources` needed). Asserts producer `<slug>.json` is staged
   and the backend discriminator is correct. Reference:
   `compile.rs::smtp_step_with_template_refs_wires_into_scenario`.
2. **Resource-envelope borrow** — uses
   `compile_to_air_with_subworkflows_and_interfaces` with a populated
   `KnownResources` map. Asserts the resource `<alias>.json` IS staged
   AND `__resources["<alias>"]` is referenced in the prepare logic.
   This is the path the live publish handler takes; without this test
   the compile-e2e in (1) passes while production silently breaks.
   Reference:
   `compile.rs::smtp_step_with_resource_alias_stages_resource_envelope`.

The two tests are not redundant. (1) goes through the simpler
`compile_to_scenario` entry; (2) only fires when `KnownResources` is
populated. If you skip (2), you can't tell from CI whether the resource
borrow path works.

### 10.4 Conformance tests (`executor-service/tests/conformance_<name>.rs`)

A real container, in CI. For SMTP this is `mailhog`:

```bash
just dev mailhog-up        # ports 1025 (SMTP) + 8025 (HTTP UI/API)
SMTP_E2E=1 cargo test -p aithericon-executor-service --test conformance_smtp
```

Gate via env var so default CI stays Docker-light.

**Honest scope warning.** A conformance test that hand-stages
`<alias>.json` (writing the resolved-resource JSON directly into the
test's `inputs_dir`) only exercises the backend's READ path. It does
NOT prove the compiler emits the borrow. The SMTP conformance suite
includes a test (`smtp_e2e_send_via_mailhog_lands_in_inbox`) that
takes this shortcut so the wire send can be verified without a full
mekhan-service running — but it's paired with the §10.3 (2) compile-e2e
that exercises the production publish path. **Don't skip the compile-e2e
just because conformance is green.** They cover different layers.

### 10.5 Demo loads + compiles

Add a `<name>_demo_loads_and_compiles` test to `service/src/demos.rs::tests`
that mirrors `email_welcome_demo_loads_and_compiles`. If the demo
binds a resource, the test must construct a `KnownResources` map with
that resource and call `compile_to_air_with_subworkflows_and_interfaces`
(NOT the no-resources `compile_to_air`) — otherwise the test compiles
the demo through the wrong code path and misses the resource-borrow
arm entirely.

---

## 11. Add a demo

Demos live at `demos/<name>/` with two files:

- `demo.json` — `{templateId, name, description}`. Pick a fresh
  `templateId` (UUID) and bake it into the test in section 10.5.
- `graph.json` — the visual graph + nodes + edges. Same shape as the
  editor would emit.

If your backend needs a typed resource, document the resource setup in
`demo.json::description` — the seeder doesn't auto-create resources, the
operator is expected to add one through `/resources`.

For SMTP: `demos/email-welcome/` is the canonical example. It uses
`mail` as the resource alias, expects the operator to create an
`smtp`-typed resource at `mail` pointing at the MailHog dev container.

---

## 12. OpenAPI regeneration + CI

After any schema-visible change (new `ExecutionBackendType` variant,
new DTO field, new handler), regenerate the OpenAPI spec + frontend types:

```bash
just dev::openapi
```

Which is:

```bash
cargo run --bin mekhan -- openapi > openapi-mekhan.json
(cd app && pnpm openapi:generate)
```

CI's `just ci::openapi-drift` fails any PR with stale generated files.
You'll catch it locally first if you run `just dev::openapi` before
committing.

---

## 13. Gotchas / known footguns

These all bit a previous backend author. They are not theoretical.

1. **Resource envelopes do NOT come through `resolved_config`.** They
   come through staged input files at `inputs/<alias>.json`. The
   `resolved_config` channel is for `{{secret:KEY}}` patterns inline in
   `spec.config`. Mixing them up gives the runtime error
   "smtp backend: resolved_config is absent — the launcher's resource
   resolver did not run for this step" with no obvious fix. See §5.3.

2. **A compile-e2e that doesn't pass `KnownResources` doesn't test
   resource borrows.** Use `compile_to_air_with_subworkflows_and_interfaces`
   for the resource-borrow assertion, not `compile_to_scenario`. See §10.3.

3. **A conformance test that hand-stages files bypasses the compiler.**
   It proves the backend reads correctly but says nothing about
   whether the compiler emits the right borrows. Pair it with a
   compile-e2e against the publish entry. See §10.4.

4. **A demo test that uses `compile_to_air` (no resources) on a
   resource-binding demo compiles the demo through the wrong code
   path.** It will pass even when the production publish flow is
   broken for that demo. Use
   `compile_to_air_with_subworkflows_and_interfaces` with the
   right `KnownResources` populated. See §10.5.

5. **Don't write a new `<lang>_refs.rs` scanner for `{{ }}` grammar
   surfaces.** `placeholder_refs::scan_placeholders` already handles
   `{{ <head>.<attr> }}` for Tera, HumanTask markdown, LLM prompts,
   and Kreuzberg file refs. A new sibling scanner is only needed for a
   genuinely different lexer (Python's bare-identifier `<slug>.<attr>`
   is the canonical other case). See §6.1.

6. **The Tera scanner only catches `{{ }}` placeholder bodies.** Tera
   also supports `{% if user.active %}` block syntax — refs inside
   `{%...%}` are NOT picked up by the compile-time scope checker. v1
   accepts this limitation; document it on your backend's panel if
   relevant. See `placeholder_refs.rs`.

7. **Forgetting to add your feature to `just/dev.just::executor_features`**
   means `just dev` builds an executor without your backend. Symptom:
   `register_executor_backend` logs "backend '<name>' declared in
   aithericon-backends but not built into this executor binary —
   skipping," `supports()` returns false at runtime, the registry's
   dispatch picks no backend, the job sits in Accepted forever.

8. **`from_spec` deserialization fails silently when a field rename
   doesn't match the JSON.** `serde_json::from_value` returns
   `Result<Self, Error>` but the error string is often unhelpful when
   the schema has many optional fields. If the editor's draft + your
   DTO disagree, lean on `#[serde(default)]` for every optional field
   so partial drafts round-trip cleanly. See §4.

9. **Republish required after fixing a compile bug.** A workflow's
   AIR is persisted at publish time. If a compiler change (new
   borrow, scanner extension, validate fix) is needed, every affected
   template must be re-published — the in-DB AIR is frozen until
   then. Existing instances keep using the old AIR. Communicate this
   in the PR description when shipping a fix.

10. **The `BackendDecl::validate_ref_kind` validator runs per-site,
    per-resolved-ref.** Most backends use `accept_any_ref_kind`. If
    you set a custom validator, remember it fires once per
    `<slug>.<field>` access at every site your `ref_scanner` returns
    — failing one ref in one site fails the whole publish. LLM's
    `validate_ref_kind` enforces `images[].path → File` and content
    sites → not-File; cargo-cult that pattern only if your backend has
    similar per-site kind constraints.

11. **`BackendMeta` and `BackendDecl` must agree on `backend_type`.**
    The conformance test catches drift; don't paper over it by
    hand-syncing fields. Always reference the same `*_META` const
    from both `shared/backends` and `service/src/backends/<name>.rs`.

---

## 14. The Checklist

Use this when shipping. Each item maps to a section above.

**Cross-crate metadata (§3.1)**
- [ ] `ExecutionBackendType` variant added in `shared/backends/src/types.rs`
- [ ] `as_wire_str` arm for the new variant
- [ ] `<NAME>_META` const in `shared/backends/src/registry.rs` (display name, icon, dispatch mode, schedulable, resource channel)
- [ ] `<NAME>_META` added to the `BACKENDS` slice
- [ ] `<NAME>_META` re-exported from `shared/backends/src/lib.rs`

**Compile-time decl (§3.2)**
- [ ] `service/src/backends/<name>.rs` with `BackendDecl` referencing `<NAME>_META`
- [ ] Module + decl registered in `service/src/backends/mod.rs::BACKENDS`
- [ ] `default_output_fields`, `default_editor_config`, `validate`, `ref_scanner`, `resource_alias_paths`, `consumes_declared_outputs`, `pyi_introspection`, `borrow_shape`, `validate_ref_kind` all set deliberately
- [ ] DSL parser arm in `service/src/bin/cli/formats/dsl.rs` if CLI DSL support is wanted

**Wire-format DTO (§4)**
- [ ] DTO in `executor-backend-configs/src/<name>.rs` with `#[serde(default)]` everywhere
- [ ] Module exported in `executor-backend-configs/src/lib.rs`
- [ ] Resolved-resource view struct (if backend binds a resource), with `from_resolved_value` (canonical) + `from_resolved` (harness fallback)

**Runtime impl (§5)**
- [ ] Crate `executor-<name>/` added, in workspace `members` + `workspace.dependencies` (§5.1)
- [ ] `ExecutionBackend` impl returns structured outcome variant, respects cancel + timeout (§5.2)
- [ ] **Resource view** read from `staged_inputs["<alias>.json"]` (production) with `resolved_config` fallback for harness tests — never logs either (§5.3)
- [ ] `MessageSink`-style unit test seam in place (§5.4)
- [ ] Feature flag in `executor-service/Cargo.toml`
- [ ] Match arm in `executor-service/src/main.rs::register_executor_backend` (§5.5)
- [ ] Feature added to `just/dev.just::executor_features` so `just dev` builds it

**Service-side compiler (§6)**
- [ ] `validate` body uses `placeholder_refs::validate_placeholders` on every template surface
- [ ] `ref_scanner` returns the full set of `<head>.<attr>` refs from all template/source surfaces (§6.1)
- [ ] `resource_alias_paths` covers every JSON path your config carries an alias on (§6.3)
- [ ] `borrow_shape` set correctly: `Envelope` for whole-`<slug>.json` consumers (Python/SMTP), `PerField` for per-field staging (Kreuzberg/LLM) (§6.2)

**Frontend (§7, §8)**
- [ ] `<Name>ConfigPanel.svelte` + entry in `app/src/lib/editor/backend-panels.ts::BACKEND_PANELS`
- [ ] `<Name>Envelope.svelte` renderer + predicate in `output-renderers/index.ts`
- [ ] Per-failure-mode detail block in the renderer (§9)

**Tests (§10)**
- [ ] Unit tests in backend crate (§10.1)
- [ ] Fixture + entry in `service/tests/registry_parity.rs` (§10.2)
- [ ] **Two** compile-e2e tests if backend binds a resource: upstream-producer AND resource-envelope (with `KnownResources` populated) (§10.3)
- [ ] Conformance test against a real container, gated by env var (§10.4)
- [ ] Demo at `demos/<name>/` + loader test using `compile_to_air_with_subworkflows_and_interfaces` + `KnownResources` if the demo binds a resource (§10.5, §11)

**Release hygiene**
- [ ] `just dev::openapi` run; `openapi-mekhan.json` + `schema.d.ts` committed (§12)
- [ ] Re-read §13 — verify none of the listed footguns apply
- [ ] `cargo check --workspace` + `cargo test --workspace` green (root level)
- [ ] `(cd executor && cargo check)` green (separate workspace)
- [ ] `(cd app && pnpm exec svelte-check)` clean
- [ ] `just ci::openapi-drift` clean
- [ ] `cargo test -p mekhan-service --test backend_registry_coverage` green (the conformance test that asserts bijection across `shared/backends` ↔ `service/src/backends`)
