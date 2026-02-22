# SOP Migration Strategy: Legacy Web-Platform to Petri-Lab / Human-UI / Aithericon-Executor

> Produced by the coordinator, synthesizing findings from legacy-expert and petri-expert (Feb 2025)

---

## 1. Feature-by-Feature Mapping

### 1.1 SOP Templates -> Petri-Lab SDK Scenario Definitions

| Legacy Feature | New Framework Equivalent | Notes |
|---|---|---|
| `SopTemplate` (name, description, phases, steps) | SDK `Context` + `definition()` function compiled to AIR JSON | SDK scenarios are code-defined in Rust, not DB-stored. Deployed via `--deploy` to engine HTTP API. |
| Template versioning (`version`, `parent_id`, `is_latest`) | Git-versioned scenario code + AIR JSON artifacts | No built-in version management in petri-lab; needs external versioning strategy |
| Template publishing (`published`, `published_at`) | Deployment to engine (scenario deployed = published) | Binary published/unpublished state maps to deployed/not-deployed |
| `PhaseTemplate` (name, position, batchable, slug) | SDK `ctx.scope("Phase Name", \|ctx\| { ... })` for grouping | Scopes are metadata for visualization; engine ignores them. Ordering is implicit in token flow, not explicit position numbers. |
| `StepTemplate` (input_type, input_config, output_type, output_config) | Human task transitions with form schema in token data (`steps` + `blocks` array) | Human-UI renders blocks: mdsvex, input (text/textarea/number/select/checkbox/file/signature), download, table, image, callout, pdf, divider |
| Step properties: `is_optional`, `is_repeatable`, `max_repetitions` | Guard conditions + retry pattern with counter in token data | No native "optional step" concept; must be modeled as conditional transitions with guards |
| `jump_to_on_success` / `jump_to_on_failure` | Guard-based branching transitions in the Petri net | `#[guard("condition")]` on competing transitions naturally model conditional routing |
| `slug` on phases/steps | Place/transition IDs in SDK | Direct mapping - IDs serve same purpose |
| `StepConcurrencyMode` (Bulk/Sequential/Hybrid) | Inherent in Petri net topology | Petri nets are naturally concurrent; sequential behavior requires explicit ordering via token flow |
| `PhaseProcessingMode` (StepCentric/InstanceCentric) | Net topology design choice | StepCentric = batch all instances at each step; InstanceCentric = each instance progresses independently |

### 1.2 SOP Instances -> Petri Net Instances

| Legacy Feature | New Framework Equivalent |
|---|---|
| `SopInstance` (state, progress tracking) | Net instance with event-sourced state via `DomainEvent` stream |
| `ProgressState` (New/InProgress/Done/Failed/Canceled/Skipped) | Net lifecycle: Created -> running -> Completed/Failed/Cancelled (via terminal places + lifecycle events) |
| `current_phase_id` / `current_step_id` | Current marking (which places have tokens) - queryable via API |
| `completed_steps` / `completed_phases` counters | Derivable from event log (count `TransitionFired` events) |
| `started` / `end` / `duration` timestamps | `NetCreated` / `NetCompleted` events with timestamps |
| `PhaseInstance` (operator_id, location_id, state) | Token data carries operator/location context; state derived from marking |
| `StepInstance` (input_value, output_value, error) | Token data at places; effect results in event log |
| Step repetitions tracking | Counter field in token, guard checks `token.repetitions < max` |
| Cancel SOP instance | `NetCancelled` event via API |

### 1.3 Batch Controller -> Campaign/Fan-Out Pattern

