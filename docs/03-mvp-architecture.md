# Mekhan MVP Architecture

> Produced by the architect after investigating petri-lab, aithericon-human-ui, aithericon-executor, and the legacy web-platform.

---

## 1. Project Structure

### Recommendation: New SvelteKit App + New Rust Crate

The MVP consists of two new deliverables plus integration with existing systems:

```
AithericonResearch/
├── mekhan/                          # Documentation (existing)
│   ├── 01-legacy-sop-requirements.md
│   ├── 02-migration-strategy.md
│   └── 03-mvp-architecture.md       # This document
│
├── mekhan-app/                      # NEW — SvelteKit frontend
│   ├── src/
│   │   ├── lib/
│   │   │   ├── components/
│   │   │   │   ├── editor/          # svelte-flow workflow editor
│   │   │   │   │   ├── WorkflowCanvas.svelte
│   │   │   │   │   ├── nodes/       # Custom node components
│   │   │   │   │   │   ├── StartNode.svelte
│   │   │   │   │   │   ├── EndNode.svelte
│   │   │   │   │   │   ├── HumanTaskNode.svelte
│   │   │   │   │   │   ├── AutomatedStepNode.svelte
│   │   │   │   │   │   ├── DecisionNode.svelte
│   │   │   │   │   │   ├── ParallelSplitNode.svelte
│   │   │   │   │   │   └── LoopNode.svelte
│   │   │   │   │   ├── edges/       # Custom edge components
│   │   │   │   │   ├── panels/      # Property panels for node config
│   │   │   │   │   └── toolbar/     # Editor toolbar (save, publish, etc.)
│   │   │   │   ├── template/        # Template management components
│   │   │   │   ├── instance/        # Instance monitoring components
│   │   │   │   └── ui/              # Shared UI primitives (shadcn-svelte)
│   │   │   ├── compiler/            # AIR compilation (client-side TypeScript)
│   │   │   │   ├── compile.ts       # Main compilation entry point
│   │   │   │   ├── topology.ts      # Block-to-topology expansion
│   │   │   │   └── validate.ts      # Pre-compilation validation
│   │   │   ├── stores/              # Svelte 5 stores (runes)
│   │   │   ├── types/               # TypeScript type definitions
│   │   │   │   ├── editor.ts        # Editor data model types
│   │   │   │   ├── air.ts           # AIR JSON types
│   │   │   │   └── api.ts           # API request/response types
│   │   │   └── api/                 # API client (openapi-fetch)
│   │   └── routes/
│   │       ├── +layout.svelte
│   │       ├── +page.svelte         # Dashboard / template list
│   │       ├── templates/
│   │       │   ├── +page.svelte     # Template list
│   │       │   ├── new/+page.svelte # Create new template
│   │       │   └── [id]/
│   │       │       ├── +page.svelte # View/edit template (workflow editor)
│   │       │       └── versions/+page.svelte
│   │       └── instances/
│   │           ├── +page.svelte     # Instance list
│   │           └── [id]/+page.svelte # Instance detail (live state)
│   ├── package.json
│   ├── svelte.config.js
│   └── vite.config.ts
│
├── mekhan-service/                  # NEW — Rust backend (Axum)
│   ├── Cargo.toml
│   ├── src/
│   │   ├── main.rs                  # Axum server entry point
│   │   ├── config.rs                # Configuration (env vars, config file)
│   │   ├── db/                      # Database layer
│   │   │   ├── mod.rs
│   │   │   ├── pool.rs              # SQLx connection pool
│   │   │   └── migrations/          # SQLx migrations
│   │   ├── models/                  # Domain models
│   │   │   ├── template.rs          # WorkflowTemplate, TemplateVersion
│   │   │   └── instance.rs          # WorkflowInstance
│   │   ├── handlers/                # Axum route handlers
│   │   │   ├── templates.rs         # Template CRUD + versioning
│   │   │   └── instances.rs         # Instance management
│   │   ├── compiler/                # AIR compilation (server-side mirror)
│   │   │   ├── mod.rs
│   │   │   └── compile.rs
│   │   └── petri/                   # petri-lab integration
│   │       ├── client.rs            # HTTP client for petri-lab engine
│   │       └── instance.rs          # Instance lifecycle management
│   └── migrations/                  # SQL migration files
│
├── petri-lab/                       # EXISTING — Workflow engine
├── aithericon-human-ui/             # EXISTING — Human task UI
└── aithericon-executor/             # EXISTING — Automated execution
```

### Why Separate App (Not Extending human-ui or lab-ui)

1. **aithericon-human-ui** is a task execution UI for operators. Mekhan is a workflow design tool for managers/engineers. Different audiences, different UX paradigms.
2. **lab-ui** is a Petri net visualizer/debugger. Mekhan abstracts away Petri net concepts entirely.
3. A separate app avoids coupling deployment/release cycles.
4. The new app can still share dependencies (@xyflow/svelte, TailwindCSS, shadcn-svelte).

### Tech Stack

| Layer | Technology | Rationale |
|-------|-----------|-----------|
| Frontend | SvelteKit + Svelte 5 | Matches human-ui; team has expertise |
| Flow editor | @xyflow/svelte (v1.5+) | Already used in lab-ui; mature library |
| UI primitives | shadcn-svelte (bits-ui) + TailwindCSS 4 | Matches human-ui |
| Backend | Rust + Axum | Matches petri-lab and executor; team expertise |
| Database | PostgreSQL + SQLx | Async, compile-time checked queries |
| Message bus | NATS JetStream | Already used by petri-lab and human-ui |
| Runtime | Deno (frontend) + native (backend) | human-ui uses Deno adapter |

---

## 2. Visual Editor Data Model

### Node Types

The user sees these block types in the editor. Each block type maps to a specific Petri net topology on compilation.

```typescript
// src/lib/types/editor.ts

/** Base properties shared by all nodes */
type BaseNodeData = {
  label: string;
  description?: string;
};

/** Start block — entry point of the workflow */
type StartNodeData = BaseNodeData & {
  type: 'start';
  initialData?: Record<string, unknown>; // Seed token data
};

/** End block — terminal state */
type EndNodeData = BaseNodeData & {
  type: 'end';
};

/** Human Task block — creates a human-ui task */
type HumanTaskNodeData = BaseNodeData & {
  type: 'human_task';
  taskTitle: string;
  instructionsMdsvex?: string;
  steps: TaskStepConfig[];   // Human-UI step definitions (blocks array)
};

/** Automated Step block — triggers executor */
type AutomatedStepNodeData = BaseNodeData & {
  type: 'automated_step';
  executionSpec: ExecutionSpecConfig; // Backend type + config
};

/** Decision/Branch block — conditional routing */
type DecisionNodeData = BaseNodeData & {
  type: 'decision';
  conditions: BranchCondition[];  // Guard expressions for each output
  defaultBranch?: string;         // Edge ID for "else" case
};

/** Parallel Split block — fan out to concurrent paths */
type ParallelSplitNodeData = BaseNodeData & {
  type: 'parallel_split';
};

/** Parallel Join block — synchronization point */
type ParallelJoinNodeData = BaseNodeData & {
  type: 'parallel_join';
};

/** Loop block — retry or iterate */
type LoopNodeData = BaseNodeData & {
  type: 'loop';
  maxIterations: number;
  loopCondition: string;  // Rhai expression that returns boolean
};

type WorkflowNodeData =
  | StartNodeData
  | EndNodeData
  | HumanTaskNodeData
  | AutomatedStepNodeData
  | DecisionNodeData
  | ParallelSplitNodeData
  | ParallelJoinNodeData
  | LoopNodeData;

/** TaskStep configuration (maps to human-ui TaskStep) */
type TaskStepConfig = {
  id: string;
  title: string;
  descriptionMdsvex?: string;
  blocks: TaskBlockConfig[];
};

/** Block configuration within a task step */
type TaskBlockConfig =
  | { type: 'input'; field: TaskFieldConfig }
  | { type: 'mdsvex'; content: string }
  | { type: 'callout'; severity: 'info' | 'warning' | 'error' | 'success'; title?: string; content: string }
  | { type: 'divider' };

type TaskFieldConfig = {
  name: string;
  label: string;
  kind: 'text' | 'textarea' | 'number' | 'select' | 'checkbox' | 'file' | 'signature';
  required?: boolean;
  placeholder?: string;
  options?: string[];    // For select
};

type BranchCondition = {
  edgeId: string;         // Which output edge this condition maps to
  label: string;          // User-visible label (e.g., "Approved", "Rejected")
  guard: string;          // Rhai expression (e.g., "data.decision == \"approve\"")
};

type ExecutionSpecConfig = {
  backendType: 'python' | 'process' | 'docker';
  config: Record<string, unknown>;
};
```

