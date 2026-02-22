# Legacy SOP System - Comprehensive Requirements Document

> Captured by legacy-expert during the SOP migration investigation (Feb 2025)

## 1. Architecture Overview

The legacy SOP system is a 3-tier hierarchy: **SOP Template -> Phase Template -> Step Template** with corresponding runtime instances. It is built with Rust/Axum + Diesel ORM (PostgreSQL) on the backend and SvelteKit on the frontend.

---

## 2. SOP Templates

### Data Model (`sop_templates` table)
- `id: i32` (auto-increment PK)
- `parent_id: Option<i32>` - FK to parent template (for versioning)
- `author_id: Uuid` - FK to users table
- `name: Text`
- `description: Text`
- `total_phases: i32` - denormalized count
- `total_steps: i32` - denormalized count
- `version: i32` - incremental version number
- `published: Bool` - if true, template is locked for edits
- `published_at: Option<Timestamp>`
- `created: Timestamp`
- `is_latest: Bool` - flag for the latest version in a version chain
- `base_sop_template_id: Option<i32>` - root ID representing the version strain/chain

### Versioning System
- Templates use a **version chain** linked by `base_sop_template_id`
- When creating a new version from a published parent:
  1. Parent must be published
  2. New template inherits: name, description, total_phases, total_steps
  3. Version increments from parent
  4. All phases and steps are **deep-copied** (cloned)
  5. Previous `is_latest` template is set to false
  6. New template gets `is_latest = true`
- Root templates (no parent) set their own ID as `base_sop_template_id`

### Publishing
- `PATCH /api/v1/sop_templates/{id}/publish` - marks template as published
- Once published: **no more edits allowed** - only creating a new version
- Business rule: Only published templates can be instantiated

### Device Type Relationship (M:N)
- **`sop_templates_device_types`** junction table: `(sop_template_id, device_type_id)`
- At least one device type required when creating a template
- Device types are validated against `device_types` table
- When versioning, device types are inherited from parent (can be overridden)
- Response includes `SopTemplateWithDeviceTypes` with `allowed_device_types` and `device_type_ids`

### API Endpoints
- `GET /api/v1/sop_templates` - paginated list with `QueryBuilder`, supports filters, sorting, text search, `showAllVersions`, `isPublished`
- `POST /api/v1/sop_templates` - create (with optional `parent_id` for versioning)
- `GET /api/v1/sop_templates/{id}` - get with optional `includePhases`, `includeSteps`
- `PUT /api/v1/sop_templates/{id}` - update (blocked if published)
- `DELETE /api/v1/sop_templates/{id}` - delete
- `PATCH /api/v1/sop_templates/{id}/publish` - publish
- `GET /api/v1/sop_templates/{id}/device_types` - list allowed device types
- `POST /api/v1/sop_templates/{id}/device_types/{device_type_id}` - add device type
- `DELETE /api/v1/sop_templates/{id}/device_types/{device_type_id}` - remove device type

---

## 3. Phase Templates

### Data Model (`phase_templates` table)
- `id: i32` (auto-increment PK)
- `sop_template_id: i32` - FK to sop_templates
- `name: Text`
- `description: Text`
- `position: i32` - ordering within the SOP
- `batchable: Bool` - whether this phase supports batch processing
- `slug: Option<Text>` - auto-generated snake_case identifier
- `processing_mode: PhaseProcessingMode` - enum: `StepCentric` (default) | `InstanceCentric`

### Processing Modes
- **StepCentric**: All instances in a batch must be at the same step; batch advances together
- **InstanceCentric**: Each instance progresses independently; only one update at a time; less constrained

### Business Rules
- Phases ordered by `position` within an SOP template
- Editing blocked if parent SOP template is published
- Slugs auto-generated from name or validated if user-provided (lowercase, numbers, hyphens, underscores only)

### API Endpoints
- `GET /api/v1/sop_phase_templates` - list with optional `sopTemplateId`, `includeSteps`, pagination
- `POST /api/v1/sop_phase_templates` - create
- `GET /api/v1/sop_phase_templates/{id}` - get with optional `includeSteps`
- `PUT /api/v1/sop_phase_templates/{id}` - update (blocked if SOP published)
- `DELETE /api/v1/sop_phase_templates/{id}` - delete

---

## 4. Step Templates

