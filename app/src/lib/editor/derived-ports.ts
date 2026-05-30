// Phase 4: TS twin of `WorkflowNodeData::output_ports` for the variants
// whose ports are *derived* from inner config rather than carried as
// editable state. The editor displays these read-only — keeping them in
// sync with the backend definition ensures the visual port summary on each
// node card matches what the compiler will see at publish.

import type { components } from '$lib/api/schema';
import { getWorkflowDefinitions } from '$lib/editor/workflow-definitions.svelte';
import { fromTaskFieldKind, fromJsonType } from '$lib/fields/adapters';

type WorkflowNodeData = components['schemas']['WorkflowNodeData'];
type Port = components['schemas']['Port'];
type PortField = components['schemas']['PortField'];
type FieldKind = components['schemas']['FieldKind'];
type TaskFieldKind = components['schemas']['TaskFieldKind'];

export function outputPortsFor(data: WorkflowNodeData): Port[] {
	switch (data.type) {
		case 'start':
			return data.initial ? [data.initial] : [];
		case 'automated_step':
			// Success output + the always-present "error" output (retries
			// exhausted / infra failure). Mirrors the compiler's NodePorts.
			return [
				...(data.output ? [data.output] : []),
				{ id: 'error', label: 'On error', fields: [] }
			];
		case 'agent':
			// Derived — TS twin of `Self::Agent::output_ports`. The
			// canonical four LLM fields always; agent-loop extras (turn /
			// history / final_response / input) only when the agent will
			// take the loop path (`max_turns > 1 || stop_when set`). Read-
			// only in the panel; the compiler is the source of truth.
			return deriveAgentOutputPorts(data);
		case 'human_task':
			return [deriveHumanTaskOutputPort(data)];
		case 'decision':
			return deriveDecisionOutputPorts(data);
		case 'timeout':
			// Two fixed outputs: `out` (done — body finished in time) and
			// `timeout` (deadline won). Pass-through envelope, no declared
			// fields (the inbound token rides through unchanged).
			return [
				{ id: 'out', label: 'Done', fields: [] },
				{ id: 'timeout', label: 'Timed out', fields: [] }
			];
		case 'parallel_split':
		case 'loop':
		case 'delay':
		case 'scope':
		case 'phase_update':
		case 'progress_update':
		case 'failure':
		case 'trigger':
			// Triggers "wear the shape" of the target port — the editor resolves
			// it at render time via the outgoing edge. Statically we expose an
			// empty pass-through port so handles render correctly. Delay is the
			// same: it forwards the inbound token unchanged on its one output.
			return [{ id: 'out', label: 'Output', fields: [] }];
		case 'join':
			// Join carries an explicit output Port whose fields describe what
			// downstream `<slug>.<field>` borrows can read.
			return [data.output ?? { id: 'out', label: 'Output', fields: [] }];
		default:
			return [];
	}
}

export function inputPortsFor(data: WorkflowNodeData): Port[] {
	switch (data.type) {
		case 'start':
			return [];
		case 'end':
			return data.terminal ? [data.terminal] : [];
		case 'automated_step':
			return data.input ? [data.input] : [];
		case 'human_task':
		case 'decision':
		case 'parallel_split':
		case 'join':
		case 'loop':
		case 'delay':
		case 'timeout':
		case 'scope':
		case 'phase_update':
		case 'progress_update':
		case 'failure':
			return [{ id: 'in', label: 'Input', fields: [] }];
		case 'trigger':
			// Triggers are never edge targets — no input port.
			return [];
		default:
			return [];
	}
}

type HumanTaskNodeData = Extract<WorkflowNodeData, { type: 'human_task' }>;
type DecisionNodeData = Extract<WorkflowNodeData, { type: 'decision' }>;
type AgentNodeData = Extract<WorkflowNodeData, { type: 'agent' }>;

// Metadata fields always present in the runtime LLM envelope — appended after
// the content field(s). Mirrors the tail of `backends::llm::derive_output_port`.
const LLM_METADATA_FIELDS: PortField[] = [
	{ name: 'usage', label: 'Token usage', kind: 'json' },
	{ name: 'finish_reason', label: 'Finish reason', kind: 'text' },
	{ name: 'model', label: 'Model', kind: 'text' }
];

function llmTextResponseField(): PortField {
	return { name: 'response', label: 'Response', kind: 'textarea' };
}

