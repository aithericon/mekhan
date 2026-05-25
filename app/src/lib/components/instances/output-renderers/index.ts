/**
 * Registered renderers, ordered most-specific → most-general.  Each entry's
 * `matches` predicate is the only thing dispatch reads — predicates must be
 * structurally safe on arbitrary JSON (no throws on missing keys, nested
 * arrays, nulls, etc.) since the dispatcher walks the list in order and takes
 * the first hit.
 */
import HumanTaskEnvelope from './HumanTaskEnvelope.svelte';
import AutomatedStepEnvelope from './AutomatedStepEnvelope.svelte';
import LlmResponseEnvelope from './LlmResponseEnvelope.svelte';
import KreuzbergExtractionEnvelope from './KreuzbergExtractionEnvelope.svelte';
import ProcessTokenEnvelope from './ProcessTokenEnvelope.svelte';
import FileReference from './FileReference.svelte';
import TabularArray from './TabularArray.svelte';
import KeyValueList from './KeyValueList.svelte';
import PrimitiveValue from './PrimitiveValue.svelte';
import JsonBlock from './JsonBlock.svelte';
import type { OutputRenderer, RenderContext } from './types';

export { default as SmartValue } from './SmartValue.svelte';
export type { RenderContext, RenderPosition } from './types';

function isObj(v: unknown): v is Record<string, unknown> {
	return !!v && typeof v === 'object' && !Array.isArray(v);
}

// ── Renderer predicates ──────────────────────────────────────────────────────

/** HumanTask response envelope — see `service/src/compiler/token_shape.rs`
 *  `WorkflowNodeData::HumanTask` arm. Stable signature is `{task_id, status,
 *  data: object}`. Optional `nodeKind === 'human_task'` confirms.  */
function matchesHumanTask(value: unknown, ctx: RenderContext): boolean {
	if (!isObj(value)) return false;
	if (typeof value.task_id !== 'string') return false;
	if (typeof value.status !== 'string') return false;
	if (!isObj(value.data)) return false;
	// Either the shape matches AND the producer is a HumanTask (strong), or
	// the shape matches and we don't know the kind (we still trust the shape —
	// the `data` envelope key isn't a coincidence).
	return ctx.nodeKind === undefined || ctx.nodeKind === 'human_task';
}

/** Executor result envelope from an AutomatedStep — see
 *  `service/src/compiler/token_shape.rs` `WorkflowNodeData::AutomatedStep` arm.
 *  Stable signature is `{execution_id, job_id, detail: {outputs, outcome,
 *  progress, ...}}`. The renderer leads with `detail.outputs` (the actual
 *  business result) and surfaces metrics + phase timeline + logs/streams. */
function matchesAutomatedStep(value: unknown, ctx: RenderContext): boolean {
	if (!isObj(value)) return false;
	if (typeof value.execution_id !== 'string') return false;
	if (typeof value.job_id !== 'string') return false;
	if (!isObj(value.detail)) return false;
	return ctx.nodeKind === undefined || ctx.nodeKind === 'automated_step';
}

/** Process-rooted token (carrying `_instance_id` stamped by Start, plus
 *  the other `_*` system fields and the declared business fields). Also
 *  matches the inbound at HumanTask after the wire-edge injection merges
 *  in form scaffold. The renderer hides the noise (scaffold + metadata
 *  disclosures) and surfaces the business fields. */
function matchesProcessToken(value: unknown): boolean {
	if (!isObj(value)) return false;
	return typeof value._instance_id === 'string';
}

/** Kreuzberg `ExtractionResult` envelope, emitted 1:1 by `executor-kreuzberg`
 *  (see `executor/crates/executor-kreuzberg/src/backend.rs::build_single_outputs`).
 *  Stable signature is `content: string` + `mime_type: string` +
 *  `metadata: object` + `tables: array` — distinctive enough that other LLM /
 *  envelope shapes won't collide. The renderer surfaces extracted text and
 *  per-table markdown bodies first, hides the diagnostic `metadata` blob
 *  behind a disclosure. */
function matchesKreuzbergExtraction(value: unknown): boolean {
	if (!isObj(value)) return false;
	if (typeof value.content !== 'string') return false;
	if (typeof value.mime_type !== 'string') return false;
	if (!isObj(value.metadata)) return false;
	if (!Array.isArray(value.tables)) return false;
	return true;
}