| Legacy Feature | New Framework Equivalent |
|---|---|
| `BatchController` (groups SOP instances for parallel processing) | Campaign net with fan-out/fan-in via cross-net bridge (see `five_layer_campaign_net.rs`) |
| Batch creation with auto-provisioning devices | Campaign net `init` transition that fans out to N child nets via `bridge_out` |
| `batch_update_step_instances` (bulk step update) | Campaign net receives signals from all child nets; fan-in transition collects results |
| Batch state tracking (batch_size, failed_instances_amount) | Aggregation logic in fan-in transition; state derived from child net completion events |
| `SopInstanceSyncStatus` (Aligned/Lagging/Detached/Exception) | Advisory state via KV projections (ADR-08); metadata materialized from event stream |
| `bulk_step_cursor` | Campaign net's current step tracking via token data |
| Assign/unassign instances to batch | Dynamic net creation via `CreateNetListener` + bridge connections |

### 1.4 Procedure Elements -> Human-UI Block Types

| Legacy Input Type | Human-UI Block Type | Status |
|---|---|---|
| `text` (TextInputBlueprint) | `{ type: "input", field: { kind: "text"/"textarea"/"number" } }` | READY |
| `checklist` (ChecklistBlueprint) | Multiple `{ type: "input", field: { kind: "checkbox" } }` blocks | READY |
| `fileUpload` (FileUploadBlueprint) | `{ type: "input", field: { kind: "file" } }` | READY |
| `openApi` (OpenApiRequestBlueprint) | Effect transition in Petri net (executor_submit) | Different paradigm: API calls are automated effects, not human steps |
| `labelPrint` (LabelPrintBlueprint) | **GAP** - no equivalent block type | Needs custom block type in human-ui OR separate effect transition |
| `javascript` (JavaScriptBlueprint) | Rhai scripting in transitions OR Python executor | Different execution model: code runs in engine, not in browser |
| Context variable selector | Token data flow (data passes between transitions automatically) | Natural mapping - tokens carry all context data |
| Context store (phaseSlug.stepSlug.property access) | Token data accumulated through net traversal | Rhai scripts access all input port data |

### 1.5 Frontend UI -> Human-UI

| Legacy Feature | Human-UI Equivalent | Status |
|---|---|---|
| SOP template editor (create/edit phases, steps, drag-drop) | **GAP** - no template editor exists | Human-UI is task execution only; template editing is code-based in SDK |
| SOP instance execution UI (step-by-step wizard) | Task page (`/task/[id]`) with stepper, form fields, validation | READY |
| Phase selector / phase cards | Process tracking page (`/process/[id]`) with timeline | READY |
| Batch controller UI (bulk operations, progress grid) | **GAP** - no batch/campaign UI exists | Would need new pages in human-ui |
| Instance stepper (progress indicator) | Stepper component in task page | READY |
| Real-time updates | SSE-based live updates from NATS | READY |
| Draft persistence (localStorage) | Draft persistence in task page | READY |
| PDF export of completed tasks | Print/export button on completed tasks | READY |
| Signature capture | SignaturePad block with audit trail | READY |

---

## 2. Gap Analysis

### Critical Gaps (Must Build)

1. **SOP Template Editor / Management UI**
   - Legacy has a full CRUD editor for templates, phases, steps with drag-drop ordering
   - New framework: scenarios are Rust code (SDK) compiled to AIR JSON
   - **Options:**
     a. Build a visual scenario editor in human-ui that generates AIR JSON (major effort)
     b. Keep scenarios as code and provide a CLI/admin tool for deployment (simpler)
     c. Build a lightweight "SOP builder" that generates SDK Rust code from a form (hybrid)
   - **Recommendation:** Option (b) for Phase 1, evaluate (a) for Phase 2

2. **Batch Controller / Campaign UI**
   - Legacy has dedicated batch management pages (create batch, assign instances, bulk update steps, progress tracking)
   - Human-UI has no concept of batch/campaign management
   - **Needs:** New `/campaign/[id]` route in human-ui showing child net status, aggregated progress, bulk actions
   - **Effort:** Medium - the backend primitives exist (campaign net pattern + NATS events), but UI is missing

3. **Label Print Block Type**
   - Legacy has a specialized label printing procedure element
   - Human-UI has no printing-specific block type
   - **Needs:** Custom `{ type: "label_print" }` block in human-ui with printer integration
   - **Effort:** Medium - mostly frontend work

