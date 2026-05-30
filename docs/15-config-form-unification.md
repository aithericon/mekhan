# 15 — Unifying Node Configuration Forms in the Editor

## Thesis

The visual editor today runs **three parallel form systems**, each with its own
field-kind vocabulary and its own renderer:

1. **Bespoke per-node property sections** — one hand-written Svelte component per
   node type, dispatched by an exhaustive registry
   (`app/src/lib/editor/node-property-sections.ts`).
2. **Backend config panels** — resource / step config editors, several of which
   *already* render generically from a JSON Schema via
   `SchemaForm.svelte`.
3. **The HPI HumanTask block model** — a fully data-driven renderer over a tagged
   union of field kinds (`app/src/lib/hpi/types.ts`), already shipping in
   production.

The opportunity is **not** "make the editor data-driven" — HPI already proves the
data-driven approach works at production quality. The opportunity is to **collapse
the three field-kind vocabularies and three renderers into one**: a single
`FieldKind`, a single kind→widget renderer, fed by a **declarative node-config
spec** and a registry, reusing the node-type-agnostic **slot widgets** that
already exist (ref picker, resource picker, code editor, key/value, string list),
with **explicit, named escape hatches** for the relational UX that a flat schema
genuinely cannot express.

This is a structural refactor with a low-regret incremental path. Roughly half the
bespoke sections collapse to spec; the rest get thinner by consuming shared slots.

---

## Current state — three systems, one dispatch backbone

| System | Where it lives | How it renders | Field vocabulary |
|--------|----------------|----------------|------------------|
| Per-node property sections | `app/src/lib/components/editor/panels/property-sections/*.svelte`, registered in `app/src/lib/editor/node-property-sections.ts` | One bespoke Svelte component per node type | none (each field hand-coded) |
| Backend config panels | `app/src/lib/components/editor/panels/shared/SchemaForm.svelte` + resource editors (e.g. `ResourceEditModal`) | Generic JSON-Schema → widget renderer | `JsonType` / `FieldSpec` |
| HPI HumanTask blocks | `app/src/lib/hpi/types.ts` + the HumanTask block renderer | Data-driven tagged-union block tree | `TaskFieldKind` |

The one piece of unification that **already exists** is the dispatch backbone. Every
section is invoked through a uniform contract and an exhaustive registry.

```ts
// app/src/lib/editor/node-property-sections.ts
export type SectionProps = {
	data: WorkflowNodeData;
	readonly: boolean;
	onchange: (data: WorkflowNodeData) => void;
	binding?: YjsGraphBinding;
	nodeId?: string;
	templateId?: string;
	scope?: ScopeEntry[];
	resourceScope?: ScopeEntry[];
	onselectnode?: (id: string) => void;
};

export const NODE_PROPERTY_SECTIONS: Record<string, Component<SectionProps>> = {
	start: StartNodeSection,
	end: EndNodeSection,
	automated_step: AutomatedStepNodeSection,
	decision: DecisionNodeSection,
	agent: AgentNodeSection,
	loop: LoopNodeSection,
	parallel: ParallelNodeSection,
	join: JoinNodeSection,
	map: MapNodeSection,
	human_task: HumanTaskNodeSection,
	sub_workflow: SubWorkflowNodeSection,
	delay: DelayNodeSection,
	timeout: TimeoutNodeSection,
	progress_update: ProgressUpdateNodeSection,
	phase_update: PhaseUpdateNodeSection,
	failure: FailureNodeSection,
	scope: ScopeNodeSection,
	trigger: TriggerNodeSection
};
```

This is the right shape to build on: a `Record<NodeKind, Component<SectionProps>>`
with a uniform `data / onchange / scope / resourceScope` contract. The work is to
let *most* entries in that map be **generated from a spec** instead of hand-written,
while keeping the map (and the escape hatch) for the genuinely relational ones.

---

## The core duplication: three field-kind vocabularies

These three enums describe overwhelmingly the same widget set, each maintained
independently.