### Data Model (`step_templates` table)
- `id: i32` (auto-increment PK)
- `phase_template_id: i32` - FK to phase_templates
- `name: Text`
- `description: Text`
- `input_type: Text` - type of expected input (e.g., "text", "number", "json")
- `input_config: Option<Jsonb>` - configuration for input (e.g., validation rules)
- `output_type: Text` - type of expected output
- `output_config: Option<Jsonb>` - configuration for output
- `is_optional: Bool` - if true, step can be skipped
- `position: i32` - ordering within the phase
- `is_repeatable: Bool` - whether step supports retries
- `is_automatic: Bool` - whether step runs automatically
- `jump_to_on_success: Option<i32>` - FK to another step_template (skip ahead)
- `jump_to_on_failure: Option<i32>` - FK to another step_template (error recovery)
- `slug: Option<Text>` - auto-generated identifier
- `concurrency_mode: StepConcurrencyMode` - enum: `Sequential` | `Bulk` | `Hybrid`
- `bulk_defaults: Jsonb` - default values for batch operations (default: `{}`)
- `max_repetitions: i32` - max retry count before failure

### Concurrency Modes
- **Sequential**: Only one instance updated at a time in batch context
- **Bulk**: All instances in batch can be updated simultaneously
- **Hybrid**: Mixed approach

### Jump Logic
- `jump_to_on_failure`: When step fails and retries available, jump to target step
  - Forward jump: intermediate steps get `Skipped`
  - Backward jump: intermediate steps get reset to `New`
- `jump_to_on_success`: jump after successful completion (NOT implemented in update logic yet based on code)

### API Endpoints
- `GET /api/v1/sop_step_templates` - list with optional `phaseTemplateId`, pagination
- `POST /api/v1/sop_step_templates` - create
- `GET /api/v1/sop_step_templates/{id}` - get
- `PUT /api/v1/sop_step_templates/{id}` - update
- `DELETE /api/v1/sop_step_templates/{id}` - delete

---

## 5. SOP Instances

### Data Model (`sop_instances` table)
- `id: i32` (auto-increment PK)
- `sop_template_id: i32` - FK to sop_templates
- `device_id: Uuid` - FK to devices (each instance tied to a device)
- `author_id: Uuid` - FK to users
- `created: Timestamp`
- `end: Option<Timestamp>`
- `duration: Option<Interval>`
- `current_phase_id: Option<i32>` - FK to phase_instances (runtime pointer)
- `current_step_id: Option<i32>` - FK to step_instances (runtime pointer)
- `state: ProgressState` - New/InProgress/Done/Failed/Canceled/Skipped
- `completed_steps: i32` - counter
- `completed_phases: i32` - counter
- `started: Option<Timestamp>`
- `current_step_template_id: Option<i32>` - denormalized for query performance
- `current_phase_template_id: Option<i32>` - denormalized
- `current_step_name: Option<Text>` - denormalized
- `current_phase_name: Option<Text>` - denormalized
- `sync_status: SopInstanceSyncStatus` - Aligned/Lagging/Detached/Exception (for batch)
- `last_bulk_step_id: Option<i32>` - batch sync tracking
- `last_bulk_applied_at: Option<Timestamp>` - batch sync tracking
- `detached_reason: Option<Text>` - why detached from batch

### Instantiation Process (Transaction)
1. Insert SOP instance (state=New)
2. Validate template is published
3. Validate device type is allowed for this template
4. Create phase instances for ALL phases in template (all start as New)
5. Create step instances for ALL steps in ALL phases (all start as New)
6. Set `current_phase_id` to first phase instance
7. Return enriched response with all nested data

### EnrichedSopInstance (API Response)
Joins SOP instance data with template data: name, description, total_phases, total_steps, version, device_type_id

### State Transitions
- **New** -> **InProgress** (when first phase started)
- **InProgress** -> **Done** (when last phase completes)
- **InProgress** -> **Failed** (when any step fails)
- **InProgress** -> **Canceled** (explicit cancel)

### API Endpoints
- `GET /api/v1/sop_instances` - paginated list with JOIN to sop_templates and devices, supports `parentType=DEVICE|TEMPLATE`, filter/sort/text search
- `POST /api/v1/sop_instances` - create with `{sopTemplateId, deviceId, locationId?}`
- `GET /api/v1/sop_instances/{id}` - get with optional `includePhases`, `includeSteps`
- `PUT /api/v1/sop_instances/{id}` - update
- `DELETE /api/v1/sop_instances/{id}` - delete
- `PUT /api/v1/sop_instances/{id}/cancel` - cancel SOP and cascade to children

---

## 6. Phase Instances