### Edge Types

```typescript
type WorkflowEdge = {
  id: string;
  source: string;      // Source node ID
  target: string;      // Target node ID
  sourceHandle?: string; // For decision nodes: branch identifier
  label?: string;        // User-visible label on the edge
  type: 'sequence' | 'conditional' | 'loop_back';
};
```

### Graph Serialization (Storage Format)

The workflow graph is stored as JSON in the database. This is the canonical representation that the frontend edits and the compiler reads.

```typescript
type WorkflowGraph = {
  nodes: Array<{
    id: string;
    type: WorkflowNodeData['type'];
    position: { x: number; y: number };
    data: WorkflowNodeData;
  }>;
  edges: WorkflowEdge[];
  viewport?: { x: number; y: number; zoom: number };
};
```

---

## 3. Structural Blocks (Topologies)

Each user-facing block type compiles to a specific Petri net topology. The user never sees places, transitions, or arcs — they see named blocks with configured properties.

### 3.1 Start Block

**User sees:** A green circle labeled "Start". Optional: configure initial data fields.

**Compiles to:**
```
[p_{id}_ready]  (state place, with initial token)
```

One `state` place with a single initial token. The token carries any initial data configured by the user, merged with instance-level context (e.g., `instance_id`, `created_by`, `created_at`).

### 3.2 End Block

**User sees:** A red circle labeled "End".

**Compiles to:**
```
[p_{id}_done]  (terminal place)
```

One `terminal` place. When a token arrives here, the net completes (triggers `NetCompleted` event).

### 3.3 Sequence (Edge)

**User sees:** An arrow connecting two blocks.

**Compiles to:** A transition that passes data from one place to another.

```
[p_source_out] ---> (t_{edge_id}_pass) ---> [p_target_in]
```

The pass-through transition uses identity logic: `#{ out: input }`. This creates a clean handoff between blocks.

### 3.4 Human Task Block

**User sees:** A block where they configure a form (title, instructions, steps with input fields, markdown, etc.). This is the most important block type.

**Compiles to:**
```
                    ┌─────────────────────┐
[p_{id}_input] --> (t_{id}_request)       │
                    │  effect: human_task  │
                    │  config: {place: p_{id}_signal}
                    └──────┬──────────────┘
                           │
                    [p_{id}_active]    [p_{id}_signal] (signal place)
                           │                   │
                    ┌──────┴───────────────────┘
                    │
               (t_{id}_finalize)
                    │  guard: "signal.task_id == state.task_id"
                    │  logic: merge signal data into token
                    │
                    [p_{id}_output]
```

**Places:**
- `p_{id}_input` — Receives the workflow token
- `p_{id}_active` — Holds the active task reference
- `p_{id}_signal` — Signal place for human-ui responses
- `p_{id}_output` — Emits the enriched token (original data + human input)

**Transitions:**
- `t_{id}_request` — Effect transition (`human_task`). Takes the input token (which contains the form schema as part of the token data), creates a human task via NATS. Outputs a reference token with `task_id`.
- `t_{id}_finalize` — Rhai transition. Joins the active task reference with the human response signal. Merges human input data into the workflow token.

**Token data flow:** The form definition (title, instructions, steps/blocks) is embedded in the token sent to the `request` transition. This matches the pattern in `human_task_net.rs` and `expense_approval_net.rs`.

### 3.5 Automated Step Block

**User sees:** A block where they configure an execution backend (Python script, Docker container, etc.).

**Compiles to:**
```
                    ┌─────────────────────┐
[p_{id}_input] --> (t_{id}_prepare)       │
                    │  logic: build spec   │
                    └──────┬──────────────┘
                           │
                    [p_{id}_job]
                           │
                    ┌──────┴──────────────┐
               (t_{id}_submit)            │
                    │  effect: executor_submit
                    │  causes: sig_complete, sig_failed
                    └──────┬──────────────┘
                           │
                    [p_{id}_submitted]    [p_{id}_sig_complete] [p_{id}_sig_failed]
                           │                   │                      │
                    ┌──────┴───────────────────┘                      │
                    │                                                  │
               (t_{id}_done)                                   (t_{id}_failed)
                    │  correlate: execution_id                        │
                    │                                                  │
                    [p_{id}_output]                            [p_{id}_error]
```

**Transitions:**
- `t_{id}_prepare` — Builds the `ExecutionSpec` from the block configuration and input data
- `t_{id}_submit` — Effect transition (`executor_submit`), submits job to aithericon-executor
- `t_{id}_done` — Joins submitted state with completion signal, extracts results
- `t_{id}_failed` — Handles execution failure (routes to error handling)

### 3.6 Decision/Branch Block

**User sees:** A diamond shape with multiple outgoing arrows. Each arrow has a label and a condition (expressed in simple terms like "If status is approved" or as a Rhai expression).

**Compiles to:**
```
[p_{id}_input]
       │
       ├──> (t_{id}_branch_0)  --guard: condition_0--> [p_{id}_out_0]
       ├──> (t_{id}_branch_1)  --guard: condition_1--> [p_{id}_out_1]
       └──> (t_{id}_default)   --guard: none---------> [p_{id}_out_default]
```

Multiple competing transitions from the same input place. Each has a guard expression. The engine's specificity priority and non-deterministic tie-breaking handle the routing. The "default" branch (no guard) fires if no other guard matches.

**Important:** Guards are mutually exclusive by design. The compiler validates that conditions don't overlap, or inserts negation guards on the default branch.

### 3.7 Loop/Retry Block

**User sees:** A block with "max iterations" and a condition. It wraps a section of the workflow and repeats it until the condition is met or max iterations exceeded.

