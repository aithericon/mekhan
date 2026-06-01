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
import SmtpEnvelope from './SmtpEnvelope.svelte';
import PostgresResultEnvelope from './PostgresResultEnvelope.svelte';
import EndTerminalEnvelope from './EndTerminalEnvelope.svelte';
import ProcessTokenEnvelope from './ProcessTokenEnvelope.svelte';
import FileReference from './FileReference.svelte';
import TabularArray from './TabularArray.svelte';
import KeyValueList from './KeyValueList.svelte';
import StorageRefValue from './StorageRefValue.svelte';
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

/** Workflow-exit terminal envelope deposited at `workflow_terminals[*]` by
 *  End / Failure nodes. Built by `lower_end`'s result_shape transition
 *  (`exit_code: { ok: true, value: <result_mapping> }`) and `lower_failure`
 *  (`exit_code: { ok: false, error }`), riding atop the process token's
 *  workflow-level `name` / `process_id` / `status` fields (plus an optional
 *  `task_id` when a HumanTask ran on the path — pure-automated workflows
 *  reach End with no task_id ever stamped). Distinguishing signature: the
 *  `name + process_id + status` triple. `exit_code` is treated as optional
 *  so bare-End terminals (no result mapping) still match and render the
 *  metadata cleanly. Accepts both `end` and `failure` node kinds because
 *  `lower_failure` emits the identical shape — only the `exit_code.ok` arm
 *  differs. */
function matchesEndTerminal(value: unknown, ctx: RenderContext): boolean {
	if (!isObj(value)) return false;
	if (typeof value.process_id !== 'string') return false;
	if (typeof value.status !== 'string') return false;
	if (typeof value.name !== 'string') return false;
	if ('exit_code' in value && value.exit_code !== null && !isObj(value.exit_code)) return false;
	return ctx.nodeKind === undefined || ctx.nodeKind === 'end' || ctx.nodeKind === 'failure';
}

/** Bare result envelope — the `exit_code` payload on its own, as stored on
 *  `WorkflowInstance.result` once the run terminates: `{ok: true, value}` or
 *  `{ok: false, error}`. Same successful-path/failed-path UX as the full
 *  End-terminal envelope, just without the workflow-metadata frame, so we
 *  route it to the same component. */
function matchesResultEnvelope(value: unknown): boolean {
	if (!isObj(value)) return false;
	if (typeof value.ok !== 'boolean') return false;
	// Reject anything that already has the End-terminal frame; that goes
	// through `matchesEndTerminal` for the richer metadata strip.
	if (typeof value.name === 'string' && typeof value.process_id === 'string') return false;
	return value.ok ? 'value' in value : 'error' in value;
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

const SMTP_OUTCOME_TYPES = new Set([
	'success',
	'template_render',
	'invalid_address',
	'invalid_config',
	'connect_failed',
	'tls_failed',
	'auth_failed',
	'recipient_rejected',
	'server_error',
	'timeout',
	'attachment_error'
]);

/** SMTP backend's outputs map — `{outcome: {type: <SMTP reason>}, subject:
 *  string, body_text_preview?: string, body_html_preview?: string}`. The
 *  distinguishing signature is the outcome.type drawn from `SMTP_OUTCOME_TYPES`
 *  (wire-stable per `executor-smtp::outcome::SmtpOutcome::reason()`). The
 *  generic AutomatedStep envelope still wraps this — the SMTP renderer
 *  catches the leaf outputs map specifically. */
function matchesSentEmail(value: unknown): boolean {
	if (!isObj(value)) return false;
	const outcome = value.outcome;
	if (!isObj(outcome)) return false;
	if (typeof outcome.type !== 'string') return false;
	if (!SMTP_OUTCOME_TYPES.has(outcome.type)) return false;
	// Subject preview is always present on SMTP output. Distinguishes from
	// arbitrary tagged outcome objects from other backends.
	return typeof value.subject === 'string';
}

/** Postgres backend's result envelope — `{rows: array, row_count?: number,
 *  rows_affected?: number|null}` (see `executor-postgres`). The distinguishing
 *  signature is an array `rows` field paired with at least one of the numeric
 *  count fields, which keeps it from colliding with arbitrary objects that
 *  happen to carry a `rows` array. */
function matchesPostgresResult(value: unknown): boolean {
	if (!isObj(value)) return false;
	if (!Array.isArray(value.rows)) return false;
	const hasRowCount = typeof value.row_count === 'number';
	const hasRowsAffected = 'rows_affected' in value;
	return hasRowCount || hasRowsAffected;
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

/** An S3 object key the backend serves at `/api/v1/files/{key}` — the agent's
 *  `history_ref` (`instances/{id}/{node}/turn-N.json`), a config blob, an
 *  artifact. Must out-rank `matchesPrimitive` (a key is a string). The
 *  known-prefix anchor + no-whitespace keeps free-text outputs (an LLM
 *  `response`/`final_response`, a sentence) from matching. */
function matchesStorageRef(value: unknown): boolean {
	return typeof value === 'string' && /^(instances|templates|artifacts)\/\S+\.\w+$/.test(value);
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
		name: 'smtp-result',
		label: 'Email send result',
		matches: matchesSentEmail,
		component: SmtpEnvelope
	},
	{
		name: 'postgres-result',
		label: 'Postgres query result',
		matches: matchesPostgresResult,
		component: PostgresResultEnvelope
	},
	{
		name: 'end-terminal',
		label: 'Workflow result',
		matches: matchesEndTerminal,
		component: EndTerminalEnvelope
	},
	{
		name: 'result-envelope',
		label: 'Workflow result',
		matches: matchesResultEnvelope,
		component: EndTerminalEnvelope
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
		name: 'storage-ref',
		label: 'Storage reference',
		matches: matchesStorageRef,
		component: StorageRefValue
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