### Data Model (`phase_instances` table)
- `id: i32`
- `phase_template_id: i32`
- `operator_id: Uuid` - who is executing this phase
- `sop_instance_id: i32`
- `location_id: i32` - where this phase is being performed
- `state: ProgressState`
- `started: Option<Timestamp>`
- `end: Option<Timestamp>`
- `duration: Option<Interval>`

### Start Phase Logic
- `PUT /api/v1/sop_phase_instances/{id}/start`
- Validates prior phase is Done (if not first phase)
- Sets state to InProgress, sets started timestamp
- Automatically starts the first step instance
- Updates SOP instance with current phase/step pointers

### State Transitions
- **New** -> **InProgress** (explicit start)
- **InProgress** -> **Done** (last step completes)
- **InProgress** -> **Failed** (any step fails)
- **InProgress** -> **Canceled** (parent SOP canceled)

---

## 7. Step Instances

### Data Model (`step_instances` table)
- `id: i32`
- `step_template_id: i32`
- `phase_instance_id: i32`
- `input_value: Option<Jsonb>` - data entered by operator
- `output_value: Option<Jsonb>` - result data
- `state: ProgressState`
- `started: Option<Timestamp>`
- `end: Option<Timestamp>`
- `duration: Option<Interval>`
- `repetitions: i32` - current retry count
- `error: Option<Jsonb>` - error information `{"message": "..."}`

### Update Step Logic (`PUT /api/v1/sop_step_instances/{id}`)
Input: `{inputValue?, outputValue?, errorMessage?, doSkip?}`

The update logic is the core state machine:

1. **Validation**: Step must be InProgress, Phase must be InProgress, step must have started timestamp
2. **Skip handling**: Only optional steps can be skipped (state -> Skipped)
3. **Error handling with retries**:
   - If `errorMessage` provided:
     - Increment repetitions
     - If `jump_to_on_failure` set: jump to target step
     - If `repetitions > max_repetitions`: mark Failed, cascade to phase and SOP
     - Otherwise: retry (stay InProgress)
4. **Jump logic**: Forward jumps skip intermediate steps, backward jumps reset them
5. **Normal completion** (state -> Done):
   - End time and duration calculated
   - If last step in phase: phase -> Done; if last phase: SOP -> Done
   - If not last step: automatically start next step

### Response: `UpdateStepInstanceResponse`
- `step_instance`: the updated step
- `next_step_instance`: the next step (if auto-started)
- `updated_phase`: phase update (if completed/failed)
- `updated_sop_instance`: SOP instance update
- `affected_steps`: all steps modified by jump logic

### Skip Step Endpoint
- `PUT /api/v1/sop_step_instances/{id}/skip` - dedicated skip endpoint

---

## 8. Batch Controller

### Data Model (`batch_controllers` table)
- `id: i32`
- `created_at: Timestamp`
- `updated_at: Timestamp`
- `started_at: Option<Timestamp>`
- `finished_at: Option<Timestamp>`
- `duration: Option<Interval>`
- `operator_id: Uuid`
- `phase_template_id: i32` - scoped to a single phase
- `phase_template_name: Text` - denormalized
- `sop_template_id: i32`
- `sop_template_name: Text` - denormalized
- `batch_size: i32`
- `failed_instances_amount: i32`
- `current_step_template_id: Option<i32>` - batch-wide cursor
- `current_step_name: Option<Text>` - denormalized
- `state: ProgressState`
- `target_entity_type: BatchTargetEntityType` - Device/Customer/Asset/Project/EntityView
- `target_entity_id: Option<Uuid>`
- `qualification_snapshot: Jsonb` - snapshot of qualification criteria
- `bulk_step_cursor: Option<i32>` - tracks batch progress
- `sync_summary: Jsonb` - `{"aligned": N, "lagging": N, "detached": N, "exception": N}`
- `current_sop_instance_id: Option<i32>` - for instance-centric mode

### Junction Table (`batch_controllers_sop_instances`)
- `batch_controller_id: i32`
- `sop_instance_id: i32`
- Composite PK

### Create Batch Logic
1. Validate SOP template is published
2. If `batch_size > 0`: phase must be the FIRST phase (position 0) since new instances cannot skip phases
3. Validate device type is allowed for SOP template
4. Create batch controller record
5. If `batch_size > 0`: For each instance:
   - Create a ThingsBoard device
   - Create a SOP instance
   - Assign instance to batch via junction table