/** Canonical LLM output envelope from `executor-llm` (see
 *  `executor/crates/executor-llm/src/backend.rs:203-212`). Signature is
 *  `{response, model, usage, finish_reason}`. The renderer prints the
 *  response prominently below a compact metadata strip so the markdown
 *  body doesn't get squeezed into KeyValueList's right column. */
function matchesLlmResponse(value: unknown): boolean {
	if (!isObj(value)) return false;
	// `response` may be string or JSON depending on the spec's output
	// declarations; `model` is always a string. Pair them as the
	// distinguishing signature.
	if (typeof value.model !== 'string') return false;
	if (!('response' in value)) return false;
	// Distinguish from arbitrary objects that happen to have a `model`
	// field by also requiring `usage` or `finish_reason` (one of the
	// other backend.rs-stamped keys).
	return 'usage' in value || 'finish_reason' in value;
}

/** Catalogue file reference — `{url, filename?, content_type?}`. */
function matchesFileRef(value: unknown): boolean {
	if (!isObj(value)) return false;
	return typeof value.url === 'string';
}

/** Array of similar objects — useful when the rows share at least one key. */
function matchesTabular(value: unknown): boolean {
	if (!Array.isArray(value) || value.length === 0) return false;
	// Require the first element to be a plain object; tolerate heterogeneity
	// past that (TabularArray itself unions keys across rows).
	const first = value[0];
	if (!isObj(first)) return false;
	// Reject single-row arrays — they don't benefit from a table.
	if (value.length === 1) return false;
	return true;
}

/** Flat-ish object — every value is either a primitive or a single nested
 *  object (typically a file ref). Punts to JsonBlock for deeply nested shapes
 *  that would render awkwardly. */
function matchesKeyValue(value: unknown): boolean {
	if (!isObj(value)) return false;
	const entries = Object.entries(value);
	if (entries.length === 0) return false;
	for (const [, v] of entries) {
		if (v === null || v === undefined) continue;
		if (typeof v !== 'object') continue; // primitive ok
		if (Array.isArray(v)) {
			// Allow arrays of primitives; reject arrays-of-objects so the user
			// gets a proper TabularArray when they should.
			for (const item of v) {
				if (item !== null && typeof item === 'object') return false;
			}
			continue;
		}
		// Nested object: only fine if it's a file ref (renders inline).
		if (!matchesFileRef(v)) return false;
	}
	return true;
}

/** Strings, numbers, booleans, null. */
function matchesPrimitive(value: unknown): boolean {
	return value === null || value === undefined || typeof value !== 'object';
}

export const REGISTRY: OutputRenderer[] = [
	{
		name: 'human-task',
		label: 'Human task response',
		matches: matchesHumanTask,
		component: HumanTaskEnvelope
	},
	{
		name: 'automated-step',
		label: 'Automated step result',
		matches: matchesAutomatedStep,
		component: AutomatedStepEnvelope
	},
	{
		name: 'llm-response',
		label: 'LLM response',
		matches: matchesLlmResponse,
		component: LlmResponseEnvelope
	},
	{
		name: 'kreuzberg-extraction',
		label: 'Document extraction',
		matches: matchesKreuzbergExtraction,
		component: KreuzbergExtractionEnvelope
	},
	{
		name: 'process-token',
		label: 'Process token',
		matches: matchesProcessToken,
		component: ProcessTokenEnvelope
	},
	{
		name: 'file-ref',
		label: 'File reference',
		matches: matchesFileRef,
		component: FileReference
	},
	{
		name: 'tabular',
		label: 'Table',
		matches: matchesTabular,
		component: TabularArray
	},
	{
		name: 'key-value',
		label: 'Fields',
		matches: matchesKeyValue,
		component: KeyValueList
	},
	{
		name: 'primitive',
		label: 'Value',
		matches: matchesPrimitive,
		component: PrimitiveValue
	}
];

export const FALLBACK: OutputRenderer = {
	name: 'json',
	label: 'JSON',
	matches: () => true,
	component: JsonBlock
};

export function pickRenderer(value: unknown, ctx: RenderContext): OutputRenderer {
	for (const r of REGISTRY) {
		if (r.matches(value, ctx)) return r;
	}
	return FALLBACK;
}