**Compiles to:**
```
[p_{id}_input]
       │
  (t_{id}_enter)
       │  logic: initialize counter { ...data, _loop_count: 0 }
       │
[p_{id}_body_in] ─── (body of loop: connected blocks) ─── [p_{id}_body_out]
       ↑                                                         │
       │                                                         │
       │              (t_{id}_continue)                          │
       │                guard: "data._loop_count < max && loopCondition"
       │                logic: { ...data, _loop_count: data._loop_count + 1 }
       └─────────────────────────────────────────────────────────┘
                                                                  │
                      (t_{id}_exit)
                        guard: "data._loop_count >= max || !loopCondition"
                                                                  │
                                                           [p_{id}_output]
```

Two competing transitions at the loop exit point:
- `t_{id}_continue` — Loop back (guard: condition true AND under max iterations)
- `t_{id}_exit` — Exit loop (guard: condition false OR max iterations reached)

The loop counter is embedded in the token data as `_loop_{id}_count`.

### 3.8 Parallel Split Block

**User sees:** A block that fans out to multiple parallel paths.

**Compiles to:**
```
[p_{id}_input]
       │
  (t_{id}_fork)
       │  logic: duplicate token to N output ports
       │
  ┌────┼────┐
  │    │    │
[p_0] [p_1] [p_2]  (one output place per outgoing edge)
```

A single transition with one input and N outputs. The logic duplicates the token data to all output ports.

### 3.9 Parallel Join Block

**User sees:** A block that waits for all parallel paths to complete.

**Compiles to:**
```
[p_0] [p_1] [p_2]  (one input place per incoming edge)
  │    │    │
  └────┼────┘
       │
  (t_{id}_join)
       │  logic: merge all input tokens
       │
[p_{id}_output]
```

A single transition with N inputs (one per parallel path) and one output. The transition only fires when ALL input places have tokens (natural Petri net synchronization). The logic merges data from all branches.

---

## 4. AIR Compilation

### Algorithm Overview

The compiler transforms a `WorkflowGraph` into an AIR JSON document. It runs client-side in TypeScript (for instant preview) and server-side in Rust (for deployment validation).

### Step-by-Step Compilation

```
1. VALIDATE
   - Exactly one Start node
   - At least one End node
   - All nodes reachable from Start
   - All paths lead to End (no dangling nodes)
   - No disconnected subgraphs
   - Decision branches have valid guards
   - Loop blocks have valid conditions and max > 0

2. TOPOLOGICAL SORT
   - Sort nodes in dependency order (BFS from Start)
   - Detect cycles (only valid inside Loop blocks)

3. EXPAND NODES
   For each node in topological order:
     - Generate places and transitions per block type (Section 3)
     - Assign unique IDs: p_{nodeId}_{suffix}, t_{nodeId}_{suffix}
     - Record input/output place IDs for wiring

4. WIRE EDGES
   For each edge:
     - Connect source node's output place to target node's input place
     - Insert a pass-through transition if needed
     - For decision edges: wire to the correct branch transition

5. GENERATE TOKEN SCHEMAS
   - Collect all field names from human task blocks
   - Build a merged workflow data schema (definitions section)
   - All tokens use a single "WorkflowToken" schema with optional fields

6. EMIT AIR JSON
   - Assemble places, transitions, groups, definitions
   - Groups correspond to user-defined labels/sections
```

### Example: Simple 3-Step Approval Workflow

**Visual graph:**
```
[Start] → [Fill Form] → [Manager Review] → [End]
```

Where:
- "Fill Form" is a Human Task with text input fields
- "Manager Review" is a Human Task with approve/reject decision

**Compiled AIR JSON:**

```json
{
  "name": "simple-approval",
  "description": "Simple 3-step approval workflow",
  "places": [
    {
      "id": "p_start_ready",
      "name": "Start",
      "type": "state",
      "initial_tokens": [{ "instance_id": "__INSTANCE_ID__", "created_at": "__TIMESTAMP__" }]
    },
    {
      "id": "p_fill_input",
      "name": "Fill Form - Input",
      "type": "state"
    },
    {
      "id": "p_fill_active",
      "name": "Fill Form - Active",
      "type": "state"
    },
    {
      "id": "p_fill_signal",
      "name": "Fill Form - Signal",
      "type": "signal"
    },
    {
      "id": "p_fill_output",
      "name": "Fill Form - Output",
      "type": "state"
    },
    {
      "id": "p_review_input",
      "name": "Manager Review - Input",
      "type": "state"
    },
    {
      "id": "p_review_active",
      "name": "Manager Review - Active",
      "type": "state"
    },
    {
      "id": "p_review_signal",
      "name": "Manager Review - Signal",
      "type": "signal"
    },
    {
      "id": "p_review_output",
      "name": "Manager Review - Output",
      "type": "state"
    },
    {
      "id": "p_end_done",
      "name": "End",
      "type": "terminal"
    },
    {
      "id": "p_effect_errors",
      "name": "Effect Errors",
      "type": "state"
    }
  ],
  "transitions": [
    {
      "id": "t_edge_start_to_fill",
      "name": "Start → Fill Form",
      "input_ports": [{ "name": "input", "cardinality": "single" }],
      "output_ports": [{ "name": "output", "cardinality": "single" }],
      "inputs": [{ "place": "p_start_ready", "port": "input" }],
      "outputs": [{ "port": "output", "place": "p_fill_input" }],
      "logic": {
        "type": "rhai",
        "source": "let d = input; d.title = \"Fill Application Form\"; d.instructions_mdsvex = \"Please fill in the required information.\"; d.steps = [{\"id\":\"fill\",\"title\":\"Application Details\",\"blocks\":[{\"type\":\"input\",\"field\":{\"name\":\"applicant_name\",\"label\":\"Applicant Name\",\"kind\":\"text\",\"required\":true}},{\"type\":\"input\",\"field\":{\"name\":\"amount\",\"label\":\"Requested Amount\",\"kind\":\"number\",\"required\":true}},{\"type\":\"input\",\"field\":{\"name\":\"reason\",\"label\":\"Reason\",\"kind\":\"textarea\",\"required\":true}}]}]; #{ output: d }"
      }
    },
    {
      "id": "t_fill_request",
      "name": "Fill Form - Request Human Task",
      "input_ports": [{ "name": "task", "cardinality": "single" }],
      "output_ports": [{ "name": "assigned", "cardinality": "single" }],
      "inputs": [{ "place": "p_fill_input", "port": "task" }],
      "outputs": [{ "port": "assigned", "place": "p_fill_active" }],
      "logic": {
        "type": "effect",
        "handler_id": "human_task",
        "config": { "place": "p_fill_signal" }
      }
    },
    {
      "id": "t_fill_finalize",
      "name": "Fill Form - Finalize",
      "input_ports": [
        { "name": "state", "cardinality": "single" },
        { "name": "signal", "cardinality": "single" }
      ],
      "output_ports": [{ "name": "done", "cardinality": "single" }],
      "inputs": [
        { "place": "p_fill_active", "port": "state" },
        { "place": "p_fill_signal", "port": "signal" }
      ],
      "outputs": [{ "port": "done", "place": "p_fill_output" }],
      "guard": { "type": "rhai", "source": "signal.task_id == state.task_id" },
      "logic": {
        "type": "rhai",
        "source": "#{ done: #{ instance_id: state.instance_id, applicant_name: signal.applicant_name, amount: signal.amount, reason: signal.reason } }"
      }
    },
    {
      "id": "t_edge_fill_to_review",
      "name": "Fill Form → Manager Review",
      "input_ports": [{ "name": "input", "cardinality": "single" }],
      "output_ports": [{ "name": "output", "cardinality": "single" }],
      "inputs": [{ "place": "p_fill_output", "port": "input" }],
      "outputs": [{ "port": "output", "place": "p_review_input" }],
      "logic": {
        "type": "rhai",
        "source": "let d = input; d.title = \"Manager Review\"; d.instructions_mdsvex = \"Review the application and make a decision.\"; d.steps = [{\"id\":\"review\",\"title\":\"Review Application\",\"blocks\":[{\"type\":\"mdsvex\",\"content\":\"**Applicant:** \" + input.applicant_name + \"\\n**Amount:** $\" + input.amount + \"\\n**Reason:** \" + input.reason},{\"type\":\"input\",\"field\":{\"name\":\"decision\",\"label\":\"Decision\",\"kind\":\"select\",\"options\":[\"approve\",\"reject\"],\"required\":true}},{\"type\":\"input\",\"field\":{\"name\":\"comments\",\"label\":\"Comments\",\"kind\":\"textarea\"}}]}]; #{ output: d }"
      }
    },
    {
      "id": "t_review_request",
      "name": "Manager Review - Request Human Task",
      "input_ports": [{ "name": "task", "cardinality": "single" }],
      "output_ports": [{ "name": "assigned", "cardinality": "single" }],
      "inputs": [{ "place": "p_review_input", "port": "task" }],
      "outputs": [{ "port": "assigned", "place": "p_review_active" }],
      "logic": {
        "type": "effect",
        "handler_id": "human_task",
        "config": { "place": "p_review_signal" }
      }
    },
    {
      "id": "t_review_finalize",
      "name": "Manager Review - Finalize",
      "input_ports": [
        { "name": "state", "cardinality": "single" },
        { "name": "signal", "cardinality": "single" }
      ],
      "output_ports": [{ "name": "done", "cardinality": "single" }],
      "inputs": [
        { "place": "p_review_active", "port": "state" },
        { "place": "p_review_signal", "port": "signal" }
      ],
      "outputs": [{ "port": "done", "place": "p_review_output" }],
      "guard": { "type": "rhai", "source": "signal.task_id == state.task_id" },
      "logic": {
        "type": "rhai",
        "source": "#{ done: #{ instance_id: state.instance_id, applicant_name: state.applicant_name, amount: state.amount, reason: state.reason, decision: signal.decision, comments: signal.comments } }"
      }
    },
    {
      "id": "t_edge_review_to_end",
      "name": "Manager Review → End",
      "input_ports": [{ "name": "input", "cardinality": "single" }],
      "output_ports": [{ "name": "output", "cardinality": "single" }],
      "inputs": [{ "place": "p_review_output", "port": "input" }],
      "outputs": [{ "port": "output", "place": "p_end_done" }],
      "logic": { "type": "rhai", "source": "#{ output: input }" }
    }
  ],
  "groups": [
    { "id": "grp_fill", "name": "Fill Form" },
    { "id": "grp_review", "name": "Manager Review" }
  ],
  "definitions": {}
}
```