### Assign Instance to Batch
- Validates same SOP template
- Validates same phase template (instance must be in batch's phase)
- Validates same step (if batch has current step)
- Validates instance not already in another batch
- Updates batch_size counter

### Batch Update Steps (`PUT /api/v1/batch_controllers/{id}/update_steps`)
- Accepts `BatchUpdateStepInstanceRequest` with `step_updates: Vec<StepInstanceUpdate>`
- Each update specifies: `sop_instance_id, step_instance_id, input_value?, output_value?, error_message?, do_skip?`
- Processing modes:
  - **StepCentric**: All updates must be for the same step template as batch's current step
  - **InstanceCentric**: Only one update at a time, can work on any instance
- Concurrency:
  - **Sequential**: Only 1 update per request
  - **Bulk/Hybrid**: Multiple updates per request
- After all updates, determines next step and updates batch controller state

### Start Batch (`PUT /api/v1/batch_controllers/{id}/start`)
### Cancel Batch (`PUT /api/v1/batch_controllers/{id}/cancel`)

### API Endpoints Summary
- `POST /api/v1/batch_controllers` - create
- `GET /api/v1/batch_controllers` - list (paginated, filterable by phaseTemplate/sopTemplate, text search)
- `GET /api/v1/batch_controllers/{id}` - get with optional `includeInstances`
- `POST /api/v1/batch_controllers/{id}/assign` - assign existing SOP instance
- `DELETE /api/v1/batch_controllers/{id}/unassign/{sop_instance_id}` - unassign
- `DELETE /api/v1/batch_controllers/{id}` - delete (only if no instances assigned)
- `PUT /api/v1/batch_controllers/{id}/update_steps` - batch update steps
- `PUT /api/v1/batch_controllers/{id}/start` - start batch
- `PUT /api/v1/batch_controllers/{id}/cancel` - cancel batch

---

## 9. Custom Enums

### ProgressState
`New | InProgress | Done | Failed | Canceled | Skipped`

### StepConcurrencyMode
`Sequential | Bulk | Hybrid`

### PhaseProcessingMode
`StepCentric | InstanceCentric`

### BatchTargetEntityType
`Device | Customer | Asset | Project | EntityView`

### SopInstanceSyncStatus
`Aligned | Lagging | Detached | Exception`

---

## 10. Business Rules Summary

1. **Template immutability**: Published templates cannot be edited; must create new version
2. **Version chain**: `base_sop_template_id` + `is_latest` track version lineage
3. **Device type validation**: SOP template must be compatible with device type
4. **Phase ordering**: Phases execute in `position` order; prior phase must be Done before starting next
5. **Step ordering**: Steps execute in `position` order within a phase
6. **Optional steps**: Only `is_optional=true` steps can be skipped
7. **Retry logic**: `max_repetitions` controls retries; `jump_to_on_failure` enables recovery paths
8. **Failure cascade**: Step failure -> Phase failure -> SOP failure (unless retries available)
9. **Batch constraints**: All instances in batch must be in same phase/step (StepCentric mode)
10. **Batch creation**: Auto-provisioning only for first phase; later phases require existing instances
11. **Slug validation**: Only lowercase, numbers, hyphens, underscores
12. **Auth**: API key header (`suessco_apikey`); user context from ThingsBoard JWT

---

## 11. Query Builder / Filtering

The system uses a custom `QueryBuilder` for paginated queries supporting:
- **Filters**: `FilterExpression` with field/operator/value (supports camelCase and snake_case field names)
- **Sorting**: field + direction (ASC/DESC)
- **Pagination**: page + page_size
- **Text search**: full-text search across specified columns
- **Column mapping**: Maps frontend camelCase field names to qualified SQL column names

---

## 12. Frontend Pages

- `/sop-templates` - List all templates (with version management)
- `/sop-templates/create` - Create new template (select device types)
- `/sop-templates/[id]` - View/edit template with phases and steps
- `/sop-instances` - List all instances
- `/sop-instances/create` - Create instance (select device, template)
- `/sop-instances/[id]` - Execute/view instance with step-by-step workflow
- `/batch-sop` - List batch controllers
- `/batch-sop/create` - Create batch (select SOP template, phase, batch size)
- `/batch-sop/[id]` - Manage batch (update steps, assign/unassign instances)

---

## 13. Additional Tables

- `sop_instance_locations`: M:N between instances and locations
- `sop_instance_operators`: M:N between instances and operators (users)
- `locations`: hierarchical location tree referenced by phase instances