4. **Template Versioning**
   - Legacy has built-in version management (parent_id, version number, is_latest, published flag)
   - Petri-lab scenarios are stateless code artifacts
   - **Needs:** External version management (git tags, deployment registry, or a metadata service)
   - **Effort:** Low-Medium - can leverage git for version control; may need a lightweight registry API

### Moderate Gaps (Should Address)

5. **Device Binding**
   - Legacy SOPs are tightly bound to ThingsBoard devices (device_id on SopInstance)
   - New framework is device-agnostic; device context must be carried in token data
   - **Needs:** Token schema design that includes device reference; integration adapter for ThingsBoard
   - **Effort:** Low - token data already supports arbitrary JSON

6. **Optional Steps / Skip Logic**
   - Legacy has `is_optional` flag and `skip_step_instance` endpoint
   - Petri-lab doesn't have native "optional" concept
   - **Needs:** Model as alternative transitions: one that processes the step, one that skips it (guard-based)
   - **Effort:** Low - pattern is natural in Petri nets

7. **Step Repetition with Jump-to-on-Failure**
   - Legacy has explicit repetition counters and jump targets
   - Petri-lab can model this with loopback transitions + guard on repetition count in token
   - **Needs:** Standard pattern/template for retry-with-jump in SDK scenarios
   - **Effort:** Low - the primitives exist, just needs a documented pattern

8. **JavaScript-in-Browser Execution**
   - Legacy runs JavaScript in the browser (Monaco editor) as a step type
   - New framework runs logic server-side (Rhai in engine, Python via executor)
   - **Options:**
     a. Run JS via Python executor (Node.js subprocess)
     b. Add a "code display" block to human-ui that shows results from server-side execution
     c. Drop browser-side JS execution; all computation is server-side
   - **Recommendation:** Option (c) - server-side execution is more secure and auditable

### Minor Gaps (Nice to Have)

9. **Operator Assignment per Phase**
   - Legacy assigns `operator_id` per phase instance
   - Human-UI tasks are assigned to authenticated users but not phase-scoped
   - **Needs:** Operator routing logic in the campaign/SOP net definition
   - **Effort:** Low - token data can carry assigned operator

10. **Location Tracking per Phase**
    - Legacy tracks `location_id` per phase instance
    - **Needs:** Location field in token data
    - **Effort:** Trivial - just a token field

11. **Duration Tracking**
    - Legacy tracks start/end/duration at step, phase, and SOP level
    - New framework has event timestamps but no aggregated duration fields
    - **Needs:** Derivation from event log timestamps; possibly a materialized projection
    - **Effort:** Low

---

## 3. Recommended Migration Phases

### Phase 0: Foundation (2-3 weeks)
- Define standard SOP token schemas (device ref, operator, location, step data)
- Create a reference SDK scenario that models a simple SOP (3 phases, 2-3 steps each, all human tasks)
- Deploy to petri-lab engine and validate end-to-end flow with human-ui
- Document the pattern: "How to model an SOP as a Petri net"

### Phase 1: Core SOP Execution (4-6 weeks)
- **Backend:** Build SDK scenario templates for existing SOP types (parameterized by phase/step config)
- **Backend:** Create a "SOP deployment service" that takes SOP configuration and generates/deploys AIR JSON
- **Human-UI:** Add any missing block types (label_print at minimum)
- **Human-UI:** Enhance process tracking page to show SOP-style progress (phases, steps, completion %)
- **Integration:** Build ThingsBoard adapter for device context injection into nets
- Migrate 1-2 real SOPs as proof of concept

### Phase 2: Batch/Campaign Support (3-4 weeks)
- **Backend:** Build campaign net pattern for batch SOP execution
- **Backend:** Create `CreateNetListener`-based batch provisioning (dynamic child net creation)
- **Human-UI:** Build campaign management pages (`/campaign/[id]`) with:
  - Child instance grid with status
  - Bulk step update action
  - Aggregated progress tracking
  - Failure tracking and exception handling
