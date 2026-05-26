# Adding a New Executor Backend

This is the end-to-end recipe for adding a new executor backend to the
Aithericon platform — Rust crate, service-side compiler arm, frontend
config panel, instance-view renderer, failure handling, demo, and tests.

The **SMTP backend** (added at commit `5d595ee`) is the worked
example throughout — every section cites real file paths from the merged
implementation. If a step here looks abstract, open the SMTP file in the
same section to see exactly what it became in practice.

> **Read §13 (Gotchas) before §3.** The SMTP backend shipped with a
> latent regression — resource envelopes silently dropped on nodes
> that also had upstream-producer borrows. That fix landed alongside
> the guide. The §13 footguns enumerate the failure modes that bit a
> previous author so you can avoid them rather than re-discover them.

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

Then wire the crate into the workspace by editing `executor/Cargo.toml`
(add to `members` + `workspace.dependencies`).

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
for each resource the workflow references; `apply_resource_borrows`
splices a `job_inputs.push(#{ "name": "<alias>.json", "source": #{
"type": "inline", "value": __resources["<alias>"] } });` snippet into
the prepare transition. At publish time the resolver computes the
envelope (public fields inline + `{{secret:...}}` templates for secret
fields) and the launcher splices `let __resources = #{ ... };` at the
top of the prepare logic. When the run fires, `PlanSecretsHook` walks
inline-source JSON values (`InputSource::Inline { value }`), resolves
embedded secret patterns, then `StageInputsHook` writes the resolved
JSON to `inputs/<alias>.json`. The backend reads it back:

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
type. Don't write a parallel scanner for any backend that uses the
`{{ }}` grammar — reuse the shared one.

### 6.2 The `BORROW_MARKER` multi-arm contract

A single prepare transition can collect borrows from multiple sources:
upstream producers (`apply_python_borrows`), workspace resources
(`apply_resource_borrows`), and backend-field stage rewrites
(`apply_backend_borrows`). All three splice their `job_inputs.push(...)`
snippets into the same `/*__BORROWED_INPUTS__*/` sentinel in the
lowered Rhai.

**Each arm must PREPEND its pushes before the marker, not REPLACE it.**
The canonical pattern is:

```rust
let replacement = format!("{pushes}{BORROW_MARKER}");
let new_source = source.replace(BORROW_MARKER, &replacement);
```

After all arms finish, `strip_borrow_markers` (called at the bottom of
`apply_borrows`) removes the residual marker. A `replace(BORROW_MARKER,
&pushes)` consumes the marker, and any subsequent arm silently no-ops.
This is exactly the regression that hit the SMTP backend: an SMTP step
with both a `{{ intake.email }}` upstream borrow and a `resource_alias:
"mail"` resource borrow had its resource arm skipped because Python's
arm ran first and ate the marker.

**Test it**: any backend that mixes borrow types on the same node must
have a compile-e2e (§10.3) that asserts BOTH `<slug>.json` (upstream)
AND `<alias>.json` (resource) land in the prepare transition. The
existing SMTP test
`compile.rs::smtp_step_with_resource_alias_stages_resource_envelope`
is the model.

### 6.3 Publish-time resource discovery

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
populated AND when the borrow-marker composition works correctly across
multi-arm dispatch (see §6.2). If you skip (2), the regression class
"resource arm silently no-ops" stays uncovered.

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

2. **`BORROW_MARKER` is shared across arms — don't consume it.** If
   your backend introduces a new arm or your existing arm uses
   `String::replace(BORROW_MARKER, &pushes)` without preserving the
   marker, you break multi-arm composition silently. The correct
   pattern is `format!("{pushes}{BORROW_MARKER}")` as replacement.
   `strip_borrow_markers` cleans up. See §6.2.

3. **A compile-e2e that doesn't pass `KnownResources` doesn't test
   resource borrows.** Use `compile_to_air_with_subworkflows_and_interfaces`
   for the resource-borrow assertion, not `compile_to_scenario`. See §10.3.

4. **A conformance test that hand-stages files bypasses the compiler.**
   It proves the backend reads correctly but says nothing about
   whether the compiler emits the right borrows. Pair it with a
   compile-e2e against the publish entry. See §10.4.