**1. Port `FieldKind`** (typed-port field kind, consumed by `PortFieldEditor` via
`PortsSection`). Generated in `app/src/lib/api/schema.d.ts`; `PortField.kind` uses
it. (Note: it is *not* in the `editor.ts` re-export list — `PortsSection.svelte`
imports `Port`/`PortField` directly from `components['schemas']`.)

```
text · textarea · number · bool · select · file · signature · timestamp · json
```

**2. HPI `TaskFieldKind`** (`app/src/lib/hpi/types.ts`, `TASK_FIELD_KINDS`; the
generated `schema.d.ts` mirror at line 4808 is identical and must stay in sync with
the engine's `TaskFieldKind`).

```
text · textarea · number · select · checkbox · file · signature · radio · date · range · rating
```

**3. SchemaForm `JsonType` / `FieldSpec`** (`SchemaForm.svelte` module script), the
JSON-Schema-derived widget vocabulary.

```
string · integer · number · boolean · array · object · unknown
```

Side by side:

| Concept | Port FieldKind | HPI TaskFieldKind | SchemaForm JsonType |
|---------|----------------|-------------------|---------------------|
| short text | `text` | `text` | `string` |
| long text | `textarea` | `textarea` | `string` (no widget hint) |
| number | `number` | `number` | `integer` / `number` |
| boolean | `bool` | `checkbox` | `boolean` |
| enum | `select` | `select` / `radio` | `string` + `enum` |
| file | `file` | `file` | — |
| signature | `signature` | `signature` | — |
| date/time | `timestamp` | `date` | `string` (format) |
| structured | `json` | — | `object` / `array` |
| (HPI-only) | — | `range` · `rating` | — |

There is ~70% overlap. The differences are not conceptual — they are naming drift
(`bool` vs `checkbox` vs `boolean`) and a few specializations (`radio`, `range`,
`rating` are HPI presentation variants of `select`/`number`; `json` is the port
side's name for `object`/`array`). Three teams maintaining three names for "a
checkbox" is exactly the kind of duplication that silently drifts.

**Collapsing these into ONE `FieldKind` + ONE kind→widget renderer is the
highest-payoff, lowest-risk move and a prerequisite for everything else.** Until
the vocabulary is single-sourced, every other unification step has to translate
between three dialects.

---

## The slots already exist

The renderers are already decoupled. The shared widgets below take plain
`props + onchange` and have **zero node-type coupling** — they don't know whether
they're inside a Delay, an Agent, or a resource editor. A data-driven layer
therefore needs only a declarative field-spec plus a dispatcher; it does **not**
need new primitives.

- **`RefPicker`** — `property-sections/RefPicker.svelte`. Two-column popover
  (producer/alias column + recursive type-tree variable column), Refs/Resources tab
  switch when `resourceScope` is non-empty. Emits a synthesized `ScopeEntry` whose
  `.qualified` is the picked `<slug>.<field>` (nested / `[*]` paths supported via
  `allowArrayBoundary`). Props: `{ scope, resourceScope?, disabled?, selected?,
  placeholder?, triggerClass?, allowArrayBoundary?, onpick }`.
- **`ResourcePicker`** — `property-sections/shared/ResourcePicker.svelte`.
  Self-loads workspace resources via `listResources({ resource_type, perPage: 200 })`;
  renders a `Select` with a "None — provide inline" option plus loading/empty/error
  hints. Props: `{ resourceType, selected, onChange, label?, readonly?, testId?,
  typeLabel? }`.
- **`CodeEditor`** — `panels/shared/CodeEditor.svelte`. CodeMirror 6 lazy-loaded on
  mount, one-way `onchange` (no `bind`), external value synced via `$effect`. Props:
  `{ value, language?: 'python'|'json'|'rhai', readonly?, dimWhenReadonly?,
  minHeight?, maxHeight?, onchange? }`. Yjs-collaborative sibling
  `CollabCodeEditor.svelte` binds to a `YjsGraphBinding` text channel (the
  `SectionProps.binding`); pair it with `CodeEditor` when the field is Yjs-backed
  multi-author text.
- **`KeyValueEditor`** — `panels/shared/KeyValueEditor.svelte`. Local-authoritative
  draft rows (empty-key rows persist until filled); values `JSON.parse`'d back to
  native, falling back to string. Optional `scope` adds a per-row `InsertRefButton`
  appending `{{ <slug>.<field> }}`. Props: `{ entries, readonly?, keyPlaceholder?,
  valuePlaceholder?, onchange, scope? }`.
- **`StringListEditor`** — `panels/shared/StringListEditor.svelte`. Add/remove rows
  of `Input`, fully derived (no local draft state). Props: `{ items, readonly?,
  placeholder?, onchange }`.

And the central renderer itself:

- **`SchemaForm`** — `panels/shared/SchemaForm.svelte`. THE generic schema→widget
  renderer, already shared with resources / `ResourceEditModal`. Props:
  `{ schema, value, secretFields?, readonly?, fieldOrder?, booleanWidget?,
  secretPlaceholder?, coerceNumbers?, onchange }`. Exports
  `deriveFieldSpecs(schema, secretFields, fieldOrder)` plus the `JsonType` /
  `FieldSpec` types. Its widget chain off `jsonType` / `enum` / `secret`:
  `enum → Select`, `boolean → Checkbox|Select`, `integer|number → numeric Input`,
  `array<string> → StringListEditor`, `object(properties) → recursive Self`,
  `object(additionalProperties) → KeyValueEditor`, `secret → password Input`,
  else `text Input`. Built on `ui Input`/`Checkbox`/`FormField` + `Select.*`.

Primitive layer (consume directly in custom slots, or via `SchemaForm`):
`ui Input` (uncontrolled: `value={...}` + `oninput`, `class='text-sm'`),
`ui Select` (namespace import; `Select.Root type='single'` +
`onValueChange`), `ui Checkbox` (`onCheckedChange`, wrapped in a `text-sm`
`<label>`), `ui Textarea` (`oninput`), `ui FormField`
(labeled-row host; keep label/description `>= text-sm`).

The implication: **`SchemaForm` is already 80% of the data-driven renderer.** What
is missing is (a) a unified field-kind vocabulary it dispatches on, (b) the two
ref-aware slots (`RefPicker`, `ResourcePicker`) plugged into the widget chain, and
(c) a node-level *spec* that says which fields a node has and which slot each binds.

---

## Declarative `NodeConfigSpec` + slot dispatcher

The proposal is a declarative `ConfigFieldSpec` describing one field — its kind, the
path in `WorkflowNodeData` it binds to, its label/description, and per-kind options
— plus a generic `SchemaDrivenSection` that renders a `NodeConfigSpec` against the
existing `SectionProps` contract and wires `onchange` by bind path.

```ts
// proposed: app/src/lib/editor/config-spec/types.ts
export type ConfigFieldKind =
	| 'text'
	| 'textarea'
	| 'number'
	| 'bool'
	| 'select'
	| 'ref'          // RefPicker slot
	| 'resource'     // ResourcePicker slot
	| 'code'         // CodeEditor / CollabCodeEditor slot
	| 'keyvalue'     // KeyValueEditor slot
	| 'stringlist'   // StringListEditor slot
	| 'schema';      // nested SchemaForm sub-form

export type ConfigFieldSpec = {
	kind: ConfigFieldKind;
	/** dot-path into WorkflowNodeData, e.g. 'durationMsExpr' or 'config.timeoutMs' */
	bind: string;
	label: string;
	description?: string;
	required?: boolean;
	/** per-kind options */
	options?: string[];                 // select
	resourceType?: string;              // resource
	language?: 'python' | 'json' | 'rhai'; // code
	allowRefs?: boolean;                // text/textarea: show InsertRefButton
	min?: number; max?: number; step?: number; // number
};

export type NodeConfigSpec = {
	nodeType: string;
	fields: ConfigFieldSpec[];
};
```

`SchemaDrivenSection` is the generic component that closes the loop. It receives the
standard `SectionProps`, looks up the `NodeConfigSpec` for `data.type`, and for each
field reads the value at `bind`, renders the slot for `kind` (reusing the widgets
above), and on change writes a shallow-cloned, path-updated copy back through
`onchange`. It is registered into `NODE_PROPERTY_SECTIONS` for every node whose
section is spec-only.

Example specs:

```ts
// delay collapses to a single ref-aware code/expression field
export const delaySpec: NodeConfigSpec = {
	nodeType: 'delay',
	fields: [
		{
			kind: 'code',
			language: 'rhai',
			bind: 'durationMsExpr',
			label: 'Wait for (ms)',
			allowRefs: true,
			description: 'Rhai expression evaluated to milliseconds. Supports {{ slug.field }} refs.'
		}
	]
};

// progress_update — four scalar fields, message ref-aware
export const progressUpdateSpec: NodeConfigSpec = {
	nodeType: 'progress_update',
	fields: [
		{ kind: 'number', bind: 'fraction', label: 'Fraction', min: 0, max: 1, step: 0.05 },
		{ kind: 'number', bind: 'currentStep', label: 'Current step', min: 0 },
		{ kind: 'number', bind: 'totalSteps', label: 'Total steps', min: 0 },
		{ kind: 'textarea', bind: 'message', label: 'Message', allowRefs: true }
	]
};
```

Today these are two hand-written components —
`property-sections/DelayNodeSection.svelte` (which wraps `GuardEditor` →
`RefPicker` under a "Wait for (ms)" label) and
`property-sections/ProgressUpdateNodeSection.svelte` (with `clampFraction`,
`optInt`, a `pct%` label, a `currentStep`/`totalSteps` flex row, and a `Textarea` +
`InsertRefButton`). Both reduce to ~12-line spec objects with **no behavior loss**;
clamping/coercion (`clampFraction`, `optInt`) becomes a property of the `number`
slot, shared by every numeric field everywhere.

---

## Tiering — what collapses vs what stays bespoke

**Tier 1 — collapses cleanly to schema-driven.** Flat scalar/expression fields,
optionally ref-aware. No relational UX.

- `delay`, `timeout` (byte-identical `durationMsExpr` shape — the natural
  second/third migration after the spike), `phase_update`, `progress_update`,
  `failure`, `join`, `scope`, `map`, plus **most backend config panels** (which
  already lean on `SchemaForm`).

**Tier 2 — schema body + one custom slot.** Mostly spec-driven, with a single
bespoke region that the spec references as a `{ kind: 'custom', component }` slot.

- `start` (initial-port field declarations), `end` (result mapping),
  `agent` (the `response_format` JSON-schema builder is the custom region; the rest
  — model, retry, deployment — is flat spec), `trigger`.

**Tier 3 — stays bespoke, but consumes shared slots.** Genuinely relational
authoring surfaces a flat field list cannot express; they keep a hand-written
component but stop reimplementing primitives.

- `decision` (branch reorder + per-branch guard via `RefPicker`),
  `loop` (accumulator rows + loop-condition guard — see `docs/14-loop-carried-state.md`),
  `sub_workflow` (async child I/O contract; derived ports),
  `human_task` (recursive HPI block tree — the most relational of all).

Estimated outcome: **roughly half** of the eighteen registered sections collapse to
spec objects (Tier 1), Tier 2 shrinks to "spec + one slot", and Tier 3 keeps its
component but sheds duplicated widget code by consuming the shared slots.

---

## Where the registry lives — and the honest tension

The project's standing principles — *"declared over inferred contracts"* and
*"single-source-of-truth registries over auto-derive"* — point toward an eventual
**backend-sourced field spec**: the node metadata registry already exists at
`GET /api/v1/node-types`, so the config spec could be served alongside node metadata
rather than hand-maintained twice (engine schema + frontend spec).

But there is a sharp tension that must be named, not papered over: **a flat backend
schema cannot express relational UX.** Branch reordering, accumulator rows bound to
loop scope, a recursive block tree, an async child I/O contract — none of these are
"a list of fields." If the backend registry tries to model them, it reinvents a UI
layout language inside JSON Schema.

The resolution: the registry is **"schema + named escape hatches,"** not pure
schema. The spec carries flat fields *and* a `{ kind: 'custom', component: 'DecisionBranches' }`
slot that names a frontend component by key. The backend declares *what fields exist
and their kinds*; the frontend owns *how the relational ones render*. This keeps the
contract single-sourced for the 90% that is flat, without forcing the relational 10%
through a schema it doesn't fit.

Two enabling pieces just landed in this area and are exactly what schema-driven typed
sub-forms need: client-side `definitions` / `$ref` resolution in
`app/src/lib/editor/workflow-definitions.svelte.ts` and the derived-port plumbing in
`app/src/lib/editor/derived-ports.ts`. A `{ kind: 'schema' }` field that points at a
`$ref` can now resolve it client-side before handing the resolved schema to
`SchemaForm` — no round-trip, no inlining (which the LLM-node migration had to do as
a stopgap because output-port derivation couldn't reach the `definitions`).

---

## HPI is the engine, not a complication

It is tempting to treat the HumanTask block model as a fourth thing to reconcile. It
is the opposite: **HPI is the most mature data-driven renderer in the codebase**, and
it already proves the whole premise — a tagged union of field kinds rendered
generically, authored as pure runtime JSON, with answers as a flat `name → value`
map (see the HPI reference impl notes and `app/src/lib/hpi/types.ts`).

The move is to **generalize HPI's philosophy to node *authoring***, not to build a
parallel system beside it. Concretely: unify `TaskFieldKind` into the one shared
`FieldKind`, so a "select" or "file" or "signature" field means the same thing and
renders through the same widget whether it appears in a human-task block, a typed
port, or a node config spec. HumanTask stays Tier 3 (its recursive block tree is
relational), but it draws from the same vocabulary and the same slot widgets as
everything else instead of owning a private dialect.

---

## Incremental, low-regret path

1. **Unify the field-kind vocabulary first.** One `FieldKind` + one kind→widget
   renderer, shared by ports (`FieldKind`), HPI (`TaskFieldKind`), and config
   (`JsonType`). This is the prerequisite and the highest-payoff step; everything
   below assumes it. Keep the engine-facing names in sync (the `schema.d.ts` mirror
   of `TaskFieldKind` must track the engine).
2. **Define `NodeConfigSpec` + the generic `SchemaDrivenSection`, migrate Tier 1.**
   Replace bespoke Tier-1 components with spec objects registered through
   `NODE_PROPERTY_SECTIONS`. No behavior change; clamp/coerce becomes a slot property.
3. **Add the `{ kind: 'custom', component }` escape hatch, migrate Tier 2.** Spec
   body + one named slot for the bespoke region (Agent `response_format`, Start/End
   declarations, Trigger).
4. **Only then lift the spec into the backend registry** (`/api/v1/node-types`), with
   the escape hatch preserved as a named key, leaning on the just-landed `$ref`
   resolution for typed sub-forms.
5. **Leave Tier 3 bespoke**, consuming shared slots (Decision, Loop, SubWorkflow,
   HumanTask authoring).

Each step is independently shippable and reversible. Nothing forces a big-bang cut.

---

## Prototype delivered alongside this doc

A spike is committed in this same worktree to de-risk steps 2–3 concretely. It is an
**additive** config-spec layer:

- `config-spec/types.ts` — the `ConfigFieldSpec` / `NodeConfigSpec` shapes above.
- a `FieldRenderer` — the kind→slot dispatcher, reusing the existing shared widgets
  (`Input`, `Textarea`, `RefPicker`/`InsertRefButton`, etc.) with no new primitives.
- `SchemaDrivenSection.svelte` — renders a `NodeConfigSpec` against `SectionProps`,
  wiring `onchange` by bind path.
- example specs for **`delay`** and **`progress_update`**, wired into
  `NODE_PROPERTY_SECTIONS` to demonstrate spec-driven sections replacing the two
  hand-written components.

The spike is deliberately **additive and scoped**: the existing three enums are left
untouched, so the build stays green and no in-flight work is disturbed. It
demonstrates the dispatcher and the spec ergonomics on the two cleanest Tier-1 nodes
— it is **not** the full vocabulary unification, which is step 1 above and the real
prerequisite for the rest of the plan. The spike validates that step 1's payoff is
reachable without a rewrite: once the vocabulary is unified, the spec layer here
generalizes to the whole of Tier 1 and the spec body of Tier 2.
