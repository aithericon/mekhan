# Adding a New Executor Backend

This is the end-to-end recipe for adding a new executor backend to the
Aithericon platform — Rust crate, service-side compiler arm, frontend
config panel, instance-view renderer, failure handling, demo, and tests.

The **SMTP backend** (added at commit `<smtp-merge-sha>`) is the worked
example throughout — every section cites real file paths from the merged
implementation. If a step here looks abstract, open the SMTP file in the
same section to see exactly what it became in practice.

This guide assumes you already know:

- Rust workspaces (the executor lives in its own workspace under
  `executor/`; mekhan-service is the umbrella's only direct member).
- The control-data token model (`docs/10-control-data-token-model.md`).
- The resource model (`shared/resources/` + `service/src/petri/resource_resolver.rs`).
  Optional — you can add a backend that doesn't consume a resource.

---

## 1. What a backend is

A backend is one implementation of the `ExecutionBackend` trait at
`executor/crates/executor-backend/src/traits.rs`:

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

Backends are dispatched by `BackendRegistry::find` (one per executor process,
populated at boot in `executor-service/src/main.rs::build_executor`). The
match key is the `spec.backend` string — `"python"`, `"http"`, `"smtp"`, …

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
3. **Resource binding?** — does the backend need credentials from a
   workspace-scoped `Resource` (the typed credential surface in
   `shared/resources/`)? If yes, the resource type must exist there before
   you start; the launcher's `ResourceResolver` is what hands you the
   resolved view at run time. SMTP consumes `shared/resources/src/types.rs::Smtp`.
4. **Authoring surface** — does the workflow author write *code/templates*
   (Python source, Tera templates, SQL) or just *config* (HTTP URL +
   method, Docker image)? If yes, decide whether the source lives in node
   files (Python-style: per-node Y.Map, IDE-editable, scanned at compile
   time for refs) or inline in the config (SMTP-style: embedded string
   plus a `label` for diagnostics).
5. **Output shape** — what does `ExecutionResult.outputs` look like? Pick
   field names the editor's port picker will surface. SMTP emits
   `outcome` (structured) + `subject`, `body_text_preview?`,
   `body_html_preview?`. The shape pins the wire contract — once a
   workflow downstream of this step references one of these fields,
   renaming it breaks the workflow.
6. **Failure model** — every error class that a workflow author or
   operator will want to filter on. Define them up front as an enum and
   commit to the names; the frontend instance-view renderer pattern-matches
   on them.

SMTP's failure model:
`Success | TemplateRender | InvalidAddress | InvalidConfig | ConnectFailed
| TlsFailed | AuthFailed | RecipientRejected | ServerError | Timeout |
AttachmentError`

---

## 3. Register the backend type in mekhan

mekhan models every backend through the `ExecutionBackendType` enum at
`service/src/models/template.rs` (around line 1645). Add a variant:

```rust
pub enum ExecutionBackendType {
    Python, Process, Docker, Http, Llm, FileOps, Kreuzberg,
    Smtp,                  // ← new
    CatalogueQuery,
}
```

Three pinned methods need the new arm:

- `as_wire_str` — snake_case wire string (`"smtp"`). The serde derive does
  the JSON shape; this method is for backend-name comparisons in code.
- `default_output_port` (same file, ~line 1345) — the canonical output
  port the editor displays when the user resets to default. SMTP:
  ```rust
  ExecutionBackendType::Smtp => vec![
      port_field("outcome", "Outcome", FieldKind::Json),
      port_field("subject", "Subject", FieldKind::Text),
      port_field("body_text_preview", "Body (text)", FieldKind::Textarea),
      port_field("body_html_preview", "Body (html)", FieldKind::Textarea),
  ],
  ```
- The DSL-format parser's "unknown backend" error string (search for
  `"expected one of: python, process, …"`). Add your wire-name to the
  list so the error message stays useful.

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
   service-side compiler arm (next step).

If the backend consumes a typed resource, also define a "resolved view"
struct in this file. SMTP's is `ResolvedSmtpResource` plus a constant
key `"smtp_resource"` the launcher's resolver writes under. The backend
reads it back from `RunContext.resolved_config`.

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

Then wire the crate into the workspace by editing `executor/Cargo.toml`
(add to `members` + `workspace.dependencies`).

### 5.2 The trait impl

The minimal `lib.rs` does six things:

1. Deserialize `SmtpConfig::from_spec(&ctx.spec)` and call `.validate()`.
2. Pull the resolved resource view from `ctx.resolved_config`. If it's
   `None` and your backend needs one, fail with `SmtpOutcome::InvalidConfig`.
