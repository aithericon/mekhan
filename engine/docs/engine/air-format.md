# AIR Format Specification

The **Actor Interface Runtime (AIR)** format is the JSON specification that defines workflows for the Petri-Lab engine. The SDK generates this format automatically, but understanding it helps with debugging and advanced use cases.

## Overview

```json
{
  "name": "Workflow Name",
  "description": "Optional description",
  "places": [...],
  "transitions": [...],
  "groups": [...],
  "mock_adapters": [...],
  "definitions": {...}
}
```

## Top-Level Structure

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `name` | string | Yes | Workflow identifier |
| `description` | string | No | Human-readable description |
| `places` | array | Yes | Place definitions |
| `transitions` | array | Yes | Transition definitions |
| `groups` | array | No | Visual grouping metadata |
| `mock_adapters` | array | No | External service simulators |
| `definitions` | object | No | JSON Schema definitions for token types |

---

## Places

Places are containers that hold tokens.

### Place Structure

```json
{
  "id": "p_tasks",
  "name": "Pending Tasks",
  "type": "state",
  "group_id": "intake_phase",
  "capacity": 100,
  "initial_tokens": [
    {"id": "t1", "name": "Task 1"},
    {"id": "t2", "name": "Task 2"}
  ],
  "token_schema": "#/definitions/Task"
}
```

### Place Fields

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `id` | string | Yes | Unique identifier |
| `name` | string | Yes | Display name |
| `type` | string | Yes | Place type (see below) |
| `group_id` | string | No | Visual group membership |
| `capacity` | integer | No | Maximum token count |
| `initial_tokens` | array | No | Seed tokens at startup |
| `token_schema` | string | No | JSON Schema reference |

### Place Types

| Type | Purpose | Behavior |
|------|---------|----------|
| `state` | Workflow state markers | Standard place |
| `resource` | Shared resource pools | Standard place |
| `signal` | External event inputs | Standard place |
| `terminal` | Exit points | Tokens never leave |

### Initial Tokens

Tokens can be:

```json
// Unit token (marker)
null

// Integer token (fungible)
42

// Complex token (structured data)
{"id": "t1", "name": "Task", "priority": 5}
```

---

## Transitions

Transitions consume and produce tokens.

### Transition Structure

```json
{
  "id": "t_process_1",
  "name": "Process Task",
  "group_id": "processing_phase",
  "input_ports": [
    {"name": "task", "schema_ref": "#/definitions/Task", "cardinality": "single"},
    {"name": "worker", "schema_ref": "#/definitions/Worker", "cardinality": "single"}
  ],
  "output_ports": [
    {"name": "result", "schema_ref": "#/definitions/Result", "cardinality": "single"}
  ],
  "inputs": [
    {"place": "p_tasks", "port": "task", "weight": 1},
    {"place": "p_workers", "port": "worker", "weight": 1}
  ],
  "outputs": [
    {"port": "result", "place": "p_results", "weight": 1}
  ],
  "guard": {
    "type": "rhai",
    "source": "task.priority > 0"
  },
  "logic": {
    "type": "rhai",
    "source": "#{result: #{id: task.id, worker: worker.id}}"
  }
}
```

### Transition Fields

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `id` | string | Yes | Unique identifier |
| `name` | string | Yes | Display name |
| `group_id` | string | No | Visual group membership |
| `input_ports` | array | Yes | Input port definitions |
| `output_ports` | array | Yes | Output port definitions |
| `inputs` | array | Yes | Input arcs (place → port) |
| `outputs` | array | Yes | Output arcs (port → place) |
| `guard` | object | No | Firing condition |
| `logic` | object | Yes | Transformation logic |

---

## Ports

Ports are named connection points on transitions.

### Port Structure

```json
{
  "name": "task",
  "schema_ref": "#/definitions/Task",
  "cardinality": "single"
}
```

### Port Fields

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `name` | string | Yes | - | Port identifier |
| `schema_ref` | string | No | - | JSON Schema reference |
| `cardinality` | string | No | `"single"` | `"single"` or `"batch"` |

### Cardinality

| Value | Behavior |
|-------|----------|
| `single` | Consumes/produces one token |
| `batch` | Consumes/produces all available tokens |

---

## Arcs

Arcs connect places to ports.

### Arc Structure

```json
{
  "place": "p_tasks",
  "port": "task",
  "weight": 1
}
```

### Arc Fields

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `place` | string | Yes | - | Place ID |
| `port` | string | Yes | - | Port name |
| `weight` | integer | No | `1` | Token multiplicity |

### Arc Direction

- **Input arcs**: `place` → `port` (tokens flow from place to transition)
- **Output arcs**: `port` → `place` (tokens flow from transition to place)

---

## Guards

Guards are preconditions that control transition firing.

### Guard Types

```json
// Rhai guard
{
  "type": "rhai",
  "source": "task.priority > 0 && worker.available"
}

// Wasm guard (future)
{
  "type": "wasm",
  "module": "base64_encoded_or_path",
  "function": "check_guard"
}
```

### Guard Evaluation

- Variables match input port names
- Must return boolean
- `false` prevents transition from firing
- Evaluated before logic execution

---

## Logic

Logic defines the transformation performed when a transition fires.

### Logic Types

```json
// Rhai logic
{
  "type": "rhai",
  "source": "#{output_port: #{field1: input1.field1, field2: input2.field2}}"
}

// Wasm logic (future)
{
  "type": "wasm",
  "module": "base64_encoded_or_path",
  "function": "execute"
}
```