5. **A demo test that uses `compile_to_air` (no resources) on a
   resource-binding demo compiles the demo through the wrong code
   path.** It will pass even when the production publish flow is
   broken for that demo. Use
   `compile_to_air_with_subworkflows_and_interfaces` with the
   right `KnownResources` populated. See §10.5.

6. **Don't write a new `<lang>_refs.rs` scanner for `{{ }}` grammar
   surfaces.** `placeholder_refs::scan_placeholders` already handles
   `{{ <head>.<attr> }}` for Tera, HumanTask markdown, LLM prompts,
   and Kreuzberg file refs. A new sibling scanner is only needed for a
   genuinely different lexer (Python's bare-identifier `<slug>.<attr>`
   is the canonical other case). See §6.1.

7. **The Tera scanner only catches `{{ }}` placeholder bodies.** Tera
   also supports `{% if user.active %}` block syntax — refs inside
   `{%...%}` are NOT picked up by the compile-time scope checker. v1
   accepts this limitation; document it on your backend's panel if
   relevant. See `placeholder_refs.rs`.

8. **Forgetting to add your feature to `just/dev.just::executor_features`**
   means `just dev` builds an executor without your backend. Symptom:
   `supports()` returns false at runtime, the registry's first-match
   dispatch picks no backend, the job sits in Accepted forever.

9. **`from_spec` deserialization fails silently when a field rename
   doesn't match the JSON.** `serde_json::from_value` returns
   `Result<Self, Error>` but the error string is often unhelpful when
   the schema has many optional fields. If the editor's draft + your
   DTO disagree, lean on `#[serde(default)]` for every optional field
   so partial drafts round-trip cleanly. See §4.

10. **Republish required after fixing a compile bug.** A workflow's
    AIR is persisted at publish time. If a compiler change (new
    borrow, scanner extension, marker fix) is needed, every affected
    template must be re-published — the in-DB AIR is frozen until
    then. Existing instances keep using the old AIR. Communicate this
    in the PR description when shipping a fix.

---

## 14. The Checklist

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
- [ ] **Resource view** read from `staged_inputs["<alias>.json"]` (production)
      with `resolved_config` fallback for harness tests — never logs either (§5.3)
- [ ] `MessageSink`-style unit test seam in place (§5.4)
- [ ] Feature flag + registry registration in `executor-service` (§5.5)
- [ ] Feature added to `just/dev.just::executor_features` so `just dev` builds it (§5.5)
- [ ] `validate_and_transform` arm with placeholder syntax validation (§6)
- [ ] Borrow scanner arms in `populate_borrowed_paths`, `automated_step_borrow_plan`,
      `automated_step_resource_borrow_plan` (§6.1)
- [ ] **`BORROW_MARKER` splice prepends the marker, doesn't consume it** —
      `strip_borrow_markers` does final cleanup (§6.2)
- [ ] Resource discovery arm in `discover_known_resources` (§6.3)
- [ ] `<Name>ConfigPanel.svelte` panel + wired into `AutomatedStepSection` (§7)
- [ ] `<Name>Envelope.svelte` renderer + predicate in `value-renderers/index.ts` (§8)
- [ ] Per-failure-mode detail block in the renderer (§9)
- [ ] Unit tests in backend crate (§10.1)
- [ ] `backend_configs::Smtp` arm unit tests (§10.2)
- [ ] **Two** compile-e2e tests if backend binds a resource: upstream-producer
      AND resource-envelope (with `KnownResources` populated) (§10.3)
- [ ] Conformance test against a real container, gated by env var (§10.4)
- [ ] Demo at `demos/<name>/` + loader test using
      `compile_to_air_with_subworkflows_and_interfaces` + `KnownResources`
      if the demo binds a resource (§10.5, §11)
- [ ] `just dev::openapi` run; `openapi-mekhan.json` + `schema.d.ts` committed (§12)
- [ ] Re-read §13 — verify none of the listed footguns apply
- [ ] `cargo check --workspace` + `cargo test --workspace` green (root level)
- [ ] `(cd app && pnpm exec svelte-check)` clean
- [ ] `just ci::openapi-drift` clean