3. Build the runtime context (Tera context, HTTP client, DB connection).
4. Render / dispatch / send.
5. Map results to `ExecutionResult` — `outcome` field +
   `outputs["outcome"]` carrying the structured detail. The
   `ExecutionOutcome` enum from `executor-domain` is intentionally
   coarse (`Success | BackendError | TimedOut | Cancelled`); the
   structured per-backend reason lives in the outputs map.
6. Respect the cancellation token (`tokio::select! { _ = cancel.cancelled() => …, … }`).

See `executor/crates/executor-smtp/src/lib.rs` for the full implementation.

### 5.3 The `resolved_config` side-channel

`RunContext.resolved_config` is `#[serde(skip)]` — populated by the
worker's `PlanSecretsHook` at staging time, never serialized to disk
or logged. The HTTP backend at
`executor/crates/executor-http/src/lib.rs::prepare` is the reference
pattern:

```rust
let mut config = match run_context.resolved_config.as_ref() {
    Some(resolved) => serde_json::from_value::<HttpConfig>(resolved.clone())?,
    None           => HttpConfig::from_spec(&run_context.spec)?,
};
```

The fallback to `spec.config` keeps the no-secrets test path simple.

**Never log a `resolved_*` field** (env, config, input_storage,
output_storage). `RunContext`'s `Debug` impl is hand-written to elide
them precisely so a stray `tracing::debug!(?ctx, …)` can't leak.

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

### 5.5 Register the backend

Add to `executor/crates/executor-service/Cargo.toml`:

```toml
[dependencies]
aithericon-executor-smtp = { workspace = true, optional = true }

[features]
smtp = ["dep:aithericon-executor-smtp"]
```

And to `executor-service/src/main.rs::build_executor`:

```rust
#[cfg(feature = "smtp")]
{
    registry = registry.register(SmtpBackend::new());
    info!("smtp backend registered");
}
```

Finally, add the feature to the dev recipe so `just dev` builds with it
on by default — see `just/dev.just::executor_features`.

---

## 6. Service-side compiler arm

In `service/src/compiler/backend_configs.rs::validate_and_transform`, add
a `Smtp` arm that:

1. Deserializes the editor's config JSON into `SmtpConfig`.
2. Calls `.validate()` for the local invariants.
3. Walks every Tera template + recipient string + from-override through
   `validate_placeholders(…)` so a typo like `{{ user.emial }}` fails at
   publish time, with the precise field name in the error.
4. Validates attachment input-name uniqueness.
5. Re-serializes the validated config (canonical shape) and returns it.

See `service/src/compiler/backend_configs.rs` (the `Smtp` arm at the
bottom of `validate_and_transform`).

### 6.1 Borrow scanner integration

If your backend has author-written code or templates that reference
upstream-step data (`{{ intake.email }}` in SMTP, `intake.email` in
Python), three sites need to know about it:

- `service/src/compiler/compile.rs::populate_borrowed_paths` — surfaces
  upstream refs for the frontend's instance-view "borrowed reads" badges.
  Add a `match` arm on `ExecutionBackendType::Smtp` that calls
  `placeholder_refs::scan_placeholders` over the template / recipient /
  from-override sources.
- `service/src/compiler/token_shape.rs::automated_step_borrow_plan` —
  synthesizes read-arcs into the producer's parked place so the
  prepare transition gets the upstream data staged. Add an arm that
  uses the same scanner.
- `service/src/compiler/token_shape.rs::automated_step_resource_borrow_plan`
  — same shape, but matches against workspace resources (e.g. `{{ mail.from_address }}`
  where `mail` is the SMTP resource binding). Always include the
  declared `resource_alias` in the scan results so the launcher's
  `ResourceResolver` knows which resource to splice even when the
  templates don't directly reference its public fields.

The shared `placeholder_refs.rs` scanner already handles `{{ a.b }}`
(Tera, HumanTask markdown, LLM prompts). For Python the scanner is
`python_refs.rs`. For a new language with different lexical rules, add
a sibling `<lang>_refs.rs` module that returns `Vec<PlaceholderRef>`
(the canonical pair shape) and dispatch from the call sites by backend
type.

### 6.2 Publish-time resource discovery

`service/src/process/publish.rs::discover_known_resources` walks the
graph to find every resource name the workflow references, then queries
the workspace's `resources` table. Extend the per-node `match` to scan
your backend's template surfaces the same way it scans Python source:

```rust
ExecutionBackendType::Smtp => {
    for (head, _) in crate::compiler::token_shape::smtp_template_placeholder_refs(&execution_spec.config) {
        heads.insert(head);
    }
    if let Some(alias) = execution_spec.config.get("resource_alias").and_then(|v| v.as_str()) {
        if !alias.is_empty() { heads.insert(alias.to_string()); }
    }
}
```