### Handling Branches

For decision blocks, the compiler generates competing transitions with guards:

```
Source output place
    ├──> (t_branch_approved)  guard: "data.decision == \"approve\""  --> next block A
    ├──> (t_branch_rejected)  guard: "data.decision == \"reject\""   --> next block B
    └──> (t_branch_default)   guard: none                            --> next block C
```

### Handling Loops

The compiler detects loop blocks and generates:
1. A loop entry transition (initializes counter)
2. Body connections (the blocks inside the loop)
3. A loop-back transition (guard: continue condition AND counter < max)
4. A loop-exit transition (guard: negation of continue)

### Handling Parallel Paths

The compiler detects parallel split/join pairs and generates:
1. Fork transition with N output ports
2. Independent paths (each gets its own sequence of places/transitions)
3. Join transition with N input ports (only fires when all paths complete)

---

## 5. Template Storage Schema

### Database Tables

```sql
-- Workflow templates (top-level container)
CREATE TABLE workflow_templates (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),

    -- Identity
    name TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',

    -- Version chain
    base_template_id UUID REFERENCES workflow_templates(id),  -- Root of version chain (self for first version)
    parent_id UUID REFERENCES workflow_templates(id),          -- Previous version
    version INTEGER NOT NULL DEFAULT 1,
    is_latest BOOLEAN NOT NULL DEFAULT TRUE,

    -- Publishing
    published BOOLEAN NOT NULL DEFAULT FALSE,
    published_at TIMESTAMPTZ,
    published_by UUID,  -- User who published

    -- Graph data (the visual workflow)
    graph JSONB NOT NULL,  -- WorkflowGraph JSON (nodes, edges, viewport)

    -- Compiled AIR (populated on publish)
    air_json JSONB,  -- Compiled AIR JSON (NULL until published)

    -- Metadata
    author_id UUID NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Index for version chain queries
CREATE INDEX idx_wt_base_template ON workflow_templates(base_template_id);
CREATE INDEX idx_wt_is_latest ON workflow_templates(is_latest) WHERE is_latest = TRUE;
CREATE INDEX idx_wt_published ON workflow_templates(published) WHERE published = TRUE;

-- Workflow instances (running executions)
CREATE TABLE workflow_instances (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),

    -- Template reference (immutable after creation)
    template_id UUID NOT NULL REFERENCES workflow_templates(id),
    template_version INTEGER NOT NULL,

    -- petri-lab mapping
    net_id TEXT NOT NULL UNIQUE,  -- The net_id used in petri-lab engine

    -- State (derived from petri-lab, cached for queries)
    status TEXT NOT NULL DEFAULT 'created'
        CHECK (status IN ('created', 'running', 'completed', 'failed', 'cancelled')),

    -- Context
    created_by UUID NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    started_at TIMESTAMPTZ,
    completed_at TIMESTAMPTZ,

    -- Runtime data (optional, for quick queries without hitting petri-lab)
    current_step TEXT,        -- Which block the token is currently at
    metadata JSONB DEFAULT '{}' -- Arbitrary instance metadata
);

CREATE INDEX idx_wi_template ON workflow_instances(template_id);
CREATE INDEX idx_wi_status ON workflow_instances(status);
CREATE INDEX idx_wi_net_id ON workflow_instances(net_id);
```

### Version Management Design

The versioning follows the same pattern as the legacy system:

1. **Create first version:** `base_template_id = id` (self-referential), `version = 1`, `is_latest = true`
2. **Edit draft:** Directly update `graph` column (only while `published = false`)
3. **Publish:** Set `published = true`, `published_at = now()`, compile `graph` to `air_json`
4. **Create new version from published:**
   - Deep-copy the template row
   - New row: `parent_id = old.id`, `version = old.version + 1`, `published = false`
   - Old row: `is_latest = false`
   - New row: `base_template_id = old.base_template_id`, `is_latest = true`

### Publishing Workflow

```
Draft ──(edit graph)──> Draft ──(publish)──> Published (locked)
                                                  │
                                           (new version)
                                                  │
                                                Draft (v2) ──(edit)──> ...
```

On publish:
1. Validate graph (Section 4, step 1)
2. Compile to AIR JSON
3. Store compiled AIR in `air_json` column
4. Set `published = true`, `published_at = now()`