// TS twin of `backends::llm::kind_from_json_schema`.
// Delegates to the canonical fromJsonType adapter; the string→textarea-by-format
// branch is inspected here (this caller can see prop.format; fromJsonType cannot).
// The `hasEnum` path is not relevant in the LLM output-field context (json_schema
// response_format properties are not keyed by enum), so we always pass false.
function kindFromJsonSchema(prop: Record<string, unknown>): FieldKind {
	const ty = typeof prop.type === 'string' ? prop.type : 'unknown';
	const format = typeof prop.format === 'string' ? prop.format : undefined;
	// fromJsonType is total/exhaustive — any unrecognised ty maps to 'json'.
	// Cast: ty here is unconstrained string from JSON; the adapter handles
	// unknown values via its `default:never` guard by mapping 'unknown'→json.
	const jsonType = (
		ty === 'string' || ty === 'integer' || ty === 'number' ||
		ty === 'boolean' || ty === 'array' || ty === 'object'
			? ty
			: 'unknown'
	) as import('$lib/fields/adapters').JsonType;
	return fromJsonType(jsonType, { hasEnum: false, format }) as FieldKind;
}

// TS twin of `backends::llm::derive_output_port` — the LLM success output a
// single-shot Agent unpacks from its `response_format`. Display-only (canvas
// card + read-only DerivedPortsSection); the compiler + variable picker derive
// the same shape server-side, which stays the source of truth.
//
// `{"$ref": "#/definitions/X"}` schemas can't be resolved here (this twin gets
// only the node data, not the workflow `definitions`), so they degrade to the
// text-mode fallback in the editor display — the server-backed picker/compile
// resolve them correctly via the compile-entry pre-pass.
function deriveLlmSuccessFields(responseFormat: unknown): PortField[] {
	const rf = (responseFormat ?? {}) as Record<string, unknown>;
	const fields: PortField[] = [];

	if (rf.type === 'json_schema') {
		const schema =
			rf.schema && typeof rf.schema === 'object'
				? (rf.schema as Record<string, unknown>)
				: {};
		const schemaType = typeof schema.type === 'string' ? schema.type : undefined;
		const props =
			schema.properties && typeof schema.properties === 'object'
				? (schema.properties as Record<string, Record<string, unknown>>)
				: undefined;
		const required = new Set(
			Array.isArray(schema.required)
				? (schema.required as unknown[]).filter((s): s is string => typeof s === 'string')
				: []
		);

		if (schemaType === 'object' && props) {
			// Match the server: `serde_json::Map` iterates alphabetically (no
			// `preserve_order`), so emit properties in sorted-key order.
			for (const name of Object.keys(props).sort()) {
				const prop = props[name] ?? {};
				fields.push({
					name,
					label: typeof prop.title === 'string' ? prop.title : name,
					kind: kindFromJsonSchema(prop),
					required: required.has(name),
					description: typeof prop.description === 'string' ? prop.description : undefined
				});
			}
		} else if (
			schemaType === 'string' ||
			schemaType === 'integer' ||
			schemaType === 'number' ||
			schemaType === 'boolean' ||
			schemaType === 'array'
		) {
			// Root-level scalar/array → a single `response` field typed by the schema.
			fields.push({
				name: 'response',
				label: typeof schema.title === 'string' ? schema.title : 'Response',
				kind: kindFromJsonSchema(schema),
				description: typeof schema.description === 'string' ? schema.description : undefined
			});
		} else {
			// Object with no properties / unrecognized → text-mode fallback.
			fields.push(llmTextResponseField());
		}
	} else {
		// text mode (or no response_format).
		fields.push(llmTextResponseField());
	}

	fields.push(...LLM_METADATA_FIELDS);
	return fields;
}

const AGENT_LOOP_EXTRAS: PortField[] = [
	{
		name: 'turn',
		label: 'Final turn count',
		kind: 'number',
		description: 'Number of LLM round-trips before the agent exited.'
	},
	{
		name: 'history',
		label: 'Conversation history',
		kind: 'json',
		description: 'Array of `{role, content, …}` entries the agent sent + received.'
	},
	{
		name: 'final_response',
		label: 'Full LLM turn result',
		kind: 'json',
		description: 'The last `LlmTurnResult` (content, tool_calls, stop_reason, usage).'
	},
	{
		name: 'input',
		label: 'Original input',
		kind: 'json',
		description: 'The inbound token the agent received.'
	}
];

const REF_PREFIX = '#/definitions/';
const REF_DEPTH_CAP = 64;