The launcher's resolver picks up from there — no changes needed in
`service/src/petri/resource_resolver.rs`.

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
- **Min `text-sm`** for every label / input — the workspace convention
  (`feedback_min_text_sm` memory). No smaller, ever.
- **Defaults survive partial drafts** — every field reads
  `(config.foo as T | undefined) ?? defaultValue` so a freshly created
  step renders cleanly.

Wire the panel in two places in
`app/src/lib/components/editor/panels/property-sections/AutomatedStepSection.svelte`:

1. Add to the `defaultConfigs` record (seeds a sensible initial config
   when the user picks the new backend type).
2. Add to the `backendLabels` record + the dropdown `<Select.Item>` list.
3. Add the `{:else if data.executionSpec.backendType === 'smtp'}` arm.

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

### 10.2 Compiler unit tests (`service/src/compiler/backend_configs.rs`)

Covers the service-side validation arm:

- Minimal valid config compiles.
- Missing required fields produce the right `CompileError::Validation`.
- Malformed `{{ … }}` in any template surface raises
  `CompileError::BackendPlaceholderSyntax` with the right `site`.

### 10.3 Compile e2e (`service/src/compiler/compile.rs`)

A complete graph with the new step produces an AIR with:

- The right backend discriminator (`"smtp"`).
- Read-arcs into the upstream producers your templates reference.
- Staged-input filenames (`"intake.json"`) for the runtime's context
  builder.

See `compile.rs::smtp_step_with_template_refs_wires_into_scenario`.

### 10.4 Conformance tests (`executor-service/tests/conformance_<name>.rs`)

A real container, in CI. For SMTP this is `mailhog`:

```bash
just dev mailhog-up        # ports 1025 (SMTP) + 8025 (HTTP UI/API)
SMTP_E2E=1 cargo test -p aithericon-executor-service --test conformance_smtp
```

Gate via env var so default CI stays Docker-light.

### 10.5 Demo loads + compiles

Add a `<name>_demo_loads_and_compiles` test to `service/src/demos.rs::tests`
that mirrors the existing `llm_smoke_demo_loads_and_compiles` — proves
the demo doesn't drift away from the wire format.

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

## 13. The Checklist

Use this when shipping. Each item maps to a section above.

- [ ] Wire-name + display name + icon decided (§2)
- [ ] Resource binding (if any) — type already exists in `shared/resources/src/types.rs` (§2)
- [ ] `ExecutionBackendType` variant added in `service/src/models/template.rs` (§3)
- [ ] `default_output_port` arm added (§3)
- [ ] "Unknown backend" error string updated (§3)
- [ ] DTO in `executor-backend-configs/src/<name>.rs` with `#[serde(default)]` everywhere (§4)
- [ ] Module exported in `executor-backend-configs/src/lib.rs` (§4)
- [ ] Crate `executor-<name>/` added, in workspace `members` + `workspace.dependencies` (§5.1)
- [ ] `ExecutionBackend` impl returns structured outcome variant, respects cancel + timeout (§5.2)
- [ ] Consumes `resolved_config` side-channel — never logs it (§5.3)
- [ ] `MessageSink`-style unit test seam in place (§5.4)
- [ ] Feature flag + registry registration in `executor-service` (§5.5)
- [ ] Feature added to `just/dev.just::executor_features` so `just dev` builds it (§5.5)
- [ ] `validate_and_transform` arm with placeholder syntax validation (§6)
- [ ] Borrow scanner arms in `populate_borrowed_paths`, `automated_step_borrow_plan`,
      `automated_step_resource_borrow_plan` (§6.1)
- [ ] Resource discovery arm in `discover_known_resources` (§6.2)
- [ ] `<Name>ConfigPanel.svelte` panel + wired into `AutomatedStepSection` (§7)
- [ ] `<Name>Envelope.svelte` renderer + predicate in `value-renderers/index.ts` (§8)
- [ ] Per-failure-mode detail block in the renderer (§9)
- [ ] Unit tests in backend crate (§10.1)
- [ ] `backend_configs::Smtp` arm unit tests (§10.2)
- [ ] Compile e2e test (§10.3)
- [ ] Conformance test against a real container, gated by env var (§10.4)
- [ ] Demo at `demos/<name>/` + loader test (§10.5, §11)
- [ ] `just dev::openapi` run; `openapi-mekhan.json` + `schema.d.ts` committed (§12)
- [ ] `cargo check --workspace` + `cargo test --workspace` green (root level)
- [ ] `(cd app && pnpm exec svelte-check)` clean
- [ ] `just ci::openapi-drift` clean