### Logic Return Value

Logic must return a Rhai map (`#{}`) where:
- Keys are output port names
- Values are token data

```rhai
#{
  result: #{
    id: task.id,
    processed_by: worker.id,
    timestamp: now()
  },
  freed_worker: #{
    id: worker.id
  }
}
```

---

## Groups

Groups provide visual organization (metadata only, ignored by engine).

### Group Structure

```json
{
  "id": "grp_intake",
  "name": "Intake Phase",
  "parent_id": "grp_main",
  "metadata": {
    "color": "#3498db",
    "image": "intake-icon.svg"
  }
}
```

### Group Fields

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `id` | string | Yes | Unique identifier |
| `name` | string | Yes | Display name |
| `parent_id` | string | No | Parent group for nesting |
| `metadata` | object | No | Arbitrary visualization data |

---

## Mock Adapters

Mock adapters simulate external services for testing.

### Adapter Structure

```json
{
  "name": "Payment Gateway",
  "trigger_place_id": "p_pending_payment",
  "latency_ms": 2000,
  "check_token_exists": false,
  "logic": {
    "type": "rhai",
    "source": "#{target_place: \"p_sig_payment_complete\", data: #{id: token.id, success: true}}"
  }
}
```

### Adapter Fields

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `name` | string | Yes | - | Adapter identifier |
| `trigger_place_id` | string | Yes | - | Place that triggers adapter |
| `latency_ms` | integer | Yes | - | Delay before execution |
| `check_token_exists` | boolean | No | `false` | Verify token still exists |
| `logic` | object | Yes | - | Token injection logic |

### Adapter Logic

Must return:

```rhai
#{
  target_place: "place_id",  // Where to inject token
  data: #{...}               // Token data
}
```

### Timeout Pattern

Set `check_token_exists: true` for SLA timeouts:

```json
{
  "name": "SLA Timeout",
  "trigger_place_id": "p_waiting",
  "latency_ms": 30000,
  "check_token_exists": true,
  "logic": {
    "type": "rhai",
    "source": "#{target_place: \"p_sig_timeout\", data: #{id: token.id}}"
  }
}
```

The adapter only fires if the token still exists after the delay.

---

## Definitions

JSON Schema definitions for token types.

### Definitions Structure

```json
{
  "definitions": {
    "Task": {
      "type": "object",
      "properties": {
        "id": {"type": "string"},
        "name": {"type": "string"},
        "priority": {"type": "integer"}
      },
      "required": ["id", "name"]
    },
    "Worker": {
      "type": "object",
      "properties": {
        "id": {"type": "string"},
        "skills": {
          "type": "array",
          "items": {"type": "string"}
        }
      }
    }
  }
}
```

### Schema References

Places and ports reference schemas via `$ref` syntax:

```json
"token_schema": "#/definitions/Task"
"schema_ref": "#/definitions/Worker"
```

---

## Complete Example

```json
{
  "name": "Simple Task Processor",
  "description": "Demonstrates basic workflow patterns",
  "places": [
    {
      "id": "p_tasks",
      "name": "Pending Tasks",
      "type": "state",
      "initial_tokens": [
        {"id": "t1", "name": "Task 1"},
        {"id": "t2", "name": "Task 2"}
      ],
      "token_schema": "#/definitions/Task"
    },
    {
      "id": "p_workers",
      "name": "Available Workers",
      "type": "resource",
      "initial_tokens": [
        {"id": "w1"},
        {"id": "w2"}
      ],
      "token_schema": "#/definitions/Worker"
    },
    {
      "id": "p_completed",
      "name": "Completed",
      "type": "terminal",
      "token_schema": "#/definitions/Result"
    }
  ],
  "transitions": [
    {
      "id": "t_process_1",
      "name": "Process Task",
      "input_ports": [
        {"name": "task", "cardinality": "single"},
        {"name": "worker", "cardinality": "single"}
      ],
      "output_ports": [
        {"name": "result", "cardinality": "single"},
        {"name": "freed", "cardinality": "single"}
      ],
      "inputs": [
        {"place": "p_tasks", "port": "task"},
        {"place": "p_workers", "port": "worker"}
      ],
      "outputs": [
        {"port": "result", "place": "p_completed"},
        {"port": "freed", "place": "p_workers"}
      ],
      "logic": {
        "type": "rhai",
        "source": "#{result: #{task_id: task.id, worker_id: worker.id}, freed: #{id: worker.id}}"
      }
    }
  ],
  "definitions": {
    "Task": {
      "type": "object",
      "properties": {
        "id": {"type": "string"},
        "name": {"type": "string"}
      }
    },
    "Worker": {
      "type": "object",
      "properties": {
        "id": {"type": "string"}
      }
    },
    "Result": {
      "type": "object",
      "properties": {
        "task_id": {"type": "string"},
        "worker_id": {"type": "string"}
      }
    }
  }
}
```

---

## Validation

The engine validates AIR documents for:

1. **Structural validity**: Required fields present
2. **Reference integrity**: All place/port IDs exist
3. **Type consistency**: Arcs connect compatible types
4. **Rhai syntax**: Scripts parse correctly
5. **Variable binding**: Guard/logic variables match port names

---

## Next Steps

- [Execution Rules](./execution-rules.md) - How the engine processes workflows
- [SDK Macros](../sdk/macros.md) - Generate AIR from Rust code