---

## 6. REST API Design

### Template Endpoints

```
GET    /api/v1/templates                    # List templates (paginated, filterable)
POST   /api/v1/templates                    # Create new template
GET    /api/v1/templates/:id                # Get template (with graph)
PUT    /api/v1/templates/:id                # Update template (draft only)
DELETE /api/v1/templates/:id                # Delete template (draft only)
POST   /api/v1/templates/:id/publish        # Publish template (compiles + locks)
POST   /api/v1/templates/:id/new-version    # Create new version from published
GET    /api/v1/templates/:id/versions       # List all versions in chain
GET    /api/v1/templates/:id/air            # Get compiled AIR JSON (published only)
POST   /api/v1/templates/:id/compile        # Preview AIR compilation (without publishing)
```

### Instance Endpoints

```
GET    /api/v1/instances                     # List instances (paginated, filterable)
POST   /api/v1/instances                     # Create + deploy instance
GET    /api/v1/instances/:id                 # Get instance details
GET    /api/v1/instances/:id/state           # Get live state from petri-lab
GET    /api/v1/instances/:id/events          # Get event stream from petri-lab
DELETE /api/v1/instances/:id                 # Cancel instance
```

### Request/Response Schemas

#### Create Template
```typescript
// POST /api/v1/templates
Request: {
  name: string;
  description?: string;
  graph?: WorkflowGraph;  // Optional initial graph (defaults to Start + End)
}

Response: {
  id: string;
  name: string;
  description: string;
  version: 1;
  published: false;
  graph: WorkflowGraph;
  created_at: string;
}
```

#### Update Template
```typescript
// PUT /api/v1/templates/:id
Request: {
  name?: string;
  description?: string;
  graph?: WorkflowGraph;
}

Response: { /* full template */ }
// Returns 409 Conflict if template is published
```

#### Publish Template
```typescript
// POST /api/v1/templates/:id/publish
Request: {} // No body needed

Response: {
  id: string;
  published: true;
  published_at: string;
  air_json: object;  // The compiled AIR
}
// Returns 400 if graph has validation errors
// Returns 409 if already published
```

#### Create Instance
```typescript
// POST /api/v1/instances
Request: {
  template_id: string;           // Must be a published template
  metadata?: Record<string, unknown>;  // Instance-specific context
}

Response: {
  id: string;
  template_id: string;
  template_version: number;
  net_id: string;
  status: 'created';
  created_at: string;
}
```

#### Get Instance State
```typescript
// GET /api/v1/instances/:id/state
Response: {
  instance_id: string;
  net_id: string;
  status: 'running' | 'completed' | 'failed' | 'cancelled';
  marking: {
    [place_id: string]: Array<{ id: string; color: object }>;
  };
  enabled_transitions: string[];
  current_step?: string;  // Friendly name of current block
}
```

#### List Templates
```typescript
// GET /api/v1/templates?page=1&per_page=20&published=true&search=approval
Response: {
  items: Template[];
  total: number;
  page: number;
  per_page: number;
}
```

---

## 7. Instance Execution Flow

### Overview

```
┌──────────────┐     ┌──────────────┐     ┌──────────────┐     ┌──────────────┐
│  mekhan-app  │────>│mekhan-service│────>│  petri-lab   │────>│  human-ui    │
│  (frontend)  │     │  (backend)   │     │  (engine)    │     │(task render) │
└──────────────┘     └──────────────┘     └──────────────┘     └──────────────┘
                                                │
                                                ├────>│aithericon-executor│
                                                │     │ (automated steps) │
                                                │
                                          NATS JetStream
```

### Step-by-Step Instance Lifecycle

#### 1. Create Instance

```
User clicks "Run Workflow" on a published template
    │
    ├── Frontend: POST /api/v1/instances { template_id }
    │
    ├── mekhan-service:
    │   1. Fetch template from DB (must be published)
    │   2. Generate net_id: "mekhan-{instance_uuid}"
    │   3. Parameterize AIR JSON:
    │      - Replace __INSTANCE_ID__ with instance UUID
    │      - Replace __TIMESTAMP__ with current time
    │      - Inject any instance metadata into initial token
    │   4. POST /api/nets/{net_id}/scenario to petri-lab engine
    │      (body: parameterized AIR JSON)
    │   5. PUT /api/nets/{net_id}/run-mode { "mode": "running" }
    │   6. Insert workflow_instance row in DB
    │   7. Return instance to frontend
    │
    └── petri-lab engine:
        - Creates net, loads topology
        - Starts evaluation loop
        - First transition fires (Start → first block)
```

#### 2. Human Task Surfaces in UI

```
petri-lab engine fires t_{id}_request (human_task effect)
    │
    ├── Engine publishes to NATS: human.request.{net_id}.{place_id}
    │   Body: HumanTaskRequest {
    │     task_id, net_id, place, corr_id,
    │     title, instructions_mdsvex, steps,
    │     response_subject: "petri.signal.{net_id}.{place_id}",
    │     process_id, process_step
    │   }
    │
    ├── aithericon-human-ui (NATS consumer):
    │   - Picks up message from HUMAN_REQUESTS stream
    │   - Stores in in-memory task store
    │   - Emits SSE event to connected browsers
    │
    └── Browser (human-ui task page):
        - Renders form based on steps/blocks
        - User fills in fields
        - User submits → human-ui publishes completion:
          1. human.completed.{net_id}.{place} (durable)
          2. petri.signal.{net_id}.{place} (ExternalSignal to engine)
```

#### 3. Engine Processes Human Response

```
ExternalSignal arrives at signal place p_{id}_signal
    │
    ├── Engine evaluates: t_{id}_finalize is now enabled
    │   (guard matches: signal.task_id == state.task_id)
    │
    ├── Transition fires:
    │   - Consumes tokens from p_{id}_active and p_{id}_signal
    │   - Merges human input data into workflow token
    │   - Produces token at p_{id}_output
    │
    └── Engine continues evaluation:
        - Next edge transition fires
        - Token moves to next block's input place
        - Cycle repeats
```

#### 4. Automated Step Triggers Executor

```
Token arrives at p_{id}_input (automated step block)
    │
    ├── t_{id}_prepare fires:
    │   - Builds ExecutionSpec from block config + token data
    │
    ├── t_{id}_submit fires (executor_submit effect):
    │   - Engine publishes execution job to NATS
    │   - aithericon-executor picks up job
    │   - Executor runs the job (Python/Docker/etc.)
    │   - Executor publishes status updates to NATS
    │
    ├── On completion: signal arrives at p_{id}_sig_complete
    │   - t_{id}_done fires, extracts results
    │   - Token moves to output place
    │
    └── On failure: signal arrives at p_{id}_sig_failed
        - t_{id}_failed fires
        - Token moves to error place
```

#### 5. State Tracking and Display

```
mekhan-app frontend polls or subscribes to state:
    │
    ├── Option A: Polling
    │   GET /api/v1/instances/{id}/state (via mekhan-service)
    │   mekhan-service proxies to: GET /api/nets/{net_id}/state
    │   Returns marking (which places have tokens) + enabled transitions
    │
    ├── Option B: SSE (preferred for live updates)
    │   GET /api/v1/instances/{id}/events (via mekhan-service)
    │   mekhan-service proxies to: GET /api/nets/{net_id}/events/stream
    │   Returns real-time DomainEvent stream
    │
    └── Frontend maps place IDs back to block names:
        - "Token at p_review_active" → "Currently at: Manager Review (waiting for response)"
        - "Token at p_end_done" → "Workflow completed"
```