- Migrate 1 batch SOP workflow

### Phase 3: Template Management (3-4 weeks)
- Build lightweight SOP template registry (version tracking, deployment history)
- **Option A:** CLI tool for SOP management (`aithericon sop create/deploy/list/rollback`)
- **Option B:** Admin UI for visual SOP configuration (generates AIR JSON)
- Template import tool: migrate existing SOP templates from legacy DB to new format
- Data migration scripts for historical SOP instance data

### Phase 4: Full Migration (2-3 weeks)
- Migrate all remaining SOP templates
- Run legacy and new systems in parallel for validation
- Data migration for active SOP instances (if needed)
- Decommission legacy SOP endpoints

---

## 4. Risk Assessment

| Risk | Severity | Mitigation |
|---|---|---|
| **No visual template editor** - Operators accustomed to GUI SOP creation | High | Phase 0 reference patterns + Phase 3 admin UI; interim: developer-managed scenario code |
| **Batch complexity** - Campaign pattern is more complex than legacy batch controller | Medium | Phase 2 is dedicated to this; campaign net examples already prove the pattern |
| **State query differences** - Legacy uses SQL queries on state columns; new uses event sourcing | Medium | Build materialized projections (KV store) for frequently queried state; petri-lab already has this pattern |
| **Data migration** - Active SOP instances in legacy system | Medium | Design migration scripts that create equivalent net instances with pre-seeded token state |
| **Performance** - Event sourcing overhead vs direct DB mutations | Low | Petri-lab has hibernation (ADR-13/16); NATS is very performant for event streaming |
| **Learning curve** - Team needs to learn Petri net concepts | Medium | SDK `#[step]` macro and `scope()` API abstract away most complexity; good documentation needed |
| **ThingsBoard integration** - Legacy is tightly coupled | Low | Device context is just token data; adapter pattern is straightforward |

---

## 5. Open Questions

1. **Template authoring model:** Should SOP templates be code-only (SDK), or do we need a visual editor for non-technical users? This is the single biggest architectural decision.

2. **Historical data:** Do we need to migrate historical SOP instance data (completed SOPs) for reporting, or can legacy data stay in the legacy DB with a read-only interface?

3. **Concurrent systems:** During migration, will both systems need to operate on the same devices simultaneously? If so, we need a synchronization strategy.

4. **ThingsBoard dependency:** Is the ThingsBoard device binding required in the new framework, or can we abstract device identity more generically?

5. **JavaScript execution:** Are there SOPs that critically depend on browser-side JavaScript execution? Can those be converted to server-side Python/Rhai?

6. **Label printing:** What is the actual label printing integration? Is it browser-based (window.print) or does it integrate with a specific printer API? This affects the migration approach.

7. **Batch processing modes:** The legacy has `PhaseProcessingMode::StepCentric` vs `InstanceCentric`. Which is more common? This affects the campaign net design.

---

## 6. Architectural Decision Summary

The recommended approach is: **Petri-lab as the workflow engine + Human-UI as the operator interface + Aithericon-Executor for automated steps**

- **SOP Template** = SDK scenario definition (Rust code or generated AIR JSON)
- **SOP Instance** = Petri net instance (event-sourced, NATS-streamed)
- **Phase** = SDK `scope()` group (metadata) + sequential token flow pattern
- **Step (human)** = Human task effect transition -> Human-UI task with form blocks
- **Step (automated)** = Executor effect transition (Python, API call, etc.)
- **Batch** = Campaign net with fan-out/fan-in via cross-net bridge
- **Progress tracking** = Process tracking in Human-UI (timeline, live SSE updates)
- **State queries** = KV materialized projections from event stream

This architecture gives us: event sourcing (full audit trail), distributed execution, hibernation for idle instances, schema validation on token data, and a clean separation of concerns (engine / UI / execution backends).