// Client mirror of `compiler::schema_refs::inline_refs`: deep-resolve local
// `{"$ref":"#/definitions/<name>"}` pointers against the workflow definitions
// (sourced from the loaded template — they're absent from the Yjs doc).
// Display-only + permissive: an unresolved/cyclic ref is left/blanked rather
// than thrown (the server-side pass is the authoritative validator).
function resolveDefinitionRefs(
	value: unknown,
	definitions: Record<string, unknown>,
	depth = 0,
	seen: Set<string> = new Set()
): unknown {
	if (depth > REF_DEPTH_CAP) return value;
	if (Array.isArray(value)) {
		return value.map((v) => resolveDefinitionRefs(v, definitions, depth + 1, seen));
	}
	if (value && typeof value === 'object') {
		const obj = value as Record<string, unknown>;
		const ref = obj['$ref'];
		if (typeof ref === 'string' && ref.startsWith(REF_PREFIX)) {
			const name = ref.slice(REF_PREFIX.length);
			if (seen.has(name)) return {}; // cycle → bail (display-only)
			const target = definitions[name];
			if (target === undefined) return obj; // unresolved → leave as-is (text fallback)
			const nextSeen = new Set(seen);
			nextSeen.add(name);
			return resolveDefinitionRefs(target, definitions, depth + 1, nextSeen);
		}
		const out: Record<string, unknown> = {};
		for (const [k, v] of Object.entries(obj)) {
			out[k] = resolveDefinitionRefs(v, definitions, depth + 1, seen);
		}
		return out;
	}
	return value;
}

function deriveAgentOutputPorts(data: AgentNodeData): Port[] {
	// Content field(s) derived from response_format (json_schema → schema
	// fields; text → `response`) + the always-present metadata tail — matching
	// `nodes::agent::output_ports`. Loop-path agents (`max_turns > 1 ||
	// stop_when`) additionally expose turn/history/final_response/input.
	//
	// Resolve `{"$ref":"#/definitions/X"}` against the editor's workflow
	// definitions first so a $ref schema expands to its fields, same as the
	// server's compile-entry pre-pass. Reading the store here is reactive: the
	// card/panel re-derive once the template (and its definitions) load.
	const takesLoopPath = (data.maxTurns ?? 1) > 1 || !!data.stopWhen;
	const responseFormat = resolveDefinitionRefs(data.responseFormat, getWorkflowDefinitions());
	const fields = deriveLlmSuccessFields(responseFormat);
	if (takesLoopPath) fields.push(...AGENT_LOOP_EXTRAS);
	return [
		{ id: 'out', label: 'Output', fields },
		{ id: 'error', label: 'On error', fields: [] }
	];
}

function deriveHumanTaskOutputPort(data: HumanTaskNodeData): Port {
	const seen = new Set<string>();
	const fields: PortField[] = [];
	for (const step of data.steps ?? []) {
		for (const block of step.blocks ?? []) {
			if (block.type !== 'input') continue;
			const f = block.field;
			if (seen.has(f.name)) continue;
			seen.add(f.name);
			fields.push({
				name: f.name,
				label: f.label,
				kind: taskFieldKindToFieldKind(f.kind),
				required: f.required ?? false,
				options: f.options ?? undefined
			});
		}
	}
	return { id: 'out', label: 'Output', fields };
}

function deriveDecisionOutputPorts(data: DecisionNodeData): Port[] {
	const out: Port[] = (data.conditions ?? []).map((c) => ({
		id: c.edgeId,
		label: c.label,
		fields: []
	}));
	if (data.defaultBranch) {
		out.push({ id: data.defaultBranch, label: 'Default', fields: [] });
	}
	return out;
}

// Delegates to the canonical fromTaskFieldKind adapter (total, exhaustive —
// all 11 TaskFieldKind values are handled including radio/date/range/rating
// which the old `default: 'text'` silently swallowed).
// The return type is cast to FieldKind (the port-wire type) because canonical
// FieldKind is a superset; any canonical value that is NOT a valid port FieldKind
// (e.g. 'radio', 'range', 'rating') will be stored in the derived PortField.kind
// for display purposes only — the compiler + engine do not see these fields.
function taskFieldKindToFieldKind(k: TaskFieldKind): FieldKind {
	return fromTaskFieldKind(k) as FieldKind;
}

/**
 * Whether the editor offers a UI to edit this node kind's ports. Used by the
 * NodePropertyPanel to switch between an editable PortsSection (Start,
 * AutomatedStep, End.terminal, Scope) and a read-only "Derived" summary.
 */
export function hasEditableOutputPorts(kind: WorkflowNodeData['type']): boolean {
	return kind === 'start' || kind === 'automated_step';
}

export function hasEditableInputPorts(kind: WorkflowNodeData['type']): boolean {
	return kind === 'end' || kind === 'automated_step';
}