### Instance State Derivation

The frontend derives user-friendly state from the Petri net marking:

| Place Pattern | User-Visible State |
|---|---|
| `p_start_ready` has token | "Not started" |
| `p_{id}_input` has token | "Ready: {block label}" |
| `p_{id}_active` has token | "Waiting: {block label} (human task pending)" |
| `p_{id}_submitted` has token | "Running: {block label} (automated step)" |
| `p_{id}_output` has token | "Completed: {block label}" |
| `p_end_done` has token | "Workflow completed" |
| `p_effect_errors` has token | "Error in: {block label}" |

---

## 8. Token Schema

### Standard Workflow Token

All tokens in a Mekhan workflow share a common base structure. Fields are added progressively as the token flows through blocks.

```typescript
type WorkflowToken = {
  // --- Core fields (set at instance creation) ---
  _instance_id: string;       // Mekhan instance UUID
  _template_id: string;       // Template UUID
  _template_version: number;  // Template version at deployment time
  _created_at: string;        // ISO 8601 timestamp
  _created_by?: string;       // User ID who started the instance

  // --- Loop tracking (injected by loop blocks) ---
  _loop_counts?: Record<string, number>;  // { "loop_node_id": count }

  // --- Human task fields (injected by edge transitions before human blocks) ---
  title?: string;                     // Current task title
  instructions_mdsvex?: string;       // Current task instructions
  steps?: TaskStep[];                 // Current task form definition

  // --- Accumulated workflow data ---
  // All fields collected from human tasks and automated steps
  // are merged directly into the token.
  // Example after a form with fields "name" and "amount":
  //   { ..., name: "Alice", amount: 1000, ... }
  [key: string]: unknown;
};
```

### Design Decisions

1. **Single token type:** All blocks share one token type. Fields accumulate as the workflow progresses. This avoids complex schema management and mirrors how the legacy system's "context store" worked (phaseSlug.stepSlug.property access).

2. **Underscore-prefixed system fields:** `_instance_id`, `_template_id`, etc. use underscore prefix to avoid collisions with user-defined field names.

3. **Form schema in token:** The human task form definition (title, instructions, steps/blocks) is injected into the token by edge transitions before human task blocks. This matches the existing pattern in petri-lab (`human_task_net.rs`) where the form is self-describing.

4. **Field merging strategy:** When a human task completes, the signal data (user input) is merged into the workflow token. Field names come from the `TaskField.name` in the block configuration. This means field names must be unique across the entire workflow (enforced by the editor).

---

## 9. Tech Stack Decisions

### Frontend

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Framework | SvelteKit + Svelte 5 | Matches human-ui; runes for state management |
| Flow editor | @xyflow/svelte v1.5+ | Used in lab-ui; handles pan/zoom/drag-drop |
| UI components | shadcn-svelte (bits-ui v2) | Matches human-ui component library |
| CSS | TailwindCSS 4 | Matches human-ui |
| Forms | sveltekit-superforms + zod | Matches human-ui pattern (formsnap) |
| API client | openapi-fetch | Type-safe API calls from OpenAPI spec |
| Runtime | Deno (via @deno/svelte-adapter) | Matches human-ui deployment |

### Backend

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Language | Rust | Matches petri-lab and executor; can share types |
| HTTP framework | Axum | Matches petri-lab; async, tower middleware |
| Database | PostgreSQL + SQLx | Async, compile-time query checking, migrations |
| Serialization | serde + serde_json | Standard Rust ecosystem |
| petri-lab client | reqwest | HTTP client for engine API calls |
| NATS client | async-nats | For listening to instance state events |
| Config | config-rs + env vars | Matches executor pattern |

### Infrastructure

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Database | PostgreSQL | Robust, JSONB for graph storage, already used in legacy |
| Message bus | NATS JetStream | Already used by petri-lab, human-ui, executor |
| Workflow engine | petri-lab (existing) | Core execution engine; no changes needed |
| Human tasks | aithericon-human-ui (existing) | Task rendering; no changes needed for MVP |
| Automated steps | aithericon-executor (existing) | Execution backends; no changes needed |

### What We Do NOT Build (MVP Scope)

- No batch controller / campaign net (Phase 2)
- No custom backend for aithericon-executor (use existing Python/process backends)
- No authentication system (use existing auth from human-ui or basic API key)
- No multi-tenant isolation (single deployment)
- No workflow analytics or reporting
- No template import/export

---

## 10. Integration Points Summary

```
                        ┌─────────────────────┐
                        │    mekhan-app        │
                        │   (SvelteKit)        │
                        │                      │
                        │  - Workflow editor    │
                        │  - Template CRUD UI   │
                        │  - Instance monitor   │
                        └──────────┬───────────┘
                                   │ HTTP
                        ┌──────────▼───────────┐
                        │   mekhan-service      │
                        │     (Rust/Axum)       │
                        │                       │
                        │  - Template CRUD API   │
                        │  - AIR compilation     │
                        │  - Instance lifecycle  │
                        │  - State proxy         │
                        └──┬──────────┬─────────┘
                           │          │
              HTTP API     │          │  NATS (listen for
              to engine    │          │  state events)
                           │          │
                ┌──────────▼──┐   ┌───▼──────────────┐
                │  petri-lab   │   │  NATS JetStream   │
                │  (engine)    │──>│                    │
                │              │   │  Streams:          │
                │  - Deploy    │   │  - PETRI_GLOBAL    │
                │  - Run       │   │  - HUMAN_REQUESTS  │
                │  - State     │   │  - HUMAN_COMPLETED │
                │  - Events    │   │  - EXECUTOR_STATUS │
                └──────────────┘   └──┬──────────┬─────┘
                                      │          │
                          ┌───────────▼──┐  ┌────▼──────────────┐
                          │ human-ui      │  │ aithericon-executor│
                          │ (SvelteKit)   │  │ (Rust service)     │
                          │               │  │                    │
                          │ - Task inbox  │  │ - Python backend   │
                          │ - Form render │  │ - Process backend  │
                          │ - Submit      │  │ - Docker backend   │
                          └───────────────┘  └────────────────────┘
```

### Key Integration Contracts

1. **mekhan-service → petri-lab:**
   - `POST /api/nets/{net_id}/scenario` — Deploy compiled AIR JSON
   - `PUT /api/nets/{net_id}/run-mode` — Set to "running"
   - `GET /api/nets/{net_id}/state` — Query current marking
   - `GET /api/nets/{net_id}/events/stream` — SSE event stream
   - `DELETE /api/nets/{net_id}` — Tear down instance

2. **petri-lab → human-ui (via NATS):**
   - `human.request.{net_id}.{place}` — Human task request (HUMAN_REQUESTS stream)
   - `human.completed.{net_id}.{place}` — Human task completion (HUMAN_COMPLETED stream)
   - `petri.signal.{net_id}.{place}` — External signal back to engine (PETRI_GLOBAL stream)

3. **petri-lab → aithericon-executor (via NATS):**
   - Executor submit/cancel via effect handlers
   - Status updates via `executor.status.{execution_id}.{status}`
   - Signals back via `petri.signal.{net_id}.{place}`

4. **mekhan-service → NATS (state tracking):**
   - Subscribe to `petri.events.{net_id}.>` for real-time instance state
   - Update `workflow_instances.status` based on lifecycle events
   - Forward events to frontend via SSE

---

## 11. Net Lifecycle & Cleanup

### Problem

Every Mekhan workflow instance creates a petri-lab net. Without cleanup:
- `KV_NET_METADATA` accumulates entries indefinitely, showing up in lab-ui's net list
- `PETRI_GLOBAL` stream accumulates events for every net, growing unbounded
- `KV_NET_ACTIVITY` retains stale entries for finished nets
- lab-ui (the Petri net debugger) becomes polluted with thousands of Mekhan workflow nets

### 11.1 Net ID Namespacing

All Mekhan nets use a `mekhan-` prefix:

```
net_id = "mekhan-{instance_uuid}"
```

Examples:
- `mekhan-a1b2c3d4-e5f6-7890-abcd-ef1234567890`
- `mekhan-550e8400-e29b-41d4-a716-446655440000`

This enables:
- **Metadata filtering:** lab-ui can filter out `mekhan-*` nets (or Mekhan can filter to only its own)
- **NATS subject-based purge:** `petri.events.mekhan-{uuid}.>` is a unique subject filter per net
- **KV key scanning:** `KV_NET_METADATA` keys starting with `mekhan-` are identifiable
- **Mekhan-service queries:** When listing instances, query `KV_NET_METADATA` for keys matching `mekhan-*`

The `template_id` field in `NetCreated` events is set to the Mekhan template UUID, providing additional cross-reference.

### 11.2 Instance Lifecycle State Machine

```
                  POST /api/v1/instances
                        │
                        ▼
                    ┌─────────┐
                    │ created │
                    └────┬────┘
                         │  deploy AIR to petri-lab
                         │  set run-mode = running
                         ▼
                    ┌─────────┐
                    │ running │◄──────────────────────┐
                    └────┬────┘                       │
                         │                            │
              ┌──────────┼──────────┐                │
              │          │          │           (rehydrate from
              │          │          │            hibernation)
              ▼          ▼          ▼
         ┌──────┐  ┌────────┐  ┌──────────┐
         │ done │  │ failed │  │cancelled │
         └──┬───┘  └───┬────┘  └────┬─────┘
            │          │            │
            └──────────┼────────────┘
                       │
                       ▼
              (cleanup sequence)
                       │
                       ▼
                  ┌──────────┐
                  │ archived │  (metadata purged, events retained with TTL)
                  └──────────┘
```

### 11.3 What mekhan-service Does on Instance Completion/Failure/Cancellation

mekhan-service subscribes to lifecycle events for its nets via a NATS consumer filtered on `petri.events.mekhan-*.net.>`. When a terminal event arrives:

#### On `NetCompleted` (workflow reached End block)

```
1. Update DB: workflow_instances SET status='completed', completed_at=NOW()
2. Extract exit data from the terminal place token (optional, for reporting)
3. Schedule deferred cleanup (see 11.4)
```

#### On `NetCancelled` (user cancelled or system error)

```
1. Update DB: workflow_instances SET status='cancelled', completed_at=NOW()
2. Schedule deferred cleanup (see 11.4)
```

#### On User-Initiated Cancel (DELETE /api/v1/instances/:id)

```
1. Call petri-lab: POST /api/nets/{net_id}/terminate
   Body: { "reason": "User cancelled", "cancelled_by": user_id }
   → This emits NetCancelled event and hibernates the net
2. Update DB: workflow_instances SET status='cancelled', completed_at=NOW()
3. Schedule deferred cleanup (see 11.4)
```

### 11.4 Archival Strategy: Deferred Cleanup with Retention Window

The strategy is **deferred cleanup** — finished nets are not immediately purged. A configurable retention window allows debugging and auditing before data is cleaned up.

#### Configuration

```toml
[cleanup]
# How long to retain finished net data after completion/cancellation
retention_hours = 72       # 3 days default

# How often the cleanup task runs
sweep_interval_minutes = 60  # 1 hour default

# Whether to purge NATS event data (vs just metadata)
purge_events = true
```

#### Cleanup Sequence (per finished net)

When a net has been in terminal state (completed/cancelled) longer than `retention_hours`:

```
Step 1: Remove from petri-lab in-memory registry (if still loaded)
        DELETE /api/nets/{net_id}
        → Idempotent; returns 404 if already hibernated, which is fine

Step 2: Delete KV_NET_METADATA entry
        NATS KV: kv.purge("mekhan-{uuid}")
        → Removes the metadata tombstone
        → lab-ui will no longer see this net

Step 3: Delete KV_NET_ACTIVITY entry (if exists)
        NATS KV: kv.purge("mekhan-{uuid}")
        → Removes idle tracking data

Step 4: Purge NATS event stream data (if purge_events=true)
        NATS JetStream: stream.purge_subject("petri.events.mekhan-{uuid}.>")
        → Removes all events for this net from PETRI_GLOBAL
        → Frees disk space
        → Net can no longer be rehydrated (intentional — it's archived)

Step 5: Purge NATS signal data
        NATS JetStream: stream.purge_subject("petri.signal.mekhan-{uuid}.>")
        → Removes stale signals

Step 6: Update DB
        workflow_instances SET status='archived'
        → Or simply leave as completed/cancelled — the DB record IS the archive
```

#### Background Cleanup Task

mekhan-service runs a periodic background task:

```rust
// In mekhan-service startup
tokio::spawn(async move {
    let mut interval = tokio::time::interval(
        Duration::from_secs(config.cleanup.sweep_interval_minutes * 60)
    );
    loop {
        interval.tick().await;
        cleanup_finished_instances(&db, &nats, &petri_client, &config).await;
    }
});

async fn cleanup_finished_instances(...) {
    // Query DB for instances that are completed/cancelled/failed
    // AND completed_at < NOW() - retention_hours
    let stale = sqlx::query!(
        r#"SELECT id, net_id FROM workflow_instances
           WHERE status IN ('completed', 'failed', 'cancelled')
           AND completed_at < NOW() - $1::interval"#,
        format!("{} hours", config.cleanup.retention_hours)
    ).fetch_all(&db).await?;

    for instance in stale {
        cleanup_net(&instance.net_id, &nats, &petri_client).await;
    }
}
```

### 11.5 NATS Event Stream Retention

#### Short-Term: Subject-Based Purge (MVP)

For the MVP, rely on the deferred cleanup (Section 11.4) to purge per-net events after the retention window. This uses `stream.purge_subject()` which is a NATS built-in operation.

#### Long-Term: Stream-Level TTL

NATS JetStream supports `max_age` on streams. The `PETRI_GLOBAL` stream could be configured with a global TTL (e.g., 30 days). However, this affects ALL nets (including non-Mekhan ones), so it should be configured at the petri-lab deployment level, not by Mekhan.

#### Human Task Stream Cleanup

Human tasks created by Mekhan nets are in `HUMAN_REQUESTS` and `HUMAN_COMPLETED` streams. These are NOT purged by Mekhan — the human-ui manages its own retention. Human task data is valuable for audit trails and should persist independently of the net lifecycle.

### 11.6 Preventing lab-ui Pollution

Three complementary approaches:

#### Approach 1: Metadata Filtering (Recommended for MVP)

lab-ui's `GET /api/nets/metadata` returns ALL nets from `KV_NET_METADATA`. The endpoint already returns `template_id` per net. Two options:

**Option A — Client-side filter in lab-ui:**
lab-ui adds a UI filter to hide nets by prefix. Users can toggle "Show Mekhan nets" on/off. This requires a small lab-ui change (add a filter toggle to the net list).

**Option B — Server-side filter parameter:**
Add a query parameter to `GET /api/nets/metadata?exclude_prefix=mekhan-`. This requires a small petri-lab API change.

**Recommendation:** Option A for MVP (simpler, no petri-lab changes needed).

#### Approach 2: Metadata Cleanup (Already Covered)

The deferred cleanup (Section 11.4, Step 2) purges `KV_NET_METADATA` entries after the retention window. Finished Mekhan nets disappear from lab-ui automatically after 72 hours (configurable).

#### Approach 3: Separate NATS Prefix (NOT Recommended)

Using a different NATS subject prefix (e.g., `mekhan.events.>` instead of `petri.events.>`) would completely isolate Mekhan nets from petri-lab's global stream. However, this breaks the engine's assumption of a single `PETRI_GLOBAL` stream and would require forking petri-lab's NATS infrastructure. Not worth it for the MVP.

### 11.7 Template Deletion Cascade

When a user deletes a workflow template, associated test/draft instances must be cleaned up.

#### Rules

1. **Published templates cannot be deleted** if they have active (running) instances.
2. **Draft templates** can be deleted freely (they have no instances).
3. **Published templates with only finished instances** can be deleted — trigger cascade cleanup.

#### Cascade Sequence

```
DELETE /api/v1/templates/:id
    │
    ├── Check: any running instances?
    │   YES → 409 Conflict: "Cannot delete template with active instances"
    │   NO  → continue
    │
    ├── For each instance (completed/failed/cancelled):
    │   ├── Immediate cleanup (skip retention window):
    │   │   Steps 1-5 from Section 11.4
    │   └── Delete workflow_instances row
    │
    ├── Delete all versions in chain:
    │   DELETE FROM workflow_templates WHERE base_template_id = :base_id
    │
    └── Return 204 No Content
```

### 11.8 Backend-Dev Implementation Guidance

#### petri-lab HTTP Client Methods Needed

Add these to the `PetriLabClient` in `mekhan-service/src/petri/client.rs`:

```rust
impl PetriLabClient {
    // Already needed for instance creation:
    pub async fn deploy_scenario(&self, net_id: &str, air_json: &Value) -> Result<()>;
    pub async fn set_run_mode(&self, net_id: &str, mode: &str) -> Result<()>;
    pub async fn get_state(&self, net_id: &str) -> Result<NetState>;

    // NEW — needed for cleanup:

    /// Remove net from in-memory registry. Idempotent (404 is OK).
    /// DELETE /api/nets/{net_id}
    pub async fn delete_net(&self, net_id: &str) -> Result<()>;

    /// Terminate a running net (emits NetCancelled, then hibernates).
    /// Uses registry.terminate() — currently no direct HTTP endpoint,
    /// so implement as: set run-mode to paused, then delete.
    /// Alternative: add a POST /api/nets/{net_id}/terminate endpoint to petri-lab.
    pub async fn terminate_net(&self, net_id: &str, reason: &str) -> Result<()>;
}
```

#### NATS Operations Needed

```rust
impl MekhanNatsClient {
    /// Purge all event data for a specific net from PETRI_GLOBAL stream.
    pub async fn purge_net_events(&self, net_id: &str) -> Result<()> {
        let stream = self.jetstream.get_stream("PETRI_GLOBAL").await?;
        // Purge events
        stream.purge()
            .filter(&format!("petri.events.{}.>", net_id))
            .await?;
        // Purge signals
        stream.purge()
            .filter(&format!("petri.signal.{}.>", net_id))
            .await?;
        Ok(())
    }

    /// Delete metadata KV entry for a net.
    pub async fn delete_net_metadata(&self, net_id: &str) -> Result<()> {
        let kv = self.jetstream.get_key_value("KV_NET_METADATA").await?;
        kv.purge(net_id).await?;
        Ok(())
    }

    /// Delete activity KV entry for a net.
    pub async fn delete_net_activity(&self, net_id: &str) -> Result<()> {
        let kv = self.jetstream.get_key_value("KV_NET_ACTIVITY").await?;
        kv.purge(net_id).await?;
        Ok(())
    }
}
```

#### Cleanup Execution Order

For a single net cleanup, call in this order:

```
1. petri_client.delete_net(net_id)          // Free engine memory (idempotent)
2. nats_client.delete_net_metadata(net_id)  // Remove from lab-ui listing
3. nats_client.delete_net_activity(net_id)  // Remove idle tracking
4. nats_client.purge_net_events(net_id)     // Free NATS disk (optional per config)
```

All operations are idempotent — safe to retry on partial failure.

#### Database Migration

Add a status check constraint update to support 'archived' if desired:

```sql
-- Optional: add 'archived' to the status enum
ALTER TABLE workflow_instances
    DROP CONSTRAINT IF EXISTS workflow_instances_status_check,
    ADD CONSTRAINT workflow_instances_status_check
        CHECK (status IN ('created', 'running', 'completed', 'failed', 'cancelled', 'archived'));
```

#### Lifecycle Event Listener

mekhan-service needs a NATS consumer for lifecycle events:

```rust
/// Subscribe to lifecycle events for all Mekhan nets.
/// Filter: petri.events.mekhan-*.net.>
async fn start_lifecycle_listener(
    jetstream: async_nats::jetstream::Context,
    db: PgPool,
) {
    let stream = jetstream.get_stream("PETRI_GLOBAL").await.unwrap();
    let consumer = stream.get_or_create_consumer(
        "mekhan-lifecycle",
        ConsumerConfig {
            durable_name: Some("mekhan-lifecycle".into()),
            filter_subject: "petri.events.mekhan-*.net.>".into(),
            ack_policy: AckPolicy::Explicit,
            deliver_policy: DeliverPolicy::New,
            ..Default::default()
        }
    ).await.unwrap();

    let messages = consumer.messages().await.unwrap();
    while let Some(msg) = messages.next().await {
        let msg = msg.unwrap();
        // Parse the lifecycle event and update DB
        // Extract net_id from subject: petri.events.{net_id}.net.{event}
        let parts: Vec<&str> = msg.subject.as_str().split('.').collect();
        let net_id = parts[2];
        let event_type = parts.last().unwrap();

        match *event_type {
            "completed" => {
                sqlx::query!(
                    "UPDATE workflow_instances SET status='completed', completed_at=NOW()
                     WHERE net_id=$1 AND status='running'",
                    net_id
                ).execute(&db).await.ok();
            }
            "cancelled" => {
                sqlx::query!(
                    "UPDATE workflow_instances SET status='cancelled', completed_at=NOW()
                     WHERE net_id=$1 AND status='running'",
                    net_id
                ).execute(&db).await.ok();
            }
            _ => {} // Ignore created, initialized
        }
        msg.ack().await.ok();
    }
}
```
